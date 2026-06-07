//! CLI surface — clap derive structs for every subcommand.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(version, about = "Leekscript workspace tool")]
pub struct Cli {
    /// Path to `Miku.toml`. If omitted, walks up from the current
    /// directory until a manifest is found.
    #[arg(long, global = true, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Suppress informational output.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    /// Print extra progress information.
    #[arg(long, short = 'v', global = true)]
    pub verbose: bool,

    /// ANSI color policy. Default: auto.
    #[arg(long, global = true, value_enum, default_value_t = ColorWhen::Auto)]
    pub color: ColorWhen,

    /// Diagnostic output format. Default: human.
    #[arg(long, global = true, value_enum, default_value_t = MessageFormat::Human)]
    pub message_format: MessageFormat,

    /// Load a host-environment function library. Repeatable. A built-in
    /// name (`leekwars`) or a path to a library-definition file. Its
    /// functions are recognized across the workspace (diagnostics, check,
    /// build) and `build` dispatches them to the library's classes.
    #[arg(long = "library", global = true, value_name = "NAME|PATH")]
    pub libraries: Vec<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorWhen {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MessageFormat {
    /// Human-readable rendering with source context.
    Human,
    /// Newline-delimited JSON; one object per diagnostic.
    Json,
    /// JUnit XML — only meaningful for `miku test`. Other subcommands
    /// fall back to human rendering.
    Junit,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a new project skeleton in a new directory.
    New(New),
    /// Initialize a project skeleton in the current directory.
    Init(Init),
    /// Compile per the manifest's default backend (Java by default).
    Build(Build),
    /// Build with the interpreter backend and execute.
    Run(Run),
    /// Run a leek-wars fight from a scenario file, or test an AI against many
    /// settings (matrix sweep, tournament, randomized builds).
    Fight(Fight),
    /// Run diagnostics across the project without producing output.
    Check,
    /// Run every `.leek` file under `tests/` through the interpreter.
    Test(Test),
    /// Format all `.leek` sources.
    Fmt(Fmt),
    /// Run the linter.
    Lint,
    /// Print the extended explanation for a diagnostic code.
    Explain(Explain),
    /// Apply machine-applicable diagnostic suggestions in place.
    Fix(Fix),
    /// Start the language server on stdio.
    Lsp,
    /// Remove the build/ directory.
    Clean,
    /// Print shell completion script to stdout.
    Completions(Completions),
    /// Migrate .leek sources between language versions.
    Migrate(Migrate),
    /// Print per-function complexity / big-O analysis.
    Analyze(Analyze),
    /// Run the program under the interpreter and emit a per-stack
    /// ops profile (folded format or human table).
    Profile(Profile),
    /// Generate HTML API documentation from `.leek` sources.
    Doc(Doc),
    /// Developer hygiene checks (layers, builtin drift, pipeline timing).
    Dev(Dev),
}

#[derive(Debug, clap::Args)]
pub struct Dev {
    #[command(subcommand)]
    pub command: DevCommand,
}

#[derive(Debug, Subcommand)]
pub enum DevCommand {
    /// Run `tools/check-layers.sh`.
    Layers,
    /// Verify builtin Java metadata (`tools/builtin-extract.sh --check`).
    Builtins,
    /// Run focused builtin tests (`leek-builtin-suite`).
    BuiltinSuite,
    /// Run the front/middle pipeline with per-step timings.
    Pipeline(DevPipeline),
}

#[derive(Debug, clap::Args)]
pub struct DevPipeline {
    /// Source file to compile. Defaults to `tests/fixtures/hello.leek`.
    pub path: Option<PathBuf>,
    /// Language version (1..=4).
    #[arg(long = "lang-version", default_value_t = 4)]
    pub lang_version: u8,
}

#[derive(Debug, clap::Args)]
pub struct Profile {
    /// Output format.
    #[arg(long, value_enum, default_value_t = ProfileFormat::Table)]
    pub format: ProfileFormat,
    /// Show stacks accumulating fewer than this many ops as
    /// "(other)" in the human table. Folded output is unaffected.
    #[arg(long, default_value_t = 0)]
    pub min_ops: u64,
    /// Override the manifest's entry point. Defaults to
    /// `[project].entry`.
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProfileFormat {
    /// Human-readable table: top stacks by self-ops.
    Table,
    /// Brendan Gregg's folded-stack format. One line per stack:
    /// `frame1;frame2;... N`. Pipe to `flamegraph.pl`.
    Folded,
}

#[derive(Debug, clap::Args)]
pub struct Doc {
    /// Output directory. Defaults to `target/doc/`.
    #[arg(long, value_name = "PATH")]
    pub out_dir: Option<PathBuf>,
    /// Open the generated index page in the system browser
    /// after generation.
    #[arg(long)]
    pub open: bool,
}

#[derive(Debug, clap::Args)]
pub struct Analyze {
    /// Show the full ops formula in addition to the big-O class.
    #[arg(long)]
    pub formula: bool,
    /// Only analyse this file. Defaults to every `.leek` source
    /// under the project's `src/`.
    pub path: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct Migrate {
    /// Target language version (`v1`, `v2`, `v3`, `v4`). Required.
    #[arg(long, value_name = "VERSION")]
    pub to: MigrateVersion,
    /// Override the source version. If omitted, each file's
    /// `@version` pragma is used (falling back to the manifest's
    /// `[project].language`).
    #[arg(long, value_name = "VERSION")]
    pub from: Option<MigrateVersion>,
    /// Don't write changes; print what would happen and exit
    /// non-zero if any file would change.
    #[arg(long)]
    pub dry_run: bool,
    /// Files or directories to migrate. If omitted, walks
    /// `src/` and `tests/` from the manifest.
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MigrateVersion {
    V1,
    V2,
    V3,
    V4,
}

impl MigrateVersion {
    pub fn to_syntax(self) -> leek_syntax::Version {
        use leek_syntax::Version;
        match self {
            MigrateVersion::V1 => Version::V1,
            MigrateVersion::V2 => Version::V2,
            MigrateVersion::V3 => Version::V3,
            MigrateVersion::V4 => Version::V4,
        }
    }
}

#[derive(Debug, clap::Args)]
pub struct New {
    /// Directory to create. The basename also becomes `[project].name`.
    pub name: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct Init {
    /// Override the derived project name (defaults to the current
    /// directory's basename).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct Explain {
    /// Diagnostic code to explain, e.g. `L0022` or `E0100`.
    /// Case-insensitive.
    pub code: String,
}

#[derive(Debug, clap::Args)]
pub struct Fix {
    /// Don't write changes; print what would change.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, clap::Args)]
pub struct Completions {
    /// Shell to generate completions for.
    #[arg(value_enum)]
    pub shell: Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}

#[derive(Debug, clap::Args)]
pub struct Build {
    /// Override the manifest's default backend.
    #[arg(long, value_name = "KIND")]
    pub backend: Option<String>,
    /// For the Java backend: emit clean (readable) output.
    #[arg(long)]
    pub clean: bool,
    /// Override the output directory for the chosen backend.
    #[arg(long, value_name = "PATH")]
    pub out_dir: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct Run {
    /// Override the backend used for execution. Only `interp` is
    /// supported in v0.1; `native` returns a clean error.
    #[arg(long, value_name = "KIND")]
    pub backend: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct Test {
    /// Stop at the first failing test.
    #[arg(long)]
    pub fail_fast: bool,
}

#[derive(Debug, clap::Args)]
pub struct Fmt {
    /// Don't write changes; exit non-zero if anything would change.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, clap::Args)]
pub struct Fight {
    /// Scenario file (`.toml` or `.json`) describing the fight. Falls back to
    /// the manifest's `[fight].default_scenario` if omitted.
    pub scenario: Option<PathBuf>,

    /// What to run. `single` plays one fight; the others test the hero AI
    /// against many settings.
    #[arg(long, value_enum, default_value_t = FightMode::Single)]
    pub mode: FightMode,

    /// Override the scenario seed.
    #[arg(long)]
    pub seed: Option<u64>,
    /// Apply a named `[profiles.<name>]` block before running.
    #[arg(long)]
    pub profile: Option<String>,
    /// Override the turn limit.
    #[arg(long)]
    pub max_turns: Option<u32>,
    /// Team treated as the AI under test for win/loss accounting (default: the
    /// first team in the scenario, or `[testing].hero_team`).
    #[arg(long)]
    pub hero_team: Option<i64>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = FightFormat::Human)]
    pub format: FightFormat,

    /// Instead of running, generate a self-contained native executable that
    /// runs this fight and writes it to the given path. Requires `cargo` on
    /// PATH. Applies `--seed`/`--profile`/`--max-turns` to the baked-in fight.
    #[arg(long, value_name = "PATH")]
    pub emit: Option<PathBuf>,

    // --- matrix mode ---
    /// Sweep these seeds (comma-separated). Matrix mode.
    #[arg(long, value_delimiter = ',')]
    pub seeds: Vec<u64>,
    /// Swap an opposing AI in (repeatable). Matrix mode.
    #[arg(long = "vs")]
    pub vs: Vec<PathBuf>,
    /// Apply each of these profiles across the sweep (repeatable). Matrix mode.
    #[arg(long = "with-profile")]
    pub with_profile: Vec<String>,

    // --- tournament mode ---
    /// Competing AI files (repeatable). Tournament mode.
    #[arg(long = "entrant")]
    pub entrant: Vec<PathBuf>,
    /// Tournament format.
    #[arg(long, value_enum, default_value_t = BracketArg::RoundRobin)]
    pub bracket: BracketArg,
    /// Seeds (games) played per pairing. Tournament mode.
    #[arg(long, value_delimiter = ',')]
    pub games: Vec<u64>,

    // --- random mode ---
    /// Number of random builds to generate and fight. Random mode.
    #[arg(long)]
    pub runs: Option<u32>,
    /// Total stat points to distribute per random build. Random mode.
    #[arg(long)]
    pub capital: Option<i64>,
    /// Stats eligible for random point-buy (comma-separated). Random mode.
    #[arg(long = "random-stats", value_delimiter = ',')]
    pub random_stats: Vec<String>,
    /// Whose build to randomize. Random mode.
    #[arg(long, value_enum, default_value_t = RandomTargetArg::Opponent)]
    pub random_target: RandomTargetArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FightMode {
    Single,
    Matrix,
    Tournament,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FightFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BracketArg {
    RoundRobin,
    SingleElim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RandomTargetArg {
    Hero,
    Opponent,
    Both,
}
