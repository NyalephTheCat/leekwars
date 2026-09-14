//! Execution-control requests: continue, step, pause.
//!
//! These drive the native [`DebugController`](crate::debug::NativeDebugSession):
//! `continue` releases a parked debuggee, the step requests single-step it,
//! and `pause` asks it to stop at the next statement. The DAP `stopped`
//! event that follows a stop is emitted by the controller (from the debuggee
//! thread), so these handlers only need to acknowledge the request.
//!
//! Step granularity is one statement — a safepoint — and all three steps are
//! depth-aware against the controller's shadow call stack: `stepIn` stops at
//! the next statement wherever it is, `next` stops at the next one in this
//! frame or a caller (a callee is stepped over whole), and `stepOut` stops
//! once this frame has returned.

use std::io::{Read, Write};

use dap::prelude::*;
use dap::responses::ContinueResponse;

use crate::handlers::Flow;
use crate::session::Session;

pub(crate) fn continue_<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    if let Some(ctl) = &session.native_debug {
        ctl.resume();
    }
    let response = ContinueResponse {
        all_threads_continued: Some(true),
    };
    server.respond(req.success(ResponseBody::Continue(response)))?;
    Ok(Flow::Continue)
}

pub(crate) fn next<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    if let Some(ctl) = &session.native_debug {
        ctl.step_over();
    }
    server.respond(req.success(ResponseBody::Next))?;
    Ok(Flow::Continue)
}

pub(crate) fn step_in<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    if let Some(ctl) = &session.native_debug {
        ctl.step_into();
    }
    server.respond(req.success(ResponseBody::StepIn))?;
    Ok(Flow::Continue)
}

pub(crate) fn step_out<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    if let Some(ctl) = &session.native_debug {
        ctl.step_out();
    }
    server.respond(req.success(ResponseBody::StepOut))?;
    Ok(Flow::Continue)
}

pub(crate) fn pause<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    if let Some(ctl) = &session.native_debug {
        ctl.request_pause();
    }
    server.respond(req.success(ResponseBody::Pause))?;
    Ok(Flow::Continue)
}
