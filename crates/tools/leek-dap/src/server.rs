//! Transport setup and the request loop.
//!
//! The whole adapter is synchronous and single-threaded: poll one
//! request, dispatch it (which may emit responses and events), repeat
//! until the client disconnects or sends EOF.

use std::io::{self, BufReader, BufWriter, Read, Write};

use dap::prelude::*;

use crate::handlers::{self, Flow};
use crate::session::Session;
use crate::wire::{RawMessages, tee};

/// Run the DAP server over stdio until the client disconnects.
///
/// # Errors
/// Returns any I/O or protocol error from the underlying transport.
pub fn run_stdio() -> anyhow::Result<()> {
    let (input, raw) = tee(io::stdin());
    let output = BufWriter::new(io::stdout());
    let mut server = Server::new(BufReader::new(input), output);
    serve(&mut server, &raw)
}

/// Drive a [`Server`] to completion. Generic over the transport so
/// tests can feed a scripted request stream.
///
/// `raw` is the transport's record of the message it last forwarded — see
/// [`crate::wire`] for why one request field has to be read from there rather
/// than from the typed arguments.
pub(crate) fn serve<R: Read, W: Write + Send + 'static>(
    server: &mut Server<R, W>,
    raw: &std::sync::Arc<RawMessages>,
) -> anyhow::Result<()> {
    let mut session = Session::new(std::sync::Arc::clone(raw));
    // `poll_request` yields `None` at EOF (client closed the pipe).
    while let Some(req) = server.poll_request()? {
        if let Flow::Shutdown = handlers::dispatch(&mut session, server, req)? {
            break;
        }
    }
    // However the loop ended — `disconnect`, `terminate` or a closed pipe —
    // the debuggee may still be parked at a breakpoint waiting for a client
    // that has gone. Let it go before the session drops.
    session.stop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, BufWriter, Cursor};
    use std::path::Path;

    use serde_json::Value;

    use super::*;
    use crate::testing::{
        Client, SharedOut, debug_session_guard, messages, project, project_with, set_breakpoints,
    };

    /// Feed a scripted request stream through the real request loop and hand
    /// back every message the adapter wrote. This is the whole adapter: the
    /// framing, the dispatch, the handlers and the session, driven exactly as
    /// an editor drives them.
    fn converse(requests: &str) -> Vec<Value> {
        let sink = SharedOut::new();
        let (input, raw) = tee(Cursor::new(requests.as_bytes().to_vec()));
        let mut server = Server::new(BufReader::new(input), BufWriter::new(sink.clone()));
        serve(&mut server, &raw).expect("request loop");
        drop(server);
        messages(&sink)
    }

    /// The `breakpoints` array of the nth `setBreakpoints` response.
    fn nth_response(messages: &[Value], n: usize) -> &Vec<Value> {
        messages
            .iter()
            .filter(|m| m["type"] == "response" && m["command"] == "setBreakpoints")
            .nth(n)
            .unwrap_or_else(|| panic!("no setBreakpoints response #{n}"))["body"]["breakpoints"]
            .as_array()
            .expect("breakpoints array")
    }

    #[test]
    fn a_second_set_breakpoints_replaces_the_first() {
        let main = Path::new("/tmp/leek-dap-proto/main.leek");
        let out = converse(&format!(
            "{}{}",
            set_breakpoints(1, main, &[2, 3]),
            set_breakpoints(2, main, &[5])
        ));
        assert_eq!(nth_response(&out, 0).len(), 2);
        let second = nth_response(&out, 1);
        assert_eq!(second.len(), 1, "the replacement kept the old lines");
        assert_eq!(second[0]["line"], 5);
    }

    #[test]
    fn an_empty_set_breakpoints_clears_the_file() {
        let main = Path::new("/tmp/leek-dap-proto/main.leek");
        let out = converse(&format!(
            "{}{}",
            set_breakpoints(1, main, &[2, 3]),
            set_breakpoints(2, main, &[])
        ));
        assert!(
            nth_response(&out, 1).is_empty(),
            "clearing a file answered with breakpoints"
        );
    }

    #[test]
    fn every_breakpoint_is_answered_with_an_id_and_its_own_file() {
        let (main, lib) = (
            Path::new("/tmp/leek-dap-proto/main.leek"),
            Path::new("/tmp/leek-dap-proto/lib.leek"),
        );
        let out = converse(&format!(
            "{}{}",
            set_breakpoints(1, main, &[12]),
            set_breakpoints(2, lib, &[12])
        ));
        let first = &nth_response(&out, 0)[0];
        let second = &nth_response(&out, 1)[0];
        assert!(first["id"].is_i64(), "no breakpoint id: {first}");
        assert_ne!(
            first["id"], second["id"],
            "line 12 of two files was one breakpoint"
        );
        assert_eq!(first["source"]["path"], main.display().to_string());
        assert_eq!(second["source"]["path"], lib.display().to_string());
    }

    #[test]
    fn a_breakpoint_set_before_launch_is_not_claimed_verified() {
        let out = converse(&set_breakpoints(
            1,
            Path::new("/tmp/leek-dap-proto/main.leek"),
            &[2],
        ));
        let bp = &nth_response(&out, 0)[0];
        assert_eq!(
            bp["verified"], false,
            "verified with nothing compiled: {bp}"
        );
        assert!(bp["message"].is_string(), "no reason given: {bp}");
    }

    /// The whole handshake up to `configurationDone`, with breakpoints set in
    /// the entry (a real line and a comment-only one) and in a file the
    /// program knows nothing about.
    fn launch_script(dir: &Path, scratch: &Path) -> String {
        let entry = dir.join("main.leek");
        format!(
            "{}{}{}{}{}",
            crate::testing::frame(&serde_json::json!({
                "seq": 1, "type": "request", "command": "initialize",
                "arguments": { "adapterID": "leek" },
            })),
            crate::testing::frame(&serde_json::json!({
                "seq": 2, "type": "request", "command": "launch",
                "arguments": {
                    "noDebug": true,
                    "program": entry.display().to_string(),
                },
            })),
            set_breakpoints(3, &entry, &[2]),
            set_breakpoints(4, scratch, &[1]),
            crate::testing::frame(&serde_json::json!({
                "seq": 5, "type": "request", "command": "configurationDone",
            })),
        )
    }

    #[test]
    fn a_breakpoint_in_an_unknown_file_says_so_once_the_program_is_compiled() {
        let dir = project("proto-unknown");
        let scratch = dir.join("scratch.leek");
        std::fs::write(&scratch, "var untouched = 1\n").expect("scratch file");
        let out = converse(&launch_script(&dir, &scratch));

        // The entry's breakpoint and the scratch file's were both answered
        // "pending launch"; `configurationDone` re-answers each one.
        let changes: Vec<&Value> = out
            .iter()
            .filter(|m| m["event"] == "breakpoint")
            .map(|m| &m["body"]["breakpoint"])
            .collect();
        assert_eq!(changes.len(), 2, "not every breakpoint was re-answered");

        let entry_path = dir.join("main.leek").display().to_string();
        let entry = changes
            .iter()
            .find(|bp| bp["source"]["path"] == entry_path)
            .expect("the entry's breakpoint");
        assert_eq!(entry["verified"], true, "a real code line: {entry}");

        let unknown = changes
            .iter()
            .find(|bp| bp["source"]["path"] != entry_path)
            .expect("the scratch file's breakpoint");
        assert_eq!(unknown["verified"], false, "a file the program never saw");
        assert!(
            unknown["message"]
                .as_str()
                .is_some_and(|m| m.contains("not part of")),
            "no explanation: {unknown}"
        );

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    // --- interactive protocol tests -------------------------------------
    //
    // Everything above scripts a fixed request stream and reads the output
    // once the loop has ended. These drive a live [`Client`] instead, so the
    // test answers what the adapter says: a `stopped` event with `continue`,
    // a stop with `stackTrace`. That is the only way to reach the execution
    // and inspection handlers at all — they do nothing unless a debuggee is
    // parked.

    /// A program with a callee and a loop, so a test has somewhere to step
    /// into, over and out of:
    ///
    /// ```text
    /// 1  function twice(x) {
    /// 2      var doubled = x * 2
    /// 3      return doubled
    /// 4  }
    /// 5  var sum = 0
    /// 6  for (var i = 0; i < 3; i++) {
    /// 7      sum = sum + twice(i)
    /// 8  }
    /// 9  return sum
    /// ```
    const STEPS: &str = concat!(
        "function twice(x) {\n",
        "    var doubled = x * 2\n",
        "    return doubled\n",
        "}\n",
        "var sum = 0\n",
        "for (var i = 0; i < 3; i++) {\n",
        "    sum = sum + twice(i)\n",
        "}\n",
        "return sum\n",
    );

    /// [`STEPS`] with `iterations` passes through the loop instead of three.
    /// Same text otherwise, so the line numbers above still hold.
    fn steps_looping(iterations: u32) -> String {
        STEPS.replace("i < 3", &format!("i < {iterations}"))
    }

    /// The two requests every session opens with. `initialized` is what tells
    /// a client it may start sending `setBreakpoints`.
    fn handshake(client: &mut Client, config: Value) {
        client.ok("initialize", serde_json::json!({ "adapterID": "leek" }));
        client.event("initialized");
        client.ok("launch", config);
    }

    /// The frames of the current stop, innermost first.
    fn stack(client: &mut Client) -> Vec<Value> {
        let response = client.ok("stackTrace", serde_json::json!({ "threadId": 1 }));
        assert_eq!(
            response["totalFrames"],
            Value::from(response["stackFrames"].as_array().map_or(0, Vec::len)),
            "totalFrames disagrees with the frames sent: {response}"
        );
        response["stackFrames"]
            .as_array()
            .expect("stack frames")
            .clone()
    }

    /// The names of the variables behind a `variablesReference`.
    fn variable_names(client: &mut Client, reference: i64) -> Vec<String> {
        client.ok(
            "variables",
            serde_json::json!({ "variablesReference": reference }),
        )["variables"]
            .as_array()
            .expect("variables")
            .iter()
            .map(|variable| variable["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    #[test]
    fn initialize_advertises_only_what_is_implemented() {
        let mut client = Client::spawn();
        let capabilities = client.ok("initialize", serde_json::json!({ "adapterID": "leek" }));
        for implemented in [
            "supportsConfigurationDoneRequest",
            "supportsTerminateRequest",
            "supportsConditionalBreakpoints",
            "supportsLogPoints",
            "supportsBreakpointLocationsRequest",
        ] {
            assert_eq!(
                capabilities[implemented], true,
                "did not advertise {implemented}: {capabilities}"
            );
        }
        // A client that believes one of these sends the adapter something it
        // cannot answer well, so each stays off until it is real: hit counts
        // ride on an arrival test that fires several times per source line
        // (#408), and a hover sends expressions outside the subset `evaluate`
        // accepts.
        for unimplemented in [
            "supportsHitConditionalBreakpoints",
            "supportsEvaluateForHovers",
        ] {
            assert!(
                capabilities[unimplemented].is_null(),
                "advertised {unimplemented}: {capabilities}"
            );
        }
        client.event("initialized");
    }

    #[test]
    fn a_launch_with_no_program_is_an_error_and_starts_nothing() {
        let mut client = Client::spawn();
        client.ok("initialize", serde_json::json!({ "adapterID": "leek" }));
        let response = client.request("launch", serde_json::json!({}));
        assert_eq!(
            response["success"], false,
            "a launch with no program: {response}"
        );
        assert!(
            response["message"]
                .as_str()
                .is_some_and(|message| message.contains("program")),
            "the error does not say what is missing: {response}"
        );
        // Nothing was stashed, so `configurationDone` starts nothing — and the
        // session keeps answering requests rather than falling over.
        client.ok("configurationDone", Value::Null);
        assert_eq!(client.ok("threads", Value::Null)["threads"][0]["id"], 1);
    }

    #[test]
    fn daps_own_no_debug_flag_is_honored() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-nodebug-top", &[("main.leek", STEPS)]);
        let mut client = Client::spawn();
        // `noDebug` here is DAP's own launch field: it is parsed out of the
        // arguments before the adapter-specific blob is formed, so the config
        // that reaches the target would lose it unless the two are merged.
        // `stopOnEntry` is the tell — a debug launch would stop at line 5.
        handshake(
            &mut client,
            serde_json::json!({
                "noDebug": true,
                "program": dir.join("main.leek").display().to_string(),
                "stopOnEntry": true,
            }),
        );
        client.ok("configurationDone", Value::Null);
        let (event, _) = client.event_any(&["stopped", "terminated"]);
        assert_eq!(event, "terminated", "a noDebug launch stopped the debuggee");

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_compile_error_ends_the_session_instead_of_starting_a_debuggee() {
        let dir = project_with("proto-broken", &[("main.leek", "var a = \n")]);
        let mut client = Client::spawn();
        handshake(
            &mut client,
            serde_json::json!({ "program": dir.join("main.leek").display().to_string() }),
        );
        client.ok("configurationDone", Value::Null);

        let output = client.event("output");
        assert_eq!(
            output["category"], "stderr",
            "a diagnostic on stdout: {output}"
        );
        assert!(
            output["output"]
                .as_str()
                .is_some_and(|text| text.contains("compilation failed")),
            "the diagnostic was not reported: {output}"
        );
        assert_eq!(client.event("exited")["exitCode"], 1);
        client.event("terminated");

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_breakpoint_stops_the_debuggee_and_its_whole_state_is_inspectable() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-breakpoint", &[("main.leek", STEPS)]);
        let entry = dir.join("main.leek").display().to_string();
        let mut client = Client::spawn();
        handshake(&mut client, serde_json::json!({ "program": entry }));

        // Line 3 is `return doubled`, inside the callee: stopping there proves
        // the stack has the caller under it.
        let set = client.ok(
            "setBreakpoints",
            serde_json::json!({
                "source": { "path": entry },
                "breakpoints": [{ "line": 3 }],
            }),
        );
        let id = set["breakpoints"][0]["id"].clone();
        assert!(
            id.is_i64(),
            "no breakpoint id to match the stop against: {set}"
        );
        client.ok("configurationDone", Value::Null);

        let stopped = client.event("stopped");
        assert_eq!(stopped["reason"], "breakpoint");
        assert_eq!(stopped["threadId"], 1);
        assert_eq!(
            stopped["hitBreakpointIds"],
            serde_json::json!([id]),
            "the stop does not name the breakpoint that caused it: {stopped}"
        );

        assert_eq!(
            client.ok("threads", Value::Null)["threads"],
            serde_json::json!([{ "id": 1, "name": "main" }])
        );

        let frames = stack(&mut client);
        assert_eq!(frames.len(), 2, "the caller is missing: {frames:?}");
        assert_eq!(frames[0]["name"], "twice");
        assert_eq!(frames[0]["line"], 3);
        assert_eq!(frames[0]["source"]["path"], entry.as_str());
        assert_eq!(frames[1]["line"], 7, "the caller is not on the call line");

        // `scopes` encodes the frame in the reference `variables` decodes:
        // frame 0 → 1, frame 1 → 2.
        for frame in &frames {
            let scopes = client.ok("scopes", serde_json::json!({ "frameId": frame["id"] }));
            assert_eq!(scopes["scopes"][0]["name"], "Locals");
            assert_eq!(
                scopes["scopes"][0]["variablesReference"],
                Value::from(frame["id"].as_i64().expect("frame id") + 1)
            );
        }
        assert_eq!(variable_names(&mut client, 1), ["x", "doubled"]);
        assert_eq!(variable_names(&mut client, 2), ["sum", "i"]);
        assert_eq!(
            client.ok("variables", serde_json::json!({ "variablesReference": 1 }))["variables"][0]
                ["value"],
            "0",
            "the callee's argument is not the value it was called with"
        );

        // The loop calls `twice` three times, so the breakpoint fires three
        // times and the program then ends on its own.
        let mut stops = 1;
        loop {
            client.ok("continue", serde_json::json!({ "threadId": 1 }));
            if client.event_any(&["stopped", "terminated"]).0 == "terminated" {
                break;
            }
            stops += 1;
        }
        assert_eq!(stops, 3, "the breakpoint did not fire once per iteration");

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn stepping_walks_into_a_callee_and_back_out_without_losing_the_depth() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-stepping", &[("main.leek", STEPS)]);
        let mut client = Client::spawn();
        handshake(
            &mut client,
            serde_json::json!({
                "program": dir.join("main.leek").display().to_string(),
                "stopOnEntry": true,
            }),
        );
        client.ok("configurationDone", Value::Null);

        /// Send one step request, wait for the stop it produces, and report
        /// the frames it left the debuggee in.
        fn step(client: &mut Client, command: &str) -> Vec<Value> {
            client.ok(command, serde_json::json!({ "threadId": 1 }));
            let stopped = client.event("stopped");
            assert_eq!(stopped["reason"], "step", "after `{command}`: {stopped}");
            stack(client)
        }

        assert_eq!(client.event("stopped")["reason"], "entry");
        let entry = stack(&mut client);
        assert_eq!(entry.len(), 1);
        assert_eq!(entry[0]["name"], "<main>");
        assert_eq!(entry[0]["line"], 5, "not the program's first statement");

        // Down to the call line, one statement at a time.
        assert_eq!(step(&mut client, "stepIn")[0]["line"], 6);
        let at_call = step(&mut client, "stepIn");
        assert_eq!(at_call[0]["line"], 7);
        assert_eq!(at_call.len(), 1);

        // Into the callee: a frame deeper, and the caller is still under it.
        let inside = step(&mut client, "stepIn");
        assert_eq!(
            inside.len(),
            2,
            "stepIn did not enter the callee: {inside:?}"
        );
        assert_eq!(inside[0]["name"], "twice");
        assert_eq!(inside[1]["line"], 7);

        // Out again: back to the caller's depth, on the line that called.
        let back = step(&mut client, "stepOut");
        assert_eq!(back.len(), 1, "stepOut did not leave the callee: {back:?}");
        assert_eq!(back[0]["line"], 7);

        // Over the call line: the next stop is the loop header in *this*
        // frame, not the callee's first statement.
        let over = step(&mut client, "next");
        assert_eq!(over.len(), 1, "next stepped into the callee: {over:?}");
        assert_eq!(over[0]["line"], 6);

        client.ok("continue", serde_json::json!({ "threadId": 1 }));
        client.event("terminated");

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn pause_stops_the_debuggee_at_the_next_statement() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-pause", &[("main.leek", STEPS)]);
        let mut client = Client::spawn();
        handshake(
            &mut client,
            serde_json::json!({
                "program": dir.join("main.leek").display().to_string(),
                "stopOnEntry": true,
            }),
        );
        client.ok("configurationDone", Value::Null);
        assert_eq!(client.event("stopped")["reason"], "entry");

        // Asked for while the debuggee is parked, then released: a `pause`
        // aimed at a *running* debuggee would race the program to its end and
        // decide the test on timing rather than on behavior.
        client.ok("pause", serde_json::json!({ "threadId": 1 }));
        client.ok("continue", serde_json::json!({ "threadId": 1 }));

        let stopped = client.event("stopped");
        assert_eq!(stopped["reason"], "pause", "{stopped}");
        assert!(
            stopped["hitBreakpointIds"].is_null(),
            "a pause blamed a breakpoint: {stopped}"
        );
        // Which statement it lands on is a lowering detail (one source line
        // can carry several safepoints); that it landed inside the program,
        // with a live frame to inspect, is the behavior.
        let frames = stack(&mut client);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["name"], "<main>");

        client.ok("continue", serde_json::json!({ "threadId": 1 }));
        client.event("terminated");

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn variables_for_a_frame_that_is_not_there_is_empty_not_a_panic() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-badref", &[("main.leek", STEPS)]);
        let mut client = Client::spawn();
        handshake(
            &mut client,
            serde_json::json!({
                "program": dir.join("main.leek").display().to_string(),
                "stopOnEntry": true,
            }),
        );
        client.ok("configurationDone", Value::Null);
        client.event("stopped");

        // Reference 1 is frame 0 and has locals; 0 underflows the `ref - 1`
        // decoding and the huge one is simply past the end. Neither is a
        // frame, and a client asks about both (a stale reference from the
        // previous stop, a scope it never got from `scopes`).
        assert!(!variable_names(&mut client, 1).is_empty());
        for bad in [0, 99_999] {
            assert!(
                variable_names(&mut client, bad).is_empty(),
                "variablesReference {bad} resolved to a frame"
            );
        }

        client.ok("continue", serde_json::json!({ "threadId": 1 }));
        client.event("terminated");

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn an_unsupported_request_is_answered_with_an_error() {
        let mut client = Client::spawn();
        client.ok("initialize", serde_json::json!({ "adapterID": "leek" }));
        // `setFunctionBreakpoints` is a request the adapter parses but does
        // not implement; answering it is what keeps a client from waiting
        // forever.
        let response = client.request(
            "setFunctionBreakpoints",
            serde_json::json!({ "breakpoints": [{ "name": "twice" }] }),
        );
        assert_eq!(response["success"], false, "{response}");
        assert_eq!(response["message"], "unsupported request");
        // The loop carries on afterwards.
        assert_eq!(client.ok("threads", Value::Null)["threads"][0]["id"], 1);
    }

    #[test]
    fn set_exception_breakpoints_is_answered_even_though_there_are_no_filters() {
        let mut client = Client::spawn();
        client.ok("initialize", serde_json::json!({ "adapterID": "leek" }));
        let body = client.ok(
            "setExceptionBreakpoints",
            serde_json::json!({ "filters": [] }),
        );
        assert!(
            body["breakpoints"].as_array().is_none_or(Vec::is_empty),
            "filters were claimed that the adapter does not have: {body}"
        );
    }

    #[test]
    fn disconnecting_while_the_debuggee_is_parked_releases_it() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-disconnect", &[("main.leek", STEPS)]);
        let mut client = Client::spawn();
        handshake(
            &mut client,
            serde_json::json!({
                "program": dir.join("main.leek").display().to_string(),
                "stopOnEntry": true,
            }),
        );
        client.ok("configurationDone", Value::Null);
        client.event("stopped");

        let response = client.request("disconnect", serde_json::json!({}));
        assert_eq!(response["success"], true, "{response}");
        // The debuggee is parked on a condvar with nobody left to answer its
        // stop. Ending the request loop has to let it go: otherwise that
        // thread sits there for the life of the process with the debug hook
        // still installed, and this event never arrives.
        client.event("terminated");

        // And the loop really did end (`finish` asserts it), which it cannot
        // do until the worker has been joined.
        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// A `noDebug` run ending must leave a debug session that is already in
    /// flight exactly as it found it.
    ///
    /// The backend's debug hook is one process-global slot. A run that never
    /// installed a hook has none to remove, and a run that did owns only its
    /// own: clearing the slot wholesale pulls the live session's controller
    /// out from under a parked debuggee, which then sails through every later
    /// breakpoint and runs to the end — exactly the failure the next test is
    /// named for, a breakpoint set while parked that never fires.
    #[test]
    fn a_no_debug_run_ending_leaves_a_live_debug_session_alone() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-nodebug-bystander", &[("main.leek", STEPS)]);
        let entry = dir.join("main.leek").display().to_string();

        // A debug session, parked at program entry with its hook installed.
        let mut debugged = Client::spawn();
        handshake(
            &mut debugged,
            serde_json::json!({ "program": entry, "stopOnEntry": true }),
        );
        debugged.ok("configurationDone", Value::Null);
        assert_eq!(debugged.event("stopped")["reason"], "entry");

        // A second session runs the same program to completion with `noDebug`.
        // Its worker sends `terminated` last of all, so by the time that event
        // arrives whatever tidying up it does has already happened.
        let mut bystander = Client::spawn();
        handshake(
            &mut bystander,
            serde_json::json!({ "program": entry, "noDebug": true }),
        );
        bystander.ok("configurationDone", Value::Null);
        bystander.event("terminated");
        bystander.finish();

        // The first session is still being debugged: a breakpoint set now
        // still fires, on this run.
        let set = debugged.ok(
            "setBreakpoints",
            serde_json::json!({
                "source": { "path": entry },
                "breakpoints": [{ "line": 3 }],
            }),
        );
        debugged.ok("continue", serde_json::json!({ "threadId": 1 }));
        let stopped = debugged.event("stopped");
        assert_eq!(
            stopped["reason"], "breakpoint",
            "the noDebug run tore down the live debug session: {stopped}"
        );
        assert_eq!(
            stopped["hitBreakpointIds"],
            serde_json::json!([set["breakpoints"][0]["id"]])
        );

        debugged.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_breakpoint_set_while_parked_fires_on_this_run() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-live-bp", &[("main.leek", STEPS)]);
        let entry = dir.join("main.leek").display().to_string();
        let mut client = Client::spawn();
        handshake(
            &mut client,
            serde_json::json!({ "program": entry, "stopOnEntry": true }),
        );
        client.ok("configurationDone", Value::Null);
        client.event("stopped");

        // The user clicks the gutter while parked. The program is compiled by
        // now, so the answer is the real verdict rather than "pending launch".
        let set = client.ok(
            "setBreakpoints",
            serde_json::json!({
                "source": { "path": entry },
                "breakpoints": [{ "line": 3 }],
            }),
        );
        let placed = &set["breakpoints"][0];
        assert_eq!(placed["verified"], true, "{placed}");
        assert_eq!(placed["line"], 3);

        client.ok("continue", serde_json::json!({ "threadId": 1 }));
        let stopped = client.event("stopped");
        assert_eq!(
            stopped["reason"], "breakpoint",
            "the new breakpoint waited for the next run: {stopped}"
        );
        assert_eq!(
            stopped["hitBreakpointIds"],
            serde_json::json!([placed["id"]])
        );

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    // --- conditions, logpoints, evaluate, breakpointLocations -------------

    /// Launch [`STEPS`] under the debugger with one breakpoint on line 3
    /// (`return doubled`, inside `twice`), carrying whatever the client hung
    /// off it. Returns the client and the `setBreakpoints` answer.
    ///
    /// Line 3 on purpose: it is a bare `return <var>`, which arrives exactly
    /// once per call, so a test can count stops without tripping over #408.
    fn launch_with_breakpoint(dir: &Path, breakpoint: &Value) -> (Client, Value) {
        let entry = dir.join("main.leek").display().to_string();
        let mut client = Client::spawn();
        handshake(&mut client, serde_json::json!({ "program": entry }));
        let set = client.ok(
            "setBreakpoints",
            serde_json::json!({
                "source": { "path": entry },
                "breakpoints": [breakpoint.clone()],
            }),
        );
        client.ok("configurationDone", Value::Null);
        (client, set)
    }

    /// Everything the adapter says from here until the program ends, as
    /// `(event name, body)` pairs.
    fn drain_to_terminated(client: &mut Client) -> Vec<(String, Value)> {
        let mut seen = Vec::new();
        loop {
            let (event, body) = client.event_any(&["stopped", "output", "terminated"]);
            if event == "terminated" {
                return seen;
            }
            seen.push((event, body));
        }
    }

    #[test]
    fn a_condition_stops_only_on_the_call_that_satisfies_it() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-condition", &[("main.leek", STEPS)]);
        // `twice` is called with 0, 1 and 2; only one of those is 1.
        let (mut client, _set) = launch_with_breakpoint(
            &dir,
            &serde_json::json!({ "line": 3, "condition": "x == 1" }),
        );

        let stopped = client.event("stopped");
        assert_eq!(stopped["reason"], "breakpoint");
        let locals = client.ok("variables", serde_json::json!({ "variablesReference": 1 }));
        assert_eq!(
            locals["variables"][0],
            serde_json::json!({ "name": "x", "value": "1", "variablesReference": 0 }),
            "stopped on a call the condition excludes: {locals}"
        );

        client.ok("continue", serde_json::json!({ "threadId": 1 }));
        let rest = drain_to_terminated(&mut client);
        assert!(
            !rest.iter().any(|(event, _)| event == "stopped"),
            "the condition was true more than once: {rest:?}"
        );

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_condition_that_cannot_be_evaluated_stops_and_says_so() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-cond-unknown", &[("main.leek", STEPS)]);
        // `sum` is a local of the *caller*, not of `twice`. Failing open —
        // stop, and print why — is the decision under test: a condition that
        // quietly switched the breakpoint off would be invisible.
        let (mut client, _set) = launch_with_breakpoint(
            &dir,
            &serde_json::json!({ "line": 3, "condition": "sum > 1" }),
        );

        let complaint = loop {
            let (event, body) = client.event_any(&["stopped", "output"]);
            if event == "output" {
                break body;
            }
        };
        assert!(
            complaint["output"]
                .as_str()
                .is_some_and(|text| text.contains("sum")),
            "the broken condition was not reported: {complaint}"
        );
        assert_eq!(client.event("stopped")["reason"], "breakpoint");

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_condition_that_does_not_compile_leaves_the_breakpoint_unverified() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-cond-broken", &[("main.leek", STEPS)]);
        let entry = dir.join("main.leek").display().to_string();
        let mut client = Client::spawn();
        handshake(
            &mut client,
            serde_json::json!({ "program": entry, "stopOnEntry": true }),
        );
        client.ok("configurationDone", Value::Null);
        client.event("stopped");

        // Parked, so the program is compiled and the answer is the real
        // verdict rather than "pending launch".
        let set = client.ok(
            "setBreakpoints",
            serde_json::json!({
                "source": { "path": entry },
                "breakpoints": [
                    { "line": 3, "condition": "x ==" },
                    { "line": 3, "hitCondition": "soon" },
                ],
            }),
        );
        for placed in set["breakpoints"].as_array().expect("breakpoints") {
            assert_eq!(
                placed["verified"], false,
                "an expression that does not compile was armed: {placed}"
            );
            assert!(placed["message"].is_string(), "no reason given: {placed}");
        }

        // And it does not fire: the client was told it is dead.
        client.ok("continue", serde_json::json!({ "threadId": 1 }));
        let rest = drain_to_terminated(&mut client);
        assert!(
            !rest.iter().any(|(event, _)| event == "stopped"),
            "a breakpoint reported unverified stopped the debuggee: {rest:?}"
        );

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_logpoint_prints_once_per_hit_and_never_stops() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-logpoint", &[("main.leek", STEPS)]);
        let (mut client, set) = launch_with_breakpoint(
            &dir,
            &serde_json::json!({ "line": 3, "logMessage": "doubled={doubled}" }),
        );
        // Set before `configurationDone`, so the response could only say
        // "pending launch"; the real verdict arrives as a change event.
        assert_eq!(set["breakpoints"][0]["verified"], false);
        let announced = client.event("breakpoint")["breakpoint"].clone();
        assert_eq!(
            announced["verified"], true,
            "the logpoint was never armed: {announced}"
        );

        let seen = drain_to_terminated(&mut client);
        assert!(
            !seen.iter().any(|(event, _)| event == "stopped"),
            "a logpoint stopped the debuggee: {seen:?}"
        );
        let logged: Vec<&str> = seen
            .iter()
            .filter_map(|(_, body)| body["output"].as_str())
            .filter(|text| text.starts_with("doubled="))
            .collect();
        assert_eq!(
            logged,
            ["doubled=0", "doubled=2", "doubled=4"],
            "the loop's three calls did not each log their own value"
        );

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn evaluate_reads_the_locals_of_the_frame_it_names() {
        let _guard = debug_session_guard();
        let dir = project_with("proto-evaluate", &[("main.leek", STEPS)]);
        let (mut client, _set) = launch_with_breakpoint(
            &dir,
            &serde_json::json!({ "line": 3, "condition": "x == 2" }),
        );
        client.event("stopped");

        // Frame 0 is `twice`, frame 1 its caller: the same numbering
        // `stackTrace` hands out and `scopes` encodes.
        let in_callee = client.ok(
            "evaluate",
            serde_json::json!({ "expression": "x * 2 + 1", "frameId": 0 }),
        );
        assert_eq!(in_callee["result"], "5");
        assert_eq!(
            client.ok(
                "evaluate",
                serde_json::json!({ "expression": "i", "frameId": 1 })
            )["result"],
            "2",
            "frame 1 did not answer with the caller's own local"
        );

        // A name that is not in *that* frame is an error naming it, not the
        // other frame's value and not a panic.
        let wrong_frame = client.request(
            "evaluate",
            serde_json::json!({ "expression": "doubled", "frameId": 1 }),
        );
        assert_eq!(wrong_frame["success"], false, "{wrong_frame}");
        assert!(
            wrong_frame["message"]
                .as_str()
                .is_some_and(|message| message.contains("doubled")),
            "{wrong_frame}"
        );
        // As is a frame that is not on the stack at all.
        assert_eq!(
            client.request(
                "evaluate",
                serde_json::json!({ "expression": "1", "frameId": 99 })
            )["success"],
            false
        );

        client.ok("continue", serde_json::json!({ "threadId": 1 }));
        drain_to_terminated(&mut client);

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn evaluate_with_nothing_running_is_an_error_not_a_panic() {
        let mut client = Client::spawn();
        client.ok("initialize", serde_json::json!({ "adapterID": "leek" }));
        let response = client.request(
            "evaluate",
            serde_json::json!({ "expression": "sum", "context": "watch" }),
        );
        assert_eq!(response["success"], false, "{response}");
        assert!(response["message"].is_string(), "{response}");
        // The loop carries on afterwards.
        assert_eq!(client.ok("threads", Value::Null)["threads"][0]["id"], 1);
    }

    #[test]
    fn breakpoint_locations_lists_only_lines_that_can_be_stopped_on() {
        let _guard = debug_session_guard();
        // Line 1 is a comment, line 2 a statement, line 3 a bare `return` —
        // which lowers to no statement at all and still carries a safepoint.
        let dir = project_with(
            "proto-locations",
            &[("main.leek", "// a comment\nvar a = 1\nreturn a\n")],
        );
        let entry = dir.join("main.leek").display().to_string();
        let mut client = Client::spawn();
        handshake(
            &mut client,
            serde_json::json!({ "program": entry, "noDebug": true }),
        );
        client.ok("configurationDone", Value::Null);
        client.event("terminated");

        let body = client.ok(
            "breakpointLocations",
            serde_json::json!({ "source": { "path": entry }, "line": 1, "endLine": 9 }),
        );
        let lines: Vec<i64> = body["breakpoints"]
            .as_array()
            .expect("locations")
            .iter()
            .filter_map(|location| location["line"].as_i64())
            .collect();
        assert_eq!(lines, [2, 3], "the comment line was offered: {body}");

        // One line asks about that line only, and a file the program was
        // never compiled from is empty rather than an error.
        assert_eq!(
            client.ok(
                "breakpointLocations",
                serde_json::json!({ "source": { "path": entry }, "line": 1 })
            )["breakpoints"],
            serde_json::json!([])
        );
        let elsewhere = client.ok(
            "breakpointLocations",
            serde_json::json!({
                "source": { "path": dir.join("other.leek").display().to_string() },
                "line": 1,
                "endLine": 9,
            }),
        );
        assert_eq!(elsewhere["breakpoints"], serde_json::json!([]));

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_continue_sent_the_instant_a_stop_is_announced_still_releases_the_debuggee() {
        let _guard = debug_session_guard();
        // A hundred stops in a tight loop, each answered with no delay at all:
        // the `continue` routinely reaches the controller while the debuggee
        // is still between claiming its stop and parking on the condvar. If
        // that resume were lost the run would hang here instead of ending.
        let dir = project_with("proto-race", &[("main.leek", &steps_looping(100))]);
        let entry = dir.join("main.leek").display().to_string();
        let mut client = Client::spawn();
        handshake(&mut client, serde_json::json!({ "program": entry }));
        client.ok(
            "setBreakpoints",
            serde_json::json!({
                "source": { "path": entry },
                "breakpoints": [{ "line": 3 }],
            }),
        );
        client.ok("configurationDone", Value::Null);

        let mut stops = 0;
        while client.event_any(&["stopped", "terminated"]).0 == "stopped" {
            stops += 1;
            client.ok("continue", serde_json::json!({ "threadId": 1 }));
        }
        assert_eq!(stops, 100, "the callee did not stop once per call");

        client.finish();
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
