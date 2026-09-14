//! Project discovery — finding `Miku.toml` on disk.

use std::path::{Path, PathBuf};

use crate::error::{ManifestError, ManifestErrorKind, ManifestWarning};
use crate::types::Manifest;

/// A loaded manifest plus the project root and any non-fatal warnings.
#[derive(Debug, Clone)]
pub struct ManifestLoad {
    pub manifest: Manifest,
    /// The directory containing `Miku.toml`. All manifest-relative
    /// paths resolve against this.
    pub root: PathBuf,
    /// The `Miku.toml` itself — the label a reporter prints above a manifest
    /// diagnostic's snippet.
    pub path: PathBuf,
    /// The manifest's text, kept so a diagnostic's span can be rendered
    /// against it. Without it the spans are inert: a caret needs the line it
    /// points into. Manifests are a few kilobytes.
    pub text: String,
    pub warnings: Vec<ManifestWarning>,
}

/// Search `start` and its ancestors for the nearest `Miku.toml` and
/// load it.
pub fn discover(start: &Path) -> Result<ManifestLoad, ManifestError> {
    let mut cursor: PathBuf = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| {
                ManifestError::detached(ManifestErrorKind::Cwd {
                    message: e.to_string(),
                })
            })?
            .join(start)
    };

    // Walk up from the starting directory (inclusive).
    loop {
        let candidate = cursor.join("Miku.toml");
        if candidate.is_file() {
            return load_from(&candidate);
        }
        if !cursor.pop() {
            break;
        }
    }
    Err(ManifestError::detached(ManifestErrorKind::NotFound {
        start: start.to_path_buf(),
    }))
}

/// Load a specific `Miku.toml` file. The project root is the file's
/// parent directory.
pub fn load_from(path: &Path) -> Result<ManifestLoad, ManifestError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        ManifestError::detached(ManifestErrorKind::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    })?;
    let root = path
        .parent()
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
    let (manifest, warnings) = crate::parse::parse(&text)?;
    Ok(ManifestLoad {
        manifest,
        root,
        path: path.to_path_buf(),
        text,
        warnings,
    })
}

/// Parse a manifest from a string. The caller is responsible for
/// supplying any project-root context separately.
pub fn load_str(s: &str) -> Result<(Manifest, Vec<ManifestWarning>), ManifestError> {
    crate::parse::parse(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    fn tempdir() -> PathBuf {
        loop {
            let base = std::env::temp_dir().join(format!(
                "leek-manifest-test-{}-{}-{}",
                std::process::id(),
                random_suffix(),
                NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::create_dir(&base) {
                Ok(()) => return base,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create tempdir: {error}"),
            }
        }
    }

    fn random_suffix() -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use std::time::{SystemTime, UNIX_EPOCH};
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut h = DefaultHasher::new();
        t.hash(&mut h);
        h.finish()
    }

    #[test]
    fn discover_walks_up() {
        let root = tempdir();
        std::fs::write(
            root.join("Miku.toml"),
            r#"[project]
name = "demo"
version = "0.1.0"
"#,
        )
        .unwrap();
        let nested = root.join("src").join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let loaded = discover(&nested).expect("should find ancestor manifest");
        assert_eq!(loaded.manifest.project.name, "demo");
        assert_eq!(loaded.root, root);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discover_fails_when_absent() {
        let root = tempdir();
        let err = discover(&root).unwrap_err();
        assert!(matches!(err.kind, ManifestErrorKind::NotFound { .. }));
        assert!(err.to_string().contains("no `Miku.toml`"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_from_reads_file() {
        let root = tempdir();
        let path = root.join("Miku.toml");
        std::fs::write(
            &path,
            r#"[project]
name = "demo2"
version = "0.0.1"
"#,
        )
        .unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.manifest.project.name, "demo2");
        assert_eq!(loaded.root, root);
        std::fs::remove_dir_all(&root).ok();
    }
}
