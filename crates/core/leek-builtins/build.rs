//! Generate builtin tables from `catalog.yaml`.

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

#[derive(Debug)]
struct Builtin {
    name: String,
    java_class: Option<String>,
    return_type: Option<String>,
    op_cost: Option<u32>,
    batch_mult: Option<u32>,
}

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let catalog_path = Path::new(&manifest).join("catalog.yaml");
    println!("cargo:rerun-if-changed={}", catalog_path.display());
    println!(
        "cargo:rerun-if-changed={}",
        Path::new(&manifest).join("builtins.tsv").display()
    );

    let builtins = load_catalog(&catalog_path);
    let out_dir_path = env::var("OUT_DIR").unwrap();
    let out_dir = Path::new(&out_dir_path);

    write_java_catalog(&builtins, &out_dir.join("catalog.rs"));
    write_op_costs(&builtins, &out_dir.join("op_costs.rs"));
    write_batch_mults(&builtins, &out_dir.join("batch_mult.rs"));
    write_registry(&builtins, &out_dir.join("registry.rs"));
}

fn load_catalog(path: &Path) -> Vec<Builtin> {
    if path.exists() {
        return parse_yaml(path);
    }
    // Fallback while migrating: builtins.tsv only.
    parse_tsv(&path.with_file_name("builtins.tsv"))
}

fn parse_yaml(path: &Path) -> Vec<Builtin> {
    let text = fs::read_to_string(path).expect("read catalog.yaml");
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("parse catalog.yaml");
    let seq = doc["builtins"].as_sequence().expect("builtins array");
    seq.iter()
        .map(|row| Builtin {
            name: row["name"].as_str().expect("name").to_string(),
            java_class: row
                .get("java_class")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            return_type: row
                .get("return")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            op_cost: row
                .get("op_cost")
                .and_then(serde_yaml::Value::as_u64)
                .map(|n| u32::try_from(n).expect("op_cost exceeds u32")),
            batch_mult: row
                .get("batch_mult")
                .and_then(serde_yaml::Value::as_u64)
                .map(|n| u32::try_from(n).expect("batch_mult exceeds u32")),
        })
        .collect()
}

fn parse_tsv(path: &Path) -> Vec<Builtin> {
    let text = fs::read_to_string(path).expect("read builtins.tsv");
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.split('\t');
            Some(Builtin {
                name: parts.next()?.to_string(),
                java_class: Some(parts.next()?.to_string()),
                return_type: Some(parts.next().unwrap_or("double").to_string()),
                op_cost: None,
                batch_mult: None,
            })
        })
        .collect()
}

fn write_java_catalog(builtins: &[Builtin], path: &Path) {
    let java: Vec<_> = builtins.iter().filter(|b| b.java_class.is_some()).collect();
    let mut out = String::from(
        "#[derive(Debug, Clone, Copy)]\n\
         pub struct JavaBuiltin {\n\
             pub name: &'static str,\n\
             pub java_class: &'static str,\n\
             pub return_type: &'static str,\n\
         }\n\n\
         pub static JAVA_BUILTINS: &[JavaBuiltin] = &[\n",
    );
    for b in &java {
        let class = b.java_class.as_deref().unwrap();
        let ret = b.return_type.as_deref().unwrap_or("double");
        writeln!(
            out,
            "    JavaBuiltin {{ name: \"{}\", java_class: \"{class}\", return_type: \"{ret}\" }},",
            b.name
        )
        .unwrap();
    }
    out.push_str("];\n\npub fn lookup_java(name: &str) -> Option<&'static JavaBuiltin> {\n");
    out.push_str("    JAVA_BUILTINS.iter().find(|b| b.name == name)\n}\n");
    fs::write(path, out).expect("write catalog.rs");
}

fn write_op_costs(builtins: &[Builtin], path: &Path) {
    let mut costs: BTreeMap<&str, u32> = BTreeMap::new();
    for b in builtins {
        if let Some(c) = b.op_cost {
            costs.insert(&b.name, c);
        }
    }
    let mut out = String::from(
        "/// Per-call op cost for the interpreter. Default: 1.\n\
         pub fn op_cost(name: &str) -> u32 {\n    match name {\n",
    );
    for (name, cost) in &costs {
        writeln!(out, "        \"{name}\" => {cost},").unwrap();
    }
    out.push_str("        _ => 1,\n    }\n}\n\n");
    out.push_str("pub fn op_cost_u64(name: &str) -> u64 {\n    u64::from(op_cost(name))\n}\n\n");
    out.push_str(
        "/// Per-call op cost for Java emit. Default: 0 (only listed names charge).\n\
         pub fn op_cost_emit(name: &str) -> u32 {\n    match name {\n",
    );
    for (name, cost) in &costs {
        writeln!(out, "        \"{name}\" => {cost},").unwrap();
    }
    out.push_str("        _ => 0,\n    }\n}\n");
    fs::write(path, out).expect("write op_costs.rs");
}

fn write_batch_mults(builtins: &[Builtin], path: &Path) {
    let mut mults: BTreeMap<&str, u32> = BTreeMap::new();
    for b in builtins {
        if let Some(m) = b.batch_mult {
            mults.insert(&b.name, m);
        }
    }
    let mut out = String::from(
        "/// Per-element multiplier for batch builtins (`ai.ops(size * N)`).\n\
         pub fn batch_multiplier(name: &str) -> Option<u32> {\n    match name {\n",
    );
    for (name, mult) in &mults {
        writeln!(out, "        \"{name}\" => Some({mult}),").unwrap();
    }
    out.push_str("        _ => None,\n    }\n}\n\n");
    out.push_str(
        "pub fn batch_multiplier_u64(name: &str) -> Option<u64> {\n\
         batch_multiplier(name).map(u64::from)\n}\n",
    );
    fs::write(path, out).expect("write batch_mult.rs");
}

fn write_registry(builtins: &[Builtin], path: &Path) {
    let mut out = String::from("pub static ALL_CATALOG_NAMES: &[&str] = &[\n");
    for b in builtins {
        writeln!(out, "    \"{}\",", b.name).unwrap();
    }
    out.push_str("];\n");
    fs::write(path, out).expect("write registry.rs");
}
