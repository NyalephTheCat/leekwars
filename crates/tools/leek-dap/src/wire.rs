//! Transport-level access to the fields the `dap` crate's types drop.
//!
//! `dap` 0.4.1-alpha1 derives `Deserialize` on `SetBreakpointsArguments` with
//! `#[serde(rename_all = "camelCase")]` but on the nested `SourceBreakpoint`
//! *without* it, so that struct's `hit_condition` and `log_message` fields are
//! read from the wire under those exact spellings — while every DAP client on
//! earth sends `hitCondition` and `logMessage`. A logpoint therefore arrives
//! as a plain breakpoint and a hit condition arrives as nothing at all.
//! (`condition` is spelled the same either way, which is why conditions are
//! the one of the three that works through the typed path.)
//!
//! Upstream is at its newest published version and the dependency is pinned,
//! so the fix has to live here. Rewriting the stream into the spellings the
//! crate wants would change every message's `Content-Length`; instead this
//! passes every byte through **unchanged** and keeps a parsed copy of the
//! message it just forwarded, which the `setBreakpoints` handler consults for
//! the two fields the typed arguments cannot carry. Observing rather than
//! rewriting is deliberate: the worst a bug here can do is lose a logpoint,
//! not corrupt the protocol.

use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};

/// The message the transport most recently forwarded, parsed.
///
/// One slot, not a queue: the request loop is single-threaded and answers one
/// request before reading the next, so the message a handler asks about is
/// always the one that just arrived. The `seq` check makes that an assertion
/// rather than an assumption — a mismatch yields nothing instead of another
/// request's arguments.
#[derive(Default)]
pub(crate) struct RawMessages(Mutex<Option<serde_json::Value>>);

impl RawMessages {
    fn record(&self, message: serde_json::Value) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message);
    }

    /// The `arguments` object of the request numbered `seq`, if that is the
    /// message the transport last forwarded.
    pub(crate) fn arguments(&self, seq: i64) -> Option<serde_json::Value> {
        let message = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let message = message.as_ref()?;
        (message["seq"] == serde_json::json!(seq)).then(|| message["arguments"].clone())
    }
}

/// A reader that forwards a `Content-Length`-framed DAP stream verbatim while
/// recording each message it passes on. Pair it with [`RawMessages`].
pub(crate) struct Tee<R: Read> {
    inner: BufReader<R>,
    seen: Arc<RawMessages>,
    /// The frame being handed to the caller, and how much of it has gone.
    frame: Vec<u8>,
    at: usize,
}

/// Wrap a DAP input stream, handing back the shared record of what crosses it.
pub(crate) fn tee<R: Read>(inner: R) -> (Tee<R>, Arc<RawMessages>) {
    let seen = Arc::new(RawMessages::default());
    (
        Tee {
            inner: BufReader::new(inner),
            seen: Arc::clone(&seen),
            frame: Vec::new(),
            at: 0,
        },
        seen,
    )
}

impl<R: Read> Tee<R> {
    /// Read one whole message — headers and body — recording the body and
    /// returning the bytes exactly as they arrived. `None` at end of stream,
    /// including a stream that stops mid-frame: there is no message there to
    /// forward, and the request loop ends on EOF either way.
    fn next_frame(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        let mut frame = Vec::new();
        let mut length: Option<usize> = None;
        loop {
            let mut line = String::new();
            if self.inner.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            frame.extend_from_slice(line.as_bytes());
            let header = line.trim();
            if header.is_empty() {
                // The blank line ends the header block — but only once a
                // length has been seen; before that it is the `\r\n` some
                // senders write after the previous body.
                if length.is_some() {
                    break;
                }
                continue;
            }
            if let Some(value) = header.strip_prefix("Content-Length:") {
                length = value.trim().parse().ok();
            }
        }
        let Some(length) = length else {
            return Ok(None);
        };
        let mut body = vec![0u8; length];
        if self.inner.read_exact(&mut body).is_err() {
            return Ok(None);
        }
        if let Ok(message) = serde_json::from_slice::<serde_json::Value>(&body) {
            self.seen.record(message);
        }
        frame.extend_from_slice(&body);
        Ok(Some(frame))
    }
}

impl<R: Read> Read for Tee<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at >= self.frame.len() {
            let Some(frame) = self.next_frame()? else {
                return Ok(0);
            };
            self.frame = frame;
            self.at = 0;
        }
        let taken = (self.frame.len() - self.at).min(buf.len());
        buf[..taken].copy_from_slice(&self.frame[self.at..self.at + taken]);
        self.at += taken;
        Ok(taken)
    }
}

#[cfg(test)]
mod tests {
    use super::tee;
    use std::io::Read;

    fn framed(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    #[test]
    fn every_byte_is_forwarded_unchanged_and_every_message_is_recorded() {
        let first = r#"{"seq":1,"type":"request","command":"initialize"}"#;
        let second = r#"{"seq":2,"type":"request","command":"setBreakpoints","arguments":{"breakpoints":[{"line":3,"logMessage":"i={i}"}]}}"#;
        let stream = format!("{}{}", framed(first), framed(second));

        let (mut reader, seen) = tee(std::io::Cursor::new(stream.clone().into_bytes()));

        // Read a byte at a time: the framing must survive a consumer that
        // takes less than a whole message, which is what a `BufReader` over
        // this does when it refills.
        let mut forwarded = Vec::new();
        let mut byte = [0u8; 1];
        while reader.read(&mut byte).expect("read") == 1 {
            forwarded.push(byte[0]);
            // After the first message's last byte the record holds it, and
            // only it: one slot, replaced as each message goes by.
            if forwarded.len() == framed(first).len() {
                assert_eq!(seen.arguments(2), None, "a message that has not arrived");
                assert!(seen.arguments(1).is_some(), "message 1 was not recorded");
            }
        }
        assert_eq!(
            String::from_utf8(forwarded).expect("utf-8"),
            stream,
            "the transport changed the bytes it forwarded"
        );

        // And the field the `dap` crate's types drop is there to be read.
        let arguments = seen.arguments(2).expect("message 2 was recorded");
        assert_eq!(arguments["breakpoints"][0]["logMessage"], "i={i}");
        assert_eq!(seen.arguments(1), None, "an older message answered for 2");
    }

    #[test]
    fn a_stream_that_stops_mid_frame_is_end_of_stream_not_an_error() {
        let truncated = "Content-Length: 40\r\n\r\n{\"seq\":1}";
        let (mut reader, _seen) = tee(std::io::Cursor::new(truncated.as_bytes().to_vec()));
        let mut out = Vec::new();
        assert_eq!(reader.read_to_end(&mut out).expect("read"), 0);
    }
}
