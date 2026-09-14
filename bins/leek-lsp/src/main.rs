//! Stdio launcher. The whole server lives in `leek-lsp`.

fn main() {
    // The subscriber belongs to the binary: installing a global one from a
    // library would make `leek-lsp` unembeddable.
    leek_lsp::log::init();
    let code = match leek_lsp::run_stdio() {
        Ok(()) => 0,
        Err(error) => {
            tracing::error!(%error, "leek-lsp could not start");
            1
        }
    };
    // Terminate hard rather than returning: tokio's blocking stdin reader
    // can outlive the serve loop, and an editor's "restart server" must
    // always reclaim this process. `run_stdio` returns so that *callers*
    // (`miku lsp`, tests) get the choice; this one takes the loud option.
    std::process::exit(code);
}
