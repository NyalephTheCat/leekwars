//! CLI types and argument parsing.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use leek_recipes::{Fuel, OptConfig, OptLevel, Pass};
use leek_syntax::Version;

/// IR-optimization flags. Distinct from `--opt-level` (which is the native
/// Cranelift backend's debug/release switch): these control the
/// backend-agnostic HIR/MIR optimization passes.
///
/// `-O` picks a level; `--fuel` caps total rewrites (`0` = unlimited); the
/// `--no-*` flags disable individual passes on top of the level's defaults.
#[derive(Debug, Default, clap::Args)]
pub struct OptArgs {
    /// IR optimization level: 0 (none), 1 (fold/prop/inline/DCE), 2 (+desugar,
    /// algebraic, purity, intrinsics), 3 (+aggressive inline, jump-threading).
    #[arg(short = 'O', long = "opt", value_name = "LEVEL")]
    pub opt: Option<u8>,
    /// Cap total optimization rewrites; `0` means unlimited.
    #[arg(long, value_name = "N")]
    pub fuel: Option<u64>,
    /// Disable constant propagation.
    #[arg(long = "no-prop")]
    pub no_prop: bool,
    /// Disable constant folding.
    #[arg(long = "no-fold")]
    pub no_fold: bool,
    /// Disable function inlining.
    #[arg(long = "no-inline")]
    pub no_inline: bool,
    /// Disable dead-code elimination.
    #[arg(long = "no-dce")]
    pub no_dce: bool,
    /// Disable HIR desugaring (O2+).
    #[arg(long = "no-desugar")]
    pub no_desugar: bool,
    /// Disable algebraic / cosmetic simplification (O2+).
    #[arg(long = "no-algebraic")]
    pub no_algebraic: bool,
    /// Disable pure-function detection (O2+).
    #[arg(long = "no-pure-fn")]
    pub no_pure_fn: bool,
    /// Disable intrinsic recognition (O2+).
    #[arg(long = "no-intrinsics")]
    pub no_intrinsics: bool,
    /// Disable all MIR-level passes.
    #[arg(long = "no-mir-opt")]
    pub no_mir_opt: bool,
}

impl OptArgs {
    /// Resolve to an [`OptConfig`]: start from `-O` (or `default_level` when
    /// unset), apply `--fuel`, then the `--no-*` pass toggles.
    #[must_use]
    pub fn to_config(&self, default_level: OptLevel) -> OptConfig {
        let level = self.opt.map_or(default_level, OptLevel::from_u8);
        let mut cfg = OptConfig::for_level(level);
        if let Some(n) = self.fuel {
            cfg = cfg.with_fuel(if n == 0 {
                Fuel::Unlimited
            } else {
                Fuel::Limited(n)
            });
        }
        for (disabled, pass) in [
            (self.no_prop, Pass::ConstProp),
            (self.no_fold, Pass::ConstFold),
            (self.no_dce, Pass::Dce),
            (self.no_desugar, Pass::Desugar),
            (self.no_algebraic, Pass::Algebraic),
            (self.no_pure_fn, Pass::PureFn),
            (self.no_intrinsics, Pass::Intrinsics),
        ] {
            if disabled {
                cfg.set(pass, false);
            }
        }
        if self.no_inline {
            cfg.set(Pass::Inline, false);
            cfg.set(Pass::AggressiveInline, false);
        }
        if self.no_mir_opt {
            cfg.set(Pass::MirConstBranch, false);
            cfg.set(Pass::MirUnreachable, false);
            cfg.set(Pass::MirJumpThread, false);
        }
        cfg
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Emit {
    /// Run diagnostics only; no artifact.
    Check,
    /// Dump the token stream (lexer output).
    Tokens,
    /// Dump the flat CST (lexer + GreenNodeBuilder, no parser).
    FlatCst,
    /// Dump the structured CST (lexer + parser).
    Cst,
    /// Dump the HIR (resolved + typed tree).
    Hir,
    /// Dump the MIR (CFG of basic blocks per function).
    Mir,
    /// Execute via the native JIT and print the program's result.
    Run,
    /// Emit Java source (one `.java` file plus `.lines` sidecar).
    Java,
    /// Compile to native code via Cranelift. By default JIT-runs the
    /// program; see `--native-emit` to dump IR / disassembly or write
    /// an object file.
    Native,
    /// Format the source via `leek-fmt` and print to stdout.
    Fmt,
    /// Emit valid official LeekScript with experimental features
    /// desugared. Human-readable by default; see `--compact` and
    /// `--optimize`.
    #[value(name = "leekscript")]
    LeekScript,
}

/// Native optimization level (the debug/release switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OptLevelArg {
    /// No optimization (debug-friendly). Default.
    None,
    /// Optimize for speed (release).
    Speed,
    /// Optimize for speed and code size.
    SpeedAndSize,
}

/// What `--emit native` should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NativeEmitArg {
    /// JIT-compile and run, printing the program's value. Default.
    Run,
    /// Dump the Cranelift IR (CLIF) — inspect what the backend generates.
    Clif,
    /// Dump the target disassembly of the compiled code.
    Asm,
    /// Write a relocatable object file (requires `--native-out`).
    Object,
    /// Compile ahead-of-time to a standalone native executable at
    /// `--native-out` (default `a.out`). Needs `cargo` on PATH.
    Exe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MessageFormat {
    /// Human-readable snippet rendering with source context.
    Human,
    /// Newline-delimited JSON, one object per diagnostic.
    Json,
}

#[derive(Debug, Parser)]
#[command(version, about = "Leekscript compiler driver")]
pub struct Cli {
    /// Path to the input `.leek` file.
    pub input: PathBuf,

