//! The agent plugin: FLE Durable next to the model, an always-on local agent that works from events.
//!
//! FLE Durable (the durable runner, Node) runs as its own process, started and stopped here. Its model is this server's own
//! `/v1`, so it follows whatever model the Engine page runs. Events reach it from its agenda (planned by you on the Agent page
//! or by the agent itself), from webhooks (`POST /api/p/agent/events`) and from your chat with it. The page talks to the runner
//! only through the routes here; the runner's token never leaves this server.
//!
//! Settings in `<data>/agent/settings.json` (the plugin's own file; FLE Link's settings stay as the Studio's):
//! `runner_dir` (a checkout of github.com/sirfyyn/fle-durable, with `src/main.js` and its `node_modules`), `node` (empty: from
//! the PATH), `autostart`, `workspace` (empty: `<data>/agent/work`).
//! The runner's files live in `<data>/agent/` too: `config.json` (FLE Link sets model window, zone and device on every start and
//! keeps the rest, so `ports` and other keys you add stay), `durable.sqlite` (conversations, agenda, queue), `work/`.
use crate::server::ApiError;
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// FLE Durable needs what pi needs.
pub const NODE_MIN: (u64, u64) = (22, 19);
const LOG_LINES: usize = 300;

#[derive(Default)]
struct Run {
    child: Option<Child>,
    port: u16,
    token: String,
    started: f64,
    error: String,
    exit: Option<i32>,
}

pub struct Agent {
    dir: PathBuf,
    run: Mutex<Run>,
    log: Arc<Mutex<VecDeque<String>>>,
}

/// What the settings say, read once per call.
#[derive(Clone, Debug, Default)]
pub struct AgentSettings {
    pub runner_dir: String,
    pub node: String,
    pub workspace: String,
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(r) => dirs::home_dir().unwrap_or_default().join(r),
        None => PathBuf::from(p),
    }
}

