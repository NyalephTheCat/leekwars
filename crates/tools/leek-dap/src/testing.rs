//! Test support shared by the handler and server tests.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A `Write` sink the test can read back. The request loop needs the server's
/// writer to be `Send + 'static`, so it can't borrow a local.
#[derive(Clone)]
pub(crate) struct SharedOut(Arc<Mutex<Vec<u8>>>);

impl SharedOut {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    /// Everything written so far, as text.
    pub(crate) fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("output lock poisoned").clone())
            .expect("utf-8 output")
    }
}

impl Write for SharedOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("output lock poisoned").extend(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A fixture directory in the shape the adapter will report it back in.
///
/// The adapter keys and reports every source path through
/// [`leek_span::paths::canonical_or_normalized`], so a test that compares a
/// reported path with a raw `temp_dir()` one compares two spellings of the
/// same directory. On macOS that actually differs — the temp dir is
/// `/var/folders/…` while `/var` links to `/private/var` — so the mismatch
/// fails there and passes on Linux. Going through the one shared helper is
/// what keeps the two sides in the same shape on every platform (#181).
fn reported_shape(dir: &Path) -> PathBuf {
    leek_span::paths::canonical_or_normalized(dir)
}

/// A throw-away project directory holding `main.leek` (which includes
/// `lib.leek`) plus a `Miku.toml` naming the entry. `name` keeps concurrent
/// tests out of each other's directory.
pub(crate) fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("leek-dap-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp project");
    let dir = reported_shape(&dir);
    std::fs::write(
        dir.join("Miku.toml"),
        "[project]\nname = \"dbg\"\nversion = \"0.1.0\"\nentry = \"main.leek\"\n\n[paths]\nsrc = \".\"\n",
    )
    .expect("manifest");
    std::fs::write(dir.join("lib.leek"), "function answer() { return 42 }\n").expect("lib");
    std::fs::write(
        dir.join("main.leek"),
        "include(\"lib\")\nvar a = answer()\nreturn a\n",
    )
    .expect("entry");
    dir
}

