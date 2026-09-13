//! Stderr discipline for the hot-path traces.
//!
//! `didOpen`, `didClose` and `publishDiagnostics` used to log
//! unconditionally. `publishDiagnostics` runs on every keystroke, so the
//! editor's output channel filled with one line per edit. These drive the
//! real binary over stdio and assert the lines only appear when
//! `LEEK_LSP_LOG` asks for them.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Generous upper bound: a cold first run type-checks the builtin library.
const TIMEOUT: Duration = Duration::from_secs(120);

fn write_frame(w: &mut impl Write, body: &str) -> std::io::Result<()> {
    write!(w, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    w.flush()
}

/// Read one LSP frame; `None` once the peer closes the stream.
fn read_frame(r: &mut impl BufRead) -> Option<String> {
    let mut len: usize = 0;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok()?;
        }
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Answer the server-to-client requests `initialized` awaits on, so the
/// handshake completes instead of hanging.
fn answer_request(stdin: &Mutex<ChildStdin>, body: &str) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return;
    };
    let (Some(id), Some(method)) = (v.get("id"), v.get("method").and_then(Value::as_str)) else {
        return;
    };
    let result = if method == "workspace/configuration" {
        json!([{}])
    } else {
        Value::Null
    };
    let body = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
    let mut w = stdin.lock().expect("stdin lock");
    let _ = write_frame(&mut *w, &body);
}

struct Session {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    frames: Receiver<String>,
    stderr: JoinHandle<String>,
}

impl Session {
    /// Launch the server binary with `LEEK_LSP_LOG` either unset or set to
    /// `value`, and start draining both of its output streams.
    fn start(trace: Option<&str>) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_leek-lsp"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match trace {
            Some(v) => cmd.env("LEEK_LSP_LOG", v),
            None => cmd.env_remove("LEEK_LSP_LOG"),
        };
        let mut child = cmd.spawn().expect("spawn leek-lsp");

        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("stdin")));
        let stdout = child.stdout.take().expect("stdout");
        let mut err = child.stderr.take().expect("stderr");

        let stderr = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            s
        });

        let (tx, frames) = mpsc::channel();
        let replies = Arc::clone(&stdin);
        std::thread::spawn(move || {
            let mut out = BufReader::new(stdout);
            while let Some(body) = read_frame(&mut out) {
                answer_request(&replies, &body);
                if tx.send(body).is_err() {
                    break;
                }
            }
        });

        Self {
            child,
            stdin,
            frames,
            stderr,
        }
    }

    fn send(&self, body: &str) {
        let mut w = self.stdin.lock().expect("stdin lock");
        write_frame(&mut *w, body).expect("send frame");
    }

    /// Pull frames until one satisfies `pred`, panicking on timeout.
    fn wait_for(&self, what: &str, pred: impl Fn(&Value) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_default();
            assert!(!left.is_zero(), "timed out waiting for {what}");
            let Ok(body) = self.frames.recv_timeout(left) else {
                panic!("server closed the connection while waiting for {what}");
            };
            if serde_json::from_str::<Value>(&body).is_ok_and(|v| pred(&v)) {
                return;
            }
        }
    }

    /// Wait for the process to exit and return everything it wrote to stderr.
    fn finish(mut self) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(_) => break,
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    panic!("leek-lsp did not exit after shutdown");
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        self.stderr.join().expect("stderr reader")
    }
}

/// True for a *response* to our request `id` — a server-to-client request
/// can carry the same id, so the `method` key has to be absent.
fn is_response_to(v: &Value, id: i64) -> bool {
    v.get("method").is_none() && v.get("id").and_then(Value::as_i64) == Some(id)
}

fn is_publish(v: &Value) -> bool {
    v.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
}

/// Open and close one document, then shut down. Returns the server's stderr.
fn open_close_shutdown(trace: Option<&str>, fixture: &str) -> String {
    let dir = std::env::temp_dir().join("leek-lsp-trace-tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(fixture);
    std::fs::write(&path, "var x = 5;\n").expect("fixture");
    let uri = format!("file://{}", path.display());

    let session = Session::start(trace);
    session.send(
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "processId": null, "rootUri": null, "capabilities": {} }
        })
        .to_string(),
    );
    session.wait_for("the initialize response", |v| is_response_to(v, 1));
    session.send(&json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }).to_string());

    session.send(
        &json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "leek", "version": 1, "text": "var x = 5;\n"
            } }
        })
        .to_string(),
    );
    session.wait_for("diagnostics for the opened document", is_publish);

    session.send(
        &json!({
            "jsonrpc": "2.0", "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": uri } }
        })
        .to_string(),
    );
    session.wait_for("the cleared diagnostics on close", is_publish);

    // `shutdown` makes the server terminate the process, so don't wait for a
    // response or bother with `exit` — both race the teardown.
    session.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }).to_string());
    session.finish()
}

#[test]
fn hot_path_traces_stay_off_stderr_by_default() {
    let stderr = open_close_shutdown(None, "quiet.leek");
    // Lifecycle one-shots still log, which also proves we captured stderr.
    assert!(
        stderr.contains("initialized, ready"),
        "expected the lifecycle log; got:\n{stderr}"
    );
    for noisy in ["didOpen", "didClose", "publishDiagnostics"] {
        assert!(
            !stderr.contains(noisy),
            "`{noisy}` logged without LEEK_LSP_LOG; got:\n{stderr}"
        );
    }
}

#[test]
fn hot_path_traces_appear_with_leek_lsp_log_trace() {
    let stderr = open_close_shutdown(Some("trace"), "trace.leek");
    for noisy in ["didOpen", "didClose", "publishDiagnostics"] {
        assert!(
            stderr.contains(noisy),
            "`{noisy}` missing under LEEK_LSP_LOG=trace; got:\n{stderr}"
        );
    }
}
