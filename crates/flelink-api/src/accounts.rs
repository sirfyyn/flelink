//! Accounts connected with a click (Nr. 942). Hugging Face and Google answer on the Studio's own callback (PKCE, no secret);
//! GitHub shows a short code to enter there (device flow), and its token then lands where a pasted one does (github.rs).
//! The client ids are FLE Link's apps at these providers: they are public, nothing here is a secret.
use crate::github::GitHub;
use crate::oauth::{KnownProvider, OAuthManager};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const GITHUB_CLIENT: &str = "Ov23liNkFgE3LAJlLT2T";
const GITHUB_SCOPE: &str = "repo read:org";

pub const HUGGING_FACE: KnownProvider = KnownProvider {
    name: "Hugging Face",
    url: "https://huggingface.co",
    auth_endpoint: "https://huggingface.co/oauth/authorize",
    token_endpoint: "https://huggingface.co/oauth/token",
    revocation_endpoint: "",
    client_id: "ec189784-c8ff-4a25-b5b1-7dc7c889787a",
    secret_env: "",
    built_secret: None,
    scope: "openid profile read-repos gated-repos",
    extra: &[],
};

pub const GOOGLE: KnownProvider = KnownProvider {
    name: "Google",
    url: "https://accounts.google.com",
    auth_endpoint: "https://accounts.google.com/o/oauth2/v2/auth",
    token_endpoint: "https://oauth2.googleapis.com/token",
    revocation_endpoint: "https://oauth2.googleapis.com/revoke",
    // its own Google project "FLE Link", apart from FYOS, so the review covers only this one scope
    client_id: "1049197583997-ldsejumqjp6gav74e7fuq0vp12e0rbjq.apps.googleusercontent.com",
    secret_env: "FLE_GOOGLE_CLIENT_SECRET",
    built_secret: option_env!("FLE_GOOGLE_CLIENT_SECRET"),
    // only reading: Google's review asks for the narrowest scope that does the job (Nr. 951)
    scope: "https://www.googleapis.com/auth/calendar.events.readonly",
    // without these Google gives no refresh token, and the sign-in would end after an hour
    extra: &[("access_type", "offline"), ("prompt", "consent")],
};

