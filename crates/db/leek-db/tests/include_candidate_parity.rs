//! One candidate order, three resolvers.
//!
//! `include("name")` is resolved three times over in this workspace —
//! by [`DiskFolder`] for the `leekc` / `miku` CLIs, by [`MemFolder`] for
//! the LSP's shadowing layer and its fixtures, and by
//! [`resolve_include`] for the tracked pipeline. All three are supposed
//! to consult [`include_candidates`] and nothing else.
//!
//! They drifted once: #521 found `MemFolder` trying a fourth spelling of
//! its own — the raw include name, keyed as written — and the LSP
//! shadows disk with a `MemFolder`, so the editor accepted includes the
//! CLI rejected. A program that compiles in the editor and fails on the
//! command line is the worst way for two resolvers to disagree, so the
//! extra candidate was dropped rather than spread to the other two.
//!
//! This suite is the pin that keeps them together. Each case names a
//! tree, an includer and an include name, and asserts the three land on
//! the *same* file or all three refuse. The suite lives here, not in
//! `leek-resolver`, because this is the only crate that can see all
//! three: the query is above the folders.

mod support;

use std::path::{Path, PathBuf};

use leek_db::queries::{IncludeRef, resolve_include};
use leek_resolver::folder::{DiskFolder, Folder, LoadError, MemFolder, include_candidates};
use leek_span::paths::canonical_or_normalized;
use support::{Fixture, ROOT, vpath};

/// Every fixture file holds the same bytes: these cases are about
/// *which* file a name picks, never about what is in it.
const TEXT: &str = "// fixture\n";

// ---- The candidate list itself ----

#[test]
fn a_name_has_exactly_two_candidates_in_a_fixed_order() {
    assert_eq!(
        include_candidates(Path::new("/proj/main.leek"), "util"),
        [
            PathBuf::from("/proj/util.leek"),
            PathBuf::from("/proj/util"),
        ],
    );
}

#[test]
fn a_subfolder_name_traverses_from_the_includers_directory() {
    assert_eq!(
        include_candidates(Path::new("/proj/src/entry.leek"), "lib/util"),
        [
            PathBuf::from("/proj/src/lib/util.leek"),
            PathBuf::from("/proj/src/lib/util"),
        ],
    );
}

#[test]
fn the_raw_include_name_is_not_a_candidate() {
    // The #521 candidate, stated as the property the three resolvers
    // share: the bare `name` never appears on its own, only ever joined
    // onto the includer's directory. Every resolver may look at these
    // two paths and no others.
    let candidates = include_candidates(Path::new("/proj/src/entry.leek"), "shared/util");
    assert!(
        !candidates.contains(&PathBuf::from("shared/util")),
        "the raw include name is not a candidate: {candidates:?}",
    );
    assert!(
        !candidates.contains(&PathBuf::from("shared/util.leek")),
        "the raw include name is not a candidate: {candidates:?}",
    );
}

// ---- The three resolvers, on the same trees ----

/// One parity case: a tree, the file the `include("…")` is written in,
/// the name between the quotes, and the file all three resolvers must
/// land on — `None` when all three must refuse.
///
/// Paths are relative to each resolver's own root so the on-disk,
/// virtual and workspace spellings are comparable.
struct Case {
    /// Names the temp directory this case builds, so a failure points at
    /// a directory nobody else is using.
    slug: &'static str,
    tree: &'static [&'static str],
    includer: &'static str,
    name: &'static str,
    expected: Option<&'static str>,
}

