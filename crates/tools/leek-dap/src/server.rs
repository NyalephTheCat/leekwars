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
