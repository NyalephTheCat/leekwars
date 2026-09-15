//! One compiler session: the recipe catalogue and the driver that runs it.
//!
//! [`recipes`] plans a [`Pipeline`] for a [`Target`]; [`driver`] runs one over
//! a project's sources and renders the diagnostics it produced. They were two
//! crates and are one because nothing ever wanted the driver without the
//! recipes it plans with.
//!
//! [`session`] is the front door over both: a [`Session`] is one invocation
//! over one project, and [`Compilation`] is one file it compiled — the run,
//! its text, its diagnostics and the reporter they render through, in one
//! value instead of five a caller has to keep in step.
//!
//! Every module's public names are re-exported at the crate root, so
//! `leek_session::` is the only path a front-end needs.

pub mod driver;
pub mod error;
pub mod recipes;
pub mod session;

pub use driver::*;
pub use error::SessionError;
pub use recipes::*;
pub use session::{Compilation, Session};