/// `v24.11.0` → (24, 11)
pub fn node_version(text: &str) -> Option<(u64, u64)> {
    let mut it = text.trim().trim_start_matches('v').split('.');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// The node to use: the configured one, else the first on the PATH.
pub fn find_node(configured: &str) -> Result<(PathBuf, String), String> {
    let path = if !configured.trim().is_empty() {
        expand(configured.trim())
    } else {
        let names: &[&str] = if cfg!(windows) {
            &["node.exe", "node"]
        } else {
            &["node"]
        };
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .flat_map(|d| names.iter().map(move |n| d.join(n)))
            .find(|p| p.is_file())
            .ok_or("Node.js was not found on the PATH. Install Node 22.19 or newer, or enter its path.")?
    };
    let out = Command::new(&path)
        .arg("--version")
        .output()
        .map_err(|e| format!("{} does not start: {e}", path.display()))?;
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match node_version(&version) {
        Some(v) if v >= NODE_MIN => Ok((path, version)),
        Some(_) => Err(format!(
            "Node {version} is too old; FLE Durable needs {}.{} or newer.",
            NODE_MIN.0, NODE_MIN.1
        )),
        None => Err(format!(
            "{} gave no version: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Is FLE Durable in `dir`, with its packages installed?
pub fn runner_problem(dir: &str) -> Option<String> {
    if dir.trim().is_empty() {
        return Some("Enter the folder of FLE Durable (the one with src/main.js; git clone https://github.com/sirfyyn/fle-durable).".into());
    }
    let d = expand(dir.trim());
    if !d.join("src").join("main.js").is_file() {
        return Some(format!("{} has no src/main.js.", d.display()));
    }
    if !d
        .join("node_modules")
        .join("@earendil-works")
        .join("pi-durable")
        .is_dir()
    {
        return Some(format!(
            "The packages are missing: run npm ci --ignore-scripts in {}.",
            d.display()
        ));
    }
    None
}

fn free_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("no free port: {e}"))
}

fn token() -> String {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).expect("random bytes");
    hex::encode(b)
}

/// The runner's config: the file as it is, with the keys FLE Link owns set again (model name, window, answer limit, zone).
pub fn runner_config(existing: Option<Value>, n_ctx: u64, zone: &str) -> Value {
    let mut cfg = existing
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let o = cfg.as_object_mut().unwrap();
    o.entry("agent").or_insert(json!("agent"));
    let window = n_ctx.max(4096);
    let model = o.entry("model").or_insert(json!({}));
    if !model.is_object() {
        *model = json!({});
    }
    let model = model.as_object_mut().unwrap();
    model.insert("name".into(), json!("FLE Link"));
    model.insert("contextWindow".into(), json!(window));
    model.insert("maxTokens".into(), json!((window / 4).clamp(1024, 16384)));
    let agenda = o.entry("agenda").or_insert(json!({}));
    if !agenda.is_object() {
        *agenda = json!({});
    }
    agenda
        .as_object_mut()
        .unwrap()
        .insert("zone".into(), json!(zone));
    cfg
}

impl Agent {
    pub fn new(home: &Path) -> Self {
        Agent {
            dir: home.join("agent"),
            run: Mutex::new(Run::default()),
            log: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn settings_file(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    fn settings_raw(&self) -> Map<String, Value> {
        std::fs::read(self.settings_file())
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    /// The plugin's own settings (`<data>/agent/settings.json`), apart from FLE Link's, and whether to start with FLE Link.
    pub fn settings(&self) -> (AgentSettings, bool) {
        let raw = self.settings_raw();
        let s = |k: &str| raw.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        (
            AgentSettings {
                runner_dir: s("runner_dir"),
                node: s("node"),
                workspace: s("workspace"),
            },
            raw.get("autostart")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )
    }

    /// The key webhooks show in `X-Agent-Key` (made on first use, kept in settings.json).
    pub fn webhook_key(&self) -> String {
        let mut raw = self.settings_raw();
        if let Some(k) = raw
            .get("webhook_key")
            .and_then(Value::as_str)
            .filter(|k| k.len() >= 32)
        {
            return k.to_string();
        }
        let k = token();
        raw.insert("webhook_key".into(), json!(k));
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(
            self.settings_file(),
            serde_json::to_vec_pretty(&Value::Object(raw)).unwrap(),
        );
        k
    }

    pub fn webhook_ok(&self, shown: &str) -> bool {
        let want = self
            .settings_raw()
            .get("webhook_key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        want.len() >= 32
            && shown.len() == want.len()
            && shown
                .bytes()
                .zip(want.bytes())
                .fold(0u8, |a, (x, y)| a | (x ^ y))
                == 0
    }

    pub fn update_settings(&self, body: &Map<String, Value>) -> Result<(), String> {
        let merged = merge_settings(self.settings_raw(), body);
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let tmp = self.settings_file().with_extension("tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&Value::Object(merged)).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.settings_file()).map_err(|e| e.to_string())
    }

    fn note(&self, line: String) {
        let mut l = self.log.lock().unwrap();
        l.push_back(line);
        while l.len() > LOG_LINES {
            l.pop_front();
        }
    }

    /// Notice a runner that ended by itself.
    fn reap(run: &mut Run) {
        if let Some(c) = run.child.as_mut() {
            if let Ok(Some(st)) = c.try_wait() {
                run.exit = st.code();
                if run.error.is_empty() && !st.success() {
                    run.error = format!("FLE Durable ended with {st}; the log below says why.");
                }
                run.child = None;
            }
        }
    }

    pub fn running(&self) -> bool {
        let mut r = self.run.lock().unwrap();
        Self::reap(&mut r);
        r.child.is_some()
    }

    pub fn status(&self, s: &AgentSettings, autostart: bool) -> Value {
        let mut r = self.run.lock().unwrap();
        Self::reap(&mut r);
        let node = match find_node(&s.node) {
            Ok((p, v)) => {
                json!({"ok": true, "path": p.to_string_lossy(), "version": v, "error": ""})
            }
            Err(e) => json!({"ok": false, "path": "", "version": "", "error": e}),
        };
        let problem = runner_problem(&s.runner_dir);
        let log: Vec<String> = self
            .log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(60)
            .rev()
            .cloned()
            .collect();
        json!({
            "installed": problem.is_none(), "problem": problem.unwrap_or_default(), "runner_dir": s.runner_dir, "node": node,
            "running": r.child.is_some(), "pid": r.child.as_ref().map(Child::id), "port": if r.child.is_some() { r.port } else { 0 },
            "started": if r.child.is_some() { json!(r.started) } else { Value::Null }, "error": r.error, "exit": r.exit,
            "autostart": autostart, "workspace": s.workspace,
            "data_dir": self.dir.to_string_lossy(), "log": log, "webhook_key": self.webhook_key(),
        })
    }

    /// Start FLE Durable against this server's `/v1` (`studio_port`), with the model's name and window.
    pub fn start(
        &self,
        s: &AgentSettings,
        studio_port: u16,
        model: &str,
        n_ctx: u64,
    ) -> Result<(), String> {
        let mut r = self.run.lock().unwrap();
        Self::reap(&mut r);
        if r.child.is_some() {
            return Ok(());
        }
        if let Some(p) = runner_problem(&s.runner_dir) {
            return Err(p);
        }
        if studio_port == 0 {
            return Err("FLE Link is not serving yet.".into());
        }
        let (node, _) = find_node(&s.node)?;
        let runner = expand(s.runner_dir.trim());
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let cfg_path = self.dir.join("config.json");
        let existing = std::fs::read(&cfg_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let zone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
        let cfg = runner_config(existing, n_ctx, &zone);
        std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg).unwrap())
            .map_err(|e| e.to_string())?;
        let work = if s.workspace.trim().is_empty() {
            self.dir.join("work")
        } else {
            expand(s.workspace.trim())
        };
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let port = free_port()?;
        let tok = token();
        let mut cmd = Command::new(&node);
        cmd.arg(runner.join("src").join("main.js"))
            .current_dir(&runner)
            .env("FLE_DURABLE_CONFIG", &cfg_path)
            .env("FLE_DURABLE_HOME", &self.dir)
            .env("FLE_DURABLE_TOKEN", &tok)
            .env("FLE_DURABLE_HOST", "127.0.0.1")
            .env("FLE_DURABLE_PORT", port.to_string())
            .env("FLE_DURABLE_WORKSPACE", &work)
            // the GitHub sign-in (github.rs): GH_TOKEN and git's credential for github.com, so the agent works in repos
            .envs(crate::github::GitHub::new(self.dir.parent().unwrap_or(&self.dir)).env())
            // the MCP hub from Settings: the agent gets hub_search / hub_call for all of its tools
            .envs(crate::github::agent_hub_env(
                self.dir.parent().unwrap_or(&self.dir),
            ))
            .env(
                "FLE_DURABLE_MODEL_URL",
                format!("http://127.0.0.1:{studio_port}/v1"),
            )
            .env(
                "FLE_DURABLE_MODEL",
                if model.is_empty() { "local" } else { model },
            )
            .env("FLE_DURABLE_CONTEXT", n_ctx.max(4096).to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("FLE Durable does not start: {e}"))?;
        for pipe in [
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            child.stderr.take().map(|p| Box::new(p) as _),
        ]
        .into_iter()
        .flatten()
        {
            let log = self.log.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let mut l = log.lock().unwrap();
                    l.push_back(line);
                    while l.len() > LOG_LINES {
                        l.pop_front();
                    }
                }
            });
        }
        let shown = if model.is_empty() {
            "of the engine (none running yet)".to_string()
        } else {
            format!("{model} ({n_ctx} tokens)")
        };
        self.note(format!(
            "[flelink] started FLE Durable on 127.0.0.1:{port}, model {shown}"
        ));
        *r = Run {
            child: Some(child),
            port,
            token: tok,
            started: now(),
            error: String::new(),
            exit: None,
        };
        Ok(())
    }

    /// Wait until the runner answers (a fresh start opens its SQLite file and the conversation first).
    pub fn wait_ready(&self, limit: Duration) -> Result<(), String> {
        let t = Instant::now();
        while t.elapsed() < limit {
            if !self.running() {
                return Err(self.run.lock().unwrap().error.clone());
            }
            if self.call("GET", "/api/health", None).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err("FLE Durable did not answer in time; see the log.".into())
    }

    /// Stop the runner: SIGTERM (it closes its file cleanly), after six seconds a kill.
    pub fn stop(&self) {
        let child = self.run.lock().unwrap().child.take();
        let Some(mut c) = child else { return };
        #[cfg(unix)]
        unsafe {
            libc::kill(c.id() as i32, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = c.kill();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(6) {
            if let Ok(Some(_)) = c.try_wait() {
                self.note("[flelink] FLE Durable stopped".into());
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = c.kill();
        let _ = c.wait();
        self.note("[flelink] FLE Durable did not stop in time and was killed".into());
    }

    /// One call to the runner's API, with its token. Its error text (`fehler`) comes back as the error.
    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        let (port, tok) = {
            let mut r = self.run.lock().unwrap();
            Self::reap(&mut r);
            if r.child.is_none() {
                return Err(ApiError(
                    409,
                    "The agent is not running. Start it on the Agent page.".into(),
                ));
            }
            (r.port, r.token.clone())
        };
        let req = ureq::request(method, &format!("http://127.0.0.1:{port}{path}"))
            .set("Authorization", &format!("Bearer {tok}"))
            .timeout(Duration::from_secs(20));
        let res = match body {
            Some(b) => req
                .set("Content-Type", "application/json")
                .send_string(&b.to_string()),
            None => req.call(),
        };
        let parse = |r: ureq::Response| {
            r.into_string()
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        };
        match res {
            Ok(r) => parse(r).ok_or_else(|| ApiError(502, "FLE Durable sent no JSON".into())),
            Err(ureq::Error::Status(code, r)) => {
                let v = parse(r).unwrap_or(json!({}));
                let msg = v["error"]
                    .as_str()
                    .unwrap_or("FLE Durable refused the call")
                    .to_string();
                Err(ApiError(code, msg))
            }
            Err(e) => Err(ApiError(502, format!("FLE Durable does not answer: {e}"))),
        }
    }

    /// A one-time address for the runner's own chat page (the code turns into a cookie there, the token stays here).
    pub fn chat_url(&self) -> Result<Value, ApiError> {
        let v = self.call("POST", "/api/session/code", Some(&json!({})))?;
        let port = self.run.lock().unwrap().port;
        let code = v["code"].as_str().unwrap_or_default();
        Ok(json!({"url": format!("http://127.0.0.1:{port}/?code={code}")}))
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The routes under `/api/p/agent/`, given the runner's API: which method and path of the runner a page call maps to.
/// `None`: not a proxied route.
pub fn proxied(method: &str, rest: &str) -> Option<(&'static str, String)> {
    let id_ok = |id: &str| {
        !id.is_empty()
            && id.len() <= 40
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    match (method, rest) {
        ("GET", "agenda") => Some(("GET", "/api/agenda".into())),
        ("POST", "agenda") => Some(("POST", "/api/agenda".into())),
        ("POST", "events") => Some(("POST", "/api/events".into())),
        ("GET", "queue") => Some(("GET", "/api/queue".into())),
        ("GET", "approvals") => Some(("GET", "/api/approvals".into())),
        ("GET", "conversation") => Some(("GET", "/api/conversation".into())),
        ("POST", "message") => Some(("POST", "/api/message".into())),
        _ => {
            let (kind, tail) = rest.split_once('/')?;
            match (method, kind, tail.split_once('/')) {
                ("DELETE", "agenda", None) if id_ok(tail) => {
                    Some(("DELETE", format!("/api/agenda/{tail}")))
                }
                ("POST", "agenda", Some((id, "pause"))) if id_ok(id) => {
                    Some(("POST", format!("/api/agenda/{id}/pause")))
                }
                ("POST", "approvals", None) if id_ok(tail) => {
                    Some(("POST", format!("/api/approvals/{tail}")))
                }
                _ => None,
            }
        }
    }
}

/// The plugin's settings, merged with a patch from the page's setup form: only these keys, strings or a bool.
pub fn merge_settings(
    mut cur: Map<String, Value>,
    body: &Map<String, Value>,
) -> Map<String, Value> {
    for k in ["runner_dir", "node", "workspace"] {
        if let Some(v) = body.get(k).and_then(Value::as_str) {
            cur.insert(k.into(), json!(v.trim()));
        }
    }
    if let Some(b) = body.get("autostart").and_then(Value::as_bool) {
        cur.insert("autostart".into(), json!(b));
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(node_version("v24.11.0\n"), Some((24, 11)));
        assert_eq!(node_version("22.19.1"), Some((22, 19)));
        assert_eq!(node_version("nope"), None);
        assert!((22, 18) < NODE_MIN && (22, 19) >= NODE_MIN && (23, 0) >= NODE_MIN);
    }

    #[test]
    fn config_keeps_what_the_user_added_and_sets_ours() {
        let mine = json!({"ports": {"sources": ["agenda", "./mine.js"]}, "model": {"contextWindow": 1, "x": 1}, "instructions": "be brief"});
        let c = runner_config(Some(mine), 32768, "Europe/Berlin");
        assert_eq!(c["ports"]["sources"][1], "./mine.js");
        assert_eq!(c["instructions"], "be brief");
        assert_eq!(c["model"]["x"], 1);
        assert_eq!(c["model"]["contextWindow"], 32768);
        assert_eq!(c["model"]["maxTokens"], 8192);
        assert_eq!(c["agenda"]["zone"], "Europe/Berlin");
        assert_eq!(
            runner_config(None, 0, "UTC")["model"]["contextWindow"],
            4096
        );
    }

    #[test]
    fn routes_map_to_the_runner_and_refuse_the_rest() {
        assert_eq!(
            proxied("GET", "agenda"),
            Some(("GET", "/api/agenda".into()))
        );
        assert_eq!(
            proxied("DELETE", "agenda/a12bc"),
            Some(("DELETE", "/api/agenda/a12bc".into()))
        );
        assert_eq!(
            proxied("POST", "agenda/a12bc/pause"),
            Some(("POST", "/api/agenda/a12bc/pause".into()))
        );
        assert_eq!(
            proxied("POST", "approvals/f-1"),
            Some(("POST", "/api/approvals/f-1".into()))
        );
        assert_eq!(proxied("DELETE", "agenda/../../x"), None);
        assert_eq!(proxied("GET", "verwaltung"), None);
        assert_eq!(proxied("POST", "agenda/a1/delete"), None);
    }

    #[test]
    fn settings_merge_only_known_keys() {
        let cur = merge_settings(
            Map::new(),
            json!({"runner_dir": " ~/fle-durable ", "autostart": true, "evil": "x"})
                .as_object()
                .unwrap(),
        );
        assert_eq!(
            Value::Object(cur.clone()),
            json!({"runner_dir": "~/fle-durable", "autostart": true})
        );
        let next = merge_settings(
            cur,
            json!({"workspace": "~/w", "autostart": false})
                .as_object()
                .unwrap(),
        );
        assert_eq!(next["workspace"], "~/w");
        assert_eq!(next["autostart"], false);
        assert_eq!(next["runner_dir"], "~/fle-durable");
    }

    #[test]
    fn webhook_key_is_made_once_kept_and_checked() {
        let home = std::env::temp_dir().join(format!("flelink-agent-test-{}", token()));
        let a = Agent::new(&home);
        assert!(!a.webhook_ok("anything"), "no key yet: nothing passes");
        let k = a.webhook_key();
        assert_eq!(k.len(), 64);
        assert_eq!(a.webhook_key(), k, "kept");
        assert!(a.webhook_ok(&k));
        assert!(!a.webhook_ok(&k[..63]));
        assert!(!a.webhook_ok(&format!("{}x", &k[..63])));
        a.update_settings(json!({"autostart": true}).as_object().unwrap())
            .unwrap();
        assert!(a.webhook_ok(&k), "a setup save keeps the key");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn missing_runner_is_explained() {
        assert!(runner_problem("").unwrap().contains("folder"));
        assert!(runner_problem("/nonexistent/fle-durable")
            .unwrap()
            .contains("src/main.js"));
    }
}
