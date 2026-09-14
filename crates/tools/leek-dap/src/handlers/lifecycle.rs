//! Lifecycle requests: initialize, launch, configurationDone, and the
//! disconnect/terminate shutdown.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use dap::events::{BreakpointEventBody, StoppedEventBody};
use dap::prelude::*;
use dap::server::ServerOutput;
use dap::types::{BreakpointEventReason, OutputEventCategory, StoppedEventReason};

use crate::breakpoints::Requested;
use crate::capabilities::capabilities;
use crate::debug::{NativeDebugSession, StopInfo, StopReason};
use crate::event;
use crate::handlers::{Flow, breakpoints};
use crate::session::{MAIN_THREAD_ID, Session};
use crate::target::native::{Compiled, NativeTarget, run_compiled};
use crate::target::{LaunchConfig, RunOutcome};

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
                .map_err(|e| format!("invalid launch configuration: {e}"))
                // `noDebug` is DAP's own launch field, so it is parsed out of
                // the arguments before the adapter-specific blob is formed and
                // would otherwise be dropped on the floor.
                .map(|config| LaunchConfig {
                    no_debug: config.no_debug || args.no_debug.unwrap_or(false),
                    ..config
                }),
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
/// The debuggee always runs on a worker thread — a fight, or any program
/// with real work in it, would otherwise hold the request loop for its whole
/// run and leave `terminate`/`disconnect` unanswered until it finished. A
/// `noDebug` launch takes the same path with the instrumentation switched
/// off.
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

    // Which file is which `SourceId` and which lines carry a safepoint. Kept
    // on the session because `program` is about to be moved into the run
    // thread: without it a `setBreakpoints` arriving mid-run would have
    // nothing left to resolve a path against.
    session.program = Some(program.breakpoint_map());
    // Where each breakpoint really lands is a fact about the compiled program,
    // so it is reported either way; a `noDebug` client ignores breakpoints
    // anyway.
    announce_breakpoints(session, server)?;

    // `noDebug` runs the very same program with breakpoints, stepping and the
    // per-statement safepoints switched off.
    let debug = !config.no_debug;
    if debug {
        install_debug_hook(session, server, &program, &config);
    }

    session.run_thread = Some(spawn_run(server.output.clone(), move || {
        // A scenario debugs `program` inside the fight; otherwise it runs
        // standalone. Both honor the breakpoints installed above.
        if config.scenario.is_some() {
            crate::target::fight::run_fight_debug(&config, &program, debug)
        } else {
            run_compiled(&program, debug)
        }
    }));

    Ok(Flow::Continue)
}

/// Collect the client's breakpoints, build the debug controller, and install
/// it as the native backend's global debug hook. The controller emits the DAP
/// `stopped` events from the debuggee thread while the main loop keeps
/// servicing requests (continue, stackTrace, …).
fn install_debug_hook<R: Read, W: Write + Send + 'static>(
    session: &mut Session,
    server: &Server<R, W>,
    program: &Compiled,
    config: &LaunchConfig,
) {
    let on_stop_output = server.output.clone();
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
            // Which breakpoint fired, so the client can point at it rather
            // than guess from the line. Absent, not empty, for a stop no
            // breakpoint caused.
            hit_breakpoint_ids: (!info.hit_breakpoint_ids.is_empty())
                .then_some(info.hit_breakpoint_ids),
        };
        if let Ok(mut out) = on_stop_output.lock() {
            let _ = out.send_event(Event::Stopped(body));
        }
        let _ = info.line; // surfaced via stackTrace, not the stopped event
    });

    let by_source = session
        .program
        .as_ref()
        .map(|map| session.breakpoints.by_source(map))
        .unwrap_or_default();
    let controller = Arc::new(NativeDebugSession::new(
        program.debug_sources(),
        by_source,
        config.stop_on_entry,
        on_stop,
    ));
    leek_backend_native::set_debug_hook(Some(controller.clone()));
    session.native_debug = Some(controller);
}

