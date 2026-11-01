//! A small MCP client (Model Context Protocol): the servers named in the tool settings become tools in the chat. Port of
//! `fle_studio/mcp.py` (stdio and Streamable HTTP; the sign-in, the older HTTP+SSE revision and MCP Apps come later).
//!
//! Servers are written in the usual `mcpServers` JSON (`tools::parse_servers`):
//!
//! ```json
//! {"mcpServers": {
//!     "files":  {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/some/folder"]},
//!     "remote": {"url": "https://example.com/mcp", "headers": {"Authorization": "Bearer …"}}
//! }}
//! ```
//!
//! * stdio: a program FLE Link starts and talks to over its standard input and output (JSON-RPC, one message per line).
//! * Streamable HTTP: a URL that takes JSON-RPC in POST requests and answers with JSON or an event stream.
//!
//! Servers connect in the background and stay connected until the settings change or FLE Link ends. The last tool list of
//! each server is remembered (`<data>/mcp-tools.json`), so the tools in a request do not change while a server is still
//! starting (llama-server puts the tools at the front of the prompt; another list would cost the whole prompt cache).
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PROTOCOL: &str = "2025-11-25";
/// a first `npx -y …` may have to download the server
const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
pub const MAX_RESULT_CHARS: usize = 60_000;

/// A server could not be reached or answered with an error; `status` is the HTTP status when there was one.
#[derive(Debug, Clone)]
pub struct McpError {
    pub message: String,
    pub status: Option<u16>,
}

impl McpError {
    fn new(m: impl Into<String>) -> McpError {
        McpError {
            message: m.into(),
            status: None,
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

type R<T> = Result<T, McpError>;

fn secs(d: Duration) -> String {
    crate::launch::fmt_g(d.as_secs_f64())
}

/// The `result` of a JSON-RPC answer, or its error as McpError.
fn result_of(reply: Value) -> R<Value> {
    if let Some(err) = reply.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| err.to_string());
        return Err(McpError::new(msg));
    }
    Ok(reply.get("result").cloned().unwrap_or(Value::Null))
}

trait Transport: Send + Sync {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> R<Value>;
    fn notify(&self, method: &str) -> R<()>;
    fn alive(&self) -> bool;
    fn close(&self);
    /// Streamable HTTP keeps the protocol version the server answered with
    fn set_version(&self, _v: &str) {}
}

// ------------------------------------------------------------------------------------------------ stdio

/// The program to start: `command` as a path, or found on the PATH of the environment the server gets.
fn which_in(command: &str, path: Option<&str>) -> Option<PathBuf> {
    let p = Path::new(command);
    if p.components().count() > 1 || p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = path
        .map(String::from)
        .or_else(|| std::env::var("PATH").ok())?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .map(|e| e.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for e in &exts {
            let c = dir.join(format!("{command}{e}"));
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

struct StdioServer {
    child: Mutex<Child>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    next: AtomicU64,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    closed: Arc<Mutex<String>>,
}

impl StdioServer {
    fn start(spec: &Value) -> R<StdioServer> {
        let env: Vec<(String, String)> = spec["env"]
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let name = spec["command"].as_str().unwrap_or("");
        let path = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.as_str());
        let program = which_in(name, path).ok_or_else(|| {
            McpError::new(format!(
                "The command '{name}' was not found. Is it installed and on the PATH?"
            ))
        })?;
        let mut cmd = Command::new(&program);
        cmd.args(
            spec["args"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default(),
        )
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        if let Some(cwd) = spec["cwd"].as_str().filter(|c| !c.is_empty()) {
            cmd.current_dir(cwd);
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: only async-signal-safe calls between fork and exec; the server ends when FLE Link ends
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000 | 0x0000_0200); // no console window, own process group
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| McpError::new(format!("Could not start '{name}': {e}")))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let stdin = child.stdin.take();
        let t = StdioServer {
            child: Mutex::new(child),
            stdin: Arc::new(Mutex::new(stdin)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next: AtomicU64::new(0),
            stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
            closed: Arc::new(Mutex::new(String::new())),
        };
        let tail = t.stderr_tail.clone();
        std::thread::Builder::new()
            .name("mcp-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let line = line.trim_end().to_string();
                    if !line.is_empty() {
                        let mut q = tail.lock().unwrap();
                        q.push_back(line.chars().take(300).collect());
                        if q.len() > 30 {
                            q.pop_front();
                        }
                    }
                }
            })
            .map_err(|e| McpError::new(e.to_string()))?;
        let (pending, tail, closed) = (t.pending.clone(), t.stderr_tail.clone(), t.closed.clone());
        let input = t.stdin.clone();
        std::thread::Builder::new()
            .name("mcp-stdout".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(msg) = serde_json::from_str::<Value>(line) else {
                        let mut q = tail.lock().unwrap();
                        q.push_back(format!(
                            "(not JSON on stdout) {}",
                            line.chars().take(200).collect::<String>()
                        ));
                        continue;
                    };
                    let id = msg.get("id").cloned().unwrap_or(Value::Null);
                    if msg.get("method").is_some() {
                        if !id.is_null() {
                            let answer = if msg["method"] == "ping" {
                                json!({"jsonrpc": "2.0", "id": id, "result": {}})
                            } else {
                                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "FLE Link does not offer that"}})
                            };
                            if let Some(w) = input.lock().unwrap().as_mut() {
                                let _ = w.write_all(format!("{answer}\n").as_bytes()).and_then(|_| w.flush());
                            }
                        }
                        continue; // a notification (progress, list changes): not needed
                    }
                    if let Some(n) = id.as_u64() {
                        if let Some(w) = pending.lock().unwrap().remove(&n) {
                            let _ = w.send(msg);
                        }
                    }
                }
                let t: Vec<String> = tail.lock().unwrap().iter().cloned().collect();
                let t = t[t.len().saturating_sub(4)..].join(" | ");
                let why = if t.is_empty() {
                    "The server exited".to_string()
                } else {
                    format!("The server exited: {t}")
                };
                {
                    let mut c = closed.lock().unwrap();
                    if c.is_empty() {
                        *c = why.clone();
                    }
                }
                for (_, w) in pending.lock().unwrap().drain() {
                    let _ = w.send(json!({"error": {"code": -32000, "message": why}}));
                }
            })
            .map_err(|e| McpError::new(e.to_string()))?;
        Ok(t)
    }

    fn tail(&self) -> String {
        let q = self.stderr_tail.lock().unwrap();
        let v: Vec<&String> = q.iter().collect();
        v[v.len().saturating_sub(4)..]
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }

    fn send(&self, msg: &Value) -> R<()> {
        let tail = self.tail();
        let suffix = if tail.is_empty() {
            String::new()
        } else {
            format!(": {tail}")
        };
        if !self.alive() {
            return Err(McpError::new(format!("The server is not running{suffix}")));
        }
        let mut stdin = self.stdin.lock().unwrap();
        let Some(w) = stdin.as_mut() else {
            return Err(McpError::new(format!("The server is not running{suffix}")));
        };
        let data = format!("{}\n", serde_json::to_string(msg).unwrap_or_default());
        w.write_all(data.as_bytes())
            .and_then(|_| w.flush())
            .map_err(|_| McpError::new(format!("The server closed its input{suffix}")))
    }
}

