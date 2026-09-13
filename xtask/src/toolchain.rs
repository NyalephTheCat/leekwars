//! `cargo xtask check-toolchain`: keep the Rust pin honest.
//!
//! Clippy runs at `pedantic` with `-D warnings`, so a floating
//! `channel = "stable"` lets every new Rust release turn CI red without a
//! single commit in this repo (commit fb6e336 was exactly that). The pin must
//! therefore name an exact release. The advertised MSRV is pinned to the same
//! release, which is what makes it a real promise: nothing else in the repo
//! ever compiles with an older compiler, so a `rust-version` that drifted
//! below the pin would be an unchecked claim.
//!
//! The task needs no network and no second toolchain. It checks that:
//!
//! - `rust-toolchain.toml` pins an exact `MAJOR.MINOR.PATCH` release;
//! - the pin still requests the `rustfmt` and `clippy` components CI runs;
//! - `[workspace.package] rust-version` names the pin's `MAJOR.MINOR`, so the
//!   ordinary `cargo build` *is* the MSRV check.

use std::fmt;
use std::path::Path;
use std::process::ExitCode;

use serde::Deserialize;

/// Toolchain file location, relative to the workspace root.
const TOOLCHAIN_PATH: &str = "rust-toolchain.toml";
/// Workspace manifest location, relative to the workspace root.
const MANIFEST_PATH: &str = "Cargo.toml";

/// Components CI assumes the pinned toolchain ships.
const REQUIRED_COMPONENTS: &[&str] = &["rustfmt", "clippy"];

/// The subset of `rust-toolchain.toml` we read.
#[derive(Debug, Deserialize)]
struct ToolchainFile {
    toolchain: ToolchainSection,
}

#[derive(Debug, Deserialize)]
struct ToolchainSection {
    channel: String,
    #[serde(default)]
    components: Vec<String>,
}

/// The subset of the workspace `Cargo.toml` we read.
#[derive(Debug, Deserialize)]
struct Manifest {
    workspace: ManifestWorkspace,
}

#[derive(Debug, Deserialize)]
struct ManifestWorkspace {
    package: ManifestPackage,
}

#[derive(Debug, Deserialize)]
struct ManifestPackage {
    #[serde(rename = "rust-version")]
    rust_version: String,
}

