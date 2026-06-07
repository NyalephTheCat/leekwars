//! Stdio launcher. The whole debug adapter lives in `leek-dap`.

fn main() -> anyhow::Result<()> {
    leek_dap::run_stdio()
}
