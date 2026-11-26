//! The current branch's pull request on GitHub: finding it, and the pages to open. No
//! extra permission is asked for: public repositories answer anyone (a signed-in token
//! just raises the rate limit), and a private one answers "not found", which shows
//! nothing. Every address opened is built here, never taken from GitHub's replies.

use std::{path::PathBuf, time::Duration};

use git_core::Head;
use gpui::{Context, Entity, Subscription, Task};
use serde::Deserialize;

use crate::{
    accounts::AccountsState,
    github::{Endpoints, Secret},
    github_repos::{GithubRepo, get_json, parse_github_repo},
    repo_state::RepoState,
};

/// How often to look again while the app is open, as the background fetch does.
const REFRESH_EVERY: Duration = Duration::from_secs(300);

/// Check conclusions that mean something went wrong.
const FAILED_CONCLUSIONS: [&str; 5] = [
    "failure",
    "timed_out",
    "cancelled",
    "action_required",
    "startup_failure",
];

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

/// What the title bar and the palette offer for the current branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrLink {
    /// The branch is pushed and has no open pull request.
    Create { url: String },
    Open {
        number: u64,
        checks: Checks,
        url: String,
    },
}

/// What a lookup found for the current branch.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Found {
    repo: GithubRepo,
    /// The branch's name on `origin`.
    branch: String,
    /// The branch has an upstream on `origin`.
    pushed: bool,
    pull: Option<PullRequest>,
}

/// What `found` offers: nothing until the branch is pushed, then a way to create its pull
/// request, or the open one.
fn offer(found: &Found, endpoints: &Endpoints) -> Option<PrLink> {
    match found.pull {
        Some(pull) => Some(PrLink::Open {
            number: pull.number,
            checks: pull.checks,
            url: pull_page(endpoints, &found.repo, pull.number),
        }),
        None if found.pushed => Some(PrLink::Create {
            url: compare_page(endpoints, &found.repo, &found.branch),
        }),
        None => None,
    }
}

/// What a lookup is for: the repository, branch, commit, upstream and commits ahead. Any
/// change (a switch, a commit, a push) means look again.
type Target = (PathBuf, String, Option<String>, Option<String>, u32);

/// The current branch's pull request, kept up to date: looked up when the branch, its
/// commit or its upstream changes, and every few minutes. Failures are only logged; the
/// last answer stands.
pub struct PullRequestState {
    repo_state: Entity<RepoState>,
    accounts: Entity<AccountsState>,
    endpoints: Endpoints,
    looked_up: Option<Target>,
    found: Option<Found>,
    lookup: Option<Task<()>>,
    _observe: Subscription,
    _refresh: Task<()>,
}

