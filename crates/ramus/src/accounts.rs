//! Who is signed in to GitHub, and the sign-in in progress.

use std::{sync::Arc, time::Duration};

use gpui::{Context, Task};
use serde::{Deserialize, Serialize};

use crate::{
    github::{self, Endpoints, Poll, Secret},
    secrets::SecretStore,
};

/// What is kept in the keychain: the token, and whose it is so the name can be shown
/// without asking GitHub.
#[derive(Serialize, Deserialize)]
struct Saved {
    login: String,
    token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub login: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flow {
    Idle,
    /// Asking GitHub for a code.
    Requesting,
    /// The user has a code to type on github.com.
    Waiting {
        user_code: String,
    },
    Failed(String),
}

pub struct AccountsState {
    store: Arc<dyn SecretStore>,
    endpoints: Endpoints,
    client_id: String,
    account: Option<Account>,
    /// The signed-in token, kept from the keychain read at launch or the sign-in, so
    /// GitHub requests don't read the keychain each time.
    token: Option<Secret>,
    flow: Flow,
    /// The sign-in in progress; dropping it stops the polling.
    task: Option<Task<()>>,
}

impl AccountsState {
    pub fn new(store: Arc<dyn SecretStore>, endpoints: Endpoints, client_id: String) -> Self {
        let mut flow = Flow::Idle;
        let saved = match store.get() {
            Ok(Some(text)) => serde_json::from_str::<Saved>(&text).ok(),
            Ok(None) => None,
            Err(error) => {
                flow = Flow::Failed(error);
                None
            }
        };
        let (account, token) = match saved {
            Some(saved) => (
                Some(Account { login: saved.login }),
                Some(Secret(saved.token)),
            ),
            None => (None, None),
        };
        Self {
            store,
            endpoints,
            client_id,
            account,
            token,
            flow,
            task: None,
        }
    }

    pub fn account(&self) -> Option<&Account> {
        self.account.as_ref()
    }

    /// The signed-in token, for GitHub requests.
    pub fn token(&self) -> Option<&Secret> {
        self.token.as_ref()
    }

    pub fn flow(&self) -> &Flow {
        &self.flow
    }

    pub fn verification_page(&self) -> String {
        self.endpoints.verification_page()
    }

