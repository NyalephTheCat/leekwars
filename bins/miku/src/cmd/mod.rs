//! Subcommand implementations.

use std::process::ExitCode;

use anyhow::Result;

use crate::cli::{Cli, Command};

pub mod analyze;
pub mod build;
pub mod check;
pub mod clean;
pub mod completions;
pub mod dev;
pub mod doc;
pub mod explain;
pub mod fight;
pub mod fight_emit;
pub mod fix;
pub mod fmt;
pub mod lint;
pub mod lsp;
pub mod migrate;
pub mod new;
pub mod profile;
pub mod run;
pub mod test;

pub fn dispatch(cli: Cli) -> Result<ExitCode> {
    let manifest_path = cli.manifest_path.clone();
    let color = cli.color;
    let quiet = cli.quiet;
    let verbose = cli.verbose;
    let format = cli.message_format;

    // Load + register any host-environment libraries (`--library leekwars`,
    // `--library path/to.lib`) once, process-globally, so every command's
    // pipeline (check, build, test, …) recognizes their functions. The
    // composed catalog is also passed to `build` for Java dispatch.
    let environment: Option<std::sync::Arc<dyn leek_environment::EnvironmentCatalog>> =
        if cli.libraries.is_empty() {
            None
        } else {
            let cat = leek_recipes::load_and_register_libraries(&cli.libraries)
                .map_err(|e| anyhow::anyhow!("loading library: {e}"))?;
            Some(std::sync::Arc::new(cat))
        };

    match cli.command {
        Command::New(args) => new::new(args, quiet).map(to_exit),
        Command::Init(args) => new::init(args, quiet).map(to_exit),
        Command::Build(args) => build::run(
            args,
            manifest_path.as_deref(),
            color,
            format,
            quiet,
            verbose,
            environment,
        ),
        Command::Run(args) => run::run(args, manifest_path.as_deref(), color, format, quiet),
        Command::Fight(args) => fight::run(args, quiet),
        Command::Check => check::run(manifest_path.as_deref(), color, format, quiet),
        Command::Test(args) => test::run(args, manifest_path.as_deref(), color, format, quiet),
        Command::Fmt(args) => fmt::run(args, manifest_path.as_deref(), quiet),
        Command::Lint => lint::run(manifest_path.as_deref(), color, format, quiet),
        Command::Explain(args) => Ok(explain::run(&args)),
        Command::Fix(args) => fix::run(args, manifest_path.as_deref(), quiet),
        Command::Lsp => lsp::run(),
        Command::Clean => clean::run(manifest_path.as_deref(), quiet).map(to_exit),
        Command::Completions(args) => completions::run(args).map(to_exit),
        Command::Migrate(args) => migrate::run(args, manifest_path.as_deref(), quiet),
        Command::Analyze(args) => analyze::run(args, manifest_path.as_deref(), quiet),
        Command::Profile(args) => profile::run(args, manifest_path.as_deref(), quiet),
        Command::Doc(args) => doc::run(args, manifest_path.as_deref(), quiet),
        Command::Dev(args) => dev::run(args, quiet),
    }
}

fn to_exit(_: ()) -> ExitCode {
    ExitCode::SUCCESS
}
