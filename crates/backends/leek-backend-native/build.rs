//! Derive the **AOT ABI tag**: a short hash of the source that defines the
//! contract between a compiled program object and the prebuilt static runtime
//! archive (`libleek_aot_runtime.a`) it links against.
//!
//! The program object references the `leek_*` shims **by name only** — the
//! signatures are re-declared independently in `src/translate/imports.rs` — so
//! an archive built from an older checkout links cleanly and then corrupts
//! arguments at run time. To make that fail at *link* time instead, this script
//! emits an anchor function `leek_aot_abi_<tag>` into the crate (and thus into
//! the archive, via `leek_aot_force_link`), and `aot::main_c_source` has the
//! generated C harness call it. A mismatched archive has no such symbol, so
//! `cc` reports an undefined reference and `aot.rs` turns that into a "rebuild
//! the static runtime" message.
//!
//! The tag is deliberately **conservative**: it hashes whole files rather than
//! a distilled signature table, so a comment edit bumps it too. A spurious
//! rebuild of one small crate costs seconds; a silently mismatched archive
//! costs a debugging session. Note also that a plain workspace build already
//! rebuilds the archive whenever these files change — `leek-aot-runtime`
//! depends on this crate — so in a source checkout the bump is usually free.
//!
//! Writes into `OUT_DIR` and nowhere else (the CI guard added by #425 enforces
//! that a build script leaves the tracked tree alone).

use std::env;
use std::fs;
use std::path::Path;

/// Files whose bytes define the object ↔ archive contract.
///
/// * `translate/imports.rs` — every shim symbol name and Cranelift signature
///   the program object declares as an import.
/// * `runtime/mod.rs` — the shim definitions and the `runtime_symbols()` list
///   the archive retains.
/// * `aot.rs` — the generated C harness: the glue declarations and the
///   `leek_main` return shapes.
/// * `aot_meta.rs` — `leek_aot_install`'s signature and the metadata blob
///   format the harness passes through it.
///
/// Not covered: `leek-aot-runtime`'s own glue (`leek_aot_setup`, the
/// `leek_aot_print_*` helpers). It is a different crate, and a build script
/// reading a sibling crate's sources would be fragile. It depends on this one,
/// so in practice a revision whose glue differs differs here too.
const ABI_SOURCES: &[&str] = &[
    "src/translate/imports.rs",
    "src/runtime/mod.rs",
    "src/aot.rs",
    "src/aot_meta.rs",
];

/// FNV-1a, 64-bit. Hand-rolled to keep this crate's build free of dependencies
/// — the tag needs to be stable and short, not cryptographic.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let mut hash = Fnv::new();
    // The crate version participates: a release may change shim behaviour
    // without changing any of the files below.
    hash.write(
        env::var("CARGO_PKG_VERSION")
            .expect("CARGO_PKG_VERSION")
            .as_bytes(),
    );
    for rel in ABI_SOURCES {
        let path = Path::new(&manifest_dir).join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("reading {rel}: {e}"));
        // The path is hashed too, so moving content between the files moves
        // the tag.
        hash.write(rel.as_bytes());
        hash.write(&bytes);
    }
    let tag = format!("{:016x}", hash.0);

    let out = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("abi.rs");
    fs::write(
        &out,
        format!(
            r#"/// This build's AOT ABI tag — see `build.rs`.
pub const AOT_ABI_TAG: &str = "{tag}";

/// The ABI anchor the generated C harness calls. Defining it costs one empty
/// function; *finding* it at link time is the proof that the static runtime
/// archive was built from this same revision. Never called for its effect.
#[unsafe(no_mangle)]
pub extern "C" fn leek_aot_abi_{tag}() {{}}

/// The anchor's address, under a name that does not move with the tag, so
/// `leek-aot-runtime`'s `leek_aot_force_link` can reference it and keep the
/// symbol in the static archive.
#[must_use]
pub fn abi_marker() -> *const () {{
    leek_aot_abi_{tag} as *const ()
}}
"#
        ),
    )
    .unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
}