    /// What to produce.
    #[arg(long, value_enum, default_value_t = Emit::Check)]
    pub emit: Emit,

    /// Override the file's `@version` pragma.
    #[arg(long, value_parser = parse_version)]
    pub version_pragma: Option<Version>,

    /// How to render diagnostics.
    #[arg(long, value_enum, default_value_t = MessageFormat::Human)]
    pub message_format: MessageFormat,

    /// Force-disable ANSI color in human-format output.
    #[arg(long)]
    pub no_color: bool,

    /// Promote a code to error. Repeatable: `--deny E0240 --deny W0010`.
    #[arg(long, value_name = "CODE")]
    pub deny: Vec<String>,

    /// Force a code to warning level.
    #[arg(long, value_name = "CODE")]
    pub warn: Vec<String>,

    /// Silence a code entirely.
    #[arg(long, value_name = "CODE")]
    pub allow: Vec<String>,

    /// Also run the pedantic lints (strictness; verbose-but-fine code).
    #[arg(long)]
    pub pedantic: bool,

    /// Also run the nursery lints (teaching; points at idiomatic
    /// constructs).
    #[arg(long)]
    pub nursery: bool,

    /// Load a host-environment function library. Repeatable. Accepts a
    /// built-in name (`leekwars` for the leek-wars-generator fight
    /// functions) or a path to a library-definition file. Its functions
    /// become known to the whole pipeline (no "undefined function"), and
    /// `--emit java` dispatches them to the library's classes
    /// (`EntityClass.getCell(...)`).
    #[arg(long = "library", value_name = "NAME|PATH")]
    pub libraries: Vec<String>,

    /// For `--emit java`: emit the readable/optimized variant instead
    /// of the byte-faithful reference shape.
    #[arg(long)]
    pub clean: bool,

    /// For `--emit java`: numeric AI id baked into the class name
    /// (`AI_<id>`). Defaults to 0.
    #[arg(long, default_value_t = 0u64)]
    pub ai_id: u64,

    /// For `--emit java`: the Java base class the emitted AI extends.
    /// Defaults to `AI`; pass `EntityAI` to produce a class the
    /// leek-wars-generator can run directly in a fight.
    #[arg(long = "base-class", value_name = "NAME")]
    pub base_class: Option<String>,

    /// For `--emit leekscript`: emit minified output (no indentation /
    /// newlines, comments dropped) instead of the human-readable form.
    #[arg(long)]
    pub compact: bool,

    /// For `--emit leekscript`: run semantics-preserving optimizations
    /// (constant folding, dead-code elimination) before emitting.
    #[arg(long)]
    pub optimize: bool,

    /// Fold known library constants to their literal values before emit
    /// (e.g. `WEAPON_PISTOL` → `37`). Opt-in; requires a `--library`
    /// providing the values (currently `leekwars`).
    #[arg(long = "fold-constants")]
    pub fold_constants: bool,

    /// For `--emit java`: write `<stem>.java` (and `<stem>.lines`) to
    /// this directory instead of stdout.
    #[arg(long, short = 'o')]
    pub out_dir: Option<PathBuf>,

    /// For `--emit fmt`: path to a `Miku.toml`-style file whose
    /// `[format]` table configures the formatter. Without this flag,
    /// the formatter's hard-coded defaults are used.
    #[arg(long, value_name = "PATH")]
    pub fmt_config: Option<PathBuf>,

    /// For `--emit native`: optimization level (debug vs release).
    /// Overrides the profile chosen by `--release`.
    #[arg(long, value_enum)]
    pub opt_level: Option<OptLevelArg>,

    /// For `--emit native`: shortcut for `--opt-level speed` with debug
    /// info off (the release profile).
    #[arg(long)]
    pub release: bool,

    /// For `--emit native`: what to produce (run / clif / asm / object).
    #[arg(long, value_enum, default_value_t = NativeEmitArg::Run)]
    pub native_emit: NativeEmitArg,

    /// For `--emit native --native-emit object`: output object-file path.
    #[arg(long, value_name = "PATH")]
    pub native_out: Option<PathBuf>,

    /// For `--emit native`: emit DWARF debug info (object output) so a
    /// debugger can map machine code back to source.
    #[arg(long)]
    pub debug_info: bool,

    /// For `--emit native`: skip Cranelift's IR verifier (it runs by
    /// default in the debug profile to catch malformed IR).
    #[arg(long)]
    pub no_verifier: bool,

    /// For `--emit native`: link the host game library — compile leek-wars
    /// fight builtins (`getCell`, `useWeapon`, …) as calls to the game
    /// runtime instead of failing as unsupported. Without an installed
    /// runtime they return `null` at run time.
    #[arg(long)]
    pub link_game: bool,

    #[command(flatten)]
    pub opt: OptArgs,
}

fn parse_version(s: &str) -> Result<Version, String> {
    s.parse::<u32>()
        .ok()
        .and_then(Version::from_pragma)
        .ok_or_else(|| format!("invalid version `{s}` (expected 1..=4)"))
}