fn provider(id: &str) -> Option<&'static KnownProvider> {
    match id {
        "huggingface" => Some(&HUGGING_FACE),
        "google" => Some(&GOOGLE),
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
enum Poll {
    Wait,
    Slower,
    Done(String),
    Failed(String),
}

/// GitHub's answer while the person enters the code.
fn read_poll(doc: &Value) -> Poll {
    if let Some(t) = doc["access_token"].as_str().filter(|t| !t.is_empty()) {
        return Poll::Done(t.to_string());
    }
    match doc["error"].as_str().unwrap_or("") {
        "authorization_pending" => Poll::Wait,
        "slow_down" => Poll::Slower,
        "expired_token" => Poll::Failed("The code expired. Start again.".into()),
        "access_denied" => Poll::Failed("The sign-in was declined on GitHub.".into()),
        other => Poll::Failed(format!("GitHub refused the sign-in ({other}).")),
    }
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    let answer = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build()
        .post(url)
        .set("Accept", "application/json")
        .set("User-Agent", concat!("flelink/", env!("CARGO_PKG_VERSION")))
        .send_form(form);
    let text = match answer {
        Ok(r) => r.into_string(),
        Err(ureq::Error::Status(_, r)) => r.into_string(),
        Err(e) => return Err(format!("GitHub is not reachable: {e}")),
    }
    .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

#[derive(Clone)]
struct Device {
    started: Instant,
    user_code: String,
    uri: String,
    error: String,
}

pub struct Accounts {
    oauth: OAuthManager,
    device: Mutex<Option<Device>>,
    /// The calendar mirror looks again now, not at its next round.
    kick: AtomicBool,
    /// Signed out by hand: a file `forget-<source>` until the agent has dropped the mirror of that calendar, so a restart
    /// or an agent that is not running does not keep the events.
    dir: std::path::PathBuf,
}

const MIRRORED: [&str; 2] = ["google", "icloud"];

impl Accounts {
    pub fn new(home: &Path) -> Self {
        Self {
            oauth: OAuthManager::new(&home.join("accounts")),
            device: Mutex::new(None),
            kick: AtomicBool::new(false),
            dir: home.join("accounts"),
        }
    }

    /// `github`: what github.rs knows about the saved token. Never a token.
    pub fn status(&self, github: Value) -> Value {
        let device = self.device.lock().unwrap().clone();
        json!({
            "huggingface": self.oauth.status(HUGGING_FACE.name, HUGGING_FACE.url),
            "google": self.oauth.status(GOOGLE.name, GOOGLE.url),
            "icloud": match self.icloud() {
                Some((user, _)) => json!({"signed_in": true, "user": user}),
                None => json!({"signed_in": false}),
            },
            "github": {
                "account": github,
                "user_code": device.as_ref().map(|d| d.user_code.clone()),
                "uri": device.as_ref().map(|d| d.uri.clone()),
                "error": device.as_ref().map(|d| d.error.clone()),
            },
        })
    }

    pub fn begin(&self, id: &str, redirect_uri: &str, lang: &str) -> Option<Result<Value, String>> {
        provider(id).map(|p| {
            self.oauth
                .begin_known(p, redirect_uri, lang)
                .map_err(|e| e.message)
        })
    }

    pub fn knows(&self, state: &str) -> bool {
        self.oauth.knows(state)
    }

    pub fn finish(
        &self,
        state: &str,
        code: &str,
        error: &str,
        description: &str,
    ) -> Result<String, String> {
        let name = self
            .oauth
            .complete(state, code, error, description)
            .map_err(|e| e.message)?;
        self.kick.store(true, Ordering::SeqCst);
        Ok(name)
    }

    pub fn sign_out(&self, id: &str) -> bool {
        if MIRRORED.contains(&id) {
            let _ = std::fs::create_dir_all(&self.dir);
            let _ = std::fs::write(self.dir.join(format!("forget-{id}")), "");
            self.kick.store(true, Ordering::SeqCst);
        }
        if id == "icloud" {
            return std::fs::remove_file(self.icloud_file()).is_ok();
        }
        provider(id).is_some_and(|p| self.oauth.sign_out(p.name, p.url))
    }

    fn icloud_file(&self) -> std::path::PathBuf {
        self.dir.join("icloud.json")
    }

    /// The Apple ID and its app-specific password, when iCloud is connected.
    pub fn icloud(&self) -> Option<(String, String)> {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(self.icloud_file()).ok()?).ok()?;
        Some((
            v["user"].as_str()?.to_string(),
            v["password"].as_str()?.to_string(),
        ))
    }

    /// Check the Apple ID and app-specific password with iCloud, then keep them in a file only this user can read.
    pub fn icloud_connect(&self, user: &str, password: &str) -> Result<usize, String> {
        let (user, password) = (user.trim(), password.trim());
        if user.is_empty() || password.is_empty() || user.len() > 200 || password.len() > 200 {
            return Err("Enter your Apple ID and an app-specific password".into());
        }
        let calendars = crate::caldav::check(user, password)?;
        crate::github::write_private(
            &self.icloud_file(),
            &json!({"user": user, "password": password}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(self.dir.join("forget-icloud"));
        self.kick.store(true, Ordering::SeqCst);
        Ok(calendars)
    }

    pub fn take_kick(&self) -> bool {
        self.kick.swap(false, Ordering::SeqCst)
    }

    /// Calendars whose mirror the agent still has to drop.
    pub fn to_forget(&self) -> Vec<&'static str> {
        MIRRORED
            .into_iter()
            .filter(|s| self.dir.join(format!("forget-{s}")).exists())
            .collect()
    }

    /// The agent has dropped it.
    pub fn forgotten(&self, source: &str) {
        let _ = std::fs::remove_file(self.dir.join(format!("forget-{source}")));
    }

    pub fn google_signed_in(&self) -> bool {
        self.oauth.status(GOOGLE.name, GOOGLE.url)["signed_in"] == json!(true)
    }

    /// A current token for downloads from Hugging Face, or nothing.
    pub fn hugging_face_token(&self) -> String {
        self.oauth
            .access_token(HUGGING_FACE.name, HUGGING_FACE.url)
            .unwrap_or_default()
    }

    /// A current token for the calendar of the connected Google account.
    pub fn google_token(&self) -> Result<String, String> {
        self.oauth
            .access_token(GOOGLE.name, GOOGLE.url)
            .map_err(|e| e.message)
    }

    pub fn github_cancel(&self) {
        *self.device.lock().unwrap() = None;
    }

    /// Ask GitHub for a code, then wait in the background until it was entered; `done` runs when the token is saved.
    pub fn github_begin(
        self: &Arc<Self>,
        github: GitHub,
        api: String,
        done: Box<dyn FnOnce() + Send>,
    ) -> Result<Value, String> {
        let base =
            std::env::var("FLE_GITHUB_LOGIN").unwrap_or_else(|_| "https://github.com".into());
        let d = post_form(
            &format!("{base}/login/device/code"),
            &[("client_id", GITHUB_CLIENT), ("scope", GITHUB_SCOPE)],
        )?;
        let text = |k: &str| d[k].as_str().unwrap_or("").to_string();
        if text("device_code").is_empty() {
            return Err(format!("GitHub gave no code ({}).", text("error")));
        }
        let started = Instant::now();
        *self.device.lock().unwrap() = Some(Device {
            started,
            user_code: text("user_code"),
            uri: text("verification_uri"),
            error: String::new(),
        });
        let (me, device_code) = (self.clone(), text("device_code"));
        let mut every = d["interval"].as_u64().unwrap_or(5).max(1);
        let until = started + Duration::from_secs(d["expires_in"].as_u64().unwrap_or(900));
        std::thread::spawn(move || {
            let fail = |msg: String| {
                let mut g = me.device.lock().unwrap();
                if let Some(x) = g.as_mut().filter(|x| x.started == started) {
                    x.error = msg;
                }
            };
            loop {
                std::thread::sleep(Duration::from_secs(every));
                let current = me.device.lock().unwrap().as_ref().map(|x| x.started);
                if current != Some(started) {
                    return; // cancelled, or a new sign-in took over
                }
                if Instant::now() > until {
                    return fail("The code expired. Start again.".into());
                }
                let answer = post_form(
                    &format!("{base}/login/oauth/access_token"),
                    &[
                        ("client_id", GITHUB_CLIENT),
                        ("device_code", &device_code),
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ],
                );
                let Ok(doc) = answer else {
                    continue; // the network came and went: the code is still good
                };
                match read_poll(&doc) {
                    Poll::Wait => {}
                    Poll::Slower => every += 5,
                    Poll::Failed(msg) => return fail(msg),
                    Poll::Done(token) => {
                        match github.save(&token, &api) {
                            Ok(_) => me.github_cancel(),
                            Err((_, msg)) => fail(msg),
                        }
                        return done();
                    }
                }
            }
        });
        Ok(self.status(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_answers_while_the_code_is_entered() {
        assert_eq!(
            read_poll(&json!({"error": "authorization_pending"})),
            Poll::Wait
        );
        assert_eq!(read_poll(&json!({"error": "slow_down"})), Poll::Slower);
        assert_eq!(
            read_poll(&json!({"access_token": "gho_x"})),
            Poll::Done("gho_x".into())
        );
        assert!(matches!(
            read_poll(&json!({"error": "access_denied"})),
            Poll::Failed(_)
        ));
        assert!(matches!(
            read_poll(&json!({"error": "expired_token"})),
            Poll::Failed(_)
        ));
    }

    #[test]
    fn a_sign_in_asks_for_the_right_things() {
        let dir = std::env::temp_dir().join(format!("flelink-accounts-{}", std::process::id()));
        let a = Accounts::new(&dir);
        // the real Google entry needs its secret from the build; the request itself is the same
        let google = KnownProvider {
            secret_env: "",
            built_secret: None,
            ..GOOGLE
        };
        let v = a
            .oauth
            .begin_known(&google, "http://127.0.0.1:8642/oauth/callback", "en")
            .unwrap();
        let url = v["auth_url"].as_str().unwrap();
        for part in [
            "access_type=offline",
            "prompt=consent",
            "code_challenge_method=S256",
            "hl=en",
        ] {
            assert!(url.contains(part), "{url}");
        }
        assert!(!url.contains("resource="), "{url}");
        assert!(a.knows(v["state"].as_str().unwrap()));
        let odd = a.oauth.begin_known(&google, "http://x", "en&x=1").unwrap();
        assert!(!odd["auth_url"].as_str().unwrap().contains("hl="));
        assert!(a.begin("somewhere", "http://x", "en").is_none());
        assert_eq!(a.status(Value::Null)["google"]["signed_in"], json!(false));
        let _ = std::fs::remove_dir_all(dir);
    }
}
