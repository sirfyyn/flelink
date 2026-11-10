//! The agent plugin: FLE Durable next to the model, an always-on local agent that works from events.
//!
//! FLE Durable (the durable runner, Node) runs as its own process, started and stopped here. Its model is this server's own
//! `/v1`, so it follows whatever model the Engine page runs. Events reach it from its agenda (planned by you on the Agent page
//! or by the agent itself), from webhooks (`POST /api/p/agent/events`) and from your chat with it. The page talks to the runner
//! only through the routes here; the runner's token never leaves this server.
//!
//! FLE Durable is its own application. FLE Link either starts it from a folder (`runner_dir`) or talks to one that already runs (`url`, `token`
//! in the settings). Either way it only uses the runner's HTTP interface: /api/message, /api/stream, /api/conversation, /api/approvals, /api/abort,
//! /api/agenda, /api/queue.
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
/// The FLE Durable FLE Link installs itself (`<data>/agent/app`) when no own folder is set, like ENGINE_PIN and STUDIO_PIN.
pub const DURABLE_REPO: &str = "https://github.com/sirfyyn/fle-durable.git";
pub fn durable_pin() -> &'static str {
    include_str!("../../../DURABLE_PIN").trim()
}

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
    installing: std::sync::atomic::AtomicBool,
}

/// What the settings say, read once per call.
#[derive(Clone, Debug, Default)]
pub struct AgentSettings {
    pub runner_dir: String,
    pub node: String,
    pub workspace: String,
    /// a FLE Durable that already runs (empty: FLE Link starts it from `runner_dir`)
    pub url: String,
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn expand(p: &str) -> PathBuf {
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
        node_dirs()
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

/// Is FLE Durable in `d`, with its packages installed? `managed`: the copy FLE Link installs itself.
pub fn runner_problem(d: &Path, managed: bool) -> Option<String> {
    if managed && !d.join("src").join("main.js").is_file() {
        return Some(
            "FLE Durable is not installed yet: Install fetches it (needs git and Node 22.19+)."
                .into(),
        );
    }
    if !d.join("src").join("main.js").is_file() {
        return Some(format!("{} has no src/main.js.", d.display()));
    }
    if managed && !d.join("node_modules").is_dir() {
        return Some("The install did not finish: press Install again.".into());
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

/// Where Node may be: the PATH first, then where installers and version managers put it. An app started from the Finder or
/// at login gets a short PATH without /opt/homebrew/bin and the like, and found no Node although the terminal has one.
fn node_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                dirs.push(PathBuf::from(&base).join("nodejs"));
                dirs.push(PathBuf::from(base).join("Programs").join("nodejs"));
            }
        }
        dirs.push(home.join("AppData").join("Roaming").join("nvm"));
    } else {
        for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            dirs.push(PathBuf::from(d));
        }
        for d in [
            ".volta/bin",
            ".local/bin",
            ".asdf/shims",
            ".local/share/fnm/aliases/default/bin",
            ".fnm/aliases/default/bin",
        ] {
            dirs.push(home.join(d));
        }
        // nvm: the newest version it has
        let mut nvm: Vec<PathBuf> = std::fs::read_dir(home.join(".nvm/versions/node"))
            .map(|rd| rd.flatten().map(|e| e.path().join("bin")).collect())
            .unwrap_or_default();
        nvm.sort_by_key(|p| {
            node_version(
                &p.parent()
                    .map(|v| {
                        v.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    })
                    .unwrap_or_default(),
            )
        });
        dirs.extend(nvm.into_iter().rev());
    }
    dirs
}

/// The PATH for npm and the agent: the folder of this Node first, so `#!/usr/bin/env node` finds the same one.
pub fn path_with_node(node: &Path) -> std::ffi::OsString {
    let mut parts: Vec<PathBuf> = node.parent().map(Path::to_path_buf).into_iter().collect();
    parts.extend(node_dirs());
    std::env::join_paths(parts).unwrap_or_default()
}

