//! Test support shared by the handler and server tests.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

/// A throw-away project directory holding `main.leek` (which includes
/// `lib.leek`) plus a `Miku.toml` naming the entry. `name` keeps concurrent
/// tests out of each other's directory.
pub(crate) fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("leek-dap-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp project");
    // The server canonicalises every source path it reports, so hand back the
    // canonical directory: on macOS the temp dir is `/var/folders/…` while
    // `/var` is a symlink to `/private/var`, and a test comparing the reported
    // path with this one string-for-string would fail there but pass on Linux.
    let dir = dir.canonicalize().unwrap_or(dir);
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
