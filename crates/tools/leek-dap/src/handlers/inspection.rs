//! State-inspection requests: threads, stack frames, scopes, variables.
//!
//! Leekscript is single-threaded, so `threads` reports one fixed thread. The
//! stack/scope/variable requests serve the frame snapshot the debug
//! controller captured at the last stop — every live frame's name, line and
//! locals, read while the debuggee was parked and those frames were alive.
//! They are empty only when nothing is stopped: before the first stop, after
//! a `continue`, or on a `noDebug` launch.

use std::io::{Read, Write};

use dap::prelude::*;
use dap::responses::{
    EvaluateResponse, ScopesResponse, StackTraceResponse, ThreadsResponse, VariablesResponse,
};
use dap::types::{Scope, StackFrame, Thread, Variable};
use leek_backend_native::DebugValue;

use crate::expr;
use crate::handlers::{Flow, source_ref};
use crate::session::{MAIN_THREAD_ID, Session};

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
            // The frame's own file — an included one when the program was
            // compiled through the project include graph — falling back to
            // the launched program.
            source: frame
                .path
                .as_ref()
                .or(session.program_path.as_ref())
                .map(|path| source_ref(std::path::Path::new(path))),
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
        .map(|var| Variable {
            name: var.name,
            value: var.display,
            variables_reference: 0,
            ..Default::default()
        })
        .collect();
    let response = VariablesResponse { variables };
    server.respond(req.success(ResponseBody::Variables(response)))?;
    Ok(Flow::Continue)
}

/// `evaluate`: compute an expression against the locals of one parked frame.
///
/// The frame is named the way `scopes`/`variables` name theirs — `frameId` is
/// the index `stackTrace` handed out — and the expression is the same subset a
/// breakpoint condition may use ([`crate::expr`]), compiled and evaluated at
/// the debugged program's language version. Everything that cannot be answered
/// is an error response with the reason in it: no stop, no such frame, an
/// unknown name, a construct outside the subset. Never a panic, and never a
/// made-up value.
pub(crate) fn evaluate<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let (expression, frame_id) = {
        let Command::Evaluate(args) = &req.command else {
            unreachable!("dispatch guarantees this is an Evaluate")
        };
        (args.expression.clone(), args.frame_id.unwrap_or(0))
    };

    let Some(version) = session
        .program
        .as_ref()
        .map(crate::breakpoints::ProgramMap::version)
    else {
        server.respond(req.error("nothing is running to evaluate against"))?;
        return Ok(Flow::Continue);
    };
    let frames = session
        .native_debug
        .as_ref()
        .map(|ctl| ctl.frames())
        .unwrap_or_default();
    let frame_idx = usize::try_from(frame_id).unwrap_or(usize::MAX);
    let Some(frame) = frames.get(frame_idx) else {
        server.respond(req.error("no such frame: the debuggee is not stopped there"))?;
        return Ok(Flow::Continue);
    };

    let vars: Vec<(String, DebugValue)> = frame
        .vars
        .iter()
        .map(|var| (var.name.clone(), var.value.clone()))
        .collect();
    let evaluated = expr::compile(&expression, version).and_then(|compiled| {
        expr::eval(&compiled, &vars, version).map(|value| expr::render(&value, version))
    });

    match evaluated {
        Ok(result) => {
            let response = EvaluateResponse {
                result,
                variables_reference: 0,
                ..Default::default()
            };
            server.respond(req.success(ResponseBody::Evaluate(response)))?;
        }
        Err(message) => server.respond(req.error(&message))?,
    }
    Ok(Flow::Continue)
}
