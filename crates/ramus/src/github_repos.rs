//! Repositories on github.com: reading one from a remote address, and asking GitHub's API
//! about them. Shared by pull requests and Clone.

use serde::de::DeserializeOwned;

use crate::github::{Endpoints, Secret, agent, describe};

/// A repository on github.com.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubRepo {
    pub owner: String,
    pub name: String,
}

/// The repository a remote address points at, if it is on github.com: `https://`,
/// `git@github.com:` and `ssh://git@github.com/` addresses, with or without `.git`.
pub fn parse_github_repo(url: &str) -> Option<GithubRepo> {
    let url = url.trim();
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let plain = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
    };
    (plain(owner) && plain(name)).then(|| GithubRepo {
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

/// GETs `path` from GitHub's API as JSON. `None` for "not found".
pub(crate) fn get_json<T: DeserializeOwned>(
    endpoints: &Endpoints,
    token: Option<&Secret>,
    path: &str,
) -> Result<Option<T>, String> {
    let request = agent()
        .get(format!("{}{path}", endpoints.api))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", format!("Ramus/{}", env!("CARGO_PKG_VERSION")));
    let request = match token {
        Some(token) => request.header("Authorization", format!("Bearer {}", token.0)),
        None => request,
    };
    let mut response = request.call().map_err(describe)?;
    if response.status() == 404 {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    response.body_mut().read_json().map(Some).map_err(describe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> GithubRepo {
        GithubRepo {
            owner: "owner".into(),
            name: "repo".into(),
        }
    }

    #[test]
    fn every_kind_of_github_address_names_the_repository() {
        for url in [
            "https://github.com/owner/repo",
            "https://github.com/owner/repo.git",
            "https://github.com/owner/repo/",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
        ] {
            assert_eq!(parse_github_repo(url), Some(repo()), "{url}");
        }
    }

    #[test]
    fn other_hosts_and_odd_paths_are_not_github_repositories() {
        for url in [
            "https://gitlab.com/owner/repo",
            "https://github.com.evil.example/owner/repo",
            "https://github.com/owner",
            "https://github.com/owner/repo/extra",
            "https://github.com/own er/repo",
            "/local/path/repo",
        ] {
            assert_eq!(parse_github_repo(url), None, "{url}");
        }
    }
}
