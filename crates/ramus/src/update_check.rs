//! Finding out whether a newer Ramus release is out, so the title bar can say so. Only a
//! version number is read; nothing is downloaded or installed.

use std::fmt;

/// Where releases are published, and where the user goes to download one.
const RELEASES_PAGE: &str = "https://github.com/xiangzhe0812/ramus/releases/tag";

/// A release version, `vMAJOR.MINOR.PATCH`. Compares part by part, as numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// Reads a release tag: `v` and three dot-separated numbers, nothing else.
    pub fn parse_tag(tag: &str) -> Option<Version> {
        parse_numbers(tag.strip_prefix('v')?)
    }

    /// The version of this build.
    pub fn current() -> Version {
        let version = env!("CARGO_PKG_VERSION");
        parse_numbers(version)
            .unwrap_or_else(|| panic!("Cargo.toml version {version:?} is not MAJOR.MINOR.PATCH"))
    }
}

/// `MAJOR.MINOR.PATCH` with plain decimal digits only (no sign, no suffix).
fn parse_numbers(text: &str) -> Option<Version> {
    let mut parts = text.split('.').map(|part| {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<u64>().ok()
    });
    let version = Version {
        major: parts.next()??,
        minor: parts.next()??,
        patch: parts.next()??,
    };
    parts.next().is_none().then_some(version)
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The release `tag` names, when it is newer than `current`.
pub fn newer_release(tag: &str, current: Version) -> Option<Version> {
    Version::parse_tag(tag).filter(|release| *release > current)
}

/// The page for `version`'s release, built here rather than taken from GitHub's reply, so
/// a tampered reply can't make the app open some other address.
pub fn release_page(version: Version) -> String {
    format!("{RELEASES_PAGE}/{version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_release_is_reported() {
        let current = Version::parse_tag("v0.2.0").unwrap();

        let newer = newer_release("v0.3.0", current);

        assert_eq!(newer, Version::parse_tag("v0.3.0"));
    }

    #[test]
    fn the_same_or_an_older_release_is_not() {
        let current = Version::parse_tag("v0.2.1").unwrap();

        assert_eq!(newer_release("v0.2.1", current), None);
        assert_eq!(newer_release("v0.2.0", current), None);
    }

    #[test]
    fn parts_compare_as_numbers_not_text() {
        let current = Version::parse_tag("v0.9.0").unwrap();

        assert!(newer_release("v0.10.0", current).is_some());
    }

    #[test]
    fn only_a_plain_v_major_minor_patch_tag_counts() {
        let current = Version::parse_tag("v0.0.1").unwrap();

        for tag in [
            "0.3.0",
            "v0.3",
            "v0.3.0-beta.1",
            "ramus-v0.3.0",
            "v0.3.0.1",
            "",
            "v-1.0.0",
            "v+1.0.0",
        ] {
            assert_eq!(newer_release(tag, current), None, "{tag}");
        }
    }

    #[test]
    fn the_release_page_is_built_from_the_version() {
        let version = Version::parse_tag("v0.3.0").unwrap();

        assert_eq!(
            release_page(version),
            "https://github.com/xiangzhe0812/ramus/releases/tag/v0.3.0"
        );
    }

    #[test]
    fn the_running_version_parses() {
        Version::current();
    }
}
