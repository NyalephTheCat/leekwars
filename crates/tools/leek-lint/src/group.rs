//! Lint groups and run options.
//!
//! Every lint belongs to exactly one [`LintGroup`], following the
//! clippy model: the group communicates *why* the lint exists and
//! decides whether it runs by default. `correctness`, `suspicious`,
//! `complexity`, and `style` are always on; `pedantic` and `nursery`
//! are opt-in via [`LintOptions`] (CLI flags / `Miku.toml [lints]`).

use std::fmt;
use std::str::FromStr;

/// Category a lint belongs to. Mirrors clippy's grouping, adapted to
/// Leekscript's teaching focus:
///
/// - [`Correctness`](Self::Correctness) — code that is outright wrong
///   (division by zero, unreachable statements).
/// - [`Suspicious`](Self::Suspicious) — very likely a mistake, but
///   with rare legitimate uses (`if (a = b)`, `a < b < c`).
/// - [`Complexity`](Self::Complexity) — does something simple in a
///   needlessly complex way (`c ? true : false`).
/// - [`Style`](Self::Style) — idiom and readability nits.
/// - [`Pedantic`](Self::Pedantic) — opt-in strictness for learners:
///   size limits, structure nits, "know your builtins" hints.
/// - [`Nursery`](Self::Nursery) — opt-in lints that teach
///   Leekscript-specific features (the ops budget, `@` by-ref
///   bindings, v4 intervals). Also the incubator for new lints whose
///   false-positive rate is still being tuned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LintGroup {
    Correctness,
    Suspicious,
    Complexity,
    Style,
    Pedantic,
    Nursery,
}

impl LintGroup {
    /// Whether lints in this group run without any opt-in.
    #[must_use]
    pub fn on_by_default(self) -> bool {
        !matches!(self, Self::Pedantic | Self::Nursery)
    }

    /// Stable lowercase name, as accepted by CLI flags and
    /// `Miku.toml [lints]` keys.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Correctness => "correctness",
            Self::Suspicious => "suspicious",
            Self::Complexity => "complexity",
            Self::Style => "style",
            Self::Pedantic => "pedantic",
            Self::Nursery => "nursery",
        }
    }

    /// All groups, in severity-ish order.
    pub const ALL: [LintGroup; 6] = [
        Self::Correctness,
        Self::Suspicious,
        Self::Complexity,
        Self::Style,
        Self::Pedantic,
        Self::Nursery,
    ];
}

impl fmt::Display for LintGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LintGroup {
    type Err = UnknownGroup;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|g| g.as_str() == s)
            .ok_or(UnknownGroup)
    }
}

/// Error from parsing a [`LintGroup`] name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownGroup;

impl fmt::Display for UnknownGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown lint group (expected one of: correctness, suspicious, \
             complexity, style, pedantic, nursery)"
        )
    }
}

impl std::error::Error for UnknownGroup {}

/// Which optional lint groups to run, plus the language version the
/// source targets (some nursery lints suggest version-gated features
/// like v4 intervals).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LintOptions {
    /// Run the `pedantic` group.
    pub pedantic: bool,
    /// Run the `nursery` group.
    pub nursery: bool,
    /// Leekscript version of the source (1..=4). Defaults to 4.
    pub version: u8,
}

impl Default for LintOptions {
    fn default() -> Self {
        Self {
            pedantic: false,
            nursery: false,
            version: 4,
        }
    }
}

impl LintOptions {
    /// Whether lints in `group` should run under these options.
    #[must_use]
    pub fn enabled(&self, group: LintGroup) -> bool {
        match group {
            LintGroup::Pedantic => self.pedantic,
            LintGroup::Nursery => self.nursery,
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_skip_opt_in_groups() {
        let opts = LintOptions::default();
        assert!(opts.enabled(LintGroup::Correctness));
        assert!(opts.enabled(LintGroup::Style));
        assert!(!opts.enabled(LintGroup::Pedantic));
        assert!(!opts.enabled(LintGroup::Nursery));
    }

    #[test]
    fn group_names_round_trip() {
        for g in LintGroup::ALL {
            assert_eq!(g.as_str().parse::<LintGroup>(), Ok(g));
        }
        assert!("warnings".parse::<LintGroup>().is_err());
    }
}