impl Transport for StdioServer {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> R<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        let r = self
            .send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .and_then(|_| {
                rx.recv_timeout(timeout).map_err(|_| {
                    McpError::new(format!("No answer to {method} within {} s", secs(timeout)))
                })
            });
        self.pending.lock().unwrap().remove(&id);
        result_of(r?)
    }

    fn notify(&self, method: &str) -> R<()> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": {}}))
    }

    fn alive(&self) -> bool {
        self.closed.lock().unwrap().is_empty()
            && matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    fn close(&self) {
        {
            let mut c = self.closed.lock().unwrap();
            if c.is_empty() {
                *c = "closed".into();
            }
        }
        self.stdin.lock().unwrap().take(); // most servers quit when their input ends
        let mut child = self.child.lock().unwrap();
        let wait = |c: &mut Child, ms: u64| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(ms) {
                if !matches!(c.try_wait(), Ok(None)) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };
        if wait(&mut child, 1500) {
            return;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        if !wait(&mut child, 2000) {
            #[cfg(windows)]
            {
                // terminate ends only the .cmd shim, not the program behind it
                let _ = Command::new("taskkill")
                    .args(["/PID", &child.id().to_string(), "/T", "/F"])
                    .output();
            }
            let _ = child.kill();
            wait(&mut child, 2000);
        }
    }
}

// ------------------------------------------------------------------------------------------------ Streamable HTTP

struct Http {
    url: String,
    headers: Vec<(String, String)>,
    session: Mutex<String>,
    version: Mutex<String>,
    next: AtomicU64,
}

impl Http {
    fn new(spec: &Value) -> Http {
        Http {
            url: spec["url"].as_str().unwrap_or("").to_string(),
            headers: spec["headers"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            session: Mutex::new(String::new()),
            version: Mutex::new(String::new()),
            next: AtomicU64::new(0),
        }
    }

    fn host(&self) -> String {
        self.url
            .split_once("://")
            .map(|(_, r)| r.split(['/', '?']).next().unwrap_or("").to_string())
            .unwrap_or_default()
    }

    fn post(&self, payload: &Value, timeout: Duration, want: Option<u64>) -> R<Option<Value>> {
        let agent = ureq::AgentBuilder::new().timeout(timeout).build();
        let mut req = agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream")
            .set(
                "User-Agent",
                &format!("flelink/{}", env!("CARGO_PKG_VERSION")),
            );
        for (k, v) in &self.headers {
            req = req.set(k, v);
        }
        let session = self.session.lock().unwrap().clone();
        if !session.is_empty() {
            req = req.set("Mcp-Session-Id", &session);
        }
        let version = self.version.lock().unwrap().clone();
        if !version.is_empty() {
            req = req.set("MCP-Protocol-Version", &version);
        }
        let deadline = Instant::now() + timeout;
        let resp = match req.send_string(&payload.to_string()) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let body: String = r
                    .into_string()
                    .unwrap_or_default()
                    .trim()
                    .chars()
                    .take(300)
                    .collect();
                return Err(McpError {
                    message: format!("HTTP {code} from the server")
                        + &if body.is_empty() {
                            String::new()
                        } else {
                            format!(": {body}")
                        },
                    status: Some(code),
                });
            }
            Err(e) => {
                return Err(McpError::new(format!(
                    "Could not reach {}: {e}",
                    self.host()
                )))
            }
        };
        if let Some(s) = resp.header("Mcp-Session-Id") {
            *self.session.lock().unwrap() = s.to_string();
        }
        let Some(want) = want else {
            let _ = resp.into_string();
            return Ok(None);
        };
        let ctype = resp.content_type().to_lowercase();
        if ctype.contains("text/event-stream") {
            let mut data: Vec<String> = vec![];
            for line in BufReader::new(resp.into_reader()).lines() {
                if Instant::now() > deadline {
                    break;
                }
                let line = line.map_err(|e| McpError::new(e.to_string()))?;
                let line = line.trim_end_matches(['\r', '\n']);
                if let Some(d) = line.strip_prefix("data:") {
                    data.push(d.trim_start_matches(' ').to_string());
                } else if line.is_empty() && !data.is_empty() {
                    let text = data.join("\n");
                    data.clear();
                    let Ok(item) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    let items = match item {
                        Value::Array(a) => a,
                        v => vec![v],
                    };
                    for one in items {
                        if one["id"].as_u64() == Some(want)
                            && (one.get("result").is_some() || one.get("error").is_some())
                        {
                            return Ok(Some(one));
                        }
                    }
                }
            }
            return Err(McpError::new("The event stream ended without an answer"));
        }
        let text = resp
            .into_string()
            .map_err(|e| McpError::new(format!("No answer within {} s ({e})", secs(timeout))))?;
        let doc: Value = serde_json::from_str(&text).map_err(|e| {
            McpError::new(format!(
                "The server answered with something that is not JSON ({e})"
            ))
        })?;
        let items = match doc {
            Value::Array(a) => a,
            v => vec![v],
        };
        items
            .into_iter()
            .find(|i| i["id"].as_u64() == Some(want))
            .map(Some)
            .ok_or_else(|| McpError::new("The server's answer did not match the request"))
    }
}

