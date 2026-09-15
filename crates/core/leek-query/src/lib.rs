//! The query database every compiler pass is memoized in.
//!
//! This crate knows **nothing** about specific compiler passes. It owns
//! the salsa database and its inputs, and the stopwatch a driver times
//! its stages with:
//!
//! - [`mod@salsa`] — the [`salsa::Db`] trait, the [`salsa::LeekDb`]
//!   database, the [`salsa::SourceFile`] and [`salsa::WorkspaceFiles`]
//!   inputs, and the interned [`salsa::ProgramClasses`] key.
//! - [`OptLevel`] and [`LintGroups`] — the two settings that key a
//!   tracked query rather than living inside one.
//! - [`TimingSink`] — where a driver records how long a stage took.
//!
//! Each pass crate writes its tracked queries against `salsa::Db`, which
//! is why they depend on this crate and not on `leek-db`: `leek-db`
//! re-exports every one of them, so the edge only goes one way.
//!
//! It used to also hold a `Step` trait, a `TypeId`-keyed `Context` and a
//! `RecipePlan` that ordered them — a second orchestration model beside
//! the queries, with every pass shipping both halves. Epic
//! [R1](https://github.com/NyalephTheCat/leekwars/issues/345) collapsed
//! them onto the queries; what is left here is the database that model
//! was dispatching *into*.

// Printing is an API decision in a library, not a convenience: a crate that
// writes to the terminal behind its caller's back is unusable from a language
// server or a test harness. This crate prints nothing at all; should a print
// ever land here, it has to be the tool's *output* and say so.
#![warn(clippy::print_stdout, clippy::print_stderr)]

mod keys;
mod timed;

pub mod salsa;

pub use keys::{LintGroups, OptLevel};
pub use timed::{StepTiming, TimingSink};
