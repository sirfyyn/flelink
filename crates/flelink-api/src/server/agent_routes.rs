//! The agent in the Studio's API: starting FLE Durable with the engine's model, following a model change, the
//! calendar mirror, the routes under `/api/p/agent/…` and its event stream.
use super::*;

impl Studio {
    pub(super) fn agent_settings(&self) -> (crate::agent::AgentSettings, bool) {
        self.agent.settings()
    }

    /// Start the agent with the model the engine runs now (name and context window), and wait until it answers.
    pub(super) fn agent_start(&self) -> Result<(), String> {
        let (s, _) = self.agent_settings();
        if !s.url.is_empty() {
            return Ok(()); // it runs on its own; FLE Link only talks to it
        }
        let eng = self.engine.status();
        let model = eng["model"]["name"].as_str().unwrap_or("").to_string();
        let n_ctx = eng["n_ctx"].as_u64().unwrap_or(0);
        let port = self.port.load(std::sync::atomic::Ordering::SeqCst);
        self.agent
            .start(&s, port, &model, n_ctx, self.desktop.token())?;
        self.agent.wait_ready(std::time::Duration::from_secs(30))
    }

    /// The agent follows the engine: when a model is ready that is not the one the agent was started with (most often:
    /// the agent came up first, with no model at all), it starts again with that model's name and context window. A turn
    /// that ran against the old model ended with it; FLE Durable picks its work up from the last checkpoint.
    pub fn agent_follow(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("agent-follow".into())
            .spawn(move || {
                let mut last: Option<std::time::Instant> = None;
                loop {
                std::thread::sleep(std::time::Duration::from_secs(4));
                let Some(st) = weak.upgrade() else { return };
                // at most once a minute, whatever the engine reports
                if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60)) {
                    continue;
                }
                let eng = st.engine.status();
                if !matches!(eng["state"].as_str(), Some("ready" | "attached")) {
                    continue;
                }
                let model = eng["model"]["name"].as_str().unwrap_or("").to_string();
                let n_ctx = eng["n_ctx"].as_u64().unwrap_or(0);
                if !st.agent.outdated(&model, n_ctx) {
                    continue;
                }
                last = Some(std::time::Instant::now());
                st.agent.note_line(&format!(
                    "[flelink] the engine now serves {model} ({n_ctx} tokens): starting FLE Durable again with it"
                ));
                st.agent.stop();
                if let Err(e) = st.agent_start() {
                    eprintln!("[agent] restart for the new model: {e}");
                }
                }
            });
    }

    /// `agent.autostart`: start the agent once the server listens (in the background; a failure shows on the Agent page).
    /// Keeps the agent's mirror of connected calendars current (calendars.rs): every five minutes, and at once after a sign-in.
    pub fn calendar_mirror(self: &Arc<Self>) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(300);
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("calendar-mirror".into())
            .spawn(move || {
                let mut last: Option<std::time::Instant> = None;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    let Some(st) = weak.upgrade() else {
                        return;
                    };
                    let due = match last {
                        None => true,
                        Some(t) => t.elapsed() >= EVERY,
                    };
                    if !st.agent.running() || !(st.accounts.take_kick() || due) {
                        continue;
                    }
                    last = Some(std::time::Instant::now());
                    st.calendar_sync();
                }
            });
    }

    pub(super) fn calendar_sync(&self) {
        use chrono::{Duration, SecondsFormat, Utc};
        let at = |d: chrono::DateTime<Utc>| d.to_rfc3339_opts(SecondsFormat::Secs, true);
        let from = at(Utc::now() - Duration::days(crate::calendars::DAYS_BACK));
        let to = at(Utc::now() + Duration::days(crate::calendars::DAYS_AHEAD));
        // signed out: the whole mirror goes, also what has slid out of the window since
        for source in self.accounts.to_forget() {
            let all = json!({"source": source, "from": "1970-01-01T00:00:00Z", "to": "2200-01-01T00:00:00Z", "entries": []});
            if self
                .agent
                .call("POST", "/api/calendar/sync", Some(&all))
                .is_ok()
            {
                self.accounts.forgotten(source);
            }
        }
        let mut bodies = vec![];
        if self.accounts.google_signed_in() {
            let read = self
                .accounts
                .google_token()
                .and_then(|t| crate::calendars::google_read(&t, &from, &to));
            bodies.push(match read {
                Ok(entries) => {
                    json!({"source": "google", "from": from, "to": to, "entries": entries})
                }
                Err(reason) => json!({"source": "google", "reason": reason}),
            });
        }
        if let Some((user, password)) = self.accounts.icloud() {
            bodies.push(match crate::caldav::read(&user, &password, &from, &to) {
                Ok(entries) => {
                    json!({"source": "icloud", "from": from, "to": to, "entries": entries})
                }
                Err(reason) => json!({"source": "icloud", "reason": reason}),
            });
        }
        for body in bodies {
            let _ = self.agent.call("POST", "/api/calendar/sync", Some(&body));
        }
    }

    pub fn agent_autostart(self: &Arc<Self>) {
        if !self.agent_settings().1 {
            return;
        }
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("agent-autostart".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(2));
                // the model loads at the same start: the agent needs its name and window, so it waits for it
                let t0 = std::time::Instant::now();
                while t0.elapsed() < std::time::Duration::from_secs(1800) {
                    let Some(st) = weak.upgrade() else { return };
                    let loading = st
                        .config
                        .lock()
                        .unwrap()
                        .get("launch.autostart")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let state = st.engine.status()["state"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    if !loading || matches!(state.as_str(), "ready" | "attached" | "crashed") {
                        break;
                    }
                    drop(st);
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
                if let Some(st) = weak.upgrade() {
                    if let Err(e) = st.agent_start() {
                        eprintln!("[agent] autostart: {e}");
                    }
                }
            });
    }

    /// `/api/p/agent/…`: status, start, stop, setup, the chat address, and the runner's agenda, queue, events, approvals.
    pub(super) fn agent_route(&self, method: &str, p: &str, r: &Req) -> Result<Value, ApiError> {
        let rest = p
            .strip_prefix("/api/p/agent")
            .unwrap_or("")
            .trim_start_matches('/');
        let body = || -> Result<Map<String, Value>, ApiError> {
            if r.body.is_empty() {
                Ok(Map::new())
            } else {
                r.json(MAX_JSON)
            }
        };
        let status = || {
            let (s, auto) = self.agent_settings();
            let mut v = self.agent.status(&s, auto);
            v["always_on"] = json!(flelink_core::service::installed());
            v
        };
        match (method, rest) {
            // Always on: FLE Link starts at login with the last model and the agent (from the next login; this one runs already)
            ("POST", "always") => {
                let on = body()?.get("on").and_then(Value::as_bool).unwrap_or(false);
                crate::agent::set_always_on(
                    &self.opts.home,
                    &mut self.config.lock().unwrap(),
                    &self.agent,
                    on,
                    false,
                )
                .map_err(|e| ApiError(400, e))?;
                Ok(status())
            }
            ("GET", "" | "status") => Ok(status()),
            // Computer use: only the page, not a client paired from another computer, and never the agent (it has no page token)
            ("GET", "desktop") => Ok(self.desktop.status()),
            ("POST", "desktop") => {
                if !r.client.is_null() {
                    return Err(ApiError(
                        403,
                        "Computer use can only be allowed on this computer.".into(),
                    ));
                }
                let minutes = body()?.get("minutes").and_then(Value::as_u64).unwrap_or(0);
                self.desktop.allow(minutes);
                Ok(self.desktop.status())
            }
            ("POST", "start") => self
                .agent_start()
                .map(|_| status())
                .map_err(|e| ApiError(400, e)),
            ("POST", "stop") => {
                self.agent.stop();
                Ok(status())
            }
            ("POST", "restart") => {
                self.agent.stop();
                self.agent_start()
                    .map(|_| status())
                    .map_err(|e| ApiError(400, e))
            }
            ("POST", "setup") => {
                self.agent
                    .update_settings(&body()?)
                    .map_err(|e| ApiError(400, e))?;
                Ok(status())
            }
            // FLE Link fetches FLE Durable at DURABLE_PIN into <data>/agent/app (git, then npm ci); also updates that copy
            ("POST", "install") => {
                let (s, _) = self.agent_settings();
                self.agent.install(&s).map_err(|e| ApiError(400, e))?;
                Ok(status())
            }
            ("POST", "chat") => self.agent.chat_url(),
            _ => {
                let (m, path) = crate::agent::proxied(method, rest)
                    .ok_or_else(|| ApiError(404, "Not found".into()))?;
                // FLE Durable records every decision through its token as the owner's; the page cannot name someone else
                let b = if m == "POST" || m == "PATCH" {
                    Some(Value::Object(body()?))
                } else {
                    None
                };
                let query = if m == "GET" {
                    crate::agent::safe_query(&r.query)
                } else {
                    String::new()
                };
                self.agent.call(m, &format!("{path}{query}"), b.as_ref())
            }
        }
    }
}

