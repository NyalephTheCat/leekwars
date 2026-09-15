//! Main driver logic.

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use leek_diagnostics::ColorWhen;
use leek_fmt::FormatOptions;
use leek_project::Project;
use leek_session::{Compilation, DriverConfig, Session};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version, build_flat_tree};

use crate::cli::{Cli, Emit};
use crate::pipeline::{ENTRY_SOURCE, opt_for, resolve_code, shape_for};
use crate::print::{print_cst, print_hir, print_mir, print_tokens};

pub fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    // Load formatter options for `--emit fmt`. The `--fmt-config`
    // flag points at a `Miku.toml`-style file; absent, defaults.
    let fmt_opts = match &cli.fmt_config {
        None => FormatOptions::default(),
        // `ManifestError` is a `std::error::Error`, so `?` keeps the typed
        // value inside the `anyhow::Error` instead of flattening it to prose.
        // `leekc` has no reporter for a manifest, so it renders as one line —
        // but a caller that wants the span can still downcast for it.
        Some(path) => leek_manifest::load_from(path).map(|load| load.manifest.format)?,
    };

    // Validate the severity flags before anything else, so an unknown code
    // is a usage error (exit 2) with the catalog hint rather than the
    // session's "[lint] in Miku.toml" — there is no Miku.toml here.
    for code in cli.deny.iter().chain(&cli.warn).chain(&cli.allow) {
        resolve_code(code)?;
    }

    // Load + register any host-environment libraries (`--library leekwars`,
    // `--library path/to.lib`) BEFORE the session opens, so their functions
    // are recognized by the resolver (no "undefined function") and nothing
    // is memoized without them. The composed catalog is reused for backend
    // emit below.
    let environment: Option<std::sync::Arc<dyn leek_environment::EnvironmentCatalog>> =
        if cli.libraries.is_empty() {
            None
        } else {
            let cat = leek_session::load_and_register_libraries(&cli.libraries)
                .context("loading library")?;
            Some(std::sync::Arc::new(cat))
        };

    // Opt-in: register the library's constant values for folding so HIR
    // lowering replaces e.g. `WEAPON_PISTOL` with `37` for every backend
    // (Java, MIR, native) from the one hook. Through the shared helper,
    // which `miku`'s `[project] fold_constants` and the official-parity
    // fight runners already use — this used to repeat its body inline, so
    // `leekc --fold-constants` could fold a different set from every other
    // driver (#132).
    if cli.fold_constants {
        leek_session::activate_leekwars_constant_folding();
    }

    // `leekc` is handed a file, not a project. It compiles through the same
    // `Session` every `miku` subcommand does — one query database, one
    // reporter, one `Compilation` — over the synthetic one-file project
    // that file belongs to. The flags that a project would spell in
    // `Miku.toml` are written into it here, so there is one configuration
    // path rather than a manifest-ful and a manifest-less copy of each.
    let mut project = Project::standalone(&cli.input);
    project.manifest.lint.deny.clone_from(&cli.deny);
    project.manifest.lint.warn.clone_from(&cli.warn);
    project.manifest.lint.allow.clone_from(&cli.allow);
    project.manifest.format = fmt_opts.clone();
    // `--version-pragma` outranks the file's own `@version`, which is what
    // makes it an override rather than the index's default.
    project.index_mut().version_override = cli.version_pragma.map(u8::from);

    let (target, scope) = shape_for(cli.emit);
    let session = Session::new(
        &project,
        DriverConfig {
            target,
            scope,
            params: leek_session::driver_params()
                .with_opt(opt_for(cli.emit, cli.clean))
                .with_lints(leek_query::LintGroups {
                    pedantic: cli.pedantic,
                    nursery: cli.nursery,
                }),
            color: if cli.no_color {
                ColorWhen::Never
            } else {
                ColorWhen::Auto
            },
            format: cli.message_format.into(),
            timing: None,
        },
    )?;

    // The compiled file, its source map and the reporter as one value — the
    // same one `miku` holds, so a backend diagnostic here renders exactly as
    // it does there, and one raised inside an included file is shown against
    // *that* file's text and path rather than the entry's.
    let compiled: Compilation<'_> =
        session.compile_file(&cli.input, SourceId::new(ENTRY_SOURCE).unwrap())?;
    let text = compiled.text().to_string();
    let version = Version::from_byte(compiled.input().version_byte);
    let had_error = compiled.report();

    match cli.emit {
        Emit::Check => {}
        Emit::Tokens => {
            if let Some(tokens) = compiled.tokens() {
                print_tokens(&text, tokens);
            }
        }
        Emit::FlatCst => {
            if let Some(tokens) = compiled.tokens() {
                let green = build_flat_tree(&text, tokens);
                let node = SyntaxNode::new_root(green);
                print_cst(&node, 0);
            }
        }
        Emit::Cst => {
            if let Some(green) = compiled.green_tree() {
                let node = SyntaxNode::new_root(green);
                print_cst(&node, 0);
            }
        }
        Emit::Hir => {
            if let Some(hir) = compiled.hir() {
                print_hir(hir);
            } else {
                eprintln!("leekc: parse failed; no HIR to emit");
            }
        }
        Emit::Mir => {
            if let Some(mir) = compiled.mir() {
                print_mir(mir);
            } else {
                eprintln!("leekc: parse failed; no MIR to emit");
            }
        }
        Emit::Java => {
            if let Some(hir) = compiled.hir() {
                let mut opts = leek_backends::java_options(
                    compiled.input(),
                    cli.ai_id,
                    cli.clean,
                    cli.input.display().to_string(),
                    environment.as_ref(),
                );
                // `--base-class` has no `miku` counterpart, so it stays
                // here rather than in the shared builder.
                if let Some(base) = &cli.base_class {
                    opts = opts.with_base_class(base);
                }
                let out = leek_backend_java::emit(hir, &opts);
                // A construct the emitter has no shape for produces Java that
                // javac rejects — or that compiles to something else. Render
                // it against the Leek source and stop rather than writing a
                // file that is not a translation of this program (#152).
                if compiled.report_backend(&out.diagnostics) {
                    return Ok(ExitCode::from(1));
                }
                match &cli.out_dir {
                    Some(dir) => {
                        std::fs::create_dir_all(dir)
                            .with_context(|| format!("creating {}", dir.display()))?;
                        let java_path = dir.join(format!("{}.java", out.class_name));
                        let lines_path = dir.join(format!("{}.lines", out.class_name));
                        std::fs::write(&java_path, &out.java)
                            .with_context(|| format!("writing {}", java_path.display()))?;
                        std::fs::write(&lines_path, &out.lines)
                            .with_context(|| format!("writing {}", lines_path.display()))?;
                        eprintln!("wrote {} and {}", java_path.display(), lines_path.display());
                    }
                    None => {
                        print!("{}", out.java);
                    }
                }
            } else {
                eprintln!("leekc: parse failed; no Java to emit");
            }
        }
        Emit::LeekScript => {
            if let Some(hir) = compiled.hir() {
                let opts =
                    leek_backends::leekscript_options(compiled.input(), cli.compact, cli.optimize);
                let out = leek_backend_leekscript::emit(hir, &opts);
                // Semantics this backend could not carry across (#154). These
                // are warnings — the emitted program is valid LeekScript — so
                // they print and the file is still written, unless a `--deny`
                // promotes one.
                if compiled.report_backend(&out.diagnostics) {
                    return Ok(ExitCode::from(1));
                }
                match &cli.out_dir {
                    Some(dir) => {
                        std::fs::create_dir_all(dir)
                            .with_context(|| format!("creating {}", dir.display()))?;
                        let stem = cli.input.file_stem().map_or_else(
                            || "out".to_string(),
                            |s| s.to_string_lossy().into_owned(),
                        );
                        let path = dir.join(format!("{stem}.out.leek"));
                        std::fs::write(&path, &out.source)
                            .with_context(|| format!("writing {}", path.display()))?;
                        eprintln!("wrote {}", path.display());
                    }
                    None => print!("{}", out.source),
                }
            } else {
                eprintln!("leekc: parse failed; no LeekScript to emit");
            }
        }
        Emit::Fmt => {
            // Straight off the database rather than through a
            // `Compilation` accessor: the formatter is a tool, and
            // `crates/db` may not depend on `crates/tools` — `db_handle`
            // is the seam that lets a front-end ask a tool's query about
            // a session's file anyway.
            if let Some(formatted) = compiled.db_handle().map(|(db, file)| {
                leek_fmt::query::format_query(
                    db,
                    file,
                    leek_fmt::query::FormatConfig::new(db, fmt_opts.clone()),
                )
                .text
            }) {
                // The formatter's output is unverified. Print nothing
                // rather than corrupt LeekScript when it would change the
                // program — same policy as `miku fmt`.
                if let Err(err) = leek_fmt::check_equivalence(&text, &formatted, version) {
                    eprintln!(
                        "error: refusing to emit formatted source: {err} (please report this)"
                    );
                    return Ok(ExitCode::from(1));
                }
                print!("{formatted}");
            } else {
                eprintln!("leekc: parse failed; no formatted output");
            }
        }
        Emit::Run => {
            if let Some(hir) = compiled.hir() {
                // `--emit run` executes via the native JIT (the interpreter was
                // removed), with the same helper and budget as `miku run`.
                use leek_backend_native::{DEFAULT_OP_BUDGET, NativeArtifact, NativeOptions};
                let mut opts = NativeOptions::jit_for_input(compiled.input(), DEFAULT_OP_BUDGET);
                if let Some(depth) = cli.max_call_depth {
                    opts.max_call_depth = depth;
                }
                match leek_backend_native::compile(hir, &opts) {
                    Ok(NativeArtifact::Value(v)) => println!("{v}"),
                    Ok(_) => unreachable!("Jit emit yields a Value"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return Ok(ExitCode::from(1));
                    }
                }
            } else {
                eprintln!("leekc: parse failed; cannot run");
            }
        }
        Emit::Native => {
            if let Some(hir) = compiled.hir() {
                use crate::cli::{NativeEmitArg, OptLevelArg};
                use leek_backend_native::{NativeArtifact, NativeEmit, NativeOptions, OptLevel};

                let mut opts = if cli.release {
                    NativeOptions::release()
                } else {
                    NativeOptions::debug()
                };
                if let Some(lvl) = cli.opt_level {
                    opts.opt_level = match lvl {
                        OptLevelArg::None => OptLevel::None,
                        OptLevelArg::Speed => OptLevel::Speed,
                        OptLevelArg::SpeedAndSize => OptLevel::SpeedAndSize,
                    };
                }
                opts.debug_info = cli.debug_info || opts.debug_info;
                if cli.no_verifier {
                    opts.enable_verifier = false;
                }
                let input = compiled.input();
                opts = opts.with_lang(input.version_byte, input.strict);
                opts.link_game = cli.link_game;
                if let Some(depth) = cli.max_call_depth {
                    opts.max_call_depth = depth;
                }
                let obj_path = cli
                    .native_out
                    .clone()
                    .unwrap_or_else(|| std::path::PathBuf::from("out.o"));
                if cli.native_emit == NativeEmitArg::Exe {
                    // AOT: compile to a standalone executable. The program runs
                    // unbounded (no per-run op budget), like a normal binary.
                    opts.op_limit = u64::MAX;
                    let out = cli
                        .native_out
                        .clone()
                        .unwrap_or_else(|| std::path::PathBuf::from("a.out"));
                    if let Err(e) =
                        leek_backend_native::aot::compile_to_executable(hir, &opts, &out, false)
                    {
                        eprintln!("native: {e}");
                        return Ok(ExitCode::from(1));
                    }
                    return Ok(ExitCode::SUCCESS);
                }
                opts.emit = match cli.native_emit {
                    NativeEmitArg::Run => NativeEmit::Jit,
                    NativeEmitArg::Clif => NativeEmit::Clif,
                    NativeEmitArg::Asm => NativeEmit::Disasm,
                    NativeEmitArg::Object => NativeEmit::Object(obj_path.clone()),
                    NativeEmitArg::Exe => unreachable!("handled above"),
                };
                match leek_backend_native::compile(hir, &opts) {
                    Ok(NativeArtifact::Value(v)) => println!("{v}"),
                    Ok(NativeArtifact::Text(t)) => println!("{t}"),
                    Ok(NativeArtifact::Object) => {
                        eprintln!("wrote object to {}", obj_path.display());
                    }
                    Err(e) => {
                        eprintln!("native: {e}");
                        return Ok(ExitCode::from(1));
                    }
                }
            } else {
                eprintln!("leekc: parse failed; cannot compile");
            }
        }
    }

    Ok(if had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
