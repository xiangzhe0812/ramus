//! The current branch's pull request on GitHub: finding it, and the pages to open. No
//! extra permission is asked for: public repositories answer anyone (a signed-in token
//! just raises the rate limit), and a private one answers "not found", which shows
//! nothing. Every address opened is built here, never taken from GitHub's replies.

use serde::{Deserialize, de::DeserializeOwned};

use crate::github::{Endpoints, Secret, agent, describe};

/// Check conclusions that mean something went wrong.
const FAILED_CONCLUSIONS: [&str; 5] = [
    "failure",
    "timed_out",
    "cancelled",
    "action_required",
    "startup_failure",
];

/// A repository on github.com.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubRepo {
    pub owner: String,
    pub name: String,
}

/// An open pull request for the current branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub checks: Checks,
}

/// Where the pull request's checks stand, all taken together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    Passing,
    Failing,
    Running,
    /// The repository runs no checks.
    None,
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

/// GitHub's form for opening a pull request from `branch`.
pub fn compare_page(endpoints: &Endpoints, repo: &GithubRepo, branch: &str) -> String {
    format!(
        "{}/{}/{}/compare/{}?expand=1",
        endpoints.web,
        repo.owner,
        repo.name,
        encode(branch)
    )
}

/// The page of pull request `number`.
pub fn pull_page(endpoints: &Endpoints, repo: &GithubRepo, number: u64) -> String {
    format!(
        "{}/{}/{}/pull/{number}",
        endpoints.web, repo.owner, repo.name
    )
}

/// A branch name made safe for a URL. Slashes stay, as GitHub expects them in branch paths.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// The open pull request from `branch` of `repo` to anywhere, with its checks; `None` when
/// there is none, or when GitHub won't say (a private repository answers "not found").
/// Blocks, so run it on a background executor.
pub fn fetch_pull_request(
    endpoints: &Endpoints,
    token: Option<&Secret>,
    repo: &GithubRepo,
    branch: &str,
) -> Result<Option<PullRequest>, String> {
    #[derive(Deserialize)]
    struct Pull {
        number: u64,
        head: Head,
    }
    #[derive(Deserialize)]
    struct Head {
        sha: String,
    }
    #[derive(Default, Deserialize)]
    struct Runs {
        check_runs: Vec<CheckRun>,
    }

    let (owner, name) = (&repo.owner, &repo.name);
    let pulls = format!(
        "/repos/{owner}/{name}/pulls?head={owner}:{}&state=open&per_page=1",
        encode(branch)
    );
    let Some(pull) = get_json::<Vec<Pull>>(endpoints, token, &pulls)?
        .and_then(|open| open.into_iter().next())
    else {
        return Ok(None);
    };
    let sha = &pull.head.sha;
    // The hash goes into the next address, so it must be a plain hash.
    if sha.is_empty() || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("GitHub sent a strange commit id {sha:?}"));
    }
    let checks = format!("/repos/{owner}/{name}/commits/{sha}/check-runs?per_page=100");
    let runs: Runs = get_json(endpoints, token, &checks)?.unwrap_or_default();
    Ok(Some(PullRequest {
        number: pull.number,
        checks: summarize(&runs.check_runs),
    }))
}