/// `/h/<host id>/api/…` and `/h/<host id>/v1/…` to a paired host, with the client token. Small answers are read whole (and
/// mirrored); streams, downloads and other large answers are passed on as they arrive.
/// The agent's live conversation for the chat page: the runner's event stream, passed through. A closed page ends it at the next
/// event or at the runner's keep-alive comment (20 s).
pub(super) async fn agent_stream(st: &Ctx, head: bool) -> Response<Body> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(16);
    let (meta_tx, meta_rx) = tokio::sync::oneshot::channel::<Result<(), ApiError>>();
    let st2 = st.clone();
    std::thread::spawn(move || {
        let resp = match st2.agent.stream_open() {
            Ok(r) => r,
            Err(e) => {
                let _ = meta_tx.send(Err(e));
                return;
            }
        };
        let _ = meta_tx.send(Ok(()));
        let mut reader = resp.into_reader();
        let mut buf = vec![0u8; 65536];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx
                        .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    match meta_rx.await {
        Ok(Ok(())) => {
            let mut r = Response::new(Body::from_stream(
                tokio_stream::wrappers::ReceiverStream::new(rx),
            ));
            let hm = r.headers_mut();
            hm.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            hm.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            hm.insert("x-accel-buffering", HeaderValue::from_static("no"));
            r
        }
        Ok(Err(ApiError(s, m))) => error_json(s, &m, head),
        Err(_) => error_json(502, "FLE Durable's event stream did not open", head),
    }
}
