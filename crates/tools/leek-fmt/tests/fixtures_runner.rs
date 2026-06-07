//! Fixture-based snapshot + idempotence tests.
//!
//! Each fixture is a pair `<name>.in.leek` + `<name>.out.leek`.
//! For each pair we:
//!
//! 1. Format the `.in` file with default options.
//! 2. Compare byte-for-byte to the `.out` file (snapshot test).
//! 3. Format the resulting output again and check equality
//!    (idempotence).

use std::path::PathBuf;

use leek_fmt::{FormatOptions, format_source};
use leek_span::SourceId;
use leek_syntax::Version;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fmt(src: &str) -> String {
    format_source(
        src,
        SourceId::new(1).unwrap(),
        Version::V4,
        &FormatOptions::default(),
    )
}

fn discover_fixtures() -> Vec<(PathBuf, PathBuf)> {
    let dir = fixtures_dir();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read fixtures dir {}: {e}", dir.display()))
    {
        let entry = entry.unwrap();
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(stem) = name.strip_suffix(".in.leek") {
            let expected = dir.join(format!("{stem}.out.leek"));
            if expected.exists() {
                out.push((path, expected));
            }
        }
    }
    out.sort();
    assert!(
        !out.is_empty(),
        "no fixtures discovered under {}",
        dir.display()
    );
    out
}

#[test]
fn fixtures_match_expected() {
    let mut failures = Vec::new();
    for (input, expected) in discover_fixtures() {
        let src = std::fs::read_to_string(&input).unwrap();
        let exp = std::fs::read_to_string(&expected).unwrap();
        let got = fmt(&src);
        if got != exp {
            failures.push(format!(
                "{} differs from {}\n--- expected ---\n{exp}--- got ---\n{got}---",
                input.display(),
                expected.display(),
            ));
        }
    }
    assert!(failures.is_empty(), 
        "{} fixture(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn fixtures_idempotent() {
    let mut failures = Vec::new();
    for (input, _expected) in discover_fixtures() {
        let src = std::fs::read_to_string(&input).unwrap();
        let once = fmt(&src);
        let twice = fmt(&once);
        if once != twice {
            failures.push(format!(
                "{} not idempotent\n--- once ---\n{once}--- twice ---\n{twice}---",
                input.display(),
            ));
        }
    }
    assert!(failures.is_empty(), 
        "{} fixture(s) non-idempotent:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn idempotent_over_diverse_constructs() {
    // Complements the curated fixtures: format(format(x)) == format(x) over a
    // spread of syntax the small fixture set doesn't all exercise. A second
    // pass must be a no-op on already-formatted output.
    let snippets = [
        "var x=1+2*3 return x",
        "class A{ private x=1 public m(a,b){ return a+b } }",
        "var f=(a,b)=>a+b return f(1,2)",
        "var m=[1:2,3:4] return m[1]",
        "var s=[1,2,3] for(var i in s){ debug(i) }",
        "var r = true ? 'a' : 'b'",
        "// line comment\nvar y = 5 /* block */ return y",
        "var a=[]; for(var i=0;i<10;i++){ push(a,i) } return a",
        "function g(x=2,y=3){ return x*y } return g()",
        "global G = 42 return G",
        "var t = [[1,2],[3,4]] return t[0][1]",
        "if(1){return 1}else if(2){return 2}else{return 3}",
    ];
    let mut failures = Vec::new();
    for src in snippets {
        let once = fmt(src);
        let twice = fmt(&once);
        if once != twice {
            failures.push(format!(
                "non-idempotent for {src:?}\n--- once ---\n{once}\n--- twice ---\n{twice}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} snippet(s) non-idempotent:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
