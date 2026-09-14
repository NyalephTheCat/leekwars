//! Is `data/reference.tsv` still the dataset this checkout describes?
//!
//! Before #148 the answer was guessed from file mtimes, and a guess that
//! said "stale" started a multi-minute JVM run from inside `cargo
//! build`. The dataset now carries the provenance of the run that made
//! it — the upstream submodule commit plus git's tree hash of the
//! emitter overlay — and this test reads it back instead.
//!
//! It is deliberately quiet when it cannot decide: a checkout without
//! the `official-generator` submodule has nothing to compare against,
//! and the committed dataset predates the provenance line (regenerating
//! it needs JDK 25 and a warm Gradle cache, which no CI job has). It
//! starts gating for real the first time someone runs
//! `cargo run -p leek-test-corpus -- extract-reference`.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use leek_test_corpus::reference;

/// Only the first line: the dataset is 13 MB.
fn first_line(path: &Path) -> Option<String> {
    let mut line = String::new();
    BufReader::new(File::open(path).ok()?)
        .read_line(&mut line)
        .ok()?;
    Some(line)
}

#[test]
fn the_committed_reference_matches_this_checkout() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dataset = reference::committed_path(manifest_dir);

    let Some(head) = first_line(&dataset) else {
        panic!("{} is missing", dataset.display());
    };
    let Some(recorded) = reference::recorded_provenance(&head) else {
        eprintln!(
            "skipping: {} carries no provenance line — it predates #148. \
             `cargo run -p leek-test-corpus -- extract-reference` writes one.",
            dataset.display()
        );
        return;
    };
    let Some(current) = reference::current_provenance(manifest_dir) else {
        eprintln!("skipping: no upstream submodule checkout (or no git) to compare against");
        return;
    };

    assert_eq!(
        recorded, current,
        "\ndata/reference.tsv was generated from other sources than this checkout:\
         \n  recorded: {recorded}\
         \n  current:  {current}\
         \nRefresh it with `cargo run -p leek-test-corpus -- extract-reference` (needs JDK 25 \
         and the official-generator submodule), or check out the upstream commit it names.\n"
    );
}
