use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use leek_builtin_suite::{load_suite, run_suite};

fn main() -> Result<ExitCode> {
    let path = std::env::args().nth(1).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("suite.toml"),
        PathBuf::from,
    );
    let suite = load_suite(&path)?;
    let report = run_suite(&suite);
    eprintln!(
        "builtin-suite: {} passed, {} failed ({} total)",
        report.passed,
        report.failed,
        report.passed + report.failed
    );
    for f in &report.failures {
        eprintln!("  FAIL {f}");
    }
    Ok(if report.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
