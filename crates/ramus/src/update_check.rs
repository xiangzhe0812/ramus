//! Finding out whether a newer Ramus release is out, so the title bar can say so. Only a
//! version number is read; nothing is downloaded or installed.

use std::fmt;

use serde::Deserialize;

use crate::github::{Endpoints, agent, describe};

/// Where releases are published, and where the user goes to download one.
const RELEASES_PAGE: &str = "https://github.com/xiangzhe0812/ramus/releases/tag";
/// GitHub's newest published release; drafts and pre-releases are left out.
const LATEST_RELEASE_PATH: &str = "/repos/xiangzhe0812/ramus/releases/latest";

/// The tag of the newest release on GitHub. Blocks, so run it on a background executor.
pub fn fetch_latest_tag(endpoints: &Endpoints) -> Result<String, String> {
    let mut response = agent()
        .get(format!("{}{LATEST_RELEASE_PATH}", endpoints.api))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", format!("Ramus/{}", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(describe)?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
    }
    let release: Release = response.body_mut().read_json().map_err(describe)?;
    Ok(release.tag_name)
}

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
    use crate::github::fake::Fake;

    const LATEST: &str = "/repos/xiangzhe0812/ramus/releases/latest";

    fn endpoints(fake: &Fake) -> Endpoints {
        let address = fake.serve();
        Endpoints {
            web: address.clone(),
            api: address,
        }
    }

    #[test]
    fn the_latest_tag_is_read_from_github() {
        let fake = Fake::default().reply(
            LATEST,
            &[r#"{"tag_name":"v0.3.0","html_url":"https://evil.example"}"#],
        );

        let tag = fetch_latest_tag(&endpoints(&fake));

        assert_eq!(tag.unwrap(), "v0.3.0");
    }

    #[test]
    fn the_request_names_ramus_and_its_version_and_carries_no_token() {
        let fake = Fake::default().reply(LATEST, &[r#"{"tag_name":"v0.3.0"}"#]);

        fetch_latest_tag(&endpoints(&fake)).unwrap();

        let requests = fake.requests.lock().unwrap();
        let (_, text) = requests.iter().find(|(path, _)| path == LATEST).unwrap();
        let text = text.to_lowercase();
        assert!(text.contains(&format!("user-agent: ramus/{}", env!("CARGO_PKG_VERSION"))));
        assert!(!text.contains("authorization"));
    }

    #[test]
    fn a_reply_without_a_tag_is_an_error() {
        // The fake answers `{}` for a path it has no reply for, as when there is no release.
        let fake = Fake::default();

        let tag = fetch_latest_tag(&endpoints(&fake));

        assert!(tag.is_err());
    }

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