/// Re-answer every breakpoint now that the program is compiled.
///
/// A client sets its breakpoints before `configurationDone`, when there is
/// nothing to check them against and each one was answered "pending launch".
/// A `breakpoint` change event carrying the same id is how DAP delivers the
/// real verdict — and the moved line, when the requested one carried no code.
fn announce_breakpoints<R: Read, W: Write>(
    session: &Session,
    server: &mut Server<R, W>,
) -> anyhow::Result<()> {
    let Some(program) = session.program.as_ref() else {
        return Ok(());
    };
    // By id: the order the client set them in, so the events read the way the
    // responses did.
    let mut stored: Vec<_> = session.breakpoints.iter().collect();
    stored.sort_by_key(|&(_, _, id)| id);
    for (path, line, id) in stored {
        let requested = Requested {
            line: i64::from(line),
            stored: Some((line, id)),
        };
        server.send_event(Event::Breakpoint(BreakpointEventBody {
            reason: BreakpointEventReason::Changed,
            breakpoint: breakpoints::answer(Some(program), path, &requested),
        }))?;
    }
    Ok(())
}

/// Run the debuggee on a worker thread, then report its output, exit code and
/// termination to the client. Returns the worker's handle so the session can
/// tell a run is in flight.
fn spawn_run<W: Write + Send + 'static>(
    output: Arc<Mutex<ServerOutput<W>>>,
    run: impl FnOnce() -> RunOutcome + Send + 'static,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let outcome = run();
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
    })
}

/// Send a program's output, exit code, and the terminated event. Used by the
/// compile-error path, which never starts a debuggee and so can report from
/// the request loop itself.
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{BufReader, BufWriter};

    use super::*;
    use crate::breakpoints::BreakpointStore;
    use crate::target::native::canonical;
    use crate::testing::{SharedOut, project};

    /// Drive `configurationDone` for a launch config and return the session
    /// plus everything the adapter wrote to the client.
    fn configure(config: serde_json::Value) -> (Session, SharedOut) {
        let mut session = Session::new();
        session.pending_launch = Some(serde_json::from_value(config).expect("launch config"));
        let sink = SharedOut::new();
        let mut server = Server::new(
            BufReader::new(std::io::empty()),
            BufWriter::new(sink.clone()),
        );
        let flow = configuration_done(
            &mut session,
            &mut server,
            Request {
                seq: 1,
                command: Command::ConfigurationDone,
            },
        )
        .expect("configurationDone");
        assert!(matches!(flow, Flow::Continue));
        (session, sink)
    }

    #[test]
    fn a_no_debug_launch_runs_on_a_worker_and_resolves_includes() {
        let dir = project("nodebug");
        let (mut session, sink) = configure(serde_json::json!({
            "program": dir.join("main.leek").display().to_string(),
            "noDebug": true,
        }));

        // The handler returned while the debuggee is still owned by a worker,
        // so the request loop stays free to answer terminate/disconnect.
        let worker = session
            .run_thread
            .take()
            .expect("a noDebug launch must run the debuggee on a worker thread");
        worker.join().expect("debuggee worker");

        let out = sink.text();
        assert!(
            out.contains("\"event\":\"terminated\""),
            "no terminated event: {out}"
        );
        // `answer()` lives in the included file: without the project include
        // graph the program would not compile.
        assert!(out.contains("42"), "included function never ran: {out}");

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_breakpoint_in_an_included_file_maps_to_its_own_source() {
        let dir = project("includebp");
        let lib = dir.join("lib.leek");
        let mut breakpoints = BreakpointStore::default();
        breakpoints.replace(&lib, &[1]);

        let program = NativeTarget::launch(
            &serde_json::from_value(serde_json::json!({
                "program": dir.join("main.leek").display().to_string(),
            }))
            .expect("launch config"),
        )
        .compile()
        .unwrap_or_else(|outcome| panic!("compile failed: {}", outcome.output));

        let lib_id = program
            .sources
            .iter()
            .find(|file| canonical(&file.path) == canonical(&lib))
            .expect("the included file is one of the program's sources")
            .source
            .get();
        assert_ne!(lib_id, 1, "the included file gets its own SourceId");

        let by_source = breakpoints.by_source(&program.breakpoint_map());
        assert_eq!(
            by_source.get(&lib_id).map(HashMap::len),
            Some(1),
            "the breakpoint was not attributed to the included file"
        );
        assert!(
            !by_source.contains_key(&1),
            "an included file's breakpoint leaked into the entry"
        );

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
