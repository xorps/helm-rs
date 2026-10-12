//! Semantic versions and constraints compatible with
//! `github.com/Masterminds/semver/v3` (as pinned by Helm).

use alloc::string::String;

/// A parsed version (`semver.NewVersion`, which is lenient: `v1.2`, `1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    _private: (),
}

impl Version {
    pub fn parse(s: &str) -> Result<Version, String> {
        let _ = s;
        todo!("semver::Version::parse")
    }
}

/// A parsed constraint set (`semver.NewConstraint`), e.g. `>=1.21.0-0`.
#[derive(Clone, Debug)]
pub struct Constraints {
    _private: (),
}

impl Constraints {
    pub fn parse(s: &str) -> Result<Constraints, String> {
        let _ = s;
        todo!("semver::Constraints::parse")
    }

    /// `Constraints.Check(v)`.
    pub fn check(&self, v: &Version) -> bool {
        let _ = v;
        todo!("semver::Constraints::check")
    }
}
