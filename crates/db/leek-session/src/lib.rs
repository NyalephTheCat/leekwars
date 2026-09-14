//! One compiler session: the recipe catalogue and the driver that runs it.
//!
//! [`recipes`] plans a [`Pipeline`] for a [`Target`]; [`driver`] runs one over
//! a project's sources and renders the diagnostics it produced. They were two
//! crates and are one because nothing ever wanted the driver without the
//! recipes it plans with.
//!
//! Both modules' public names are re-exported at the crate root, so
//! `leek_session::` is the only path a front-end needs.

pub mod driver;
pub mod recipes;

pub use driver::*;
pub use recipes::*;
