//! The one error a [`Session`](crate::Session) hands back.

use std::path::PathBuf;

use leek_diagnostics::LintLevelError;
use leek_project::ProjectError;

/// Anything that stops a session before it has compiled anything.
///
/// These three failures used to be folded into one `anyhow::Error` — which
/// is what the `leek-session -> anyhow` line in `xtask/error-allowlist.txt`
/// paid for, and why removing that line is part of this type existing. They
/// are three variants because the caller can act on the difference: a
/// `[lint]` entry the catalog does not know is a `Miku.toml` the author can
/// fix, an unreadable file is a path they can correct, and a broken
/// manifest is neither.
///
/// Deliberately *not* an [`IntoDiagnostic`](leek_diagnostics::IntoDiagnostic):
/// none of these carries a span of its own. The one half that has a source
/// location — a broken manifest — keeps it inside
/// [`ProjectError::Manifest`](leek_project::ProjectError::Manifest), where a
/// caller with a reporter can still reach it.
#[derive(Debug)]
pub enum SessionError {
    /// The project could not be discovered, or could not hand over a file.
    Project(ProjectError),
    /// The manifest's `[lint]` table names a code the catalog does not know,
    /// so no [`Reporter`](leek_diagnostics::Reporter) could be built from it.
    LintLevel(LintLevelError),
    /// A source file could not be read.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Project(e) => e.fmt(f),
            SessionError::LintLevel(e) => write!(f, "[lint] in Miku.toml: {e}"),
            SessionError::Io { path, source } => {
                write!(f, "reading {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::Project(e) => Some(e),
            SessionError::LintLevel(e) => Some(e),
            SessionError::Io { source, .. } => Some(source),
        }
    }
}

impl From<ProjectError> for SessionError {
    fn from(err: ProjectError) -> Self {
        SessionError::Project(err)
    }
}

impl From<LintLevelError> for SessionError {
    fn from(err: LintLevelError) -> Self {
        SessionError::LintLevel(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_names_what_went_wrong_and_keeps_its_cause() {
        use std::error::Error as _;

        let lint = SessionError::from(LintLevelError::UnknownCode {
            raw: "NOPE9999".to_string(),
            suggestion: None,
        });
        assert!(lint.to_string().contains("NOPE9999"), "{lint}");
        assert!(lint.source().is_some());

        let io = SessionError::Io {
            path: PathBuf::from("/no/such/file.leek"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "nope"),
        };
        assert!(io.to_string().contains("/no/such/file.leek"), "{io}");
        // The `io::Error` survives whole, so a caller can match its `kind()`
        // instead of grepping the rendered sentence.
        let kind = io
            .source()
            .and_then(|e| e.downcast_ref::<std::io::Error>())
            .map(std::io::Error::kind);
        assert_eq!(kind, Some(std::io::ErrorKind::NotFound));
    }
}
