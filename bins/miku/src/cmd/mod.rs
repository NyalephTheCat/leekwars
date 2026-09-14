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

    // The manifest is read *here*, ahead of the library load, because
    // `[project] libraries` has to be merged with `--library` before anything
    // registers (leekwars#132). Each subcommand still discovers it for itself
    // — this is a second, read-only parse, not a hand-off.
    //
    // A failure is deliberately swallowed: `new`, `init`, `explain`,
    // `completions`, `dev` and a bare `miku fight scenario.toml` all run with
    // no `Miku.toml` at all, and a manifest that *is* broken must reach the
    // user as the command's own rendered diagnostic (`report_manifest`, with a
    // caret under the offending key) rather than as a bare error string from
    // a stage that only wanted to know about libraries.
    let project = leek_project::Project::discover(manifest_path.as_deref()).ok();

    // Load + register any host-environment libraries (`[project] libraries`,
    // `--library leekwars`, `--library path/to.lib`) once, process-globally,
    // so every command's pipeline (check, build, test, …) recognizes their
    // functions. The composed catalog is also passed to `build` for Java
    // dispatch.
    let declared: &[String] = project
        .as_ref()
        .map_or(&[], |p| &p.manifest.project.libraries);
    let libraries = merged_libraries(declared, &cli.libraries);
    let environment: Option<std::sync::Arc<dyn leek_environment::EnvironmentCatalog>> =
        if libraries.is_empty() {
            None
        } else {
            let cat = leek_session::load_and_register_libraries(&libraries)
                .map_err(|e| anyhow::anyhow!("loading library: {e}"))?;
            Some(std::sync::Arc::new(cat))
        };

    // `[project] fold_constants = true` — the manifest half of
    // `leekc --fold-constants`. Registration is additive and process-global,
    // so this only ever turns folding on, next to the libraries it folds the
    // constants of.
    if project
        .as_ref()
        .is_some_and(|p| p.manifest.project.fold_constants)
    {
        leek_session::activate_leekwars_constant_folding();
    }

    // Say out loud which experimental features this invocation compiles with,
    // once, before any of them can change what a command reports
    // (leekwars#206). Under `--quiet` the user asked for less, not for a
    // different compile, so the line goes with the rest of the chatter.
    leek_session::report_feature_flags(
        project
            .as_ref()
            .map_or_else(leek_span::FeatureFlags::none, |p| p.manifest.experimental),
        leek_span::FeatureFlags::from_env(),
        verbose && !quiet,
    );

    match cli.command {
        Command::New(args) => new::new(&args, quiet).map(to_exit),
        Command::Init(args) => new::init(args, quiet).map(to_exit),
        Command::Build(args) => build::run(
            &args,
            manifest_path.as_deref(),
            color,
            format,
            quiet,
            verbose,
            environment.as_ref(),
        ),
        Command::Run(args) => run::run(&args, manifest_path.as_deref(), color, format, quiet),
        Command::Fight(args) => fight::run(&args, manifest_path.as_deref(), quiet),
        Command::Check(args) => check::run(&args, manifest_path.as_deref(), color, format, quiet),
        Command::Test(args) => test::run(&args, manifest_path.as_deref(), color, format, quiet),
        Command::Fmt(args) => fmt::run(&args, manifest_path.as_deref(), quiet),
        Command::Lint(args) => lint::run(&args, manifest_path.as_deref(), color, format, quiet),
        Command::Explain(args) => Ok(explain::run(&args)),
        Command::Fix(args) => fix::run(&args, manifest_path.as_deref(), color, format, quiet),
        Command::Lsp => lsp::run(),
        Command::Clean(args) => clean::run(&args, manifest_path.as_deref(), quiet).map(to_exit),
        Command::Completions(args) => {
            completions::run(&args);
            Ok(ExitCode::SUCCESS)
        }
        Command::Migrate(args) => migrate::run(&args, manifest_path.as_deref(), quiet),
        Command::Analyze(args) => analyze::run(args, manifest_path.as_deref(), quiet),
        Command::Profile(args) => profile::run(args, manifest_path.as_deref(), quiet),
        Command::Doc(args) => doc::run(&args, manifest_path.as_deref(), quiet),
        Command::Dev(args) => dev::run(args, quiet),
    }
}

fn to_exit(_: ()) -> ExitCode {
    ExitCode::SUCCESS
}

/// The libraries this invocation loads: `[project] libraries` (`declared`)
/// first, then the repeatable `--library` (`cli`), with a spec that appears in
/// both kept once.
///
/// **Union, not override.** `--library` adds a library to the workspace; there
/// is no spelling for "load the manifest's libraries *except* this one",
/// because registration is process-global and additive — it has no inverse,
/// and `miku fight`, the fight `--emit` template and the debug adapter each
/// register `leekwars` unconditionally on top of whatever came before. A flag
/// that claimed to *replace* the manifest's list would therefore be a lie in
/// the one case it is meant to matter, so it composes instead.
///
/// Manifest first, and deduplicated, because the order decides the composed
/// catalog's lookup order for `build`'s Java dispatch and a spec loaded twice
/// would sit in it twice: the project's own declaration is the baseline, and a
/// `--library` added for one invocation layers on top of it.
fn merged_libraries(declared: &[String], cli: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(declared.len() + cli.len());
    for spec in declared.iter().chain(cli) {
        if !out.iter().any(|seen| seen == spec) {
            out.push(spec.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::merged_libraries;

    fn specs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_manifest_and_the_flag_compose_manifest_first() {
        assert_eq!(
            merged_libraries(&specs(&["leekwars"]), &specs(&["libs/host.lib"])),
            specs(&["leekwars", "libs/host.lib"])
        );
    }

    #[test]
    fn a_spec_named_in_both_places_is_loaded_once() {
        // `--library leekwars` on a project that already declares it must not
        // push the same catalog into the composite twice.
        assert_eq!(
            merged_libraries(&specs(&["leekwars", "a.lib"]), &specs(&["a.lib", "b.lib"])),
            specs(&["leekwars", "a.lib", "b.lib"])
        );
    }

    #[test]
    fn either_side_alone_is_the_whole_list() {
        assert_eq!(merged_libraries(&[], &specs(&["a.lib"])), specs(&["a.lib"]));
        assert_eq!(merged_libraries(&specs(&["a.lib"]), &[]), specs(&["a.lib"]));
        // Nothing declared anywhere stays "no environment at all", which is
        // what keeps `miku check` on a plain project from loading a catalog.
        assert!(merged_libraries(&[], &[]).is_empty());
    }
}
