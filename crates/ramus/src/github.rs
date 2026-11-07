//! GitHub's OAuth device flow: the sign-in that needs no browser redirect and no client
//! secret. The app shows a short code, the user types it on github.com, and meanwhile the
//! app polls until GitHub hands over a token.
//!
//! All calls block, so run them on a background executor.

use std::{fmt, time::Duration};

use serde::Deserialize;

/// The OAuth app's client ID. Not a secret, but it must be one registered at
/// github.com/settings/developers with "Enable Device Flow" ticked. Until the app is
/// registered this is a placeholder and sign-in explains that instead of calling GitHub.
/// `RAMUS_GITHUB_CLIENT_ID` overrides it, for development.
pub const PLACEHOLDER_CLIENT_ID: &str = "REGISTER-A-GITHUB-OAUTH-APP";

/// What the token may do. Only identity for now; ask for more when a feature needs it.
pub const SCOPE: &str = "read:user";

pub fn client_id() -> String {
    std::env::var("RAMUS_GITHUB_CLIENT_ID")
        .ok()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| PLACEHOLDER_CLIENT_ID.to_owned())
}

/// Where GitHub lives. Tests point this at a local fake.
#[derive(Clone, Debug)]
pub struct Endpoints {
    /// `https://github.com`
    pub web: String,
    /// `https://api.github.com`
    pub api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            web: "https://github.com".into(),
            api: "https://api.github.com".into(),
        }
    }
}

impl Endpoints {
    /// The page where the user types their code. Built here, never taken from GitHub's
    /// reply, so a tampered response can't make the app open some other address.
    pub fn verification_page(&self) -> String {
        format!("{}/login/device", self.web)
    }
}

/// A token that never prints itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// What GitHub returns when a sign-in starts.
#[derive(Clone, Debug, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    /// The short code the user types, such as `WDJB-MJHT`.
    pub user_code: String,
    pub expires_in: u64,
    /// Seconds to wait between polls.
    pub interval: u64,
}

/// One answer to "has the user finished yet?".
#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    Pending,
    /// Ask less often: the new interval, in seconds.
    SlowDown(u64),
    Token(Secret),
    Expired,
    Denied,
}

fn agent() -> ureq::Agent {
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        // GitHub reports device-flow problems in a 200 body, and some as 4xx; read both.
        .http_status_as_error(false)
        // None of these endpoints redirects. Following one would send the bearer token
        // wherever a response pointed, so a redirect is just a failure.
        .max_redirects(0);
    // Tests talk to a local fake over plain http; the real app never leaves https.
    #[cfg(not(test))]
    let builder = builder.https_only(true);
    builder.build().into()
}

fn describe(error: ureq::Error) -> String {
    format!("couldn't reach GitHub: {error}")
}

pub fn request_device_code(endpoints: &Endpoints, client_id: &str) -> Result<DeviceCode, String> {
    let mut response = agent()
        .post(format!("{}/login/device/code", endpoints.web))
        .header("Accept", "application/json")
        .send_form([("client_id", client_id), ("scope", SCOPE)])
        .map_err(describe)?;
    let body: serde_json::Value = response.body_mut().read_json().map_err(describe)?;
    if let Some(error) = body.get("error").and_then(|e| e.as_str()) {
        let detail = body
            .get("error_description")
            .and_then(|d| d.as_str())
            .unwrap_or(error);
        return Err(format!("GitHub refused the sign-in: {detail}"));
    }
    let code: DeviceCode = serde_json::from_value(body)
        .map_err(|error| format!("unexpected reply from GitHub: {error}"))?;
    Ok(DeviceCode {
        // Never poll faster than GitHub asked, and never in a tight loop.
        interval: code.interval.max(1),
        ..code
    })
}

