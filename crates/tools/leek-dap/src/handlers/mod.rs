//! Request handlers, split by area (mirrors `leek-lsp`'s `handlers/`).
//!
//! [`dispatch`] routes one request to the right handler. Handlers take
//! ownership of the [`Request`] (they consume it to build the
//! response) plus `&mut` access to the [`Session`] and the [`Server`]
//! (to respond and emit events), and return a [`Flow`] telling the
//! loop whether to keep going.

mod breakpoints;
mod execution;
mod inspection;
mod lifecycle;

use std::io::{Read, Write};

use dap::prelude::*;
use dap::types::Source;

use crate::session::Session;

/// Whether the request loop should continue or shut down.
pub(crate) enum Flow {
    Continue,
    Shutdown,
}

/// A DAP `source` reference for a file path. Stack frames and breakpoints
/// both name the file they point at, and a client matches the two by path.
pub(crate) fn source_ref(path: &std::path::Path) -> Source {
    Source {
        name: path.file_name().and_then(|s| s.to_str()).map(String::from),
        path: Some(path.display().to_string()),
        ..Default::default()
    }
}

/// Handle a single request, then check that the session survived it.
///
/// A panic under one of the debug controller's locks — in a handler here, or
/// on the debuggee thread inside the hook — leaves that lock poisoned. The
/// adapter recovers the guard instead of panicking a second time (see
/// [`crate::lock`]), but the state behind it is whatever the panic left, so
/// this ends the session with an error rather than answer the next
/// `stackTrace` out of a half-built stack (#176).
pub(crate) fn dispatch<R: Read, W: Write + Send + 'static>(
    session: &mut Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let flow = route(session, server, req)?;
    if session.debug_poisoned() {
        lifecycle::end_poisoned_session(server)?;
        return Ok(Flow::Shutdown);
    }
    Ok(flow)
}

/// Route a single request to its handler.
fn route<R: Read, W: Write + Send + 'static>(
    session: &mut Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    match &req.command {
        Command::Initialize(_) => lifecycle::initialize(server, req),
        Command::Launch(_) => lifecycle::launch(session, server, req),
        Command::ConfigurationDone => lifecycle::configuration_done(session, server, req),
        Command::Disconnect(_) | Command::Terminate(_) => lifecycle::shutdown(server, req),

        Command::SetBreakpoints(_) => breakpoints::set(session, server, req),
        Command::SetExceptionBreakpoints(_) => breakpoints::set_exception(server, req),
        Command::BreakpointLocations(_) => breakpoints::locations(session, server, req),

        Command::Threads => inspection::threads(server, req),
        Command::StackTrace(_) => inspection::stack_trace(session, server, req),
        Command::Scopes(_) => inspection::scopes(server, req),
        Command::Variables(_) => inspection::variables(session, server, req),
        Command::Evaluate(_) => inspection::evaluate(session, server, req),

        Command::Continue(_) => execution::continue_(session, server, req),
        Command::Next(_) => execution::next(session, server, req),
        Command::StepIn(_) => execution::step_in(session, server, req),
        Command::StepOut(_) => execution::step_out(session, server, req),
        Command::Pause(_) => execution::pause(session, server, req),

        _ => {
            server.respond(req.error("unsupported request"))?;
            Ok(Flow::Continue)
        }
    }
}
