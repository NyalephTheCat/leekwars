//! Breakpoint requests.

use std::io::{Read, Write};

use dap::prelude::*;
use dap::responses::{SetBreakpointsResponse, SetExceptionBreakpointsResponse};
use dap::types::Breakpoint;

use crate::handlers::Flow;
use crate::session::Session;

/// `setBreakpoints`: record the requested lines and echo them back.
///
/// Breakpoints are reported `verified: true` optimistically — they're set
/// before the program is compiled, so the exact code mapping isn't known
/// yet, but the native debug build honors a breakpoint when execution
/// reaches that source line.
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

    session.breakpoints.insert(path, lines.clone());

    let breakpoints = lines
        .into_iter()
        .map(|line| Breakpoint {
            verified: true,
            line: Some(line),
            ..Default::default()
        })
        .collect();

    let response = SetBreakpointsResponse { breakpoints };
    server.respond(req.success(ResponseBody::SetBreakpoints(response)))?;
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
