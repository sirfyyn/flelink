//! `flelink mcp`: an MCP server on stdin/stdout, so a coding agent (Claude Code, Codex, OpenCode, pi, ...) can run FLE
//! Link itself: see what fits this machine, download and load a model, stop it, read the engine log, change settings, and
//! get the setting that points the agent at the loaded model.
//!
//! It works through the Studio's own `/api`, the same requests the page makes; the Studio (or the desktop app) says where
//! it is in `<data>/studio.json` (run.rs `announce`). When none runs, the first call starts `flelink studio` in the
//! background, which stays after the agent quits (it holds the loaded model; Ctrl+C or quitting the app ends it).
//! JSON-RPC 2.0, one message per line, protocol 2025-06-18.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PROTOCOL: &str = "2025-06-18";

struct Studio {
    base: String,
    token: String,
    http: reqwest::blocking::Client,
}

fn client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("flelink-mcp/", env!("CARGO_PKG_VERSION")))
        // host mode serves https with a certificate of its own; this client only ever talks to 127.0.0.1
        .danger_accept_invalid_certs(true)
        .build()
        .expect("http client")
}

impl Studio {
    fn get(&self, path: &str) -> Result<Value> {
        let r = self.http.get(format!("{}{path}", self.base)).send()?;
        answer(r)
    }
    fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .header("X-FLE-Token", &self.token)
            .json(body)
            .send()?;
        answer(r)
    }
}

fn answer(r: reqwest::blocking::Response) -> Result<Value> {
    let status = r.status();
    let v: Value = r.json().unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = v["error"]["message"]
            .as_str()
            .or(v["error"].as_str())
            .unwrap_or("")
            .to_string();
        bail!("HTTP {}: {msg}", status.as_u16());
    }
    Ok(v)
}

/// The running Studio from `studio.json`, if its address still answers.
fn announced(home: &Path) -> Option<Studio> {
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("studio.json")).ok()?).ok()?;
    let base = v["url"].as_str()?.trim_end_matches('/').to_string();
    let token = v["token"].as_str()?.to_string();
    let s = Studio {
        base,
        token,
        http: client(Duration::from_secs(30)),
    };
    let ping = client(Duration::from_secs(2))
        .get(format!("{}/api/ping", s.base))
        .send()
        .ok()?;
    ping.status().is_success().then_some(s)
}

fn studio(home: &Path, explicit_home: bool) -> Result<Studio> {
    if let Some(s) = announced(home) {
        return Ok(s);
    }
    let exe = std::env::current_exe().context("where is flelink")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["studio", "--no-browser"]);
    if explicit_home {
        cmd.arg("--home").arg(home);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().context("could not start the Studio")?;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(300));
        if let Some(s) = announced(home) {
            return Ok(s);
        }
    }
    bail!("the Studio did not start within 30 s; run `flelink studio` to see why")
}

fn tools() -> Value {
    let none = json!({"type": "object", "properties": {}});
    json!([
        {"name": "status", "description": "What FLE Link is doing: engine state (stopped, starting, ready, crashed), the loaded model, context size, errors, and the API address agents use.", "inputSchema": none},
        {"name": "system", "description": "This machine: CPU, RAM, GPUs and the memory they may use, free disk in the models folder.", "inputSchema": none},
        {"name": "recipes", "description": "Models FLE Link can set up by itself (recipe id, title, size, whether it fits this machine, whether it is downloaded). Fitting ones first.", "inputSchema": none},
        {"name": "library", "description": "Model files already on this machine (id, name, size, complete).", "inputSchema": none},
        {"name": "download", "description": "Download the model of a recipe (resumes, checks SHA-256, links copies already on the machine). Returns at once; follow it with `downloads`.",
         "inputSchema": {"type": "object", "properties": {"recipe": {"type": "string", "description": "recipe id from `recipes`"}}, "required": ["recipe"]}},
        {"name": "downloads", "description": "Running and finished downloads with progress.", "inputSchema": none},
        {"name": "load", "description": "Load a model and wait until it answers: a recipe id (downloaded) or a model id from `library`. Installs the fitting engine first on a fresh machine. Stops the model loaded before.",
         "inputSchema": {"type": "object", "properties": {
            "recipe": {"type": "string", "description": "recipe id"},
            "model": {"type": "string", "description": "model id from `library`, when not a recipe"},
            "wait_s": {"type": "integer", "description": "how long to wait for ready (default 900)"}}}},
        {"name": "stop", "description": "Stop the loaded model (frees GPU and RAM).", "inputSchema": none},
        {"name": "requests", "description": "The last answers through FLE Link from any program (the Studio chat, Claude Code, Codex, apps): which API, prompt and output tokens, speed (tok/s, ~ when estimated from the wall clock), duration, HTTP status.",
         "inputSchema": {"type": "object", "properties": {"limit": {"type": "integer", "description": "default 20"}}}},
        {"name": "log", "description": "The last lines of the engine log (start command, loading, errors).",
         "inputSchema": {"type": "object", "properties": {"lines": {"type": "integer", "description": "default 60, at most 400"}}}},
        {"name": "settings", "description": "Read the Studio settings, or change some: pass `changes` as an object of the keys to set (the same keys `settings` returns).",
         "inputSchema": {"type": "object", "properties": {"changes": {"type": "object"}}}},
        {"name": "connect", "description": "The setting that points a coding agent or app at the loaded model: Claude Code (Anthropic API), Codex (Responses API), OpenCode, pi, or the plain addresses.",
         "inputSchema": {"type": "object", "properties": {"tool": {"type": "string", "enum": ["claude", "codex", "opencode", "pi", "api"]}}}},
        {"name": "ask", "description": "Send one prompt to the loaded model and return its answer with speed (tok/s): a quick check that the model works and how fast.",
         "inputSchema": {"type": "object", "properties": {"prompt": {"type": "string"}, "max_tokens": {"type": "integer", "description": "default 256"}}, "required": ["prompt"]}}
    ])
}

fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for k in keys {
        if let Some(x) = v.get(*k).filter(|x| !x.is_null()) {
            out.insert(k.to_string(), x.clone());
        }
    }
    Value::Object(out)
}

fn gb(b: &Value) -> Value {
    b.as_f64()
        .map(|x| json!((x / 1e9 * 10.0).round() / 10.0))
        .unwrap_or(Value::Null)
}

fn status(s: &Studio) -> Result<Value> {
    let e = s.get("/api/engine")?;
    let mut out = pick(&e, &["state", "phase", "error", "n_ctx", "load"]);
    out["model"] = pick(&e["model"], &["id", "name", "path"]);
    out["api"] = json!({"openai": format!("{}/v1", s.base), "anthropic": s.base});
    Ok(out)
}

/// A start that the server refuses because no engine is installed yet: install the first variant (the best fit for
/// this machine, as the page does), wait for it, then start again.
fn with_engine(s: &Studio, start: &dyn Fn() -> Result<Value>) -> Result<Value> {
    match start() {
        Err(e) if e.to_string().contains("Install an engine first") => {
            let st = s.get("/api/state")?;
            let v = st["engines"]["variants"][0].clone();
            let id = v["id"].as_str().ok_or_else(|| {
                anyhow!(
                    "no prebuilt engine fits this machine; set your own llama-server in the Studio"
                )
            })?;
            s.post("/api/engines/install", &json!({"variant": id}))?;
            let t0 = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(1500));
                let d = s.get("/api/downloads")?;
                let job = d["downloads"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|j| j["kind"] == "engine" && j["meta"]["variant"] == id);
                match job.and_then(|j| j["state"].as_str()) {
                    None | Some("done") => break,
                    Some("failed") | Some("cancelled") => bail!(
                        "installing the engine stopped: {}",
                        job.and_then(|j| j["error"].as_str()).unwrap_or("")
                    ),
                    _ if t0.elapsed() > Duration::from_secs(900) => {
                        bail!("installing the engine took over 15 minutes")
                    }
                    _ => {}
                }
            }
            start()
        }
        r => r,
    }
}