impl Transport for Http {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> R<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let reply = self.post(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
            timeout,
            Some(id),
        )?;
        result_of(reply.unwrap_or(json!({})))
    }

    fn notify(&self, method: &str) -> R<()> {
        self.post(
            &json!({"jsonrpc": "2.0", "method": method, "params": {}}),
            Duration::from_secs(15),
            None,
        )
        .map(|_| ())
    }

    fn alive(&self) -> bool {
        true
    }

    fn close(&self) {
        let session = std::mem::take(&mut *self.session.lock().unwrap());
        if session.is_empty() {
            return;
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build();
        let mut req = agent.delete(&self.url).set("Mcp-Session-Id", &session);
        for (k, v) in &self.headers {
            req = req.set(k, v);
        }
        let _ = req.call();
    }

    fn set_version(&self, v: &str) {
        *self.version.lock().unwrap() = v.to_string();
    }
}

// ------------------------------------------------------------------------------------------------ one server

/// `json.dumps(v, ensure_ascii=False)`: separators ", " and ": ", as the Python Studio hands structured results over.
fn py_dumps(v: &Value) -> String {
    match v {
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(py_dumps).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), py_dumps(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        v => v.to_string(),
    }
}

/// (text, is_error) from a `tools/call` result. Pictures and other media are named, not included.
pub fn result_text(result: &Value) -> (String, bool) {
    let Some(r) = result.as_object() else {
        let s = match result {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        return (s, false);
    };
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let mut parts: Vec<String> = vec![];
    for item in r
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item["type"].as_str() {
            Some("text") => parts.push(s(&item["text"])),
            Some("image") => parts.push(format!("[image {} not shown]", s(&item["mimeType"]))),
            Some("audio") => parts.push(format!("[audio {} not shown]", s(&item["mimeType"]))),
            Some("resource_link") => parts.push(format!("[resource {}]", s(&item["uri"]))),
            Some("resource") => {
                let res = &item["resource"];
                parts.push(match res["text"].as_str() {
                    Some(t) => t.to_string(),
                    None => format!("[resource {}]", s(&res["uri"])),
                });
            }
            _ => {}
        }
    }
    let mut text = parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        if let Some(sc) = r.get("structuredContent").filter(|v| !v.is_null()) {
            text = py_dumps(sc);
        }
    }
    if text.chars().count() > MAX_RESULT_CHARS {
        text = text.chars().take(MAX_RESULT_CHARS).collect::<String>()
            + &format!("\n[cut off after {MAX_RESULT_CHARS} characters]");
    }
    (
        text,
        r.get("isError").is_some_and(crate::settings::py_truthy),
    )
}