/// One `Content-Length`-framed DAP message, as a client writes it.
pub(crate) fn frame(message: &serde_json::Value) -> String {
    let body = message.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

/// A `setBreakpoints` request for `path`, seq `seq`.
pub(crate) fn set_breakpoints(seq: i64, path: &std::path::Path, lines: &[i64]) -> String {
    frame(&serde_json::json!({
        "seq": seq,
        "type": "request",
        "command": "setBreakpoints",
        "arguments": {
            "source": { "path": path.display().to_string() },
            "breakpoints": lines.iter().map(|&l| serde_json::json!({ "line": l })).collect::<Vec<_>>(),
        },
    }))
}

/// Every framed message the adapter wrote, parsed back out of `out`.
pub(crate) fn messages(out: &SharedOut) -> Vec<serde_json::Value> {
    let text = out.text();
    let mut rest = text.as_str();
    let mut messages = Vec::new();
    // Stop at the first incomplete frame: the debuggee's worker thread may
    // still be writing while the test reads.
    while let Some(start) = rest.find("Content-Length: ") {
        let header = &rest[start + "Content-Length: ".len()..];
        let Some((len, body)) = header.split_once("\r\n\r\n") else {
            break;
        };
        let len: usize = len.trim().parse().expect("content length");
        if body.len() < len {
            break;
        }
        messages.push(serde_json::from_str(&body[..len]).expect("json body"));
        rest = &body[len..];
    }
    messages
}

/// A throw-away project directory holding exactly `files`, plus a `Miku.toml`
/// naming `main.leek` as the entry. Same contract as [`project`] — unique per
/// `name`, canonical path — for tests that need a program of their own shape.
pub(crate) fn project_with(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("leek-dap-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp project");
    let dir = reported_shape(&dir);
    std::fs::write(
        dir.join("Miku.toml"),
        "[project]\nname = \"dbg\"\nversion = \"0.1.0\"\nentry = \"main.leek\"\n\n[paths]\nsrc = \".\"\n",
    )
    .expect("manifest");
    for (file, text) in files {
        std::fs::write(dir.join(file), text).expect("fixture file");
    }
    dir
}

/// Serialises the tests that install a debug hook.
///
/// `leek_backend_native::set_debug_hook` writes a *process-global* slot, so
/// two debug launches running at once in one test binary overwrite each
/// other's controller and the stops land on the wrong session. `cargo test`
/// runs a binary's tests in parallel, so this lock is the only thing keeping
/// them apart — `--test-threads=1` is not something CI passes.
static DEBUG_SESSION: Mutex<()> = Mutex::new(());

/// Take the debug-hook lock for the duration of a test. Poisoning is ignored:
/// one failing test must not cascade into every later one.
pub(crate) fn debug_session_guard() -> std::sync::MutexGuard<'static, ()> {
    DEBUG_SESSION.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long a client waits for a message it expects. Generous: the assertion
/// is meant to fire on a genuine hang, not on a slow machine.
const REPLY: Duration = Duration::from_secs(30);
/// How long a client waits for the request loop to end once the pipe is shut.
const EXIT: Duration = Duration::from_secs(30);

/// An interactive protocol client: drives the real request loop over a pair
/// of pipes, so a test can *react* to what the adapter says — answer a
/// `stopped` event with `continue`, ask for the stack while the debuggee is
/// parked. The scripted-`Cursor` driver cannot: it hands the server a canned
/// byte stream and only reads the output back once `serve` has returned.
pub(crate) struct Client {
    /// Requests go here. Dropping it is the EOF that ends the request loop.
    to_server: Option<std::io::PipeWriter>,
    /// Every message the adapter wrote, in order, as the reader thread parses
    /// them off the wire.
    inbox: std::sync::mpsc::Receiver<serde_json::Value>,
    /// Messages that arrived while waiting for a different one. A `stopped`
    /// event routinely overtakes the response it follows.
    held: Vec<serde_json::Value>,
    /// The thread running `serve`.
    loop_thread: Option<std::thread::JoinHandle<()>>,
    seq: i64,
}

impl Client {
    /// Start an adapter and connect to it.
    pub(crate) fn spawn() -> Self {
        let (server_in, to_server) = std::io::pipe().expect("request pipe");
        let (from_server, server_out) = std::io::pipe().expect("event pipe");

        let loop_thread = std::thread::spawn(move || {
            let mut server = dap::server::Server::new(
                std::io::BufReader::new(server_in),
                std::io::BufWriter::new(server_out),
            );
            crate::server::serve(&mut server).expect("request loop");
        });

        let (tx, inbox) = std::sync::mpsc::channel();
        // Drains the adapter's output continuously: a pipe whose reader never
        // reads blocks the writer once the kernel buffer fills, which for the
        // adapter means blocking inside `send_event`.
        std::thread::spawn(move || read_frames(&mut std::io::BufReader::new(from_server), &tx));

        Self {
            to_server: Some(to_server),
            inbox,
            held: Vec::new(),
            loop_thread: Some(loop_thread),
            seq: 0,
        }
    }

    /// Send a request and return the adapter's response to it.
    pub(crate) fn request(
        &mut self,
        command: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        self.seq += 1;
        let seq = self.seq;
        let mut message = serde_json::json!({
            "seq": seq, "type": "request", "command": command,
        });
        if !arguments.is_null() {
            message["arguments"] = arguments;
        }
        let framed = frame(&message);
        let pipe = self.to_server.as_mut().expect("client still connected");
        pipe.write_all(framed.as_bytes()).expect("send request");
        pipe.flush().expect("flush request");

        // Matched on `request_seq`, not on the command: an error response
        // carries no command at all (the `dap` crate tags it on the body, and
        // a failed request has no body).
        self.recv(&format!("a response to `{command}`"), |message| {
            message["type"] == "response" && message["request_seq"] == seq
        })
    }

    /// Send a request, assert it succeeded, and return its `body`.
    pub(crate) fn ok(&mut self, command: &str, arguments: serde_json::Value) -> serde_json::Value {
        let response = self.request(command, arguments);
        assert_eq!(response["success"], true, "`{command}` failed: {response}");
        response["body"].clone()
    }

    /// Wait for the next event named `event` and return its `body`.
    pub(crate) fn event(&mut self, event: &str) -> serde_json::Value {
        self.recv(&format!("a `{event}` event"), |message| {
            message["event"] == event
        })["body"]
            .clone()
    }

    /// Wait for whichever of `events` arrives first; returns `(name, body)`.
    /// Stepping is a race between the next stop and the end of the run, and a
    /// test that guesses wrong would hang instead of failing.
    pub(crate) fn event_any(&mut self, events: &[&str]) -> (String, serde_json::Value) {
        let message = self.recv(&format!("one of {events:?}"), |message| {
            message["event"]
                .as_str()
                .is_some_and(|name| events.contains(&name))
        });
        (
            message["event"].as_str().unwrap_or_default().to_string(),
            message["body"].clone(),
        )
    }

    /// The first message satisfying `matches`, from the held ones or the wire.
    fn recv(
        &mut self,
        what: &str,
        matches: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        if let Some(at) = self.held.iter().position(&matches) {
            return self.held.remove(at);
        }
        let deadline = Instant::now() + REPLY;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.inbox.recv_timeout(left) {
                Ok(message) if matches(&message) => return message,
                // Not the one we want, but a later wait may want it.
                Ok(message) => self.held.push(message),
                Err(e) => panic!("waiting for {what}: {e}; already had {:?}", self.held),
            }
        }
    }

    /// Shut the connection and wait for the request loop to end. [`Drop`]
    /// calls it too, so every test proves its session can be torn down.
    pub(crate) fn finish(&mut self) {
        // EOF on the request pipe: `poll_request` returns `None`, `serve`
        // releases a parked debuggee and joins its worker, then returns.
        drop(self.to_server.take());
        let Some(loop_thread) = self.loop_thread.take() else {
            return;
        };
        // There is no timed `join`, so poll: a deadlocked adapter must fail
        // the test rather than hang the whole CI job.
        let deadline = Instant::now() + EXIT;
        while !loop_thread.is_finished() {
            assert!(
                Instant::now() < deadline,
                "the request loop did not end after the client disconnected"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        loop_thread.join().expect("request loop thread");
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // The failing assertion already says what went wrong; close the
            // pipe so the adapter's threads unwind, but never panic on top of
            // a panic.
            drop(self.to_server.take());
            return;
        }
        self.finish();
    }
}

/// Parse `Content-Length`-framed messages off `input` until it closes.
fn read_frames(
    input: &mut impl std::io::BufRead,
    out: &std::sync::mpsc::Sender<serde_json::Value>,
) {
    let mut line = String::new();
    loop {
        let mut length: Option<usize> = None;
        // Header block, ending at the blank line. A blank line *before* any
        // header is the `\r\n` the sender writes after each body.
        loop {
            line.clear();
            match input.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let header = line.trim();
            if header.is_empty() {
                if length.is_some() {
                    break;
                }
                continue;
            }
            if let Some(value) = header.strip_prefix("Content-Length:") {
                length = value.trim().parse().ok();
            }
        }
        let Some(length) = length else { return };
        let mut body = vec![0u8; length];
        if input.read_exact(&mut body).is_err() {
            return;
        }
        let message = serde_json::from_slice(&body).expect("json body");
        if out.send(message).is_err() {
            return;
        }
    }
}
