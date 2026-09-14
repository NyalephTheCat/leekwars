//! Breakpoint requests.

use std::io::{Read, Write};
use std::path::Path;

use dap::prelude::*;
use dap::responses::{
    BreakpointLocationsResponse, SetBreakpointsResponse, SetExceptionBreakpointsResponse,
};
use dap::types::{Breakpoint, BreakpointLocation};

use crate::breakpoints::{BreakpointSpec, ProgramMap, Requested, StoredBreakpoint, Trigger};
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
    // `hitCondition` and `logMessage` are read off the wire rather than out of
    // the typed arguments: the `dap` crate spells those two fields
    // `hit_condition`/`log_message` on the wire and so never sees them. See
    // [`crate::wire`].
    let sent = session.raw.arguments(req.seq);
    let (path, specs) = {
        let Command::SetBreakpoints(args) = &req.command else {
            unreachable!("dispatch guarantees this is a SetBreakpoints")
        };
        let path = args.source.path.clone().unwrap_or_default();
        let as_sent = |index: usize, field: &str| -> Option<String> {
            sent.as_ref()?["breakpoints"]
                .get(index)?
                .get(field)?
                .as_str()
                .map(str::to_string)
        };
        // The condition, hit count and log message travel with the line: a
        // breakpoint is not just a place, and dropping them here is what made
        // the three capabilities unadvertisable.
        let specs: Vec<BreakpointSpec> = args
            .breakpoints
            .as_ref()
            .map(|bps| {
                bps.iter()
                    .enumerate()
                    .map(|(index, bp)| BreakpointSpec {
                        line: bp.line,
                        condition: bp.condition.clone().or_else(|| as_sent(index, "condition")),
                        hit_condition: bp
                            .hit_condition
                            .clone()
                            .or_else(|| as_sent(index, "hitCondition")),
                        log_message: bp
                            .log_message
                            .clone()
                            .or_else(|| as_sent(index, "logMessage")),
                    })
                    .collect()
            })
            .unwrap_or_default();
        (path, specs)
    };

    let path = Path::new(&path);
    let requested = session.breakpoints.replace(path, &specs);

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
/// compiled, the file is one it was compiled from, some line at or after the
/// requested one carries a safepoint, and whatever the client hung off it
/// compiles — the `line` reported back is that safepoint's, which is where the
/// debuggee will actually stop. Everything else says why not, so the client
/// shows a hollow marker instead of a live one that can never fire.
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
    // The expressions are compiled here, not when the breakpoint was set:
    // before `configurationDone` there is no program and so no language
    // version to compile them at, which is the same reason the verdict is
    // re-delivered as a change event.
    let stored = StoredBreakpoint {
        id,
        spec: requested.spec.clone(),
    };
    if let Err(message) = Trigger::check(&stored, program.version()) {
        return unverified(&message);
    }
    Breakpoint {
        id: Some(id),
        verified: true,
        source: Some(source_ref(path)),
        line: Some(i64::from(placed)),
        ..Default::default()
    }
}

/// `breakpointLocations`: which lines of a range the debuggee can actually
/// stop on, so an editor greys out the gutter of a comment or a blank line
/// instead of offering a breakpoint that would slide somewhere else.
///
/// Answered from the compiled program's safepoint lines, which means an empty
/// answer before launch and for a file the program was not compiled from —
/// "nothing here" rather than an error, since the client asks about whatever
/// file it has open.
pub(crate) fn locations<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let Command::BreakpointLocations(args) = &req.command else {
        unreachable!("dispatch guarantees this is a BreakpointLocations")
    };
    let path = args.source.path.clone().unwrap_or_default();
    let start = u32::try_from(args.line).unwrap_or(0);
    let end = args
        .end_line
        .map_or(start, |line| u32::try_from(line).unwrap_or(start));

    let lines = session
        .program
        .as_ref()
        .and_then(|program| {
            let source = program.source_of(Path::new(&path))?;
            Some(program.lines_between(source, start, end.max(start)))
        })
        .unwrap_or_default();

    let response = BreakpointLocationsResponse {
        breakpoints: lines
            .into_iter()
            .map(|line| BreakpointLocation {
                line: i64::from(line),
                ..Default::default()
            })
            .collect(),
    };
    server.respond(req.success(ResponseBody::BreakpointLocations(response)))?;
    Ok(Flow::Continue)
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