/// npm next to `node` (as Node's installers put it), else from the PATH.
fn npm_for(node: &Path) -> PathBuf {
    let name = if cfg!(windows) { "npm.cmd" } else { "npm" };
    node.parent()
        .map(|p| p.join(name))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Run one install step; its output goes to the agent log, a failure names the step.
fn step(log: &dyn Fn(String), what: &str, cmd: &mut Command) -> Result<(), String> {
    log(format!("[install] {what}"));
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{what}: {e}"))?;
    for l in String::from_utf8_lossy(&out.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&out.stderr).lines())
    {
        log(format!("[install] {l}"));
    }
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({}); the log says why.", out.status))
    }
}

pub fn free_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("no free port: {e}"))
}

pub fn token() -> String {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).expect("random bytes");
    hex::encode(b)
}

/// Always on: FLE Link starts at login and brings the last model and the agent with it. Turning it off only stops the starting
/// at login; `now` starts the service at once (not from inside a running Studio).
pub fn set_always_on(
    home: &Path,
    config: &mut crate::settings::Config,
    agent: &Agent,
    on: bool,
    now: bool,
) -> Result<(), String> {
    use flelink_core::service;
    if !on {
        return service::remove().map_err(|e| format!("{e:#}"));
    }
    let program = service::program().map_err(|e| format!("{e:#}"))?;
    let logs = home.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| e.to_string())?;
    config.update(json!({"launch": {"autostart": true}}).as_object().unwrap())?;
    agent.update_settings(json!({"autostart": true}).as_object().unwrap())?;
    let own_home = (home != flelink_core::data_dir()).then_some(home);
    service::install(&program, own_home, &logs.join("service.log"), now)
        .map_err(|e| format!("{e:#}"))
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
            installing: Default::default(),
        }
    }

    /// The copy FLE Link installs itself.
    fn app_dir(&self) -> PathBuf {
        self.dir.join("app")
    }

    /// Where FLE Durable is: your own folder if the settings name one, else FLE Link's copy.
    pub fn runner_path(&self, s: &AgentSettings) -> (PathBuf, bool) {
        if s.runner_dir.trim().is_empty() {
            (self.app_dir(), true)
        } else {
            (expand(s.runner_dir.trim()), false)
        }
    }

    /// The pin FLE Link's copy was installed from (empty: none yet).
    fn installed_pin(&self) -> String {
        std::fs::read_to_string(self.app_dir().join(".flelink-pin"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Fetch FLE Durable at DURABLE_PIN into `<data>/agent/app` and install its packages: into a fresh folder first, which
    /// replaces the old copy only once everything worked, so a failed update leaves the agent as it was.
    pub fn install(&self, s: &AgentSettings) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        if self.running() {
            return Err("Stop the agent first; it is updated while it is off.".into());
        }
        if self.installing.swap(true, Ordering::SeqCst) {
            return Err("An install is already running.".into());
        }
        let r = self.install_now(s);
        self.installing.store(false, Ordering::SeqCst);
        if let Err(e) = &r {
            self.note(format!("[install] {e}"));
        }
        r
    }

    fn install_now(&self, s: &AgentSettings) -> Result<(), String> {
        let (node, _) = find_node(&s.node)?;
        let pin = durable_pin();
        let log = |l: String| self.note(l);
        let tmp = self.dir.join("app.new");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        step(
            &log,
            &format!("git clone {DURABLE_REPO}"),
            Command::new("git")
                .args(["clone", "--quiet", DURABLE_REPO])
                .arg(&tmp),
        )
        .map_err(|e| {
            if e.contains("No such file") || e.contains("not found") {
                "git is missing: install git, then press Install again.".into()
            } else {
                e
            }
        })?;
        step(
            &log,
            &format!("git checkout {pin}"),
            Command::new("git")
                .args([
                    "-c",
                    "advice.detachedHead=false",
                    "checkout",
                    "--quiet",
                    pin,
                ])
                .current_dir(&tmp),
        )?;
        step(
            &log,
            "npm ci --ignore-scripts",
            Command::new(npm_for(&node))
                .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"])
                .env("PATH", path_with_node(&node))
                .current_dir(&tmp),
        )?;
        std::fs::write(tmp.join(".flelink-pin"), pin).map_err(|e| e.to_string())?;
        let app = self.app_dir();
        if app.exists() {
            std::fs::remove_dir_all(&app)
                .map_err(|e| format!("could not replace {}: {e}", app.display()))?;
        }
        std::fs::rename(&tmp, &app).map_err(|e| e.to_string())?;
        self.note(format!(
            "[install] FLE Durable {} is installed in {}.",
            &pin[..pin.len().min(12)],
            app.display()
        ));
        Ok(())
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
                url: s("url"),
            },
            raw.get("autostart")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )
    }

    /// A FLE Durable that runs on its own: address and token from the settings.
    fn remote(&self) -> Option<(String, String)> {
        let raw = self.settings_raw();
        let url = raw
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();
        // a token file (for example one only this user may read) wins over a saved token and is read at every use
        let token = raw
            .get("token_file")
            .and_then(Value::as_str)
            .filter(|f| !f.is_empty())
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|t| t.trim().to_string())
            .or_else(|| raw.get("token").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        (!url.is_empty()).then_some((url, token))
    }

    /// Does the connected FLE Durable answer?
    fn remote_up(url: &str, token: &str) -> Result<(), String> {
        ureq::get(&format!("{url}/api/health"))
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(3))
            .call()
            .map(|_| ())
            .map_err(|e| match e {
                ureq::Error::Status(401, _) => format!("{url} refuses the token."),
                e => format!("FLE Durable at {url} does not answer: {e}"),
            })
    }

    /// Where the runner's interface is: base address and token. 409 while nothing runs.
    fn target(&self) -> Result<(String, String), ApiError> {
        if let Some(r) = self.remote() {
            return Ok(r);
        }
        let mut r = self.run.lock().unwrap();
        Self::reap(&mut r);
        if r.child.is_none() {
            return Err(ApiError(
                409,
                "The agent is not running. Start it on the Agent page.".into(),
            ));
        }
        Ok((format!("http://127.0.0.1:{}", r.port), r.token.clone()))
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
        if let Some(u) = body
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err("The address starts with http:// or https://.".into());
            }
        }
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

    /// The newest lines of the agent's log, for a terminal that has no Agent page.
    pub fn recent_log(&self) -> Vec<String> {
        let l = self.log.lock().unwrap();
        l.iter().rev().take(20).rev().cloned().collect()
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
        if let Some((url, token)) = self.remote() {
            return Self::remote_up(&url, &token).is_ok();
        }
        let mut r = self.run.lock().unwrap();
        Self::reap(&mut r);
        r.child.is_some()
    }

    pub fn status(&self, s: &AgentSettings, autostart: bool) -> Value {
        if let Some((url, token)) = self.remote() {
            let up = Self::remote_up(&url, &token);
            return json!({
                "remote": true, "url": url, "token_set": !token.is_empty(), "installed": true, "problem": "", "runner_dir": s.runner_dir,
                "node": {"ok": true, "path": "", "version": "", "error": ""}, "running": up.is_ok(), "pid": Value::Null, "port": 0,
                "started": Value::Null, "error": up.err().unwrap_or_default(), "exit": Value::Null, "autostart": false, "workspace": s.workspace,
                "data_dir": self.dir.to_string_lossy(), "log": [], "webhook_key": self.webhook_key(),
            });
        }
        let mut r = self.run.lock().unwrap();
        Self::reap(&mut r);
        let node = match find_node(&s.node) {
            Ok((p, v)) => {
                json!({"ok": true, "path": p.to_string_lossy(), "version": v, "error": ""})
            }
            Err(e) => json!({"ok": false, "path": "", "version": "", "error": e}),
        };
        let (path, managed) = self.runner_path(s);
        let problem = runner_problem(&path, managed);
        let have = self.installed_pin();
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
            "remote": false, "url": "", "token_set": false, "installed": problem.is_none(), "problem": problem.unwrap_or_default(), "runner_dir": s.runner_dir, "node": node,
            "managed": managed, "path": path.to_string_lossy(), "pin": durable_pin(), "installed_pin": have,
            "update": managed && !have.is_empty() && have != durable_pin(),
            "installing": self.installing.load(std::sync::atomic::Ordering::SeqCst),
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
        desktop_token: &str,
    ) -> Result<(), String> {
        let mut r = self.run.lock().unwrap();
        Self::reap(&mut r);
        if r.child.is_some() {
            return Ok(());
        }
        let (runner, managed) = self.runner_path(s);
        if let Some(p) = runner_problem(&runner, managed) {
            return Err(p);
        }
        if studio_port == 0 {
            return Err("FLE Link is not serving yet.".into());
        }
        let (node, _) = find_node(&s.node)?;
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
            // its tools (git, npm, node scripts) need the same Node, also when FLE Link was started without the user's PATH
            .env("PATH", path_with_node(&node))
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
            // computer use (desktop.rs): the tools answer "locked" until the person at the Agent page allows it
            .env("FLE_DURABLE_DESKTOP", "1")
            .env(
                "FLE_DURABLE_DESKTOP_URL",
                format!("http://127.0.0.1:{studio_port}/desktop"),
            )
            .env("FLE_DURABLE_DESKTOP_TOKEN", desktop_token)
            .env("FLE_DURABLE_DESKTOP_TARGET", "mensch")
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
    /// The runner's live event stream for the chat (`/api/p/agent/stream`); it stays open, so no timeout.
    pub fn stream_open(&self) -> Result<ureq::Response, ApiError> {
        let (base, tok) = self.target()?;
        ureq::get(&format!("{base}/api/stream"))
            .set("Authorization", &format!("Bearer {tok}"))
            .call()
            .map_err(|e| ApiError(502, format!("FLE Durable's event stream: {e}")))
    }

    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        let (base, tok) = self.target()?;
        let req = ureq::request(method, &format!("{base}{path}"))
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
        ("POST", "abort") => Some(("POST", "/api/abort".into())),
        ("GET", "tickets") => Some(("GET", "/api/tickets".into())),
        ("POST", "tickets") => Some(("POST", "/api/tickets".into())),
        ("GET", "calendar") => Some(("GET", "/api/calendar".into())),
        ("POST", "calendar") => Some(("POST", "/api/calendar".into())),
        ("GET", "bus") => Some(("GET", "/api/bus".into())),
        ("GET", "bus/state") => Some(("GET", "/api/bus/state".into())),
        _ => {
            let (kind, tail) = rest.split_once('/')?;
            match (method, kind, tail.split_once('/')) {
                // a change to a ticket is a PATCH there; the page's kit has POST
                ("POST", "tickets", None) if id_ok(tail) => {
                    Some(("PATCH", format!("/api/tickets/{tail}")))
                }
                ("POST", "tickets", Some((id, "comments"))) if id_ok(id) => {
                    Some(("POST", format!("/api/tickets/{id}/comments")))
                }
                ("DELETE", "calendar", None) if id_ok(tail) => {
                    Some(("DELETE", format!("/api/calendar/{tail}")))
                }
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

/// The query a listing passes on: only these keys, and only values that need no escaping.
pub fn safe_query(query: &Map<String, Value>) -> String {
    let mut parts = vec![];
    for k in ["from", "to", "after", "limit", "topic", "status"] {
        let Some(v) = query.get(k).map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }) else {
            continue;
        };
        if !v.is_empty()
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b":.-_".contains(&b))
        {
            parts.push(format!("{k}={v}"));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
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
    if let Some(u) = body.get("url").and_then(Value::as_str) {
        cur.insert("url".into(), json!(u.trim().trim_end_matches('/')));
    }
    if let Some(f) = body.get("token_file").and_then(Value::as_str) {
        cur.insert("token_file".into(), json!(f.trim()));
    }
    // an empty field leaves a saved token alone; without an address there is nothing to keep it for
    if let Some(t) = body
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
    {
        cur.insert("token".into(), json!(t.trim()));
    }
    if cur
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .is_empty()
    {
        cur.remove("token");
        cur.remove("token_file");
    }
    if let Some(b) = body.get("autostart").and_then(Value::as_bool) {
        cur.insert("autostart".into(), json!(b));
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn node_is_found_beyond_a_short_path() {
        let dirs = node_dirs();
        for d in ["/opt/homebrew/bin", "/usr/local/bin"] {
            assert!(dirs.contains(&PathBuf::from(d)), "{d} {dirs:?}");
        }
        let p = path_with_node(Path::new("/x/node-22/bin/node"));
        assert!(
            std::env::split_paths(&p).next() == Some(PathBuf::from("/x/node-22/bin")),
            "{p:?}"
        );
    }

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
        assert!(runner_problem(Path::new("/nonexistent/app"), true)
            .unwrap()
            .contains("Install"));
        assert!(runner_problem(Path::new("/nonexistent/fle-durable"), false)
            .unwrap()
            .contains("src/main.js"));
    }

    #[test]
    fn a_connected_durable_is_used_with_its_token() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let mut c = c.unwrap();
                let mut b = [0u8; 4096];
                let n = c.read(&mut b).unwrap_or(0);
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&b[..n]).to_string());
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 9\r\nConnection: close\r\n\r\n{\"ok\":12}");
            }
        });
        let home =
            std::env::temp_dir().join(format!("flelink-agent-remote-{}", std::process::id()));
        let a = Agent::new(&home);
        a.update_settings(
            json!({"url": format!("{url}/"), "token": "tok-123"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let (s, _) = a.settings();
        assert_eq!(s.url, url, "the slash goes");
        assert!(a.running());
        let st = a.status(&s, false);
        assert_eq!(st["remote"], true);
        assert_eq!(st["running"], true);
        assert!(
            !st.to_string().contains("tok-123"),
            "the page never sees the token"
        );
        assert_eq!(
            a.call("GET", "/api/conversation", None).ok().unwrap()["ok"],
            12
        );
        let log = seen.lock().unwrap().join("\n");
        assert!(
            log.contains("GET /api/conversation") && log.contains("Bearer tok-123"),
            "{log}"
        );
        assert!(a
            .update_settings(json!({"url": "ftp://x"}).as_object().unwrap())
            .is_err());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn no_address_no_token_and_the_abort_route() {
        let mut cur = Map::new();
        cur = merge_settings(
            cur,
            json!({"url": "http://x", "token": "a"})
                .as_object()
                .unwrap(),
        );
        cur = merge_settings(
            cur,
            json!({"url": "http://x", "token": ""}).as_object().unwrap(),
        );
        assert_eq!(cur["token"], "a", "an empty field keeps the saved token");
        cur = merge_settings(cur, json!({"url": ""}).as_object().unwrap());
        assert!(!cur.contains_key("token"));
        assert_eq!(proxied("POST", "abort").unwrap().1, "/api/abort");
    }

    #[test]
    fn a_token_file_wins_and_is_read_at_each_use() {
        let home =
            std::env::temp_dir().join(format!("flelink-agent-tokfile-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let f = home.join("t");
        std::fs::write(&f, "aus-datei\n").unwrap();
        let a = Agent::new(&home);
        a.update_settings(
            json!({"url": "http://x", "token": "gespeichert", "token_file": f.to_string_lossy()})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(a.remote().unwrap().1, "aus-datei");
        std::fs::remove_file(&f).unwrap();
        assert_eq!(
            a.remote().unwrap().1,
            "gespeichert",
            "file gone: the saved token"
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn tickets_calendar_and_events_reach_the_agent_and_nothing_else() {
        assert_eq!(
            proxied("POST", "tickets/INC-P1").unwrap(),
            ("PATCH", "/api/tickets/INC-P1".to_string())
        );
        assert_eq!(
            proxied("DELETE", "calendar/c1").unwrap(),
            ("DELETE", "/api/calendar/c1".to_string())
        );
        assert_eq!(proxied("GET", "bus/state").unwrap().1, "/api/bus/state");
        assert_eq!(proxied("POST", "tickets/../x"), None);
        assert_eq!(proxied("DELETE", "tickets/INC-P1"), None);
        assert_eq!(proxied("POST", "bus"), None);
        let q =
            json!({"from": "2026-11-09T10:00:00.000Z", "after": 3, "evil": "x", "to": "a b&c=d"});
        assert_eq!(
            safe_query(q.as_object().unwrap()),
            "?from=2026-11-09T10:00:00.000Z&after=3"
        );
        assert_eq!(safe_query(&Map::new()), "");
    }

    #[test]
    fn durable_is_pinned_to_a_commit() {
        let p = durable_pin();
        assert_eq!(p.len(), 40);
        assert!(p.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn own_folder_wins_over_the_installed_copy() {
        let a = Agent::new(Path::new("/data"));
        let mut s = AgentSettings::default();
        assert_eq!(a.runner_path(&s), (PathBuf::from("/data/agent/app"), true));
        s.runner_dir = "/src/fle-durable".into();
        assert_eq!(
            a.runner_path(&s),
            (PathBuf::from("/src/fle-durable"), false)
        );
    }
}