/// An exact Rust release, `MAJOR.MINOR.PATCH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Release {
    /// Parse an exact release, rejecting everything a rustup channel may also
    /// be: `stable`, `beta`, `nightly-2026-01-01`, a bare `1.94`, or a
    /// host-suffixed `1.94.1-x86_64-unknown-linux-gnu`.
    pub fn parse_exact(channel: &str) -> Option<Self> {
        let mut parts = channel.split('.');
        let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major: number(major)?,
            minor: number(minor)?,
            patch: number(patch)?,
        })
    }

    /// The `MAJOR.MINOR` a matching `rust-version` must name.
    pub fn msrv(self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A decimal component of a version: digits only, no sign, no padding.
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Check the two files against each other. Returns the pinned release, or
/// every problem found.
pub fn check(toolchain_toml: &str, cargo_toml: &str) -> Result<Release, Vec<String>> {
    let mut errors = Vec::new();

    let toolchain = match toml::from_str::<ToolchainFile>(toolchain_toml) {
        Ok(file) => Some(file.toolchain),
        Err(e) => {
            errors.push(format!(
                "cannot read [toolchain] from {TOOLCHAIN_PATH}: {e}"
            ));
            None
        }
    };
    let rust_version = match toml::from_str::<Manifest>(cargo_toml) {
        Ok(manifest) => Some(manifest.workspace.package.rust_version),
        Err(e) => {
            errors.push(format!(
                "cannot read [workspace.package] rust-version from {MANIFEST_PATH}: {e}"
            ));
            None
        }
    };

    let Some(toolchain) = toolchain else {
        return Err(errors);
    };

    let pinned = Release::parse_exact(&toolchain.channel);
    if pinned.is_none() {
        errors.push(format!(
            "{TOOLCHAIN_PATH}: channel = \"{}\" is not an exact release. Pin one \
             (e.g. channel = \"1.94.1\") so a new stable cannot turn `-D warnings` \
             CI red on its own",
            toolchain.channel
        ));
    }

    for required in REQUIRED_COMPONENTS {
        if !toolchain.components.iter().any(|c| c == required) {
            errors.push(format!(
                "{TOOLCHAIN_PATH}: components is missing `{required}`, which CI runs"
            ));
        }
    }

    if let (Some(pinned), Some(rust_version)) = (pinned, rust_version.as_deref())
        && rust_version != pinned.msrv()
    {
        errors.push(format!(
            "{MANIFEST_PATH}: rust-version = \"{rust_version}\" does not match the pinned \
             toolchain {pinned}. Set it to \"{}\" — the pin is the only compiler anything \
             here builds with, so a different MSRV is a claim nothing checks",
            pinned.msrv()
        ));
    }

    match pinned {
        Some(pinned) if errors.is_empty() => Ok(pinned),
        _ => Err(errors),
    }
}

/// Entry point for `cargo xtask check-toolchain`.
pub fn run() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root");
    let read = |name: &str| {
        std::fs::read_to_string(root.join(name)).map_err(|e| format!("cannot read {name}: {e}"))
    };
    let (toolchain, manifest) = match (read(TOOLCHAIN_PATH), read(MANIFEST_PATH)) {
        (Ok(toolchain), Ok(manifest)) => (toolchain, manifest),
        (toolchain, manifest) => {
            for e in [toolchain.err(), manifest.err()].into_iter().flatten() {
                eprintln!("toolchain check: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    match check(&toolchain, &manifest) {
        Ok(pinned) => {
            println!(
                "toolchain check: ok (pinned to {pinned}, MSRV {})",
                pinned.msrv()
            );
            ExitCode::SUCCESS
        }
        Err(errors) => {
            for e in &errors {
                eprintln!("toolchain check: {e}");
            }
            eprintln!("toolchain check: see CONTRIBUTING.md § \"Bumping the Rust toolchain\"");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINNED: &str =
        "[toolchain]\nchannel = \"1.94.1\"\ncomponents = [\"rustfmt\", \"clippy\"]\n";
    const MANIFEST: &str = "[workspace.package]\nrust-version = \"1.94\"\n";

    #[test]
    fn exact_releases_only() {
        assert_eq!(
            Release::parse_exact("1.94.1"),
            Some(Release {
                major: 1,
                minor: 94,
                patch: 1
            })
        );
        for floating in [
            "stable",
            "beta",
            "nightly",
            "nightly-2026-01-01",
            "1.94",
            "1.94.1-x86_64-unknown-linux-gnu",
            "1.94.1.0",
            "1..1",
            "",
        ] {
            assert_eq!(Release::parse_exact(floating), None, "{floating}");
        }
    }

    #[test]
    fn a_matching_pin_and_msrv_pass() {
        assert_eq!(
            check(PINNED, MANIFEST).expect("matching pin"),
            Release {
                major: 1,
                minor: 94,
                patch: 1
            }
        );
    }

    /// The bug this task exists for: `channel = "stable"` silently re-points at
    /// whatever Rust ships next.
    #[test]
    fn a_floating_channel_is_rejected() {
        let floating =
            "[toolchain]\nchannel = \"stable\"\ncomponents = [\"rustfmt\", \"clippy\"]\n";
        let errors = check(floating, MANIFEST).expect_err("floating channel");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("is not an exact release"), "{errors:?}");
    }

    /// An MSRV below the pin is a promise nothing in the repo ever tests.
    #[test]
    fn an_msrv_that_drifted_from_the_pin_is_rejected() {
        let errors = check(PINNED, "[workspace.package]\nrust-version = \"1.90\"\n")
            .expect_err("stale MSRV");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("does not match the pinned toolchain"),
            "{errors:?}"
        );
    }

    #[test]
    fn dropped_components_are_rejected() {
        let errors =
            check("[toolchain]\nchannel = \"1.94.1\"\n", MANIFEST).expect_err("missing components");
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("missing `rustfmt`"), "{errors:?}");
        assert!(errors[1].contains("missing `clippy`"), "{errors:?}");
    }

    #[test]
    fn unreadable_files_are_reported_not_ignored() {
        let errors = check("channel = 1", "[workspace]\n").expect_err("unparsable input");
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains(TOOLCHAIN_PATH), "{errors:?}");
        assert!(errors[1].contains(MANIFEST_PATH), "{errors:?}");
    }

    /// The repo itself pins an exact release and advertises it as the MSRV.
    #[test]
    fn the_repo_pins_an_exact_toolchain() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let toolchain = std::fs::read_to_string(root.join(TOOLCHAIN_PATH)).expect(TOOLCHAIN_PATH);
        let manifest = std::fs::read_to_string(root.join(MANIFEST_PATH)).expect(MANIFEST_PATH);
        check(&toolchain, &manifest).unwrap_or_else(|e| panic!("{e:#?}"));
    }
}
