//! Breakpoint requests.

use std::io::{Read, Write};
use std::path::Path;

use dap::prelude::*;
use dap::responses::{SetBreakpointsResponse, SetExceptionBreakpointsResponse};
use dap::types::Breakpoint;

use crate::breakpoints::{ProgramMap, Requested};
use crate::handlers::{Flow, source_ref};
use crate::session::Session;

/// `setBreakpoints`: replace this file's breakpoints, hand the new set to a
/// running debuggee, and answer with where each one actually landed.
///
/// The request is a whole-file replace, and it arrives just as often *after*
/// the program is running — the user clicks the gutter while parked at a stop
/// — as before it. Both cases take the same path: the store is updated, and
/// whenever a debug controller exists it is given the complete new picture
/// there and then, so the breakpoint takes effect on this run rather than the
/// next one.
pub(crate) fn set<R: Read, W: Write>(
    session: &mut Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let (path, lines) = {
        let Command::SetBreakpoints(args) = &req.command else {
            unreachable!("dispatch guarantees this is a SetBreakpoints")
        };
        let path = args.source.path.clone().unwrap_or_default();
        let lines: Vec<i64> = args
            .breakpoints
            .as_ref()
            .map(|bps| bps.iter().map(|bp| bp.line).collect())
            .unwrap_or_default();
        (path, lines)
    };

    let path = Path::new(&path);
    let requested = session.breakpoints.replace(path, &lines);

    if let (Some(debug), Some(program)) = (&session.native_debug, &session.program) {
        debug.set_breakpoints(session.breakpoints.by_source(program));
    }

    let breakpoints = requested
        .iter()
        .map(|requested| answer(session.program.as_ref(), path, requested))
        .collect();

    let response = SetBreakpointsResponse { breakpoints };
    server.respond(req.success(ResponseBody::SetBreakpoints(response)))?;
    Ok(Flow::Continue)
}

/// Where one requested line landed, as a DAP [`Breakpoint`].
///
/// Honest by construction: a breakpoint is `verified` only once the program is
/// compiled, the file is one it was compiled from, and some line at or after
/// the requested one carries a safepoint — the `line` reported back is that
/// one, which is where the debuggee will actually stop. Everything else says
/// why not, so the client shows a hollow marker instead of a live one that
/// can never fire.
pub(crate) fn answer(
    program: Option<&ProgramMap>,
    path: &Path,
    requested: &Requested,
) -> Breakpoint {
    let unverified = |message: &str| Breakpoint {
        id: requested.id(),
        verified: false,
        message: Some(message.to_string()),
        source: Some(source_ref(path)),
        line: Some(requested.line),
        ..Default::default()
    };

    let Some((line, id)) = requested.stored else {
        return unverified("not a line number");
    };
    // Clients set their breakpoints before `configurationDone`, when nothing
    // is compiled yet. `configurationDone` re-answers each one with a
    // `breakpoint` change event as soon as it knows better.
    let Some(program) = program else {
        return unverified("pending launch");
    };
    let Some(source) = program.source_of(path) else {
        return unverified("not part of the debugged program");
    };
    let Some(placed) = program.place(source, line) else {
        return unverified("no executable code at or after this line");
    };
    Breakpoint {
        id: Some(id),
        verified: true,
        source: Some(source_ref(path)),
        line: Some(i64::from(placed)),
        ..Default::default()
    }
}

/// `setExceptionBreakpoints`: we expose no exception filters, so accept
/// and report nothing.
pub(crate) fn set_exception<R: Read, W: Write>(
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let response = SetExceptionBreakpointsResponse { breakpoints: None };
    server.respond(req.success(ResponseBody::SetExceptionBreakpoints(response)))?;
    Ok(Flow::Continue)
}
