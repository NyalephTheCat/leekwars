//! Quick CLI: `cargo run -p leek-fmt --example fmt_file -- path.leek`.
//! Lets us eyeball the formatter on a real file without wiring
//! leekc first.

use std::path::PathBuf;

use leek_fmt::{FormatOptions, format_source};
use leek_span::SourceId;
use leek_syntax::Version;

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: fmt_file <path.leek>");
    let text = std::fs::read_to_string(&path).expect("read file");
    let out = format_source(
        &text,
        SourceId::new(1).unwrap(),
        Version::V4,
        &FormatOptions::default(),
    );
    print!("{out}");
}
