//! Active language version. Selected per-file by `// @version:N`.
//!
//! Spec: `doc/versioning.md`.

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Version {
    V1 = 1,
    V2 = 2,
    V3 = 3,
    V4 = 4,
}

impl Version {
    /// The default version when no `@version` pragma is present.
    /// Matches `WordCompiler.LATEST_VERSION` in the Java reference.
    pub const LATEST: Self = Self::V4;

    pub fn from_pragma(n: u32) -> Option<Self> {
        match n {
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            3 => Some(Self::V3),
            4 => Some(Self::V4),
            _ => None,
        }
    }

    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

impl Default for Version {
    fn default() -> Self {
        Self::LATEST
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_numeric() {
        assert!(Version::V1 < Version::V2);
        assert!(Version::V3 < Version::V4);
    }

    #[test]
    fn pragma_round_trip() {
        for n in 1..=4 {
            assert_eq!(Version::from_pragma(n).unwrap().as_u32(), n);
        }
        assert_eq!(Version::from_pragma(0), None);
        assert_eq!(Version::from_pragma(5), None);
    }
}