impl PullRequestState {
    pub fn new(
        repo_state: Entity<RepoState>,
        accounts: Entity<AccountsState>,
        endpoints: Endpoints,
        cx: &mut Context<Self>,
    ) -> Self {
        let _observe = cx.observe(&repo_state, |this, _, cx| this.refresh_if_changed(cx));
        let _refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH_EVERY).await;
                if this.update(cx, |this, cx| this.look_up(cx)).is_err() {
                    break;
                }
            }
        });
        let mut state = Self {
            repo_state,
            accounts,
            endpoints,
            looked_up: None,
            found: None,
            lookup: None,
            _observe,
            _refresh,
        };
        state.refresh_if_changed(cx);
        state
    }

    /// What to offer for the current branch, if anything.
    pub fn link(&self) -> Option<PrLink> {
        self.found
            .as_ref()
            .and_then(|found| offer(found, &self.endpoints))
    }

    fn target(&self, cx: &Context<Self>) -> Option<Target> {
        let state = self.repo_state.read(cx);
        let (repo, status) = state.repo().zip(state.status())?;
        let Head::Branch(branch) = &status.branch.head else {
            return None;
        };
        Some((
            repo.root().to_owned(),
            branch.clone(),
            status.branch.oid.clone(),
            status.branch.upstream.clone(),
            status.branch.ahead,
        ))
    }

    fn refresh_if_changed(&mut self, cx: &mut Context<Self>) {
        let target = self.target(cx);
        if target == self.looked_up {
            return;
        }
        // Another branch or repository: don't show the last one's pull request meanwhile.
        let place = |target: &Option<Target>| target.as_ref().map(|t| (t.0.clone(), t.1.clone()));
        if place(&target) != place(&self.looked_up) {
            self.found = None;
        }
        self.look_up(cx);
    }

    fn look_up(&mut self, cx: &mut Context<Self>) {
        let target = self.target(cx);
        self.looked_up = target.clone();
        let (Some((_, branch, _, upstream, _)), Some(repo)) =
            (target.clone(), self.repo_state.read(cx).repo().cloned())
        else {
            self.found = None;
            self.lookup = None;
            cx.notify();
            return;
        };
        // The branch's name on origin, which is what GitHub knows it by.
        let remote_branch = upstream
            .as_deref()
            .and_then(|upstream| upstream.strip_prefix("origin/"))
            .map(str::to_owned);
        let token = self.accounts.read(cx).token().cloned();
        let endpoints = self.endpoints.clone();
        self.lookup = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let url = repo.remote_url("origin").map_err(|e| e.to_string())?;
                    let Some(github) = url.as_deref().and_then(parse_github_repo) else {
                        return Ok(None);
                    };
                    let Some(remote_branch) = remote_branch else {
                        return Ok(Some(Found {
                            repo: github,
                            branch,
                            pushed: false,
                            pull: None,
                        }));
                    };
                    let pull =
                        fetch_pull_request(&endpoints, token.as_ref(), &github, &remote_branch)?;
                    Ok::<_, String>(Some(Found {
                        repo: github,
                        branch: remote_branch,
                        pushed: true,
                        pull,
                    }))
                })
                .await;
            this.update(cx, |this, cx| {
                // The branch moved on while GitHub was being asked.
                if this.looked_up != target {
                    return;
                }
                match result {
                    Ok(found) => this.found = found,
                    Err(error) => eprintln!("[pull request] {error}"),
                }
                cx.notify();
            })
            .ok();
        }));
    }
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
    let Some(pull) =
        get_json::<Vec<Pull>>(endpoints, token, &pulls)?.and_then(|open| open.into_iter().next())
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

    fn found(pushed: bool, pull: Option<PullRequest>) -> Found {
        Found {
            repo: repo(),
            branch: "feature/login".into(),
            pushed,
            pull,
        }
    }

    #[test]
    fn nothing_is_offered_until_the_branch_is_pushed() {
        assert_eq!(offer(&found(false, None), &Endpoints::default()), None);
    }

    #[test]
    fn a_pushed_branch_without_a_pull_request_offers_to_create_one() {
        assert_eq!(
            offer(&found(true, None), &Endpoints::default()),
            Some(PrLink::Create {
                url: "https://github.com/owner/repo/compare/feature/login?expand=1".into()
            })
        );
    }

    #[test]
    fn an_open_pull_request_is_offered_with_its_checks() {
        let pull = PullRequest {
            number: 42,
            checks: Checks::Failing,
        };

        assert_eq!(
            offer(&found(true, Some(pull)), &Endpoints::default()),
            Some(PrLink::Open {
                number: 42,
                checks: Checks::Failing,
                url: "https://github.com/owner/repo/pull/42".into()
            })
        );
    }

    #[gpui::test]
    async fn the_pushed_branchs_pull_request_is_looked_up(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext;
        let dir = tempfile::TempDir::new().unwrap();
        let git = |args: &[&str]| crate::testutil::git(dir.path(), args);
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&["checkout", "-q", "-b", "feature/login"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ]);
        git(&["update-ref", "refs/remotes/origin/feature/login", "HEAD"]);
        git(&["branch", "-q", "--set-upstream-to=origin/feature/login"]);
        let fake = Fake::default()
            .reply(PULLS, &[r#"[{"number":42,"head":{"sha":"abc123"}}]"#])
            .reply(
                CHECKS,
                &[r#"{"check_runs":[{"status":"completed","conclusion":"success"}]}"#],
            );
        let endpoints = endpoints(&fake);
        let repo_state = cx.new(|_| crate::repo_state::RepoState::default());
        repo_state.update(cx, |state, cx| state.open(dir.path().to_owned(), cx));
        let accounts = cx.new(|_| {
            crate::accounts::AccountsState::new(
                std::sync::Arc::new(crate::secrets::MemoryStore::default()),
                endpoints.clone(),
                String::new(),
            )
        });

        let state = cx.new(|cx| PullRequestState::new(repo_state, accounts, endpoints, cx));
        for _ in 0..400 {
            cx.executor().run_until_parked();
            if state.read_with(cx, |state, _| {
                state
                    .link()
                    .is_some_and(|l| matches!(l, PrLink::Open { .. }))
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let link = state.read_with(cx, |state, _| state.link());
        assert!(
            matches!(
                link,
                Some(PrLink::Open {
                    number: 42,
                    checks: Checks::Passing,
                    ..
                })
            ),
            "{link:?}"
        );
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

        fetch_pull_request(
            &endpoints,
            Some(&Secret("t0ken".into())),
            &repo(),
            "feature/login",
        )
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
        assert_eq!(
            summarize(&[passed.clone(), queued.clone()]),
            Checks::Running
        );
        assert_eq!(summarize(&[queued, failed, passed]), Checks::Failing);
    }
}