#[derive(Clone)]
pub struct ServerState {
    /// idle | connecting | ready | auth_required | error
    pub status: String,
    pub error: String,
    pub tools: Vec<Value>,
    pub info: Value,
}

pub struct McpServer {
    pub name: String,
    pub spec: Value,
    call_timeout: Mutex<Duration>,
    state: Mutex<ServerState>,
    transport: Mutex<Option<Arc<dyn Transport>>>,
    connect_lock: Mutex<()>,
}

impl McpServer {
    fn new(name: &str, spec: Value, call_timeout: Duration) -> McpServer {
        McpServer {
            name: name.to_string(),
            spec,
            call_timeout: Mutex::new(call_timeout),
            state: Mutex::new(ServerState {
                status: "idle".into(),
                error: String::new(),
                tools: vec![],
                info: json!({}),
            }),
            transport: Mutex::new(None),
            connect_lock: Mutex::new(()),
        }
    }

    pub fn state(&self) -> ServerState {
        self.state.lock().unwrap().clone()
    }

    fn set(&self, f: impl FnOnce(&mut ServerState)) {
        f(&mut self.state.lock().unwrap());
    }

    fn handshake(&self, t: &Arc<dyn Transport>) -> R<()> {
        let result = t.request(
            "initialize",
            json!({"protocolVersion": PROTOCOL, "capabilities": {},
                "clientInfo": {"name": "flelink", "version": env!("CARGO_PKG_VERSION")}}),
            CONNECT_TIMEOUT,
        )?;
        let info = result
            .get("serverInfo")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(json!({}));
        t.set_version(result["protocolVersion"].as_str().unwrap_or(PROTOCOL));
        t.notify("notifications/initialized")?;
        let mut tools: Vec<Value> = vec![];
        let mut cursor = String::new();
        for _ in 0..20 {
            let params = if cursor.is_empty() {
                json!({})
            } else {
                json!({"cursor": cursor})
            };
            let r = t.request("tools/list", params, Duration::from_secs(30))?;
            tools.extend(
                r["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|t| t["name"].is_string())
                    .cloned(),
            );
            cursor = r["nextCursor"].as_str().unwrap_or("").to_string();
            if cursor.is_empty() {
                break;
            }
        }
        self.set(|s| {
            s.info = info;
            s.tools = tools;
        });
        Ok(())
    }

    /// Start the server and read its tool list; sets status and error instead of failing.
    pub fn connect(&self) {
        let _g = self.connect_lock.lock().unwrap();
        self.set(|s| {
            s.status = "connecting".into();
            s.error.clear();
        });
        let kind = self.spec["transport"].as_str().unwrap_or("");
        let t: Arc<dyn Transport> = match kind {
            "stdio" => match StdioServer::start(&self.spec) {
                Ok(t) => Arc::new(t),
                Err(e) => {
                    self.set(|s| {
                        s.status = "error".into();
                        s.error = e.message;
                        s.tools.clear();
                    });
                    return;
                }
            },
            "sse" => {
                self.set(|s| {
                    s.status = "error".into();
                    s.error = "Servers of the older HTTP+SSE revision are not supported by FLE Link yet; use the server's Streamable HTTP address.".into();
                });
                return;
            }
            _ => Arc::new(Http::new(&self.spec)),
        };
        *self.transport.lock().unwrap() = Some(t.clone());
        match self.handshake(&t) {
            Ok(()) => self.set(|s| s.status = "ready".into()),
            Err(e) => {
                t.close();
                self.set(|s| {
                    s.tools.clear();
                    if e.status == Some(401) {
                        s.status = "auth_required".into();
                        s.error = "This server wants a sign-in, which FLE Link does not do yet. Put an access token into its \"headers\" ({\"Authorization\": \"Bearer …\"}).".into();
                    } else {
                        s.status = "error".into();
                        s.error = e.message;
                    }
                });
            }
        }
    }

    fn ready_transport(&self) -> R<Arc<dyn Transport>> {
        let st = self.state();
        let t = self.transport.lock().unwrap().clone();
        let suffix = |e: &str| {
            if e.is_empty() {
                String::new()
            } else {
                format!(": {e}")
            }
        };
        let Some(t) = t.filter(|_| st.status == "ready") else {
            return Err(McpError::new(format!(
                "The server '{}' is not connected{}",
                self.name,
                suffix(&st.error)
            )));
        };
        if t.alive() {
            return Ok(t);
        }
        // it exited since the last call: one new start, before anything was sent
        self.connect();
        let st = self.state();
        match self.transport.lock().unwrap().clone() {
            Some(t) if st.status == "ready" => Ok(t),
            _ => Err(McpError::new(format!(
                "The server '{}' stopped and did not start again: {}",
                self.name, st.error
            ))),
        }
    }

    /// Run a tool; the server's whole result (content, structuredContent, isError, _meta).
    pub fn call_raw(&self, tool: &str, arguments: &Value) -> R<Value> {
        let t = self.ready_transport()?;
        let timeout = *self.call_timeout.lock().unwrap();
        let r = t.request(
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
            timeout,
        )?;
        Ok(if r.is_object() {
            r
        } else {
            json!({"content": [{"type": "text", "text": match r { Value::Null => String::new(), Value::String(s) => s, v => v.to_string() }}]})
        })
    }

    pub fn close(&self) {
        if let Some(t) = self.transport.lock().unwrap().take() {
            t.close();
        }
        self.set(|s| s.status = "idle".into());
    }
}

// ------------------------------------------------------------------------------------------------ all servers

#[derive(Default)]
pub struct McpManager {
    servers: Mutex<BTreeMap<String, Arc<McpServer>>>,
}

impl McpManager {
    /// Make the connected servers match `specs`: changed or removed ones close, new ones connect in the background.
    pub fn sync(&self, specs: &Map<String, Value>, call_timeout: Duration) {
        let mut servers = self.servers.lock().unwrap();
        let gone: Vec<String> = servers
            .iter()
            .filter(|(n, s)| specs.get(*n) != Some(&s.spec))
            .map(|(n, _)| n.clone())
            .collect();
        for n in gone {
            let s = servers.remove(&n).unwrap();
            std::thread::spawn(move || s.close());
        }
        for (name, spec) in specs {
            match servers.get(name) {
                Some(s) => *s.call_timeout.lock().unwrap() = call_timeout,
                None => {
                    let s = Arc::new(McpServer::new(name, spec.clone(), call_timeout));
                    servers.insert(name.clone(), s.clone());
                    s.set(|st| st.status = "connecting".into());
                    let _ = std::thread::Builder::new()
                        .name(format!("mcp-{name}"))
                        .spawn(move || s.connect());
                }
            }
        }
    }