fn call(s: &Studio, name: &str, a: &Value) -> Result<Value> {
    let arg = |k: &str| a.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    Ok(match name {
        "status" => status(s)?,
        "system" => s.get("/api/system")?,
        "recipes" => {
            let r = s.get("/api/recipes")?;
            let mut list: Vec<Value> = r["recipes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| {
                    json!({"id": x["id"], "title": x["title"], "size_gb": gb(&x["size"]), "fits": x["fit"]["verdict"],
                        "advice": x["fit"]["advice"], "downloaded": x["downloaded"], "note": x["note"]})
                })
                .collect();
            let rank = |v: &Value| match v["fits"].as_str() {
                Some("good") => 0,
                Some("tight") => 1,
                _ => 2,
            };
            list.sort_by_key(rank);
            json!(list)
        }
        "library" => {
            let r = s.get("/api/models")?;
            json!(r["models"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| json!({"id": m["id"], "name": m["name"], "size_gb": gb(&m["size"]), "complete": m["complete"], "kind": m["kind"]}))
                .collect::<Vec<_>>())
        }
        "download" => {
            let id = arg("recipe");
            if id.is_empty() {
                bail!("give `recipe`");
            }
            s.post(&format!("/api/recipes/{id}/download"), &json!({}))?
        }
        "downloads" => {
            let r = s.get("/api/downloads")?;
            json!(r["downloads"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| json!({"id": d["id"], "kind": d["kind"], "title": d["title"], "state": d["state"],
                    "progress": d["progress"], "error": d["error"]}))
                .collect::<Vec<_>>())
        }
        "load" => {
            let (recipe, model) = (arg("recipe"), arg("model"));
            let start = || -> Result<Value> {
                if !recipe.is_empty() {
                    s.post(&format!("/api/recipes/{recipe}/start"), &json!({}))
                } else if !model.is_empty() {
                    s.post("/api/engine/start", &json!({"model": model}))
                } else {
                    bail!("give `recipe` or `model`")
                }
            };
            with_engine(s, &start)?;
            let wait = a.get("wait_s").and_then(|v| v.as_u64()).unwrap_or(900);
            let t0 = Instant::now();
            loop {
                let st = status(s)?;
                match st["state"].as_str() {
                    Some("ready") => break st,
                    Some("crashed") | Some("stopped") => {
                        bail!(
                            "the engine did not come up: {}",
                            st["error"].as_str().unwrap_or("see `log`")
                        )
                    }
                    _ if t0.elapsed() > Duration::from_secs(wait) => {
                        let mut st = st;
                        st["note"] = json!("still loading; ask `status` again");
                        break st;
                    }
                    _ => std::thread::sleep(Duration::from_secs(1)),
                }
            }
        }
        "requests" => {
            let n = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let r = s.get("/api/live/requests")?;
            json!(r["requests"]
                .as_array()
                .into_iter()
                .flatten()
                .take(n)
                .map(|x| pick(
                    x,
                    &[
                        "t",
                        "client",
                        "api",
                        "prompt_n",
                        "predicted_n",
                        "predicted_per_second",
                        "estimated",
                        "seconds",
                        "status"
                    ]
                ))
                .collect::<Vec<_>>())
        }
        "stop" => {
            s.post("/api/engine/stop", &json!({}))?;
            status(s)?
        }
        "log" => {
            let n = a
                .get("lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(60)
                .min(400) as usize;
            let r = s.get("/api/engine/log?since=0")?;
            let lines: Vec<&str> = r["lines"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["text"].as_str())
                .collect();
            json!(lines[lines.len().saturating_sub(n)..].join("\n"))
        }
        "settings" => match a.get("changes").filter(|c| c.is_object()) {
            Some(c) => s.post("/api/settings", c)?,
            None => s.get("/api/settings")?,
        },
        "connect" => {
            let model = flelink_core::connect::model_at(&s.base).unwrap_or_else(|| "local".into());
            let tool = arg("tool");
            let list: Vec<Value> = flelink_core::connect::snippets(&s.base, &model)
                .into_iter()
                .filter(|x| tool.is_empty() || x.id == tool)
                .map(|x| json!({"tool": x.title, "where": x.place, "text": x.text}))
                .collect();
            json!(list)
        }
        "ask" => {
            let prompt = arg("prompt");
            let max = a.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(256);
            let t0 = Instant::now();
            let r = client(Duration::from_secs(600))
                .post(format!("{}/v1/chat/completions", s.base))
                .json(
                    &json!({"messages": [{"role": "user", "content": prompt}], "max_tokens": max}),
                )
                .send()?;
            let v = answer(r)?;
            json!({"answer": v["choices"][0]["message"]["content"], "tokens": v["usage"]["completion_tokens"],
                "tok_s": v["timings"]["predicted_per_second"], "prompt_tok_s": v["timings"]["prompt_per_second"],
                "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0})
        }
        _ => bail!("no tool {name}"),
    })
}

pub fn serve(home: PathBuf, explicit_home: bool) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut conn: Option<Studio> = None;
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            let e = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}});
            writeln!(out, "{e}")?;
            out.flush()?;
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue; // a notification (initialized, cancelled): nothing to answer
        };
        let method = msg["method"].as_str().unwrap_or("");
        let reply = match method {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "flelink", "title": "FLE Link", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "FLE Link runs local models. `recipes` lists what fits this machine, `download` then `load` sets one up, \
                    `status` and `log` show what the engine does, `connect` gives the setting for a coding agent, `ask` tests the model."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let args = msg["params"]["arguments"].clone();
                let r = (|| -> Result<Value> {
                    if conn.as_ref().is_none_or(|c| announced_alive(c).is_none()) {
                        conn = Some(studio(&home, explicit_home)?);
                    }
                    call(conn.as_ref().unwrap(), name, &args)
                })();
                Ok(match r {
                    Ok(v) => {
                        let text = match &v {
                            Value::String(s) => s.clone(),
                            v => serde_json::to_string_pretty(v).unwrap_or_default(),
                        };
                        json!({"content": [{"type": "text", "text": text}]})
                    }
                    Err(e) => {
                        json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true})
                    }
                })
            }
            _ => Err(json!({"code": -32601, "message": format!("unknown method {method}")})),
        };
        let resp = match reply {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        };
        writeln!(out, "{resp}")?;
        out.flush()?;
    }
    Ok(())
}

/// The Studio of an earlier call still answers (it may have been quit in between).
fn announced_alive(s: &Studio) -> Option<()> {
    let r = client(Duration::from_secs(2))
        .get(format!("{}/api/ping", s.base))
        .send()
        .ok()?;
    r.status().is_success().then_some(())
}
