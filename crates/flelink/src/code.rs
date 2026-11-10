//! `flelink code`: work on the project in this folder with FLE Durable. FLE Durable stays its own program; this starts one
//! instance of it for the folder and talks to it over its API, the agent itself is not in here. Each folder keeps its own
//! conversation in `<data>/code/`, so the next `flelink code` in the same folder goes on where the last one stopped.
use anyhow::{bail, Context, Result};
use flelink_api::agent;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Project instructions the agent gets, the first file found; at most this many characters.
const PROJECT_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
const PROJECT_MAX: usize = 20_000;
const CONTEXT_DEFAULT: u64 = 32_768;

/// The agent's process, stopped when the session ends.
struct Durable {
    child: Child,
    base: String,
    token: String,
}

impl Drop for Durable {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What one event of the agent means for the terminal.
#[derive(Debug, PartialEq)]
enum Out {
    Text(String),
    Thinking(String),
    Tool(String),
    Busy,
    Done,
}

fn short(s: &str, n: usize) -> String {
    let line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > n {
        format!("{}…", line.chars().take(n).collect::<String>())
    } else {
        line
    }
}

/// One line for a tool call: the part that says what it does.
fn tool_line(name: &str, args: &Value) -> String {
    let main = ["command", "path", "file_path", "url", "action"]
        .iter()
        .find_map(|k| args[*k].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| args.to_string());
    format!("{name}: {}", short(&main, 160))
}

/// The text of a finished assistant message.
fn final_text(ev: &Value) -> String {
    let model = &ev["entry"]["model"];
    let message = if model.is_array() { &model[0] } else { model };
    if message["role"] != "assistant" {
        return String::new();
    }
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect(),
        _ => String::new(),
    }
}

/// `shown` counts the characters of the current message already printed: a batch can bring a whole block instead of deltas,
/// so what is still missing is printed when the message ends.
fn read_event(shown: &mut usize, ev: &Value) -> Vec<Out> {
    match ev["type"].as_str().unwrap_or("") {
        "message_start" => {
            *shown = 0;
            vec![]
        }
        "message_end" => {
            let rest: String = final_text(ev).chars().skip(*shown).collect();
            *shown = 0;
            if rest.is_empty() {
                vec![]
            } else {
                vec![Out::Text(rest)]
            }
        }
        "run_start" => vec![Out::Busy],
        "run_end" => vec![Out::Done],
        "tool_execution_start" => vec![Out::Tool(tool_line(
            ev["toolName"].as_str().unwrap_or("tool"),
            &ev["args"],
        ))],
        "message_update" => ev["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let delta = c["delta"].as_str().unwrap_or("").to_string();
                match c["type"].as_str() {
                    Some("text_delta") => {
                        *shown += delta.chars().count();
                        Some(Out::Text(delta))
                    }
                    Some("thinking_delta") => Some(Out::Thinking(delta)),
                    _ => None,
                }
            })
            .collect(),
        _ => vec![],
    }
}

/// A stable folder name for a project: readable, and different for two folders of the same name.
fn project_key(dir: &Path) -> String {
    let path = dir.to_string_lossy();
    let hash = path.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    let name: String = dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{name}-{hash:016x}")
}

/// What the agent is told about this session, with the project's own instructions when it has them.
fn instructions(project: &Path) -> String {
    let mut text = "This session is a coding session in a terminal: your owner works with you on the project in your working folder. Look at the code before you change it, keep changes small, and say what you changed.".to_string();
    for name in PROJECT_FILES {
        if let Ok(t) = std::fs::read_to_string(project.join(name)) {
            let t: String = t.chars().take(PROJECT_MAX).collect();
            text.push_str(&format!("\n\nThe project's {name}:\n{t}"));
            break;
        }
    }
    text
}

fn context_of(base: &str) -> Option<u64> {
    let c = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    let get = |path: &str| -> Option<Value> {
        c.get(format!("{base}{path}"))
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .ok()
    };
    if let Some(n) = get("/props").and_then(|p| p["default_generation_settings"]["n_ctx"].as_u64())
    {
        return Some(n);
    }
    get("/api/engine").and_then(|e| e["n_ctx"].as_u64())
}

const ABOUT: &str = "FLE Durable is the agent behind `flelink code` and the chat. It is its own program: it reads and changes files and runs commands\nin its working folder on this computer, and asks you before steps that cannot be undone. Everything stays on this computer.\nFLE Link fetches the version it was tested with into its data folder and installs its packages. It needs git and Node 22.19 or newer (nodejs.org).";

