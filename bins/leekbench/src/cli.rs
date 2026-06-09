//! CLI for `leekbench`.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CorpusExpectation {
    /// Only `equals(...)` cases.
    Equals,
    /// Any case that should parse/run cleanly.
    Clean,
    /// All extracted cases.
    All,
}

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Compare backend execution speed on a Leekscript program"
)]
pub struct Cli {
    /// `.leek` source file to benchmark. Ignored when `--corpus` is set.
    pub input: Option<PathBuf>,

    /// Iterate over the embedded corpus instead of a single file.
    #[arg(long)]
    pub corpus: bool,

    /// Corpus mode: fast batch rust-java **correctness** sweep — emit every
    /// case, compile in one `javac`, run in one JVM (minutes vs. hours). Checks
    /// values only (no timing, native/upstream skipped).
    #[arg(long = "fast-java")]
    pub fast_java: bool,

    /// In corpus mode, max number of cases to run.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Optional path to a corpus manifest JSON file.
    #[arg(long)]
    pub manifest: Option<PathBuf>,

    /// Include cases marked disabled upstream.
    #[arg(long)]
    pub include_disabled: bool,

    /// Expectation filter used in corpus mode.
    #[arg(long, value_enum, default_value_t = CorpusExpectation::Equals)]
    pub corpus_expectation: CorpusExpectation,

    /// Restrict corpus cases whose id/source/method contains this text.
    #[arg(long)]
    pub case_filter: Option<String>,

    #[arg(long = "corpus-lang-version")]
    pub corpus_lang_version: Option<u8>,

    #[arg(long)]
    pub work_root: Option<PathBuf>,

    #[arg(long, default_value_t = 5)]
    pub runs: usize,

    #[arg(long = "lang-version", default_value_t = 4)]
    pub lang_version: u8,

    #[arg(long)]
    pub no_upstream: bool,

    #[arg(long)]
    pub no_rust_java: bool,

    #[arg(long)]
    pub no_native: bool,

    #[arg(long)]
    pub verbose: bool,

    #[arg(long, short = 'd')]
    pub detail: bool,
}