const CASES: &[Case] = &[
    Case {
        slug: "sibling",
        tree: &["main.leek", "util.leek"],
        includer: "main.leek",
        name: "util",
        expected: Some("util.leek"),
    },
    Case {
        // The order, pinned where it is observable: both candidates
        // exist, and the `.leek` one wins.
        slug: "extension-first",
        tree: &["main.leek", "util.leek", "util"],
        includer: "main.leek",
        name: "util",
        expected: Some("util.leek"),
    },
    Case {
        slug: "bare-fallback",
        tree: &["main.leek", "util"],
        includer: "main.leek",
        name: "util",
        expected: Some("util"),
    },
    Case {
        // Written with the extension: the first candidate is
        // `util.leek.leek`, which nothing holds, so the bare one wins.
        slug: "spelled-with-extension",
        tree: &["main.leek", "util.leek"],
        includer: "main.leek",
        name: "util.leek",
        expected: Some("util.leek"),
    },
    Case {
        slug: "subfolder",
        tree: &["main.leek", "lib/util.leek"],
        includer: "main.leek",
        name: "lib/util",
        expected: Some("lib/util.leek"),
    },
    Case {
        slug: "parent-directory",
        tree: &["src/entry.leek", "shared/constants.leek"],
        includer: "src/entry.leek",
        name: "../shared/constants.leek",
        expected: Some("shared/constants.leek"),
    },
    Case {
        // #521's shape: the file exists, its path spells the include
        // name, and it is still not reachable from an includer in
        // another directory. (The folder-level regression — the same
        // shape with the fixture keyed relatively, which `MemFolder`
        // alone used to resolve — is pinned in `leek-resolver`'s
        // `folder.rs`, where a folder can be keyed by a relative path
        // without dragging the process's working directory in.)
        slug: "raw-name-from-another-directory",
        tree: &["src/entry.leek", "shared/util.leek"],
        includer: "src/entry.leek",
        name: "shared/util",
        expected: None,
    },
    Case {
        slug: "missing",
        tree: &["main.leek"],
        includer: "main.leek",
        name: "ghost",
        expected: None,
    },
];

#[test]
fn the_three_resolvers_agree_on_every_candidate_case() {
    for case in CASES {
        let disk = disk_resolves(case);
        let mem = mem_resolves(case);
        let query = query_resolves(case);
        let expected = case.expected.map(str::to_owned);
        assert_eq!(disk, expected, "DiskFolder, case `{}`", case.slug);
        assert_eq!(mem, expected, "MemFolder, case `{}`", case.slug);
        assert_eq!(query, expected, "resolve_include, case `{}`", case.slug);
    }
}

/// `DiskFolder` over a real tree in the system temp dir.
fn disk_resolves(case: &Case) -> Option<String> {
    let tree = TempTree::new(case.slug, case.tree);
    // The folder canonicalizes what it opens, so compare against the
    // canonical root or a symlinked temp dir (macOS's `/var`) would make
    // every case look like a mismatch.
    let root = canonical_or_normalized(tree.path());
    match DiskFolder.load(&root.join(case.includer), case.name) {
        Ok(loaded) => Some(relative_to(&root, &loaded.path)),
        Err(LoadError::NotFound) => None,
        Err(other) => panic!("case `{}`: {other}", case.slug),
    }
}

/// `MemFolder` over the same tree as virtual paths.
fn mem_resolves(case: &Case) -> Option<String> {
    let mut folder = MemFolder::new();
    for path in case.tree {
        folder.insert(vpath(path), TEXT);
    }
    match folder.load(Path::new(&vpath(case.includer)), case.name) {
        Ok(loaded) => Some(relative_to(Path::new(ROOT), &loaded.path)),
        Err(LoadError::NotFound) => None,
        Err(other) => panic!("case `{}`: {other}", case.slug),
    }
}

/// The tracked query over a workspace holding the same tree.
fn query_resolves(case: &Case) -> Option<String> {
    let files: Vec<(&str, &str)> = case.tree.iter().map(|path| (*path, TEXT)).collect();
    let fixture = Fixture::new(&files);
    let site = IncludeRef::new(&fixture.db, vpath(case.includer), case.name.to_owned());
    resolve_include(&fixture.db, fixture.files, site)
        .map(|file| relative_to(Path::new(ROOT), Path::new(file.canonical_path(&fixture.db))))
}

/// `path` under `root`, spelled with `/` separators so the three roots'
/// answers compare as plain strings.
fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// A throw-away directory tree under the system temp dir, removed on
/// drop. The workspace carries no `tempfile` dependency; this is the
/// same four lines `leek_span::paths`' own tests use.
struct TempTree(PathBuf);

impl TempTree {
    fn new(slug: &str, files: &[&str]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "leek-db-include-parity-{slug}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        for relative in files {
            let path = dir.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create fixture parent");
            }
            std::fs::write(&path, TEXT).expect("write fixture file");
        }
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
