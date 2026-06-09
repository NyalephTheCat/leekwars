//! `miku profile` — previously ran the program under the interpreter with
//! per-call-stack ops profiling. The interpreter backend has been removed and
//! the native JIT has no equivalent op-profiler, so this command is currently
//! unavailable.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{bail, Result};

use crate::cli::Profile;

pub fn run(_args: Profile, _manifest_path: Option<&Path>, _quiet: bool) -> Result<ExitCode> {
    bail!(
        "`miku profile` is unavailable: it relied on the interpreter's ops profiler, \
         which was removed with the interpreter backend"
    )
}