    /// Starts the device flow: gets a code, then polls until the user approves it, it
    /// expires, or they cancel.
    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        if matches!(self.flow, Flow::Requesting | Flow::Waiting { .. }) {
            return;
        }
        if self.client_id.trim().is_empty() {
            self.flow = Flow::Failed(
                "No GitHub OAuth client ID is configured. Start Ramus with \
                 RAMUS_GITHUB_CLIENT_ID set to the client ID of an OAuth app that has Device \
                 Flow enabled."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.flow = Flow::Requesting;
        let (endpoints, client_id, store) = (
            self.endpoints.clone(),
            self.client_id.clone(),
            self.store.clone(),
        );
        self.task = Some(cx.spawn(async move |this, cx| {
            let outcome = run_flow(&this, cx, endpoints, client_id, store).await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok((account, token)) => {
                        this.account = Some(account);
                        this.token = Some(token);
                        this.flow = Flow::Idle;
                    }
                    Err(message) => this.flow = Flow::Failed(message),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Stops a sign-in in progress.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        if matches!(self.flow, Flow::Requesting | Flow::Waiting { .. }) {
            self.flow = Flow::Idle;
        }
        cx.notify();
    }

    /// Forgets the sign-in on this Mac. GitHub is not told, so the token itself stays
    /// valid until revoked at github.com/settings/applications.
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        match self.store.delete() {
            Ok(()) => {
                self.account = None;
                self.token = None;
                self.flow = Flow::Idle;
            }
            // Still signed in: say so rather than pretend.
            Err(error) => self.flow = Flow::Failed(error),
        }
        cx.notify();
    }
}

/// The whole conversation with GitHub. Blocking calls run on the background executor so
/// the window stays responsive.
async fn run_flow(
    this: &gpui::WeakEntity<AccountsState>,
    cx: &mut gpui::AsyncApp,
    endpoints: Endpoints,
    client_id: String,
    store: Arc<dyn SecretStore>,
) -> Result<(Account, Secret), String> {
    let code = {
        let (endpoints, client_id) = (endpoints.clone(), client_id.clone());
        cx.background_executor()
            .spawn(async move { github::request_device_code(&endpoints, &client_id) })
            .await?
    };
    this.update(cx, |this, cx| {
        this.flow = Flow::Waiting {
            user_code: code.user_code.clone(),
        };
        cx.notify();
    })
    .map_err(|_| "closed".to_owned())?;

    let (mut interval, mut waited) = (code.interval, 0u64);
    let token = loop {
        cx.background_executor()
            .timer(Duration::from_secs(interval))
            .await;
        waited += interval;
        if waited >= code.expires_in {
            return Err("The code expired before it was used. Try again.".into());
        }
        let (endpoints, client_id, device_code) = (
            endpoints.clone(),
            client_id.clone(),
            code.device_code.clone(),
        );
        let current = interval;
        let answer = cx
            .background_executor()
            .spawn(async move { github::poll_token(&endpoints, &client_id, &device_code, current) })
            .await?;
        match answer {
            Poll::Pending => {}
            Poll::SlowDown(seconds) => interval = seconds,
            Poll::Token(token) => break token,
            Poll::Expired => return Err("The code expired before it was used. Try again.".into()),
            Poll::Denied => return Err("Sign-in was declined on GitHub.".into()),
        }
    };

    cx.background_executor()
        .spawn(async move { finish(&endpoints, token, store.as_ref()) })
        .await
}

/// Learns whose token it is, and only then keeps it.
fn finish(
    endpoints: &Endpoints,
    token: Secret,
    store: &dyn SecretStore,
) -> Result<(Account, Secret), String> {
    let login = github::fetch_login(endpoints, &token)?;
    let saved = serde_json::to_string(&Saved {
        login: login.clone(),
        token: token.0.clone(),
    })
    .map_err(|error| error.to_string())?;
    store.set(&saved)?;
    Ok((Account { login }, token))
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::*;
    use crate::{github::fake::Fake, secrets::MemoryStore};

    const CODE: &str =
        r#"{"device_code":"dev","user_code":"WDJB-MJHT","expires_in":900,"interval":5}"#;
    const PENDING: &str = r#"{"error":"authorization_pending"}"#;
    const TOKEN: &str = r#"{"access_token":"gho_secret","token_type":"bearer"}"#;

    fn state(
        cx: &mut TestAppContext,
        fake: &Fake,
        store: Arc<MemoryStore>,
    ) -> gpui::Entity<AccountsState> {
        let address = fake.serve();
        let endpoints = Endpoints {
            web: address.clone(),
            api: address,
        };
        cx.new(|_| AccountsState::new(store, endpoints, "client-1".into()))
    }

    /// Steps the test clock in whole seconds, letting real network threads answer between
    /// steps, until `done` or the budget runs out.
    fn run_until(
        cx: &mut TestAppContext,
        state: &gpui::Entity<AccountsState>,
        done: impl Fn(&AccountsState) -> bool,
    ) {
        for _ in 0..400 {
            cx.executor().advance_clock(Duration::from_secs(1));
            cx.executor().run_until_parked();
            std::thread::sleep(Duration::from_millis(5));
            if state.read_with(cx, |s, _| done(s)) {
                return;
            }
        }
        panic!(
            "never got there: {:?}",
            state.read_with(cx, |s, _| s.flow.clone())
        );
    }

    #[gpui::test]
    fn approving_the_code_signs_in_and_keeps_the_token_in_the_store(cx: &mut TestAppContext) {
        let fake = Fake::default()
            .reply("/login/device/code", &[CODE])
            .reply("/login/oauth/access_token", &[PENDING, PENDING, TOKEN])
            .reply("/user", &[r#"{"login":"octocat"}"#]);
        let store = Arc::new(MemoryStore::default());
        let state = state(cx, &fake, store.clone());

        state.update(cx, |s, cx| s.sign_in(cx));
        run_until(cx, &state, |s| matches!(s.flow, Flow::Waiting { .. }));
        assert_eq!(
            state.read_with(cx, |s, _| s.flow.clone()),
            Flow::Waiting {
                user_code: "WDJB-MJHT".into()
            }
        );

        run_until(cx, &state, |s| s.account.is_some());
        state.read_with(cx, |s, _| {
            assert_eq!(s.account().unwrap().login, "octocat");
            assert_eq!(s.flow, Flow::Idle);
            assert_eq!(s.token(), Some(&Secret("gho_secret".into())));
        });
        let saved = store.get().unwrap().unwrap();
        assert!(
            saved.contains("gho_secret") && saved.contains("octocat"),
            "{saved}"
        );
    }

    #[gpui::test]
    fn a_saved_sign_in_is_there_on_the_next_launch(cx: &mut TestAppContext) {
        let store = Arc::new(MemoryStore::default());
        store
            .set(r#"{"login":"octocat","token":"gho_secret"}"#)
            .unwrap();
        let state = state(cx, &Fake::default(), store);
        state.read_with(cx, |s, _| {
            assert_eq!(s.account().map(|a| a.login.as_str()), Some("octocat"));
            assert_eq!(s.token(), Some(&Secret("gho_secret".into())));
        });

        state.update(cx, |s, cx| s.sign_out(cx));

        assert_eq!(state.read_with(cx, |s, _| s.token().cloned()), None);
    }

    #[gpui::test]
    fn a_garbled_saved_value_means_signed_out_not_a_crash(cx: &mut TestAppContext) {
        let store = Arc::new(MemoryStore::default());
        store.set("not json").unwrap();
        let state = state(cx, &Fake::default(), store);
        state.read_with(cx, |s, _| assert!(s.account().is_none()));
    }

    #[gpui::test]
    fn declining_on_github_fails_with_a_reason_and_saves_nothing(cx: &mut TestAppContext) {
        let fake = Fake::default().reply("/login/device/code", &[CODE]).reply(
            "/login/oauth/access_token",
            &[PENDING, r#"{"error":"access_denied"}"#],
        );
        let store = Arc::new(MemoryStore::default());
        let state = state(cx, &fake, store.clone());
        state.update(cx, |s, cx| s.sign_in(cx));
        run_until(cx, &state, |s| matches!(s.flow, Flow::Failed(_)));
        state.read_with(cx, |s, _| {
            assert!(matches!(&s.flow, Flow::Failed(m) if m.contains("declined")));
            assert!(s.account().is_none());
        });
        assert_eq!(store.get().unwrap(), None);
    }

    #[gpui::test]
    fn a_token_whose_owner_cannot_be_learned_is_not_kept(cx: &mut TestAppContext) {
        let fake = Fake::default()
            .reply("/login/device/code", &[CODE])
            .reply("/login/oauth/access_token", &[TOKEN])
            .reply("/user", &[r#"{"message":"Bad credentials"}"#]);
        let store = Arc::new(MemoryStore::default());
        let state = state(cx, &fake, store.clone());
        state.update(cx, |s, cx| s.sign_in(cx));
        run_until(cx, &state, |s| matches!(s.flow, Flow::Failed(_)));
        assert_eq!(store.get().unwrap(), None);
        state.read_with(cx, |s, _| assert!(s.account().is_none()));
    }

    #[gpui::test]
    fn slow_down_makes_it_ask_less_often(cx: &mut TestAppContext) {
        let fake = Fake::default().reply("/login/device/code", &[CODE]).reply(
            "/login/oauth/access_token",
            &[r#"{"error":"slow_down","interval":30}"#],
        );
        let state = state(cx, &fake, Arc::new(MemoryStore::default()));
        state.update(cx, |s, cx| s.sign_in(cx));
        run_until(cx, &state, |s| matches!(s.flow, Flow::Waiting { .. }));
        // First poll after 5s gets slow_down; the next must wait 30s, not 5.
        for _ in 0..6 {
            cx.executor().advance_clock(Duration::from_secs(1));
            cx.executor().run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        let after_first = fake.count("/login/oauth/access_token");
        for _ in 0..20 {
            cx.executor().advance_clock(Duration::from_secs(1));
            cx.executor().run_until_parked();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(after_first, 1);
        assert_eq!(
            fake.count("/login/oauth/access_token"),
            1,
            "asked again too soon"
        );
    }

    #[gpui::test]
    fn cancelling_stops_the_polling(cx: &mut TestAppContext) {
        let fake = Fake::default()
            .reply("/login/device/code", &[CODE])
            .reply("/login/oauth/access_token", &[PENDING]);
        let state = state(cx, &fake, Arc::new(MemoryStore::default()));
        state.update(cx, |s, cx| s.sign_in(cx));
        run_until(cx, &state, |s| matches!(s.flow, Flow::Waiting { .. }));
        state.update(cx, |s, cx| s.cancel(cx));
        for _ in 0..30 {
            cx.executor().advance_clock(Duration::from_secs(1));
            cx.executor().run_until_parked();
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.count("/login/oauth/access_token"), 0);
        state.read_with(cx, |s, _| assert_eq!(s.flow, Flow::Idle));
    }

    #[gpui::test]
    fn signing_out_removes_the_saved_token(cx: &mut TestAppContext) {
        let store = Arc::new(MemoryStore::default());
        store
            .set(r#"{"login":"octocat","token":"gho_secret"}"#)
            .unwrap();
        let state = state(cx, &Fake::default(), store.clone());
        state.update(cx, |s, cx| s.sign_out(cx));
        state.read_with(cx, |s, _| assert!(s.account().is_none()));
        assert_eq!(store.get().unwrap(), None);
    }

    #[gpui::test]
    fn without_a_client_id_it_explains_instead_of_calling_github(cx: &mut TestAppContext) {
        let fake = Fake::default();
        let address = fake.serve();
        let state = cx.new(|_| {
            AccountsState::new(
                Arc::new(MemoryStore::default()),
                Endpoints {
                    web: address.clone(),
                    api: address,
                },
                String::new(),
            )
        });
        state.update(cx, |s, cx| s.sign_in(cx));
        state.read_with(cx, |s, _| {
            assert!(matches!(&s.flow, Flow::Failed(m) if m.contains("RAMUS_GITHUB_CLIENT_ID")))
        });
        assert_eq!(fake.requests.lock().unwrap().len(), 0);
    }
}