/// GETs `path` from GitHub's API as JSON. `None` for "not found".
fn get_json<T: DeserializeOwned>(
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

/// One check run, as GitHub reports it.
#[derive(Clone, Debug, Deserialize)]
struct CheckRun {
    status: String,
    conclusion: Option<String>,
}

/// All of a commit's checks taken together: one failure fails, anything unfinished is
/// still running, otherwise they pass (skipped and neutral count as passing).
fn summarize(runs: &[CheckRun]) -> Checks {
    let failed = |run: &CheckRun| {
        run.conclusion
            .as_deref()
            .is_some_and(|conclusion| FAILED_CONCLUSIONS.contains(&conclusion))
    };
    if runs.is_empty() {
        Checks::None
    } else if runs.iter().any(failed) {
        Checks::Failing
    } else if runs.iter().any(|run| run.status != "completed") {
        Checks::Running
    } else {
        Checks::Passing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::{Secret, fake::Fake};

    fn repo() -> GithubRepo {
        GithubRepo {
            owner: "owner".into(),
            name: "repo".into(),
        }
    }

    fn endpoints(fake: &Fake) -> Endpoints {
        let address = fake.serve();
        Endpoints {
            web: address.clone(),
            api: address,
        }
    }

    const PULLS: &str = "/repos/owner/repo/pulls?head=owner:feature/login&state=open&per_page=1";
    const CHECKS: &str = "/repos/owner/repo/commits/abc123/check-runs?per_page=100";

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

    #[test]
    fn the_new_pull_request_page_is_built_for_the_branch() {
        let endpoints = Endpoints::default();

        assert_eq!(
            compare_page(&endpoints, &repo(), "feature/login"),
            "https://github.com/owner/repo/compare/feature/login?expand=1"
        );
        assert_eq!(
            compare_page(&endpoints, &repo(), "fix #1"),
            "https://github.com/owner/repo/compare/fix%20%231?expand=1"
        );
        assert_eq!(
            pull_page(&endpoints, &repo(), 42),
            "https://github.com/owner/repo/pull/42"
        );
    }

    #[test]
    fn no_open_pull_request_is_none_and_asks_nothing_more() {
        let fake = Fake::default().reply(PULLS, &["[]"]);

        let found = fetch_pull_request(&endpoints(&fake), None, &repo(), "feature/login");

        assert_eq!(found, Ok(None));
        assert_eq!(fake.count(CHECKS), 0);
    }

    #[test]
    fn an_open_pull_request_comes_with_its_checks() {
        let fake = Fake::default()
            .reply(PULLS, &[r#"[{"number":42,"head":{"sha":"abc123"}}]"#])
            .reply(
                CHECKS,
                &[r#"{"check_runs":[
                    {"status":"completed","conclusion":"success"},
                    {"status":"in_progress","conclusion":null}]}"#],
            );

        let found = fetch_pull_request(&endpoints(&fake), None, &repo(), "feature/login");

        assert_eq!(
            found,
            Ok(Some(PullRequest {
                number: 42,
                checks: Checks::Running
            }))
        );
    }

    #[test]
    fn a_signed_in_token_goes_along_and_none_is_sent_without_one() {
        let fake = Fake::default().reply(PULLS, &["[]"]);
        let endpoints = endpoints(&fake);

        fetch_pull_request(&endpoints, Some(&Secret("t0ken".into())), &repo(), "feature/login")
            .unwrap();
        fetch_pull_request(&endpoints, None, &repo(), "feature/login").unwrap();

        let requests = fake.requests.lock().unwrap();
        assert!(requests[0].1.contains("Bearer t0ken"));
        assert!(!requests[1].1.to_lowercase().contains("authorization"));
    }

    #[test]
    fn a_head_that_is_not_a_hash_is_refused() {
        let fake = Fake::default().reply(PULLS, &[r#"[{"number":42,"head":{"sha":"../../x"}}]"#]);

        let found = fetch_pull_request(&endpoints(&fake), None, &repo(), "feature/login");

        assert!(found.is_err());
    }

    fn run(status: &str, conclusion: Option<&str>) -> CheckRun {
        CheckRun {
            status: status.into(),
            conclusion: conclusion.map(Into::into),
        }
    }

    #[test]
    fn one_failure_fails_and_anything_unfinished_is_running() {
        let passed = run("completed", Some("success"));
        let skipped = run("completed", Some("skipped"));
        let failed = run("completed", Some("failure"));
        let queued = run("queued", None);

        assert_eq!(summarize(&[]), Checks::None);
        assert_eq!(summarize(&[passed.clone(), skipped]), Checks::Passing);
        assert_eq!(summarize(&[passed.clone(), queued.clone()]), Checks::Running);
        assert_eq!(summarize(&[queued, failed, passed]), Checks::Failing);
    }
}