    pub fn reconnect(&self, name: Option<&str>) {
        let targets: Vec<Arc<McpServer>> = self
            .servers
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| name.is_none_or(|x| x == *n))
            .map(|(_, s)| s.clone())
            .collect();
        for s in targets {
            s.set(|st| st.status = "connecting".into());
            std::thread::spawn(move || {
                s.close();
                s.connect();
            });
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<McpServer>> {
        self.servers.lock().unwrap().get(name).cloned()
    }

    pub fn servers(&self) -> Vec<Arc<McpServer>> {
        self.servers.lock().unwrap().values().cloned().collect()
    }

    /// Wait until no server is still connecting.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < timeout {
            if !self
                .servers()
                .iter()
                .any(|s| matches!(s.state().status.as_str(), "idle" | "connecting"))
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    pub fn close_all(&self) {
        let all: Vec<Arc<McpServer>> = std::mem::take(&mut *self.servers.lock().unwrap())
            .into_values()
            .collect();
        for s in all {
            s.close();
        }
    }
}

// ------------------------------------------------------------------------------------------------ the tool box

/// An MCP input schema as llama-server likes it: an object schema without the `$schema` marker.
pub fn simplify_schema(schema: &Value) -> Value {
    let Some(o) = schema
        .as_object()
        .filter(|o| o.get("type").is_none_or(|t| t == "object"))
    else {
        return json!({"type": "object", "properties": {}});
    };
    let mut out: Map<String, Value> = o
        .iter()
        .filter(|(k, _)| *k != "$schema" && *k != "$id")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    out.insert("type".into(), json!("object"));
    out.entry("properties").or_insert(json!({}));
    Value::Object(out)
}

/// The name a server's tool gets in the chat: `server__tool`, safe characters only, at most 64, unique.
pub fn wire_name(server: &str, tool: &str, taken: &std::collections::HashSet<String>) -> String {
    let clean: String = format!("{server}__{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base: String = clean.chars().take(64).collect();
    let (mut name, mut n) = (base.clone(), 2);
    while taken.contains(&name) {
        let suffix = format!("_{n}");
        name = base.chars().take(64 - suffix.len()).collect::<String>() + &suffix;
        n += 1;
    }
    name
}

/// Who may call a tool (MCP Apps, `_meta.ui.visibility`): the model, the app, or both (the default).
pub fn tool_visibility(t: &Value) -> Vec<String> {
    match t["_meta"]["ui"]["visibility"]
        .as_array()
        .filter(|v| !v.is_empty())
    {
        Some(v) => v
            .iter()
            .filter_map(Value::as_str)
            .filter(|x| *x == "model" || *x == "app")
            .map(String::from)
            .collect(),
        None => vec!["model".into(), "app".into()],
    }
}

/// The ui:// resource that shows a tool's result.
pub fn tool_ui_uri(t: &Value) -> String {
    let meta = &t["_meta"];
    [
        &meta["ui"]["resourceUri"],
        &meta["ui/resourceUri"],
        &meta["openai/outputTemplate"],
    ]
    .iter()
    .filter_map(|v| v.as_str())
    .find(|v| v.starts_with("ui://"))
    .unwrap_or("")
    .to_string()
}

/// The tools that are on right now: the built-in ones and those of the MCP servers, read from the settings every time.
pub struct ToolBox {
    pub mcp: McpManager,
    known: Mutex<Map<String, Value>>,
    known_file: PathBuf,
}

/// Why a call did not run: 404 (unknown tool) or 409 (needs approval).
pub enum CallRefused {
    Unknown,
    NeedsApproval,
}

impl ToolBox {
    pub fn new(data_dir: &Path) -> ToolBox {
        let known_file = data_dir.join("mcp-tools.json");
        let known = std::fs::read_to_string(&known_file)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        ToolBox {
            mcp: McpManager::default(),
            known: Mutex::new(known),
            known_file,
        }
    }

    /// Make the connected servers match the settings; the problems found in the JSON text.
    pub fn sync(&self, s: &Value) -> Vec<(String, String)> {
        let on = |k: &str| s.get(k).is_some_and(crate::settings::py_truthy);
        let text = s
            .get("mcp_json")
            .filter(|v| crate::settings::py_truthy(v))
            .map(crate::settings::py_str)
            .unwrap_or_default();
        let (mut servers, problems) = crate::tools::parse_servers(&text);
        if !(on("enabled") && on("mcp_enabled")) {
            servers = Map::new();
        }
        let timeout = s
            .get("call_timeout")
            .filter(|v| crate::settings::py_truthy(v))
            .and_then(Value::as_f64)
            .unwrap_or(60.0)
            .max(1.0);
        self.mcp.sync(&servers, Duration::from_secs_f64(timeout));
        problems
    }

    fn trusted(s: &Value) -> Vec<String> {
        s.get("mcp_trusted")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(crate::settings::py_str).collect())
            .unwrap_or_default()
    }

    /// (tools, stale): a ready server gives its own list (remembered); one still connecting gives the list it had last
    /// time, as long as its settings are the same.
    fn server_tools(&self, server: &McpServer) -> (Vec<Value>, bool) {
        let st = server.state();
        if st.status == "ready" {
            let mut tools = st.tools.clone();
            tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            let mut known = self.known.lock().unwrap();
            let entry = known.get(&server.name);
            if entry.is_none_or(|e| e["spec"] != server.spec || e["tools"] != json!(tools)) {
                known.insert(
                    server.name.clone(),
                    json!({"spec": server.spec, "tools": tools}),
                );
                let tmp = self.known_file.with_extension("tmp");
                // only a cache; the next start simply has no earlier list when this fails
                if std::fs::write(&tmp, Value::Object(known.clone()).to_string()).is_ok() {
                    let _ = std::fs::rename(&tmp, &self.known_file);
                }
            }
            return (tools, false);
        }
        if st.status == "idle" || st.status == "connecting" {
            let known = self.known.lock().unwrap();
            if let Some(e) = known.get(&server.name) {
                if e["spec"] == server.spec {
                    if let Some(t) = e["tools"].as_array() {
                        return (
                            t.iter()
                                .filter(|t| t["name"].is_string())
                                .cloned()
                                .collect(),
                            true,
                        );
                    }
                }
            }
        }
        (vec![], false)
    }

    pub fn tools(&self, s: &Value) -> Vec<crate::tools::Tool> {
        let mut out = crate::tools::tools(s);
        let on = |k: &str| s.get(k).is_some_and(crate::settings::py_truthy);
        if on("enabled") && on("mcp_enabled") {
            self.sync(s);
            let mut taken: std::collections::HashSet<String> =
                out.iter().map(|t| t.name.clone()).collect();
            let trusted = Self::trusted(s);
            for server in self.mcp.servers() {
                let (raw, stale) = self.server_tools(&server);
                for t in raw {
                    if !tool_visibility(&t).iter().any(|v| v == "model") {
                        continue; // a tool only the app may call
                    }
                    let remote = t["name"].as_str().unwrap_or("").to_string();
                    let name = wire_name(&server.name, &remote, &taken);
                    taken.insert(name.clone());
                    let desc = [&t["description"], &t["title"]]
                        .iter()
                        .filter_map(|v| v.as_str())
                        .find(|v| !v.is_empty())
                        .unwrap_or(&remote)
                        .to_string();
                    out.push(crate::tools::Tool {
                        name,
                        description: format!("[{}] {desc}", server.name)
                            .chars()
                            .take(1500)
                            .collect(),
                        parameters: simplify_schema(&t["inputSchema"]),
                        group: "mcp",
                        confirm: !trusted.contains(&server.name),
                        source: "mcp",
                        server: server.name.clone(),
                        remote_name: remote,
                        ui: tool_ui_uri(&t),
                        stale,
                    });
                }
            }
        }
        // by name, always: the tools are rendered at the front of the prompt, so another order is another prompt
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// What the page needs: the tools, the MCP servers and why something is missing.
    pub fn status(&self, s: &Value) -> Value {
        let on = |k: &str| s.get(k).is_some_and(crate::settings::py_truthy);
        let mut problems = self.sync(s);
        if !(on("enabled") && on("mcp_enabled")) {
            problems.clear(); // the servers are off, so a mistake in their text does not matter yet
        }
        let trusted = Self::trusted(s);
        let mut servers: Vec<Value> = self
            .mcp
            .servers()
            .iter()
            .map(|sv| {
                let st = sv.state();
                let remote = sv.spec["transport"] != "stdio";
                let auth = if remote {
                    json!({"remote": true, "required": st.status == "auth_required",
                        "client_id_set": sv.spec["oauth"]["client_id"].as_str().is_some_and(|c| !c.is_empty())})
                } else {
                    json!({"remote": false})
                };
                json!({"name": sv.name, "status": st.status, "error": st.error, "tools": st.tools.len(),
                    "transport": sv.spec["transport"], "trusted": trusted.contains(&sv.name),
                    "info": st.info["name"].as_str().unwrap_or(""), "auth": auth, "legacy_sse": false})
            })
            .collect();
        for (name, why) in problems {
            servers.push(json!({"name": if name.is_empty() { "(settings)".to_string() } else { name }, "status": "error",
                "error": why, "tools": 0, "transport": "", "trusted": false, "info": "", "auth": {"remote": false}, "legacy_sse": false}));
        }
        let mut out = crate::tools::status(s);
        out["tools"] = json!(self
            .tools(s)
            .iter()
            .map(crate::tools::Tool::public)
            .collect::<Vec<_>>());
        out["servers"] = json!(servers);
        out["connecting"] = json!(self
            .mcp
            .servers()
            .iter()
            .any(|sv| matches!(sv.state().status.as_str(), "idle" | "connecting")));
        out
    }

    /// Run one tool: {content, is_error, ms}.
    pub fn call(
        &self,
        s: &Value,
        name: &str,
        args: &Value,
        approved: bool,
    ) -> Result<Value, CallRefused> {
        let find = || self.tools(s).into_iter().find(|t| t.name == name);
        let mut tool = find();
        if let Some(t) = tool.as_ref().filter(|t| t.stale) {
            // a tool from the remembered list waits a moment for its server to finish connecting
            let end = Instant::now() + Duration::from_secs(20);
            while Instant::now() < end
                && self
                    .mcp
                    .get(&t.server)
                    .is_some_and(|sv| matches!(sv.state().status.as_str(), "idle" | "connecting"))
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            tool = find();
        }
        let tool = tool.ok_or(CallRefused::Unknown)?;
        if tool.confirm && !approved {
            return Err(CallRefused::NeedsApproval);
        }
        let mut args = args.clone();
        if let Value::String(t) = &args {
            args = if t.trim().is_empty() {
                json!({})
            } else {
                match serde_json::from_str::<Value>(t) {
                    Ok(v) => v,
                    Err(_) => {
                        return Ok(
                            json!({"content": "The arguments are not valid JSON.", "is_error": true, "ms": 0}),
                        )
                    }
                }
            };
        }
        if !args.is_object() {
            return Ok(
                json!({"content": "The arguments must be a JSON object.", "is_error": true, "ms": 0}),
            );
        }
        let t0 = Instant::now();
        let (content, is_error) = if tool.source == "mcp" {
            match self.mcp.get(&tool.server) {
                None => (
                    format!("The server '{}' is not connected", tool.server),
                    true,
                ),
                Some(sv) => match sv.call_raw(&tool.remote_name, &args) {
                    Ok(raw) => result_text(&raw),
                    Err(e) => (e.message, true),
                },
            }
        } else {
            match crate::tools::run(&tool, s, &args) {
                Ok(c) => (c, false),
                Err(e) => (e.0, true),
            }
        };
        Ok(json!({"content": content, "is_error": is_error, "ms": t0.elapsed().as_millis() as i64}))
    }

    /// `POST /api/tools/reload`: connect every server again.
    pub fn reload(&self, s: &Value) -> Value {
        self.sync(s);
        self.mcp.reconnect(None);
        self.mcp.wait_idle(Duration::from_secs(1));
        self.status(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_schemas_are_the_python_studios() {
        let taken: std::collections::HashSet<String> = ["files__read_file".to_string()].into();
        assert_eq!(
            wire_name("files", "read_file", &taken),
            "files__read_file_2"
        );
        assert_eq!(
            wire_name("my server", "do.it", &Default::default()),
            "my_server__do_it"
        );
        assert_eq!(
            simplify_schema(&json!({"$schema": "x", "type": "object", "required": ["a"]})),
            json!({"type": "object", "required": ["a"], "properties": {}})
        );
        assert_eq!(
            simplify_schema(&json!({"type": "string"})),
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            tool_visibility(&json!({"_meta": {"ui": {"visibility": ["app"]}}})),
            ["app"]
        );
        assert_eq!(
            tool_ui_uri(&json!({"_meta": {"openai/outputTemplate": "ui://x"}})),
            "ui://x"
        );
    }

    #[test]
    fn results_become_text() {
        let r = json!({"content": [{"type": "text", "text": "a"}, {"type": "image", "mimeType": "image/png"},
            {"type": "resource", "resource": {"uri": "file:///x"}}], "isError": true});
        assert_eq!(
            result_text(&r),
            (
                "a\n[image image/png not shown]\n[resource file:///x]".into(),
                true
            )
        );
        assert_eq!(
            result_text(&json!({"content": [], "structuredContent": {"n": 1}})).0,
            "{\"n\": 1}"
        );
    }
}
