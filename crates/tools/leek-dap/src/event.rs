//! Small constructors for the event bodies the adapter emits. The
//! `dap` event bodies have many optional fields and no `Default`, so
//! these keep the handlers readable.

use dap::events::{ExitedEventBody, OutputEventBody};
use dap::types::OutputEventCategory;

/// An `output` event in the given category (e.g. program output on
/// `Stdout`, diagnostics on `Stderr`).
pub(crate) fn output(category: OutputEventCategory, text: impl Into<String>) -> OutputEventBody {
    OutputEventBody {
        category: Some(category),
        output: text.into(),
        group: None,
        variables_reference: None,
        source: None,
        line: None,
        column: None,
        data: None,
    }
}

/// An `exited` event carrying the debuggee's exit code.
pub(crate) fn exited(exit_code: i64) -> ExitedEventBody {
    ExitedEventBody { exit_code }
}