/// Before anything loads: remember an own FLE Durable folder, and fetch FLE Durable if it is not here yet.
pub fn prepare(home: &Path, durable: Option<PathBuf>, yes: bool) -> Result<()> {
    let ag = agent::Agent::new(home);
    if let Some(d) = durable {
        let mut change = serde_json::Map::new();
        change.insert("runner_dir".into(), json!(d.to_string_lossy()));
        ag.update_settings(&change).map_err(anyhow::Error::msg)?;
    }
    let (s, _) = ag.settings();
    let (path, managed) = ag.runner_path(&s);
    let Some(problem) = agent::runner_problem(&path, managed) else {
        return Ok(());
    };
    if !managed {
        bail!("{problem}");
    }
    eprintln!("{ABOUT}\n");
    agent::find_node(&s.node).map_err(anyhow::Error::msg)?;
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!("FLE Durable is not installed yet: run `flelink code --setup --yes`");
        }
        eprint!("Install it now? [Y/n] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if matches!(answer.trim(), "n" | "N" | "no" | "nein") {
            bail!("not installed");
        }
    }
    eprintln!("Installing FLE Durable, this takes a minute …");
    if let Err(e) = ag.install(&s) {
        for line in ag.recent_log() {
            eprintln!("{line}");
        }
        bail!("{e}");
    }
    eprintln!("FLE Durable is installed in {}.", path.display());
    Ok(())
}

/// `flelink code --setup`: set FLE Durable up and say what comes next.
pub fn setup(home: &Path, durable: Option<PathBuf>, yes: bool) -> Result<()> {
    prepare(home, durable, yes)?;
    eprintln!("Ready. In a project folder: flelink code <recipe>, or just flelink code when a model is running. In the app the chat now talks to the agent, once it runs on the Agent page.");
    Ok(())
}

