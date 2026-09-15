//! One compiler session: what a front-end holds for a whole invocation.
//!
//! A [`Session`] is one invocation over one [`Project`](leek_project::Project)
//! — its query database, the file set its includes resolve against, and the
//! [`Reporter`](leek_diagnostics::Reporter) built from the manifest's
//! `[lint]` levels. A [`Compilation`] is one file it compiled: the text, the
//! diagnostics, the source map they render against and the reporter that
//! renders them, in one value instead of five a caller has to keep in step.
//!
//! [`params`] is what a compilation is asked for ([`Target`]) and under
//! which settings ([`CompileParams`]); [`driver`] is the configuration and
//! the reporting helpers around it; [`libraries`] loads and registers the
//! host-environment function libraries every driver shares.
//!
//! Every module's public names are re-exported at the crate root, so
//! `leek_session::` is the only path a front-end needs.

pub mod driver;
pub mod error;
pub mod libraries;
pub mod params;
pub mod session;

pub use driver::*;
pub use error::SessionError;
pub use libraries::*;
pub use params::*;
pub use session::{Compilation, IncludedFile, Session};