pub fn poll_token(
    endpoints: &Endpoints,
    client_id: &str,
    device_code: &str,
    current_interval: u64,
) -> Result<Poll, String> {
    let mut response = agent()
        .post(format!("{}/login/oauth/access_token", endpoints.web))
        .header("Accept", "application/json")
        .send_form([
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .map_err(describe)?;
    let body: serde_json::Value = response.body_mut().read_json().map_err(describe)?;
    if let Some(token) = body.get("access_token").and_then(|t| t.as_str()) {
        return Ok(Poll::Token(Secret(token.to_owned())));
    }
    match body.get("error").and_then(|e| e.as_str()) {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown(
            body.get("interval")
                .and_then(|i| i.as_u64())
                .unwrap_or(current_interval + 5),
        )),
        Some("expired_token") => Ok(Poll::Expired),
        Some("access_denied") => Ok(Poll::Denied),
        Some(other) => Err(format!(
            "GitHub said: {}",
            body.get("error_description")
                .and_then(|d| d.as_str())
                .unwrap_or(other)
        )),
        None => Err("unexpected reply from GitHub".into()),
    }
}

/// The account name the token belongs to.
pub fn fetch_login(endpoints: &Endpoints, token: &Secret) -> Result<String, String> {
    let mut response = agent()
        .get(format!("{}/user", endpoints.api))
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", format!("Bearer {}", token.0))
        .header("User-Agent", "Ramus")
        .call()
        .map_err(describe)?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    let user: User = response.body_mut().read_json().map_err(describe)?;
    Ok(user.login)
}

#[cfg(test)]
pub mod fake {
    //! A tiny local HTTP server that plays GitHub, so the flow can be tested end to end.

    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    type Replies = Arc<Mutex<Vec<(String, Vec<String>)>>>;

    /// Canned replies per path, consumed in order; the last one repeats.
    #[derive(Clone, Default)]
    pub struct Fake {
        replies: Replies,
        pub requests: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Fake {
        pub fn reply(self, path: &str, bodies: &[&str]) -> Self {
            self.replies.lock().unwrap().push((
                path.to_owned(),
                bodies.iter().map(|b| (*b).to_owned()).collect(),
            ));
            self
        }

        /// Starts serving on a free port and returns its base address.
        pub fn serve(&self) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let this = self.clone();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    let this = this.clone();
                    thread::spawn(move || {
                        let mut buffer = [0u8; 8192];
                        let mut request = Vec::new();
                        // Read headers, then as much body as was announced.
                        loop {
                            let n = stream.read(&mut buffer).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            request.extend_from_slice(&buffer[..n]);
                            let text = String::from_utf8_lossy(&request).to_string();
                            if let Some(split) = text.find("\r\n\r\n") {
                                let length = text
                                    .lines()
                                    .find_map(|l| {
                                        l.to_lowercase()
                                            .strip_prefix("content-length:")
                                            .and_then(|v| v.trim().parse::<usize>().ok())
                                    })
                                    .unwrap_or(0);
                                if request.len() >= split + 4 + length {
                                    break;
                                }
                            }
                        }
                        let text = String::from_utf8_lossy(&request).to_string();
                        let path = text
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or_default()
                            .to_owned();
                        this.requests.lock().unwrap().push((path.clone(), text));
                        let body = {
                            let mut replies = this.replies.lock().unwrap();
                            match replies.iter_mut().find(|(p, _)| *p == path) {
                                Some((_, bodies)) if bodies.len() > 1 => bodies.remove(0),
                                Some((_, bodies)) => bodies.first().cloned().unwrap_or_default(),
                                None => "{}".to_owned(),
                            }
                        };
                        if let Some(target) = body.strip_prefix("REDIRECT ") {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                            return;
                        }
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                    });
                }
            });
            address
        }

        /// How many times `path` was asked for.
        pub fn count(&self, path: &str) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p == path)
                .count()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fake::Fake, *};

    fn endpoints(fake: &Fake) -> Endpoints {
        let address = fake.serve();
        Endpoints {
            web: address.clone(),
            api: address,
        }
    }

    const CODE: &str = r#"{"device_code":"dev123","user_code":"WDJB-MJHT","verification_uri":"https://evil.example/login","expires_in":900,"interval":5}"#;

    #[test]
    fn a_sign_in_starts_with_a_code_for_the_user() {
        let fake = Fake::default().reply("/login/device/code", &[CODE]);
        let code = request_device_code(&endpoints(&fake), "client-1").unwrap();
        assert_eq!(code.user_code, "WDJB-MJHT");
        assert_eq!(code.device_code, "dev123");
        assert_eq!(code.interval, 5);
        let sent = fake.requests.lock().unwrap()[0].1.clone();
        assert!(sent.contains("client_id=client-1"), "{sent}");
        assert!(sent.contains("scope=read%3Auser"), "{sent}");
    }

    #[test]
    fn the_page_to_open_is_ours_not_the_one_github_names() {
        let endpoints = Endpoints::default();
        assert_eq!(
            endpoints.verification_page(),
            "https://github.com/login/device"
        );
    }

    #[test]
    fn a_refused_sign_in_says_why() {
        let fake = Fake::default().reply(
            "/login/device/code",
            &[r#"{"error":"device_flow_disabled","error_description":"Device flow must be enabled"}"#],
        );
        let error = request_device_code(&endpoints(&fake), "c").unwrap_err();
        assert!(error.contains("Device flow must be enabled"), "{error}");
    }

    #[test]
    fn a_zero_interval_is_raised_to_one_second() {
        let fake = Fake::default().reply(
            "/login/device/code",
            &[r#"{"device_code":"d","user_code":"U","expires_in":900,"interval":0}"#],
        );
        assert_eq!(
            request_device_code(&endpoints(&fake), "c")
                .unwrap()
                .interval,
            1
        );
    }

    #[test]
    fn polling_understands_every_answer_github_gives() {
        let cases = [
            (r#"{"error":"authorization_pending"}"#, Poll::Pending),
            (r#"{"error":"slow_down","interval":10}"#, Poll::SlowDown(10)),
            (r#"{"error":"slow_down"}"#, Poll::SlowDown(10)),
            (r#"{"error":"expired_token"}"#, Poll::Expired),
            (r#"{"error":"access_denied"}"#, Poll::Denied),
            (
                r#"{"access_token":"gho_abc","token_type":"bearer"}"#,
                Poll::Token(Secret("gho_abc".into())),
            ),
        ];
        for (body, expected) in cases {
            let fake = Fake::default().reply("/login/oauth/access_token", &[body]);
            let got = poll_token(&endpoints(&fake), "c", "dev123", 5).unwrap();
            assert_eq!(got, expected, "{body}");
        }
    }

    #[test]
    fn an_unknown_error_is_reported_not_retried() {
        let fake = Fake::default().reply(
            "/login/oauth/access_token",
            &[r#"{"error":"incorrect_client_credentials","error_description":"bad client"}"#],
        );
        let error = poll_token(&endpoints(&fake), "c", "d", 5).unwrap_err();
        assert!(error.contains("bad client"), "{error}");
    }

    #[test]
    fn the_token_is_sent_as_a_bearer_and_names_the_account() {
        let fake = Fake::default().reply("/user", &[r#"{"login":"octocat"}"#]);
        let login = fetch_login(&endpoints(&fake), &Secret("gho_abc".into())).unwrap();
        assert_eq!(login, "octocat");
        let sent = fake.requests.lock().unwrap()[0].1.to_lowercase();
        assert!(sent.contains("authorization: bearer gho_abc"), "{sent}");
    }

    #[test]
    fn a_redirect_never_carries_the_token_to_another_server() {
        let elsewhere = Fake::default().reply("/stolen", &[r#"{"login":"mallory"}"#]);
        let elsewhere_address = elsewhere.serve();
        let github =
            Fake::default().reply("/user", &[&format!("REDIRECT {elsewhere_address}/stolen")]);
        let result = fetch_login(&endpoints(&github), &Secret("gho_abc".into()));

        assert!(
            result.is_err(),
            "a redirect must not be followed: {result:?}"
        );
        assert_eq!(
            elsewhere.count("/stolen"),
            0,
            "the redirect target was contacted"
        );
        let seen = elsewhere.requests.lock().unwrap().len();
        assert_eq!(seen, 0);
    }

    #[test]
    fn a_secret_never_prints() {
        assert!(!format!("{:?}", Poll::Token(Secret("gho_abc".into()))).contains("gho_abc"));
    }

    #[test]
    fn an_unreachable_server_is_an_error_not_a_hang() {
        let endpoints = Endpoints {
            web: "http://127.0.0.1:1".into(),
            api: "http://127.0.0.1:1".into(),
        };
        let error = request_device_code(&endpoints, "c").unwrap_err();
        assert!(error.contains("couldn't reach GitHub"), "{error}");
    }
}