fn start(home: &Path, project: &Path, base: &str, model: &str, window: u64) -> Result<Durable> {
    let ag = agent::Agent::new(home);
    let (s, _) = ag.settings();
    let (runner, _) = ag.runner_path(&s);
    let (node, _) = agent::find_node(&s.node).map_err(anyhow::Error::msg)?;
    let data = home.join("code").join(project_key(project));
    std::fs::create_dir_all(&data)?;
    let config = data.join("config.json");
    std::fs::write(
        &config,
        serde_json::to_vec_pretty(
            &json!({"agent": "code", "instructions": instructions(project)}),
        )?,
    )?;
    let port = agent::free_port().map_err(anyhow::Error::msg)?;
    let token = agent::token();
    let log = data.join("durable.log");
    let out = std::fs::File::create(&log)?;
    let mut child = Command::new(node)
        .arg("src/main.js")
        .current_dir(runner)
        .env("FLE_DURABLE_CONFIG", &config)
        .env("FLE_DURABLE_HOME", &data)
        .env("FLE_DURABLE_PORT", port.to_string())
        .env("FLE_DURABLE_TOKEN", &token)
        .env("FLE_DURABLE_WORKSPACE", project)
        .env("FLE_DURABLE_MODEL_URL", format!("{base}/v1"))
        .env("FLE_DURABLE_MODEL", model)
        .env("FLE_DURABLE_CONTEXT", window.to_string())
        .env("FLE_DURABLE_EVENTS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out.try_clone()?))
        .stderr(Stdio::from(out))
        .spawn()
        .context("could not start FLE Durable")?;
    let base = format!("http://127.0.0.1:{port}");
    let c = reqwest::blocking::Client::new();
    let t0 = Instant::now();
    loop {
        let up = c
            .get(format!("{base}/api/health"))
            .bearer_auth(&token)
            .timeout(Duration::from_secs(2))
            .send()
            .is_ok_and(|r| r.status().is_success());
        if up {
            return Ok(Durable { child, base, token });
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "FLE Durable stopped at the start ({status}); its log: {}",
                log.display()
            );
        }
        if t0.elapsed() > Duration::from_secs(45) {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "FLE Durable did not answer within 45 s; its log: {}",
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

enum Note {
    Ready(usize),
    Event(Out),
    Lost,
}

/// Reads the agent's event stream until it ends.
fn follow(base: &str, token: &str, tx: &mpsc::Sender<Note>) -> Result<()> {
    let resp = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()?
        .get(format!("{base}/api/stream"))
        .bearer_auth(token)
        .send()?
        .error_for_status()?;
    let mut event = String::new();
    let mut shown = 0;
    for line in BufReader::new(resp).lines() {
        let line = line?;
        if let Some(e) = line.strip_prefix("event:") {
            event = e.trim().to_string();
        } else if let Some(d) = line.strip_prefix("data:") {
            let v: Value = serde_json::from_str(d)?;
            if event == "snapshot" {
                let n = v["entries"].as_array().map_or(0, Vec::len);
                let _ = tx.send(Note::Ready(n));
            } else if event == "events" {
                for ev in v.as_array().into_iter().flatten() {
                    for out in read_event(&mut shown, ev) {
                        let _ = tx.send(Note::Event(out));
                    }
                }
            }
        }
    }
    Ok(())
}

struct Session {
    agent: Durable,
    http: reqwest::blocking::Client,
    notes: mpsc::Receiver<Note>,
    lines: mpsc::Receiver<String>,
    interrupted: Arc<AtomicBool>,
    /// where the model is served, for `/direct`
    served: (String, String),
    direct: Cell<bool>,
    direct_chat: RefCell<Vec<(String, String)>>,
}

impl Session {
    /// One question straight to the model, without the agent and its tools.
    fn ask_model(&self, text: &str) -> Result<()> {
        let mut chat = self.direct_chat.borrow_mut();
        chat.push(("user".into(), text.to_string()));
        let mut answer = String::new();
        flelink_core::chat::stream(
            &self.served.0,
            &self.served.1,
            &chat,
            &mut |reasoning, t| {
                if reasoning {
                    eprint!("\x1b[2m{t}\x1b[0m");
                } else {
                    answer.push_str(t);
                    print!("{t}");
                    let _ = std::io::stdout().flush();
                }
            },
        )?;
        println!();
        chat.push(("assistant".into(), answer));
        self.interrupted.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn post(&self, path: &str, body: &Value) -> Result<()> {
        self.http
            .post(format!("{}{path}", self.agent.base))
            .bearer_auth(&self.agent.token)
            .json(body)
            .send()?
            .error_for_status()?;
        Ok(())
    }

    /// The next line typed; `None` when input ended or Ctrl+C was pressed.
    fn read(&self) -> Option<String> {
        loop {
            if self.interrupted.swap(false, Ordering::SeqCst) {
                return None;
            }
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(l) => return Some(l),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Show the turn until it ends. Ctrl+C stops the turn, not the session.
    fn follow_turn(&self) -> Result<()> {
        let t0 = Instant::now();
        let mut started = false;
        let mut thinking = false;
        loop {
            if self.interrupted.swap(false, Ordering::SeqCst) {
                eprintln!("\n[stopping the turn]");
                self.post("/api/abort", &json!({}))?;
            }
            let note = match self.notes.recv_timeout(Duration::from_millis(100)) {
                Ok(n) => n,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !started && t0.elapsed() > Duration::from_secs(60) {
                        bail!("the agent did not begin within 60 s");
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("the connection to the agent ended")
                }
            };
            match note {
                Note::Ready(_) => {}
                Note::Lost => bail!("the connection to the agent ended"),
                Note::Event(Out::Busy) => started = true,
                Note::Event(Out::Done) if started => {
                    println!();
                    return Ok(());
                }
                Note::Event(Out::Done) => {}
                Note::Event(Out::Thinking(t)) => {
                    thinking = true;
                    eprint!("\x1b[2m{t}\x1b[0m");
                }
                Note::Event(Out::Text(t)) => {
                    if thinking {
                        eprintln!();
                        thinking = false;
                    }
                    print!("{t}");
                    std::io::stdout().flush()?;
                }
                Note::Event(Out::Tool(t)) => eprintln!("\n\x1b[36m⚙ {t}\x1b[0m"),
            }
        }
    }

    /// Open approvals, one question each. True when one was answered: the agent then goes on.
    fn answer_approvals(&self) -> Result<bool> {
        let list: Value = self
            .http
            .get(format!("{}/api/approvals", self.agent.base))
            .bearer_auth(&self.agent.token)
            .send()?
            .error_for_status()?
            .json()?;
        let mut answered = false;
        for a in list["approvals"].as_array().into_iter().flatten() {
            if a["status"] != "open" {
                continue;
            }
            eprintln!("\nThe agent asks: {}", a["action"].as_str().unwrap_or(""));
            eprintln!("{}", a["context"].as_str().unwrap_or(""));
            if let Some(c) = a["command"].as_str() {
                eprintln!("  $ {c}");
            }
            eprint!("Allow? [y/N] ");
            let Some(line) = self.read() else {
                return Ok(answered);
            };
            let yes = matches!(line.trim(), "y" | "Y" | "yes" | "j" | "J" | "ja");
            let id = a["id"].as_str().unwrap_or("");
            self.post(
                &format!("/api/approvals/{id}"),
                &json!({"decision": if yes { "granted" } else { "refused" }}),
            )?;
            answered = true;
        }
        Ok(answered)
    }

    fn talk(&self) -> Result<()> {
        loop {
            eprint!("\n{}› ", if self.direct.get() { "[model] " } else { "" });
            let Some(line) = self.read() else {
                return Ok(());
            };
            let text = line.trim();
            match text {
                "" => continue,
                "/quit" | "/exit" => return Ok(()),
                "/direct" => {
                    self.direct.set(true);
                    eprintln!("Straight to the model now: no tools, nothing in this folder is touched. /agent goes back.");
                    continue;
                }
                "/agent" => {
                    self.direct.set(false);
                    eprintln!("Back with the agent.");
                    continue;
                }
                _ => {}
            }
            if self.direct.get() {
                self.ask_model(text)?;
                continue;
            }
            self.post("/api/message", &json!({"text": text}))?;
            self.follow_turn()?;
            while self.answer_approvals()? {
                self.follow_turn()?;
            }
        }
    }
}

/// `base` is the address that serves the model, `model` its name there.
pub fn run(home: &Path, base: &str, model: &str) -> Result<()> {
    let project = std::env::current_dir()?.canonicalize()?;
    let window = context_of(base).unwrap_or_else(|| {
        eprintln!("Could not read the model's context; assuming {CONTEXT_DEFAULT} tokens.");
        CONTEXT_DEFAULT
    });
    let agent = start(home, &project, base, model, window)?;
    eprintln!(
        "flelink code · {} · {model} · {window} tokens\nThe agent reads, writes and runs commands in this folder without asking; it asks only before steps that cannot be undone.\n/direct talks to the model itself, /agent comes back, /quit leaves, Ctrl+C stops a turn.",
        project.display()
    );

    let (note_tx, notes) = mpsc::channel();
    let (b, t) = (agent.base.clone(), agent.token.clone());
    std::thread::spawn(move || {
        let _ = follow(&b, &t, &note_tx);
        let _ = note_tx.send(Note::Lost);
    });
    let (line_tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin()
            .lock()
            .lines()
            .map_while(std::io::Result::ok)
        {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = interrupted.clone();
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst));

    match notes.recv_timeout(Duration::from_secs(15)) {
        Ok(Note::Ready(n)) if n > 0 => {
            eprintln!("Going on with this folder's conversation ({n} entries).")
        }
        Ok(Note::Ready(_)) => {}
        _ => bail!("no connection to the agent's event stream"),
    }
    let s = Session {
        agent,
        http: reqwest::blocking::Client::new(),
        notes,
        lines,
        interrupted,
        served: (base.to_string(), model.to_string()),
        direct: Cell::new(false),
        direct_chat: RefCell::new(vec![]),
    };
    s.talk()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_become_what_the_terminal_shows() {
        let update = json!({"type": "message_update", "changes": [
            {"type": "text_start", "contentIndex": 0},
            {"type": "text_delta", "contentIndex": 0, "delta": "Hello"},
            {"type": "thinking_delta", "delta": "hm"},
        ]});
        let mut shown = 0;
        assert_eq!(
            read_event(&mut shown, &update),
            vec![Out::Text("Hello".into()), Out::Thinking("hm".into())]
        );
        assert_eq!(shown, 5);
        assert_eq!(
            read_event(&mut shown, &json!({"type": "run_start"})),
            vec![Out::Busy]
        );
        assert_eq!(
            read_event(&mut shown, &json!({"type": "run_end"})),
            vec![Out::Done]
        );
        assert!(read_event(&mut shown, &json!({"type": "usage_changed"})).is_empty());
        let tool = json!({"type": "tool_execution_start", "toolName": "bash", "args": {"command": "cargo   test\n-p x"}});
        assert_eq!(
            read_event(&mut shown, &tool),
            vec![Out::Tool("bash: cargo test -p x".into())]
        );
    }

    #[test]
    fn text_that_came_without_deltas_is_shown_when_the_message_ends() {
        let end = |text: &str| {
            json!({"type": "message_end", "entry": {"model": [
                {"role": "assistant", "content": [{"type": "text", "text": text}]}
            ]}})
        };
        let mut shown = 0;
        assert_eq!(
            read_event(&mut shown, &end("Whole answer")),
            vec![Out::Text("Whole answer".into())]
        );
        shown = 5;
        assert_eq!(
            read_event(&mut shown, &end("Hello world")),
            vec![Out::Text(" world".into())]
        );
        assert_eq!(shown, 0);
    }

    #[test]
    fn a_project_keeps_its_key_and_two_of_one_name_differ() {
        let a = project_key(Path::new("/home/x/my app"));
        assert_eq!(a, project_key(Path::new("/home/x/my app")));
        assert!(a.starts_with("my-app-"), "{a}");
        assert_ne!(a, project_key(Path::new("/srv/my app")));
    }

    #[test]
    fn the_project_file_goes_into_the_instructions() {
        let d = std::env::temp_dir().join(format!("fle-code-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(!instructions(&d).contains("AGENTS.md"));
        std::fs::write(d.join("AGENTS.md"), "Use tabs.").unwrap();
        assert!(instructions(&d).contains("Use tabs."));
        let _ = std::fs::remove_dir_all(&d);
    }
}
