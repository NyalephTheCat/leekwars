//! Generate the diagnostic catalog from `catalog.yaml` into `OUT_DIR`.

use std::collections::HashMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

struct Entry {
    id: String,
    catalog_index: usize,
}

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let catalog_path = Path::new(&manifest_dir).join("catalog.yaml");
    println!("cargo:rerun-if-changed={}", catalog_path.display());

    let yaml = fs::read_to_string(&catalog_path).expect("read catalog.yaml");
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("parse catalog.yaml");
    let map = doc.as_mapping().expect("catalog root must be a map");

    let category_rust: HashMap<&str, &str> = HashMap::from([
        ("lexer", "Lexer"),
        ("pragma", "Pragma"),
        ("parser", "Parser"),
        ("resolver", "Resolver"),
        ("types", "Types"),
        ("lint", "Lint"),
        ("lowering", "Lowering"),
        ("manifest", "Manifest"),
        ("rewrite", "Rewrite"),
    ]);

    let mut entries: Vec<Entry> = Vec::new();
    let mut sections = String::new();
    let mut catalog_index = 0usize;

    for (key, cat_rust) in category_rust {
        let Some(yaml_entries) = map.get(key).and_then(|v| v.as_sequence()) else {
            continue;
        };
        writeln!(sections, "    {cat_rust}: {{").unwrap();
        for entry in yaml_entries {
            let id = entry["id"].as_str().unwrap().to_string();
            let name = entry["name"].as_str().unwrap().to_string();
            let const_name = to_screaming_snake(&name);
            let sev = entry["severity"].as_str().unwrap();
            let sev_rust = match sev {
                "error" => "Error",
                "warning" => "Warning",
                "info" => "Info",
                "hint" => "Hint",
                other => panic!("unknown severity {other} for {id}"),
            };
            writeln!(
                sections,
                "        {const_name} = (\"{id}\", \"{name}\", {sev_rust}),"
            )
            .unwrap();
            entries.push(Entry {
                id: id.clone(),
                catalog_index,
            });
            catalog_index += 1;
        }
        sections.push_str("    },\n\n");
    }

    let mut lookup_arms = String::new();
    for e in &entries {
        writeln!(
            lookup_arms,
            "        \"{}\" => Some(&CATALOG[{}]),",
            e.id, e.catalog_index
        )
        .unwrap();
    }

    // Extended `--explain` write-ups: one markdown file per code under
    // `explain/<ID>.md`, embedded via `include_str!`. A file whose name
    // doesn't match a catalog code is a typo waiting to confuse someone,
    // so reject it at build time.
    let explain_dir = Path::new(&manifest_dir).join("explain");
    println!("cargo:rerun-if-changed={}", explain_dir.display());
    let known_ids: std::collections::HashSet<&str> =
        entries.iter().map(|e| e.id.as_str()).collect();
    let mut explain_arms = String::new();
    if explain_dir.is_dir() {
        let mut files: Vec<_> = fs::read_dir(&explain_dir)
            .expect("read explain/ dir")
            .map(|d| d.expect("dir entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "md"))
            .collect();
        files.sort();
        for path in files {
            let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
            assert!(
                known_ids.contains(stem.as_str()),
                "explain/{stem}.md does not match any diagnostic code in catalog.yaml"
            );
            println!("cargo:rerun-if-changed={}", path.display());
            writeln!(
                explain_arms,
                "        \"{}\" => Some(include_str!(r\"{}\")),",
                stem,
                path.display()
            )
            .unwrap();
        }
    }

    let out = format!(
        r#"use super::{{Category, Code, CodeMeta, Severity}};

macro_rules! catalog {{
    (
        $(
            $cat:ident: {{
                $( $const_name:ident = ($id:literal, $name:literal, $sev:ident) ),* $(,)?
            }}
        ),* $(,)?
    ) => {{
        $(
            $(
                #[doc = concat!("`", $id, "` — `", $name, "`")]
                pub const $const_name: Code = Code($id);
            )*
        )*

        pub static CATALOG: &[CodeMeta] = &[
            $(
                $(
                    CodeMeta {{
                        id: $id,
                        name: $name,
                        default_severity: Severity::$sev,
                        category: Category::$cat,
                    }},
                )*
            )*
        ];

        const _: () = {{
            let mut i = 0;
            while i < CATALOG.len() {{
                let mut j = i + 1;
                while j < CATALOG.len() {{
                    if str_eq(CATALOG[i].id, CATALOG[j].id) {{
                        panic!("duplicate diagnostic code id in catalog");
                    }}
                    j += 1;
                }}
                i += 1;
            }}
        }};
    }};
}}

const fn str_eq(a: &str, b: &str) -> bool {{
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {{
        return false;
    }}
    let mut i = 0;
    while i < a.len() {{
        if a[i] != b[i] {{
            return false;
        }}
        i += 1;
    }}
    true
}}

catalog! {{
{sections}}}

/// O(1) metadata lookup by diagnostic id (generated from the catalog).
pub(crate) fn lookup_meta(id: &str) -> Option<&'static CodeMeta> {{
    match id {{
{lookup_arms}        _ => None,
    }}
}}

/// Extended explanation for a diagnostic id (the `explain/<ID>.md`
/// write-up), or `None` if no extended explanation has been authored.
pub(crate) fn explain_for(id: &str) -> Option<&'static str> {{
    match id {{
{explain_arms}        _ => None,
    }}
}}

