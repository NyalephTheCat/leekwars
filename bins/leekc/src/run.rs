//! Main driver logic.

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use leek_diagnostics::{Renderer, SeverityConfig};
use leek_fmt::FormatOptions;
use leek_fmt::pipeline::FormattedArtifact;
use leek_hir::pipeline::HirArtifact;
use leek_lexer::pipeline::TokensArtifact;
use leek_mir::pipeline::MirArtifact;
use leek_parser::pipeline::GreenTreeArtifact;
use leek_pipeline::Input;
use leek_span::pragma::LanguageSettings;
use leek_span::{LineTable, SourceId};
use leek_syntax::{SyntaxNode, Version, build_flat_tree};

use crate::cli::{Cli, Emit, MessageFormat};
use crate::pipeline::{is_stderr_tty, pipeline_for, resolve_code};
use crate::print::{print_cst, print_hir, print_mir, print_tokens};

pub fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let text = std::fs::read_to_string(&cli.input)
        .with_context(|| format!("reading {}", cli.input.display()))?;

    let source = SourceId::new(1).unwrap();

    // Settle the language settings once, here at the input boundary:
    // `--version-pragma` > the file's `@version` pragma > v4, and
    // `@strict`. Every pass (lexer through HIR) and every backend reads
    // them back from the `Input`; nothing re-derives them from pragmas.
    let lang = LanguageSettings::resolve(
        &text,
        cli.version_pragma.map(u8::from),
        leek_span::pragma::LATEST_VERSION,
        false,
    );
    let version = Version::from_byte(lang.version);
    let input = Input {
        source,
        text: text.clone().into(),
        version_byte: lang.version,
        strict: lang.strict,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };

    // Load formatter options for `--emit fmt`. The `--fmt-config`
    // flag points at a `Miku.toml`-style file; absent, defaults.
    let fmt_opts = match &cli.fmt_config {
        None => FormatOptions::default(),
        Some(path) => leek_manifest::load_from(path)
            .map(|load| load.manifest.format)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    };

    // Load + register any host-environment libraries (`--library leekwars`,
    // `--library path/to.lib`) BEFORE the pipeline runs, so their functions
    // are recognized by the resolver (no "undefined function"). The composed
    // catalog is reused for backend emit below.
    let environment: Option<std::sync::Arc<dyn leek_environment::EnvironmentCatalog>> =
        if cli.libraries.is_empty() {
            None
        } else {
            let cat = leek_recipes::load_and_register_libraries(&cli.libraries)
                .map_err(|e| anyhow::anyhow!("loading library: {e}"))?;
            Some(std::sync::Arc::new(cat))
        };

    // Opt-in: register the library's constant values for folding so HIR
    // lowering replaces e.g. `WEAPON_PISTOL` with `37` for every backend
    // (Java, MIR, native) from the one pipeline hook.
    if cli.fold_constants {
        leek_prelude::activate_fold_constants(
            leek_environment::leekwars_constant_values()
                .into_iter()
                .map(|(n, v)| (n.to_string(), v.to_string())),
        );
    }

    // Build a pipeline tailored to the requested emit. Each Emit
    // picks the shortest chain that produces the needed artifact;
    // result reuse comes for free within a single run.
    let pipeline = pipeline_for(
        cli.emit,
        fmt_opts,
        leek_pipeline::LintGroups {
            pedantic: cli.pedantic,
            nursery: cli.nursery,
        },
    );
    let result = pipeline.run(input);

    let mut sev_cfg = SeverityConfig::new();
    for code in &cli.deny {
        sev_cfg.deny(resolve_code(code)?);
    }
    for code in &cli.warn {
        sev_cfg.warn(resolve_code(code)?);
    }
    for code in &cli.allow {
        sev_cfg.allow(resolve_code(code)?);
    }

    let line_table = LineTable::new(&text);
    let renderer = if cli.no_color || cli.message_format == MessageFormat::Json {
        Renderer::default()
    } else if is_stderr_tty() {
        Renderer::ansi()
    } else {
        Renderer::default()
    };
    let file_label = cli.input.display().to_string();
    let mut had_error = false;
    for diag in result.diagnostics() {
        let mut adjusted = diag.clone();
        if !sev_cfg.apply_mut(&mut adjusted) {
            continue;
        }
        match cli.message_format {
            MessageFormat::Human => {
                let rendered = renderer.render(&adjusted, &text, &file_label, &line_table);
                eprint!("{rendered}");
            }
            MessageFormat::Json => {
                let json = serde_json::to_string(&adjusted).expect("diagnostic should serialize");
                println!("{json}");
            }
        }
        had_error |= matches!(adjusted.severity, leek_diagnostics::Severity::Error);
    }

    match cli.emit {
        Emit::Check => {}
        Emit::Tokens => {
            if let Some(tokens) = result.get::<TokensArtifact>() {
                print_tokens(&text, &tokens.0.tokens);
            }
        }
        Emit::FlatCst => {
            if let Some(tokens) = result.get::<TokensArtifact>() {
                let green = build_flat_tree(&text, &tokens.0.tokens);
                let node = SyntaxNode::new_root(green);
                print_cst(&node, 0);
            }
        }
        Emit::Cst => {
            if let Some(green) = result.get::<GreenTreeArtifact>() {
                let node = SyntaxNode::new_root(green.0.clone());
                print_cst(&node, 0);
            }
        }
        Emit::Hir => {
            if let Some(hir) = result.get::<HirArtifact>() {
                print_hir(hir.0.as_ref());
            } else {
                eprintln!("leekc: parse failed; no HIR to emit");
            }
        }
        Emit::Mir => {
            if let Some(mir) = result.get::<MirArtifact>() {
                print_mir(mir.0.as_ref());
            } else {
                eprintln!("leekc: parse failed; no MIR to emit");
            }
        }
        Emit::Java => {
            if let Some(hir) = result.get::<HirArtifact>() {
                let mut opts = if cli.clean {
                    leek_backend_java::Options::clean(version, cli.ai_id)
                } else {
                    leek_backend_java::Options::exact(version, cli.ai_id)
                }
                .with_source_path(cli.input.display().to_string());
                if let Some(env) = &environment {
                    opts = opts.with_environment(env.clone());
                }
                if let Some(base) = &cli.base_class {
                    opts = opts.with_base_class(base);
                }
                let out = leek_backend_java::emit(hir.0.as_ref(), &opts);
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
            if let Some(hir) = result.get::<HirArtifact>() {
                let mut opts = if cli.compact {
                    leek_backend_leekscript::Options::compact(version)
                } else {
                    leek_backend_leekscript::Options::pretty(version).with_source_text(text.clone())
                };
                opts = opts.with_optimize(cli.optimize).with_user_source(source);
                let out = leek_backend_leekscript::emit(hir.0.as_ref(), &opts);
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
            if let Some(artifact) = result.get::<FormattedArtifact>() {
                print!("{}", artifact.0);
            } else {
                eprintln!("leekc: parse failed; no formatted output");
            }
        }
        Emit::Run => {
            if let Some(hir) = result.get::<HirArtifact>() {
                // `--emit run` executes via the native JIT (the interpreter was
                // removed), with the same helper and budget as `miku run`.
                use leek_backend_native::{DEFAULT_OP_BUDGET, NativeArtifact, NativeOptions};
                let mut opts = NativeOptions::jit_for_input(result.input(), DEFAULT_OP_BUDGET);
                if let Some(depth) = cli.max_call_depth {
                    opts.max_call_depth = depth;
                }
                match leek_backend_native::compile(hir.0.as_ref(), &opts) {
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
            if let Some(hir) = result.get::<HirArtifact>() {
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
                let input = result.input();
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
                    if let Err(e) = leek_backend_native::aot::compile_to_executable(
                        hir.0.as_ref(),
                        &opts,
                        &out,
                        false,
                    ) {
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
                match leek_backend_native::compile(hir.0.as_ref(), &opts) {
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
