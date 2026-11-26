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
    GithubRepo::new(owner, name)
}

impl GithubRepo {
    /// A repository named `owner/name`, if both are plain GitHub names: letters, digits,
    /// `-`, `.` and `_`, and not `.` or `..`. Anything else could make a built address
    /// point somewhere else.
    fn new(owner: &str, name: &str) -> Option<Self> {
        let plain = |part: &str| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
        };
        (plain(owner) && plain(name)).then(|| Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// `owner/name`, as GitHub shows it.
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// The address to clone `repo` from, built from its checked name.
pub fn clone_address(endpoints: &Endpoints, repo: &GithubRepo) -> String {
    format!("{}/{}/{}.git", endpoints.web, repo.owner, repo.name)
}

/// The repositories the signed-in user owns, collaborates on, or has through an
/// organisation, most recently pushed first (at most 100). With the `read:user` token
/// GitHub lists only public ones. Blocks, so run it on a background executor.
pub fn fetch_my_repositories(
    endpoints: &Endpoints,
    token: &Secret,
) -> Result<Vec<GithubRepo>, String> {
    #[derive(serde::Deserialize)]
    struct Listed {
        name: String,
        owner: Owner,
    }
    #[derive(serde::Deserialize)]
    struct Owner {
        login: String,
    }
    let path = "/user/repos?per_page=100&sort=pushed&affiliation=owner,collaborator,organization_member";
    let listed: Vec<Listed> = get_json(endpoints, Some(token), path)?.unwrap_or_default();
    Ok(listed
        .iter()
        .filter_map(|repo| GithubRepo::new(&repo.owner.login, &repo.name))
        .collect())
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
    use crate::github::fake::Fake;

    const MINE: &str = "/user/repos?per_page=100&sort=pushed&affiliation=owner,collaborator,organization_member";

    fn endpoints(fake: &Fake) -> Endpoints {
        let address = fake.serve();
        Endpoints {
            web: address.clone(),
            api: address,
        }
    }

    #[test]
    fn my_repositories_come_back_in_githubs_order_with_odd_names_left_out() {
        let fake = Fake::default().reply(
            MINE,
            &[r#"[
                {"name":"ramus","owner":{"login":"alex"},"clone_url":"https://evil.example/x.git"},
                {"name":"site","owner":{"login":"acme"}},
                {"name":"../escape","owner":{"login":"alex"}}
            ]"#],
        );

        let listed = fetch_my_repositories(&endpoints(&fake), &Secret("t0ken".into())).unwrap();

        let names: Vec<_> = listed.iter().map(GithubRepo::full_name).collect();
        assert_eq!(names, ["alex/ramus", "acme/site"]);
        assert!(fake.requests.lock().unwrap()[0].1.contains("Bearer t0ken"));
    }

    #[test]
    fn the_clone_address_is_built_from_the_name_not_taken_from_github() {
        let repo = GithubRepo {
            owner: "alex".into(),
            name: "ramus".into(),
        };

        assert_eq!(
            clone_address(&Endpoints::default(), &repo),
            "https://github.com/alex/ramus.git"
        );
    }

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