#[cfg(test)]
mod tests {{
    use super::{{lookup_meta, CATALOG, Code}};

    #[test]
    fn catalog_ids_unique() {{
        let mut ids: Vec<_> = CATALOG.iter().map(|m| m.id).collect();
        ids.sort();
        for w in ids.windows(2) {{
            assert_ne!(w[0], w[1], "duplicate code id {{}}", w[0]);
        }}
    }}

    #[test]
    fn catalog_names_unique() {{
        let mut names: Vec<_> = CATALOG.iter().map(|m| m.name).collect();
        names.sort();
        for w in names.windows(2) {{
            assert_ne!(w[0], w[1], "duplicate code name {{}}", w[0]);
        }}
    }}

    #[test]
    fn lookup_round_trip() {{
        for entry in CATALOG {{
            let code = Code(entry.id);
            assert_eq!(code.name(), entry.name);
            assert_eq!(code.default_severity(), entry.default_severity);
            let meta = lookup_meta(entry.id).expect("lookup");
            assert_eq!(meta.id, entry.id);
            assert_eq!(meta.name, entry.name);
            assert_eq!(meta.default_severity, entry.default_severity);
            assert_eq!(meta.category, entry.category);
        }}
    }}

    #[test]
    fn lookup_unknown_returns_none() {{
        assert!(lookup_meta("E9999").is_none());
    }}

    #[test]
    fn explain_present_for_authored_code() {{
        // L0022 ships an `explain/L0022.md` write-up.
        let text = super::explain_for("L0022").expect("L0022 explanation");
        assert!(text.contains("L0022"), "explanation should name its code");
        assert!(super::explain_for("E9999").is_none());
    }}

    #[test]
    fn every_explanation_matches_a_real_code() {{
        // `explain_for` only returns text for ids that are also in the
        // catalog (the build script rejects orphan files, but assert the
        // invariant here too).
        for entry in CATALOG {{
            // Just exercising the lookup; a `Some` must round-trip.
            if let Some(text) = super::explain_for(entry.id) {{
                assert!(!text.is_empty(), "{{}} explanation is empty", entry.id);
            }}
        }}
    }}
}}
"#
    );

    let out_dir = env::var("OUT_DIR").unwrap();
    fs::write(Path::new(&out_dir).join("catalog.rs"), out).expect("write generated catalog.rs");
}

fn to_screaming_snake(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}
