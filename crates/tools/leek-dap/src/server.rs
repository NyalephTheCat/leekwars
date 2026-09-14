//! Transport setup and the request loop.
//!
//! The whole adapter is synchronous and single-threaded: poll one
//! request, dispatch it (which may emit responses and events), repeat
//! until the client disconnects or sends EOF.

use std::io::{self, BufReader, BufWriter, Read, Write};

use dap::prelude::*;

use crate::handlers::{self, Flow};
use crate::session::Session;

/// Run the DAP server over stdio until the client disconnects.
///
/// # Errors
/// Returns any I/O or protocol error from the underlying transport.
pub fn run_stdio() -> anyhow::Result<()> {
    let input = BufReader::new(io::stdin());
    let output = BufWriter::new(io::stdout());
    let mut server = Server::new(input, output);
    serve(&mut server)
}

/// Drive a [`Server`] to completion. Generic over the transport so
/// tests can feed a scripted request stream.
pub(crate) fn serve<R: Read, W: Write + Send + 'static>(
    server: &mut Server<R, W>,
) -> anyhow::Result<()> {
    let mut session = Session::new();
    // `poll_request` yields `None` at EOF (client closed the pipe).
    while let Some(req) = server.poll_request()? {
        if let Flow::Shutdown = handlers::dispatch(&mut session, server, req)? {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, BufWriter, Cursor};
    use std::path::Path;

    use serde_json::Value;

    use super::*;
    use crate::testing::{SharedOut, messages, project, set_breakpoints};

    /// Feed a scripted request stream through the real request loop and hand
    /// back every message the adapter wrote. This is the whole adapter: the
    /// framing, the dispatch, the handlers and the session, driven exactly as
    /// an editor drives them.
    fn converse(requests: &str) -> Vec<Value> {
        let sink = SharedOut::new();
        let mut server = Server::new(
            BufReader::new(Cursor::new(requests.as_bytes().to_vec())),
            BufWriter::new(sink.clone()),
        );
        serve(&mut server).expect("request loop");
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
}
