#![no_main]

use libfuzzer_sys::fuzz_target;
use leek_parser::{ParseFeatures, parse_with_features};
use leek_span::SourceId;
use leek_syntax::Version;

fuzz_target!(|data: &[u8]| {
    // Lossy UTF-8 mirrors how editors feed arbitrary bytes into the parser
    // during incremental edits; the parser must never panic.
    let text = String::from_utf8_lossy(data);
    let source = SourceId::new(1).expect("valid source id");
    let _ = parse_with_features(&text, source, Version::V4, ParseFeatures::default());
});
