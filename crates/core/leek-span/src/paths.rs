//! One spelling of "which file is this?".
//!
//! Every map that is keyed by a file — the project index, the include
//! graph, the LSP's indexed-file table, the debug adapter's breakpoint
//! map, the scenario loader's `extends` cycle set — needs two
//! spellings of the same path to collapse to one key. Before this
//! module that rule was written six times, with two different
//! fallbacks: `dir/../Code.leek` keyed as `Code.leek` in the resolver
//! and as `dir/../Code.leek` in the project index, so the two maps
//! disagreed about file identity for any file that does not exist yet
//! — an unsaved buffer, an include target being typed (#181).
//!
//! This lives in `leek-span` because it is the bottom of the
//! dependency graph (no `leek-*` dependencies at all) and every crate
//! that needs it already depends on it. It is about file *identity*,
//! the same way the rest of the crate is about position identity.
//!
//! ## The results are map keys, not I/O targets
//!
//! [`normalize_lexical`] resolves `..` without asking the filesystem,
//! which is wrong as a path to *open*: if `b` is a symlink, `a/b/..`
//! is not `a`. It is right as a key, because two spellings that name
//! the same nonexistent file must hash the same. So open the path the
//! caller handed you and key the map by the canonical form — never
//! the other way round.

use std::path::{Component, Path, PathBuf};

/// Resolve `.` and `..` textually, without touching the filesystem.
///
/// Use this only when [`canonical_or_normalized`] has already failed
/// — i.e. for a path that does not exist — and only to build a map
/// key. See the [module docs](self) for why it is not a path to open.
#[must_use]
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The canonical form of a real path, or the lexically normalized form
/// of a virtual or not-yet-existing one.
///
/// The order is load-bearing and must not be swapped. Canonicalization
/// goes first because it is the only one of the two that is correct in
/// the presence of symlinks: `real/link/..` collapses lexically to
/// `real`, but `std::fs::canonicalize` follows `link` and lands
/// wherever it actually points. Normalizing first and canonicalizing
/// the result would silently key two different files the same.
///
/// Canonicalization is also what makes a workspace reached through a
/// symlink work at all — on macOS the system temp dir is
/// `/var/folders/…` while `/var` is a symlink to `/private/var`, so a
/// client that says `file:///var/folders/…` and an index that stored
/// `/private/var/folders/…` only meet if both sides go through here.
#[must_use]
pub fn canonical_or_normalized(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| normalize_lexical(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throw-away directory under the system temp dir, removed on
    /// drop. `leek-span` has no dependencies by design, so this is
    /// four lines rather than a `tempfile` edge onto the bottom of the
    /// dependency graph.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("leek-span-paths-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn normalizes_parent_and_current_components() {
        assert_eq!(
            normalize_lexical(Path::new("/proj/dir/../Code.leek")),
            PathBuf::from("/proj/Code.leek"),
        );
        assert_eq!(
            normalize_lexical(Path::new("dir/./sub/../Code.leek")),
            PathBuf::from("dir/Code.leek"),
        );
        assert_eq!(
            normalize_lexical(Path::new("a/b/c")),
            PathBuf::from("a/b/c")
        );
    }

    /// The case from #181: the index and the resolver used to disagree
    /// here because only one of them normalized.
    #[test]
    fn nonexistent_paths_collapse_to_one_key() {
        let dir = TempDir::new("nonexistent");
        let direct = dir.path().join("Code.leek");
        let indirect = dir.path().join("sub").join("..").join("Code.leek");
        assert!(!direct.exists(), "the test file must not exist");
        assert_eq!(
            canonical_or_normalized(&direct),
            canonical_or_normalized(&indirect),
        );
    }

    #[test]
    fn existing_paths_match_std_canonicalize() {
        let dir = TempDir::new("existing");
        let file = dir.path().join("a.leek");
        std::fs::write(&file, "return 1\n").expect("write fixture");
        assert_eq!(
            canonical_or_normalized(&file),
            std::fs::canonicalize(&file).expect("canonicalize"),
        );
    }

    /// The macOS CI failure this module exists to prevent, written so
    /// it is meaningful on Linux too: a workspace reached through a
    /// symlink must key the same as the real one.
    #[cfg(unix)]
    #[test]
    fn symlinked_directory_keys_the_same_as_the_real_one() {
        let dir = TempDir::new("symlink");
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).expect("create real dir");
        std::fs::write(real.join("f.leek"), "return 1\n").expect("write fixture");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        assert_eq!(
            canonical_or_normalized(&link.join("f.leek")),
            canonical_or_normalized(&real.join("f.leek")),
        );
    }

    /// Why the order in `canonical_or_normalized` cannot be swapped:
    /// `link/..` is the symlink's *parent's* parent on disk, which is
    /// not what dropping the component textually would give. For a
    /// file that exists, only the canonicalizing branch is right.
    #[cfg(unix)]
    #[test]
    fn parent_dir_is_not_collapsed_through_a_symlink() {
        let dir = TempDir::new("symlink-parent");
        let real = dir.path().join("nested").join("real");
        std::fs::create_dir_all(&real).expect("create real dir");
        std::fs::write(real.join("f.leek"), "return 1\n").expect("write fixture");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        // `link/../f.leek` is `<dir>/f.leek` lexically, but on disk it
        // is `<dir>/nested/f.leek` — which is where the real file is.
        std::fs::write(dir.path().join("nested").join("f.leek"), "return 2\n")
            .expect("write sibling");
        let through_link = link.join("..").join("f.leek");
        assert_eq!(
            canonical_or_normalized(&through_link),
            canonical_or_normalized(&dir.path().join("nested").join("f.leek")),
        );
        assert_ne!(
            canonical_or_normalized(&through_link),
            normalize_lexical(&through_link),
        );
    }
}
