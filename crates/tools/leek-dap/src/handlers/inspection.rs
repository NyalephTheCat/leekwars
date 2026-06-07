//! State-inspection requests: threads, stack frames, scopes, variables.
//!
//! Leekscript is single-threaded, so `threads` reports one fixed
//! thread. The stack/scope/variable requests are only meaningful while
//! stopped; until the target can pause they return empty results.

use std::io::{Read, Write};

use dap::prelude::*;
use dap::responses::{ScopesResponse, StackTraceResponse, ThreadsResponse, VariablesResponse};
use dap::types::{Scope, Source, StackFrame, Thread, Variable};

use crate::handlers::Flow;
use crate::session::{Session, MAIN_THREAD_ID};


/// `threads`: the single synthetic main thread.
pub(crate) fn threads<R: Read, W: Write>(
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let response = ThreadsResponse {
        threads: vec![Thread {
            id: MAIN_THREAD_ID,
            name: "main".to_string(),
        }],
    };
    server.respond(req.success(ResponseBody::Threads(response)))?;
    Ok(Flow::Continue)
}

/// `stackTrace`: every live frame, top-first, from the shadow call stack.
/// Frame `id` is its index (0 = innermost); `scopes`/`variables` key off it.
pub(crate) fn stack_trace<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let source = session.program_path.as_ref().map(|path| Source {
        name: std::path::Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .map(String::from),
        path: Some(path.clone()),
        ..Default::default()
    });

    let frames: Vec<StackFrame> = session
        .native_debug
        .as_ref()
        .map(|ctl| ctl.frames())
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(idx, frame)| StackFrame {
            id: i64::try_from(idx).unwrap_or(0),
            name: frame.name,
            source: source.clone(),
            line: i64::from(frame.line),
            column: 1,
            ..Default::default()
        })
        .collect();

    let total = i64::try_from(frames.len()).unwrap_or(0);
    let response = StackTraceResponse {
        stack_frames: frames,
        total_frames: Some(total),
    };
    server.respond(req.success(ResponseBody::StackTrace(response)))?;
    Ok(Flow::Continue)
}

/// `scopes`: a single "Locals" scope for the requested frame. Its
/// `variablesReference` is `frameId + 1` so a later `variables` request
/// resolves back to the frame (frame 0 → ref 1).
pub(crate) fn scopes<R: Read, W: Write>(
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let frame_id = match &req.command {
        Command::Scopes(args) => args.frame_id,
        _ => 0,
    };
    let scope = Scope {
        name: "Locals".to_string(),
        variables_reference: frame_id + 1,
        expensive: false,
        ..Default::default()
    };
    let response = ScopesResponse {
        scopes: vec![scope],
    };
    server.respond(req.success(ResponseBody::Scopes(response)))?;
    Ok(Flow::Continue)
}

/// `variables`: the locals captured for the frame the `variablesReference`
/// encodes (`ref - 1` is the frame index).
pub(crate) fn variables<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let var_ref = match &req.command {
        Command::Variables(args) => args.variables_reference,
        _ => 0,
    };
    let frame_idx = usize::try_from(var_ref - 1).unwrap_or(usize::MAX);

    let variables = session
        .native_debug
        .as_ref()
        .map(|ctl| ctl.frames())
        .unwrap_or_default()
        .get(frame_idx)
        .map(|frame| frame.vars.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value)| Variable {
            name,
            value,
            variables_reference: 0,
            ..Default::default()
        })
        .collect();
    let response = VariablesResponse { variables };
    server.respond(req.success(ResponseBody::Variables(response)))?;
    Ok(Flow::Continue)
}
