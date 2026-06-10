//! Lifecycle requests: initialize, launch, configurationDone, and the
//! disconnect/terminate shutdown.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::sync::Arc;

use dap::events::StoppedEventBody;
use dap::prelude::*;
use dap::types::{OutputEventCategory, StoppedEventReason};

use crate::capabilities::capabilities;
use crate::debug::{NativeDebugSession, StopInfo, StopReason};
use crate::event;
use crate::handlers::Flow;
use crate::session::{MAIN_THREAD_ID, Session};
use crate::target::LaunchConfig;
use crate::target::native::{NativeTarget, run_compiled};

/// `initialize`: advertise capabilities, then announce readiness.
pub(crate) fn initialize<R: Read, W: Write>(
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    server.respond(req.success(ResponseBody::Initialize(capabilities())))?;
    server.send_event(Event::Initialized)?;
    Ok(Flow::Continue)
}

/// `launch`: parse the adapter-specific config and stash it. DAP defers
/// the actual program start until `configurationDone`.
pub(crate) fn launch<R: Read, W: Write>(
    session: &mut Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    // The editor's launch.json fields arrive flattened into
    // `additional_data`. Parse into an owned config before consuming
    // `req` to build the response.
    let parsed: Result<LaunchConfig, String> = {
        let Command::Launch(args) = &req.command else {
            unreachable!("dispatch guarantees this is a Launch")
        };
        match &args.additional_data {
            Some(value) => serde_json::from_value::<LaunchConfig>(value.clone())
                .map_err(|e| format!("invalid launch configuration: {e}")),
            None => {
                Err("launch request is missing a `program` (set it in launch.json)".to_string())
            }
        }
    };

    match parsed {
        Ok(config) => {
            session.pending_launch = Some(config);
            server.respond(req.success(ResponseBody::Launch))?;
        }
        Err(message) => {
            server.respond(req.error(&message))?;
        }
    }
    Ok(Flow::Continue)
}

/// `configurationDone`: breakpoints are set, so start the program.
///
/// Skeleton behavior: compile + run the native target to completion,
/// reporting its result and exit code. When live debugging lands this
/// is where execution begins and the first `stopped` event (on entry
/// or at a breakpoint) is emitted instead of running straight through.
pub(crate) fn configuration_done<R: Read, W: Write + Send + 'static>(
    session: &mut Session,
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    server.respond(req.success(ResponseBody::ConfigurationDone))?;

    let Some(config) = session.pending_launch.take() else {
        return Ok(Flow::Continue);
    };
    session.started = true;
    session.program_path = Some(config.program.display().to_string());

    // Compile first; a compile error terminates the session cleanly.
    let target = NativeTarget::launch(&config);
    let program = match target.compile() {
        Ok(program) => program,
        Err(outcome) => {
            emit_terminated(server, &outcome)?;
            return Ok(Flow::Continue);
        }
    };

    if config.no_debug {
        // Run to completion natively, no instrumentation. A scenario still runs
        // the fight, just without breakpoints.
        let outcome = if config.scenario.is_some() {
            crate::target::fight::run_fight_debug(&config, &program)
        } else {
            run_compiled(&program, false)
        };
        emit_terminated(server, &outcome)?;
        return Ok(Flow::Continue);
    }

    // Debug run: collect breakpoint lines, install a controller, and run the
    // instrumented program on a worker thread so the main loop can keep
    // servicing requests (continue, stackTrace, …) while the debuggee is
    // parked at a breakpoint.
    let breakpoint_lines: HashSet<u32> = session
        .breakpoints
        .values()
        .flatten()
        .filter_map(|&line| u32::try_from(line).ok())
        .collect();

    let output = server.output.clone();
    let on_stop_output = output.clone();
    let on_stop = Box::new(move |info: StopInfo| {
        let reason = match info.reason {
            StopReason::Breakpoint => StoppedEventReason::Breakpoint,
            StopReason::Entry => StoppedEventReason::Entry,
            StopReason::Pause => StoppedEventReason::Pause,
            StopReason::Step => StoppedEventReason::Step,
        };
        let body = StoppedEventBody {
            reason,
            description: None,
            thread_id: Some(MAIN_THREAD_ID),
            preserve_focus_hint: None,
            text: None,
            all_threads_stopped: Some(true),
            hit_breakpoint_ids: None,
        };
        if let Ok(mut out) = on_stop_output.lock() {
            let _ = out.send_event(Event::Stopped(body));
        }
        let _ = info.line; // surfaced via stackTrace, not the stopped event
    });

    let controller = Arc::new(NativeDebugSession::new(
        &program.source,
        breakpoint_lines,
        config.stop_on_entry,
        on_stop,
    ));
    leek_backend_native::set_debug_hook(Some(controller.clone()));
    session.native_debug = Some(controller);

    std::thread::spawn(move || {
        // A scenario debugs `program` inside the fight; otherwise it runs
        // standalone. Both honor the breakpoints installed above.
        let outcome = if config.scenario.is_some() {
            crate::target::fight::run_fight_debug(&config, &program)
        } else {
            run_compiled(&program, true)
        };
        leek_backend_native::set_debug_hook(None);
        if let Ok(mut out) = output.lock() {
            let category = if outcome.exit_code == 0 {
                OutputEventCategory::Stdout
            } else {
                OutputEventCategory::Stderr
            };
            let _ = out.send_event(Event::Output(event::output(category, outcome.output)));
            let _ = out.send_event(Event::Exited(event::exited(outcome.exit_code)));
            let _ = out.send_event(Event::Terminated(None));
        }
    });

    Ok(Flow::Continue)
}

/// Send a program's output, exit code, and the terminated event (used by the
/// compile-error and no-debug paths, which run on the main thread).
fn emit_terminated<R: Read, W: Write>(
    server: &mut Server<R, W>,
    outcome: &crate::target::RunOutcome,
) -> anyhow::Result<()> {
    let category = if outcome.exit_code == 0 {
        OutputEventCategory::Stdout
    } else {
        OutputEventCategory::Stderr
    };
    server.send_event(Event::Output(event::output(
        category,
        outcome.output.clone(),
    )))?;
    server.send_event(Event::Exited(event::exited(outcome.exit_code)))?;
    server.send_event(Event::Terminated(None))?;
    Ok(())
}

/// `disconnect` / `terminate`: acknowledge and stop the request loop.
pub(crate) fn shutdown<R: Read, W: Write>(
    server: &mut Server<R, W>,
    req: Request,
) -> anyhow::Result<Flow> {
    let body = match &req.command {
        Command::Terminate(_) => ResponseBody::Terminate,
        _ => ResponseBody::Disconnect,
    };
    server.respond(req.success(body))?;
    Ok(Flow::Shutdown)
}
