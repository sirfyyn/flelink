mod demo;
mod mcp;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use flelink_core::{catalog, chat, data_dir, engines, flow, system, Event};
use std::io::Write;

mod plancmd;

#[derive(Parser)]
#[command(
    name = "flelink",
    version,
    about = "Run large language models locally. No flags, no variables: just `flelink`."
)]
struct Cli {
    /// Keep engines, models and logs in this folder (default: per-user app data)
    #[arg(long, global = true)]
    home: Option<std::path::PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// print the config path
    Path,
    /// write a default config
    Init {
        #[arg(long)]
        force: bool,
    },
    /// show the effective settings
    Show,
    /// set KEY VALUE
    Set { key: String, value: String },
    /// remove KEY
    Unset { key: String },
    /// open the file in $EDITOR
    Edit,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show or change the global settings (~/.config/fle/config.toml)
    Config {
        /// use another config file
        #[arg(long, global = true, value_name = "FILE")]
        config: Option<String>,
        #[arg(long, global = true)]
        json: bool,
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// The models FLE Link knows, and whether each fits this machine
    Models,
    /// Plugins in <data>/plugins: the engines and recipes each one brings
    Plugins,
    /// Recipes: a model with its engine and start settings (built in, in <data>/recipes, or from a plugin)
    Recipes,
    /// Print a recipe as TOML, to share it or to save a changed copy in <data>/recipes
    Recipe {
        /// recipe id, a .toml file or an https:// URL
        recipe: String,
    },
    /// Load a model (a recipe id, a .toml recipe or an https:// URL to one) and serve it (OpenAI-compatible API) until Ctrl+C
    Run { model: String },
    /// An MCP server on stdin/stdout: lets a coding agent list, download, load and stop models, read the log and change settings
    Mcp {
        /// the data folder of the Studio to drive (default: per-user app data)
        #[arg(long, value_name = "DIR")]
        home: Option<std::path::PathBuf>,
    },
    /// Set up Claude Code, Codex, OpenCode, pi or any other app for the model FLE Link serves (OpenAI and Anthropic API)
    Connect {
        /// one of claude, codex, opencode, pi, api, mcp; all when left out
        tool: Option<String>,
        /// the address to use instead of the one found running (e.g. http://192.168.1.20:8642)
        #[arg(long)]
        url: Option<String>,
        /// the model name to use instead of the one the server reports
        #[arg(long)]
        model: Option<String>,
    },
    /// Read a model's header: architecture, parts, expert bytes, KV cache (no weights are loaded)
    Inspect {
        /// model file, first shard, or the folder that contains the shards
        model: std::path::PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Check parts, tensor count, offsets and tensor types of a model file
    Verify {
        model: std::path::PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Start the Studio: the web page and the local API (`http://127.0.0.1:8642`)
    Studio {
        /// data folder (settings, chats, engines, models); default: per-user app data
        #[arg(long, value_name = "DIR")]
        home: Option<std::path::PathBuf>,
        /// address to listen on; 0.0.0.0 opens it to the network (other machines need the access key)
        #[arg(long)]
        host: Option<String>,
        /// in host mode: plain http instead of https with a certificate of its own
        #[arg(long)]
        no_tls: bool,
        /// port (default 8642; the next free one when it is taken)
        #[arg(long)]
        port: Option<u16>,
        /// do not open the browser
        #[arg(long)]
        no_browser: bool,
        /// use an already running llama.cpp / FLE server
        #[arg(long, value_name = "URL")]
        attach: Option<String>,
        /// serve the page from this folder instead of the copy inside the binary
        #[arg(long, value_name = "DIR", hide = true)]
        web_dir: Option<std::path::PathBuf>,
        /// demo mode: the page takes steps from `flelink demo` (for recordings of the real Studio)
        #[arg(long)]
        demo: bool,
    },
    /// A pairing code for another FLE Link (valid 10 minutes, one use); the host side of Remote access
    PairCode,
    /// Show the llama-server command and the numbers behind it
    Plan(Box<plancmd::PlanArgs>),
    /// Check model, engine, memory, disk and the command that would start
    Doctor {
        #[command(flatten)]
        common: plancmd::Common,
        /// exit 1 on warnings too
        #[arg(long)]
        strict: bool,
    },
    /// What this computer has and which engines FLE Link could install
    System,
    /// Start llama-server with the plan
    Serve {
        #[command(flatten)]
        plan: Box<plancmd::PlanArgs>,
        /// print the plan, do not start
        #[arg(long)]
        dry_run: bool,
        /// skip doctor before starting
        #[arg(long)]
        no_preflight: bool,
        /// start even if doctor finds a failure
        #[arg(long)]
        force: bool,
        /// with --fallback: how long to watch the start for a crash before leaving the server alone
        #[arg(long, default_value_t = 3600.0, value_name = "SEC")]
        ready_timeout: f64,
    },
    /// List the llama-server builds this program can find
    Engines {
        #[command(flatten)]
        common: plancmd::Common,
        /// also show candidate paths that do not exist
        #[arg(long)]
        all: bool,
    },
    /// Download the MTP head and vocabulary for a known model
    FetchSidecars {
        #[command(flatten)]
        common: plancmd::Common,
        /// show what would be downloaded, change nothing
        #[arg(long)]
        dry_run: bool,
        /// replace files that are already there
        #[arg(long)]
        force: bool,
    },
    /// Write a redacted diagnostic report
    Report {
        #[command(flatten)]
        common: plancmd::Common,
        /// file to write, - for stdout
        #[arg(long, default_value = "-")]
        out: String,
        /// write a zip with report.json and plan.sh
        #[arg(long)]
        zip: bool,
    },
    /// Measure tokens per second through /v1
    Bench(Box<BenchArgs>),
    /// Ask one question and print the answer (loads the model for the duration)
    Ask {
        /// a recipe id (`flelink recipes`), a model id (`flelink models`) or a .gguf file
        model: String,
        /// the question; without one it is read from stdin
        question: Vec<String>,
    },
    /// Drive a Studio started with --demo (chat, page, preview, wait) and record the screen, for videos
    Demo {
        #[command(subcommand)]
        what: DemoCmd,
    },
}

#[derive(Subcommand)]
enum DemoCmd {
    /// type a message into the chat and send it, as a person would
    Chat {
        text: String,
        #[arg(long)]
        max_tokens: Option<u32>,
        #[arg(long)]
        temperature: Option<f64>,
        /// wait until the answer is finished
        #[arg(long)]
        wait: bool,
        /// milliseconds per typed character (default 12)
        #[arg(long)]
        typing_ms: Option<u64>,
    },
    /// open a page of the Studio (/, /engine, /models, /settings …)
    Page { path: String },
    /// press Preview on the last answer that has a web page
    Preview,
    /// start a new chat
    New,
    /// scroll the page (pixels, negative = up)
    Scroll { dy: i64 },
    /// wait until the page has done every step and no answer is being written (max seconds, default 1800)
    Wait { max: Option<u64> },
    /// demo mode on or off, queue, whether an answer is being written
    Status,
    /// record the screen with the system's recorder: `start FILE [--window]`, `stop`
    Record {
        #[command(subcommand)]
        what: RecordCmd,
    },
}

#[derive(Subcommand)]
enum RecordCmd {
    Start {
        file: std::path::PathBuf,
        /// only the focused window (Hyprland), else the whole screen
        #[arg(long)]
        window: bool,
    },
    Stop,
}

/// Colours on stderr only for a terminal and only without NO_COLOR.
fn color() -> bool {
    use std::io::IsTerminal;
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
    })
}

fn paint(code: &str, text: &str) -> String {
    if color() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// What the progress line needs to tell a speed: where this download started, and when.
struct Rate {
    label: String,
    t0: std::time::Instant,
    from: u64,
}
static RATE: std::sync::Mutex<Option<Rate>> = std::sync::Mutex::new(None);

fn eta(secs: f64) -> String {
    let s = secs.round() as u64;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min {:02} s", s / 60, s % 60),
        _ => format!("{} h {:02} min", s / 3600, s % 3600 / 60),
    }
}

/// Done and total for the progress line, both in the unit of the total: an engine archive of 40 MB reads as MB, not
/// as "0.0 GiB", and a model as GiB from its first byte on.
fn sizes(done: u64, total: u64) -> (String, String) {
    if total < 1 << 30 {
        let mb = |b: u64| format!("{:.0} MB", b as f64 / 1e6);
        (mb(done), mb(total))
    } else {
        (system::gib(done), system::gib(total))
    }
}

fn print_event(e: &Event) {
    match e {
        Event::Step(s) => eprintln!("\n{} {s}", paint("1;35", "==>")),
        Event::Progress { label, done, total } => {
            let pct = if *total > 0 {
                *done as f64 * 100.0 / *total as f64
            } else {
                0.0
            };
            let mut r = RATE.lock().unwrap();
            if r.as_ref()
                .is_none_or(|r| r.label != *label || *done < r.from)
            {
                *r = Some(Rate {
                    label: label.clone(),
                    t0: std::time::Instant::now(),
                    from: *done,
                });
            }
            let rt = r.as_ref().unwrap();
            let secs = rt.t0.elapsed().as_secs_f64();
            let speed = if secs > 1.0 {
                (done - rt.from) as f64 / secs
            } else {
                0.0
            };
            let tail = if speed > 0.0 && *total > *done {
                format!(
                    "  {:.0} MB/s, {} left",
                    speed / 1e6,
                    eta((total - done) as f64 / speed)
                )
            } else {
                String::new()
            };
            let width = 24usize;
            let fill = ((pct / 100.0) * width as f64).round().min(width as f64) as usize;
            let bar = format!(
                "{}{}",
                paint("36", &"━".repeat(fill)),
                paint("2", &"━".repeat(width - fill))
            );
            let (d, t) = sizes(*done, *total);
            eprint!("\r    {label}  {bar} {pct:5.1} %  ({d} / {t}){tail}   ");
            let _ = std::io::stderr().flush();
        }
        Event::Log(l) => {
            let short: String = l.chars().take(110).collect();
            eprint!("\r    {}", paint("2", &format!("{short:<110}")))
        }
        Event::Ready {
            base,
            engine,
            model,
            command,
        } => {
            eprintln!(
                "\n\n{} {model} on {engine}\n  API: {}   (OpenAI- and Anthropic-compatible, no key needed; `flelink connect` sets up your tools)\n  Command (shown, never needed):\n    {}",
                paint("1;32", "Ready:"),
                paint("1", &format!("{base}/v1")),
                paint("2", command)
            );
        }
    }
}

pub fn main() {
    if let Err(e) = run() {
        // like the Python helper: "<program>: message", then "hint: ..."; exit code 2. The name is the one it was called
        // by, so `fle` still says "fle:" and `flelink` says "flelink:".
        let prog = std::env::args_os()
            .next()
            .and_then(|a| {
                std::path::Path::new(&a)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
            })
            .filter(|s| s == "fle")
            .unwrap_or_else(|| "flelink".into());
        if let Some(fe) = e.downcast_ref::<flelink_core::FleError>() {
            eprintln!("{prog}: {fe}");
            if !fe.hint.is_empty() {
                eprintln!("hint: {}", fe.hint);
            }
        } else {
            eprintln!("{prog}: {e:#}");
        }
        std::process::exit(2);
    }
}

#[derive(clap::Args)]
struct BenchArgs {
    /// server base URL
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    url: String,
    /// also benchmark a second running server/model at URL
    #[arg(long, value_name = "URL")]
    compare: Option<String>,
    /// model name (default: first in /v1/models)
    #[arg(long = "model", default_value = "")]
    model_name: String,
    #[arg(
        long,
        default_value = "Write a short paragraph about why local language models are useful."
    )]
    prompt: String,
    /// read the prompt from a file
    #[arg(long, value_name = "FILE")]
    prompt_file: Option<String>,
    #[arg(long, default_value_t = 128)]
    max_tokens: i64,
    #[arg(long, default_value_t = 3)]
    runs: i64,
    #[arg(long, default_value_t = 1)]
    warmup: i64,
    #[arg(long, default_value_t = 600.0)]
    timeout: f64,
    /// a practical test instead of the short prompt: a real task at temperature 0 without thinking, with a check
    /// that the answer is complete and the machine in the result (pagoda: three.js scene, up to 8000 tokens)
    #[arg(long, value_name = "NAME")]
    task: Option<String>,
    /// with --task: keep each answer in DIR (pagoda-1.html, …)
    #[arg(long, value_name = "DIR")]
    answers: Option<String>,
    /// with --task: open a GitHub discussion (sirfyyn/flelink, Benchmarks) with the result filled in; you read it and post
    /// it yourself, FLE Link uploads nothing
    #[arg(long)]
    share: bool,
    /// write the JSON result to a file
    #[arg(long, value_name = "FILE")]
    output: Option<String>,
    /// JSON from an earlier --output; compare the median tok/s with it
    #[arg(long, value_name = "FILE")]
    baseline: Option<String>,
    /// with --baseline: exit 2 when the median drops by more than PCT percent
    #[arg(long, default_value_t = flelink_core::bench::DEFAULT_MAX_REGRESSION, value_name = "PCT")]
    max_regression: f64,
    #[arg(long)]
    json: bool,
    #[arg(long, hide = true)]
    color: Option<String>,
    #[arg(long, hide = true)]
    debug: bool,
}

fn run_bench(a: BenchArgs) -> Result<i32> {
    use flelink_core::{bench, FleError};
    if let Some(id) = &a.task {
        return run_praxis(&a, id);
    }
    let mut prompt = a.prompt.clone();
    if let Some(f) = &a.prompt_file {
        prompt = std::fs::read_to_string(f)
            .map_err(|e| FleError::new(format!("Cannot read {f}: {e}"), "Check the path."))?;
    }
    if a.max_regression < 0.0 {
        return Err(FleError::new(
            "--max-regression must not be negative.",
            "Use a percentage such as 10.",
        )
        .into());
    }
    let baseline = a
        .baseline
        .as_deref()
        .map(bench::load_baseline)
        .transpose()?;
    let mut urls = vec![a.url.clone()];
    urls.extend(a.compare.clone());
    let mut results = vec![];
    for u in &urls {
        let p = bench::Params {
            url: u.clone(),
            model: a.model_name.clone(),
            prompt: prompt.clone(),
            max_tokens: a.max_tokens,
            runs: a.runs,
            warmup: a.warmup,
            timeout: a.timeout,
        };
        results.push(bench::run(&p)?);
    }
    let payload = if results.len() == 1 {
        results[0].clone()
    } else {
        serde_json::json!({"primary": results[0], "compare": results[1]})
    };
    let comparison = baseline
        .as_ref()
        .map(|b| bench::compare_baseline(&results[0], b, a.max_regression));
    if let Some(o) = &a.output {
        std::fs::write(o, serde_json::to_string_pretty(&payload)?)
            .map_err(|e| FleError::new(format!("Cannot write {o}: {e}"), "Check the path."))?;
    }
    if a.json {
        let mut shown = payload.clone();
        if let (Some(c), Some(m)) = (&comparison, shown.as_object_mut()) {
            m.insert("baseline_comparison".into(), c.clone());
        }
        println!("{}", flelink_core::gguf::py_json(&shown));
    } else {
        print!("{}", bench::result_text(&results[0]));
        if results.len() > 1 {
            print!("\n{}", bench::result_text(&results[1]));
            let med = |r: &serde_json::Value| r["median_tok_s"].as_f64().unwrap_or(0.0);
            println!(
                "\ncompare: {:.2} vs {:.2} tok/s ({} vs {})",
                med(&results[0]),
                med(&results[1]),
                results[0]["url"].as_str().unwrap_or(""),
                results[1]["url"].as_str().unwrap_or("")
            );
        }
        if let Some(c) = &comparison {
            print!("\n{}", bench::comparison_text(c));
        }
    }
    Ok(
        if comparison
            .as_ref()
            .is_some_and(|c| c["status"] == "regression")
        {
            2
        } else {
            0
        },
    )
}

/// `flelink bench --task NAME`: a practical test (flelink-core praxis.rs). Runs default to 3 like the speed test; the
/// warm-up is a short count, not a whole answer.
fn run_praxis(a: &BenchArgs, id: &str) -> Result<i32> {
    use flelink_core::{praxis, FleError};
    if a.compare.is_some() || a.baseline.is_some() {
        return Err(FleError::new(
            "--task measures one server; --compare and --baseline belong to the speed test.",
            "Run --task once per server and compare the two result files.",
        )
        .into());
    }
    let t = praxis::task(id)?;
    let p = praxis::Params {
        url: a.url.clone(),
        model: a.model_name.clone(),
        runs: a.runs.max(1) as u32,
        warmup: a.warmup > 0,
        timeout: a.timeout.max(1800.0),
        answers: a.answers.as_ref().map(std::path::PathBuf::from),
    };
    let r = praxis::run(&p, t)?;
    if let Some(o) = &a.output {
        std::fs::write(o, serde_json::to_string_pretty(&r)?)
            .map_err(|e| FleError::new(format!("Cannot write {o}: {e}"), "Check the path."))?;
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        print!("{}", praxis::result_text(&r));
    }
    if a.share {
        let (title, body) = flelink_core::share::bench_post(&r);
        let sh = flelink_core::share::discussion(flelink_core::share::BENCHMARKS, &title, &body);
        if let Some(text) = &sh.paste {
            let f = std::env::temp_dir().join("flelink-share.md");
            std::fs::write(&f, text)?;
            eprintln!(
                "The result is too long for a link: paste {} into the form.",
                f.display()
            );
        }
        eprintln!("Share it (you read it and post it yourself): {}", sh.url);
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener)
            .arg(&sh.url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    } else if !a.json {
        eprintln!("Compare with others: add --share to post this result (you read it first).");
    }
    Ok(0)
}

fn run_config(config: Option<String>, json: bool, action: ConfigAction) -> Result<()> {
    use flelink_core::{config, FleError};
    let path = config::config_path(config.as_deref());
    match action {
        ConfigAction::Path => println!("{}", path.display()),
        ConfigAction::Init { force } => {
            if path.exists() && !force {
                return Err(FleError::new(
                    format!("{} already exists", path.display()),
                    "Use --force to replace it.",
                )
                .into());
            }
            let (data, _) = config::load(config.as_deref())?;
            println!("{}", config::save(&data, config.as_deref())?.display());
        }
        ConfigAction::Edit => {
            if !path.exists() {
                let (data, _) = config::load(config.as_deref())?;
                config::save(&data, config.as_deref())?;
            }
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".into());
            let st = std::process::Command::new(&editor).arg(&path).status()?;
            std::process::exit(st.code().unwrap_or(1));
        }
        ConfigAction::Show => {
            let (data, resolved) = config::load(config.as_deref())?;
            let settings = config::effective(&serde_json::Map::new(), &data)?;
            if json {
                let v =
                    serde_json::json!({"path": resolved.to_string_lossy(), "settings": settings});
                println!("{}", flelink_core::gguf::py_json(&v));
            } else {
                print!("{}", config::show_text(&resolved, &settings));
            }
        }
        ConfigAction::Set { key, value } => {
            let (mut data, _) = config::load(config.as_deref())?;
            if key == "profile" && !config::PROFILES.contains(&value.as_str()) {
                return Err(FleError::new(
                    format!("Unknown profile '{value}'"),
                    format!("Use one of: {}.", config::PROFILES.join(", ")),
                )
                .into());
            }
            // a typo would otherwise be saved and silently do nothing
            const KNOWN: [&str; 13] = [
                "profile",
                "spec",
                "engine",
                "model",
                "host",
                "port",
                "context",
                "threads",
                "gpu_cache_mib",
                "fit_target_mib",
                "mtp",
                "pinned_load",
                "env",
            ];
            const NUMBERS: [&str; 5] = [
                "port",
                "context",
                "threads",
                "gpu_cache_mib",
                "fit_target_mib",
            ];
            let top = key.split('.').next().unwrap_or("");
            if !KNOWN.contains(&top) || (top == "env" && !key.starts_with("env.")) {
                return Err(FleError::new(
                    format!("Unknown setting '{key}'"),
                    format!("Known: {}, env.NAME.", KNOWN[..12].join(", ")),
                )
                .into());
            }
            let v = if key.starts_with("env.") {
                serde_json::Value::String(value.clone())
            } else {
                config::parse_value(&value)
            };
            if NUMBERS.contains(&key.as_str()) && !v.is_number() {
                return Err(
                    FleError::new(format!("{key} needs a number, got '{value}'"), "").into(),
                );
            }
            if key == "port" && !v.as_f64().is_some_and(|p| (1.0..=65535.0).contains(&p)) {
                return Err(FleError::new(
                    format!("port must be between 1 and 65535, got {value}"),
                    "",
                )
                .into());
            }
            config::set_dotted(&mut data, &key, v);
            println!("{}", config::save(&data, config.as_deref())?.display());
        }
        ConfigAction::Unset { key } => {
            let (mut data, _) = config::load(config.as_deref())?;
            config::unset_dotted(&mut data, &key);
            println!("{}", config::save(&data, config.as_deref())?.display());
        }
    }
    Ok(())
}

/// Exit with `code` unless it is 0.
fn exit(code: i32) -> Result<()> {
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let home = cli.home.unwrap_or_else(data_dir);
    match cli.cmd {
        None => tui::run(&home),
        Some(Cmd::Config {
            config,
            json,
            action,
        }) => run_config(config, json, action),
        Some(Cmd::Studio {
            home,
            host,
            no_tls,
            port,
            no_browser,
            attach,
            web_dir,
            demo,
        }) => exit({
            if demo {
                std::env::set_var("FLE_DEMO", "1");
            }
            flelink_api::run::run(flelink_api::run::RunOptions {
                home,
                host,
                no_tls,
                port,
                no_browser,
                attach: attach.unwrap_or_default(),
                web_dir,
            })
        }),
        Some(Cmd::Demo { what }) => {
            let r = match what {
                DemoCmd::Chat { text, max_tokens, temperature, wait, typing_ms } => demo::step(serde_json::json!({
                    "aktion": "chat", "text": text, "max_tokens": max_tokens, "temperature": temperature, "tippen_ms": typing_ms
                }))
                .and_then(|_| if wait { demo::wait(std::time::Duration::from_secs(1800)) } else { Ok(()) }),
                DemoCmd::Page { path } => demo::step(serde_json::json!({"aktion": "seite", "pfad": path})),
                DemoCmd::Preview => demo::step(serde_json::json!({"aktion": "vorschau"})),
                DemoCmd::New => demo::step(serde_json::json!({"aktion": "neu"})),
                DemoCmd::Scroll { dy } => demo::step(serde_json::json!({"aktion": "scroll", "dy": dy})),
                DemoCmd::Wait { max } => demo::wait(std::time::Duration::from_secs(max.unwrap_or(1800))),
                DemoCmd::Status => demo::status(),
                DemoCmd::Record { what: RecordCmd::Start { file, window } } => demo::record_start(&file, window),
                DemoCmd::Record { what: RecordCmd::Stop } => demo::record_stop(),
            };
            if let Err(e) = r {
                eprintln!("flelink demo: {e:#}");
                std::process::exit(1);
            }
            Ok(())
        }
        Some(Cmd::PairCode) => {
            let c = flelink_api::remote::Clients::new(&home).new_code();
            println!(
                "Pairing code: {}  (valid for 10 minutes, one use)",
                c["pretty"].as_str().unwrap_or("")
            );
            println!("On the other machine: Settings → Remote access → Add host, with this machine's address and the code.");
            Ok(())
        }
        Some(Cmd::Plan(a)) => plancmd::run(*a, &home),
        Some(Cmd::System) => {
            let s = system::detect(&home);
            println!("OS        {} {}\nCPU       {} ({} cores)\nMemory    {}\nGPU       {}\nDisk free {} (in {})", s.os, s.arch, s.cpu, s.cores, system::gib(s.ram_bytes),
                if s.gpus.is_empty() { "none".into() } else { s.gpus.iter().map(|g| g.name.clone()).collect::<Vec<_>>().join(", ") }, system::gib(s.disk_free_bytes), home.display());
            if s.gpu_usable_bytes > 0 {
                println!("GPU usable {}", system::gib(s.gpu_usable_bytes));
            }
            println!("\nEngines that fit (best first):");
            for v in engines::variants(&s) {
                println!("  {:<42} {}", v.label, v.why);
            }
            let inst = engines::installed(&home);
            println!(
                "\nInstalled: {}",
                if inst.is_empty() {
                    "nothing yet (the first run installs the best one)".into()
                } else {
                    inst.iter()
                        .map(|(n, _)| n.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            Ok(())
        }
        Some(Cmd::Doctor { common, strict }) => exit(plancmd::run_doctor(common, strict, &home)?),
        Some(Cmd::Serve {
            plan,
            dry_run,
            no_preflight,
            force,
            ready_timeout,
        }) => exit(plancmd::run_serve(
            *plan,
            dry_run,
            no_preflight,
            force,
            ready_timeout,
            &home,
        )?),
        Some(Cmd::Engines { common, all }) => exit(plancmd::run_engines(common, all, &home)?),
        Some(Cmd::FetchSidecars {
            common,
            dry_run,
            force,
        }) => exit(plancmd::run_fetch_sidecars(common, dry_run, force)?),
        Some(Cmd::Report { common, out, zip }) => {
            exit(plancmd::run_report(common, out, zip, &home)?)
        }
        Some(Cmd::Bench(a)) => exit(run_bench(*a)?),
        Some(Cmd::Recipes) => {
            let s = system::detect(&home);
            let (all, problems) = flelink_core::recipe::load_all(&home);
            let w = all.iter().map(|r| r.id.len()).max().unwrap_or(22).max(22);
            for r in &all {
                let fit = if r.model.size > 0 {
                    r.fit(&home.join("models"), &s).label()
                } else {
                    "?"
                };
                let have = r
                    .fetches()
                    .iter()
                    .all(|x| home.join("models").join(&x.local).exists());
                let from = if r.source == "builtin" {
                    String::new()
                } else {
                    format!("  ({})", r.source)
                };
                println!(
                    "{:<w$} {:>9}  {:<8} {}{}{}",
                    r.id,
                    if r.model.size > 0 {
                        system::gib(r.model.size)
                    } else {
                        "-".into()
                    },
                    fit,
                    r.title,
                    if have { "  [downloaded]" } else { "" },
                    from,
                    w = w
                );
            }
            for p in problems {
                eprintln!("skipped: {p}");
            }
            println!(
                "\nRun one: flelink run ID · your own: put a .toml into {}",
                home.join("recipes").display()
            );
            Ok(())
        }
        Some(Cmd::Plugins) => {
            let (plugins, problems) = flelink_core::plugin::load_all(&home);
            let (recipes, _) = flelink_core::recipe::load_all(&home);
            if plugins.is_empty() && problems.is_empty() {
                println!(
                    "No plugins. A plugin is a folder in {} (docs/PLUGINS.md).",
                    home.join("plugins").display()
                );
            }
            for m in &plugins {
                let mine = recipes
                    .iter()
                    .filter(|r| r.source.starts_with(&format!("plugin {}:", m.id)))
                    .count();
                println!("{} {}  {}", m.id, m.version, m.title);
                for e in &m.engine {
                    let from = match (&e.path, &e.release) {
                        (Some(p), _) => p.clone(),
                        (_, Some(r)) => format!(
                            "github.com/{} {}",
                            r.repo,
                            if r.tag.is_empty() { "latest" } else { &r.tag }
                        ),
                        _ => e
                            .program
                            .as_ref()
                            .map(|p| format!("{p} on the PATH"))
                            .unwrap_or_default(),
                    };
                    println!("  engine {:<16} {}  ({from})", e.id, e.label);
                }
                println!("  {mine} recipe(s)");
            }
            for p in problems {
                eprintln!("skipped: {p}");
            }
            Ok(())
        }
        Some(Cmd::Recipe { recipe }) => {
            let r = flelink_core::recipe::find(&home, &recipe)?;
            print!("{}", r.to_toml());
            Ok(())
        }
        Some(Cmd::Models) => {
            let s = system::detect(&home);
            for m in catalog::builtin() {
                let f = catalog::fit(&m, &s);
                let have = flow::model_path(&home, &m).exists();
                println!(
                    "{:<18} {:>9}  {:<8} {}{}\n{:<18} {}",
                    m.id,
                    system::gib(m.size),
                    f.label(),
                    m.title,
                    if have { "  [downloaded]" } else { "" },
                    "",
                    m.note
                );
            }
            Ok(())
        }
        Some(Cmd::Inspect { model, json }) => {
            let m = flelink_core::gguf::read_model(&model)?;
            let sm = m.summary();
            if json {
                println!("{}", flelink_core::gguf::py_json(&sm));
                return Ok(());
            }
            let n = |k: &str| sm[k].as_u64().map(|v| v.to_string()).unwrap_or("-".into());
            println!("{}  ({})", m.name, m.arch);
            println!(
                "  size        {} in {} part(s){}",
                system::gib(m.size),
                m.parts.len(),
                if m.complete() { "" } else { "  INCOMPLETE" }
            );
            println!(
                "  layers      {}   context {}   experts {} (used {})",
                n("layers"),
                n("context_length"),
                n("experts"),
                n("experts_used")
            );
            println!(
                "  expert data {}   always-needed {}   input tables {}",
                system::gib(m.expert_bytes),
                system::gib(m.gpu_bytes),
                system::gib(m.input_bytes)
            );
            println!(
                "  KV cache    {} bytes/token (q8_0)   {} (f16)",
                n("kv_bytes_per_token_q8"),
                n("kv_bytes_per_token_f16")
            );
            println!("  quant types:");
            for (t, (c, b)) in &m.type_histogram {
                println!("    {t:<8} {c:>5} tensors  {}", system::gib(*b));
            }
            Ok(())
        }
        Some(Cmd::Verify { model, json }) => {
            let m = flelink_core::gguf::read_model(&model)?;
            let r = flelink_core::gguf::verify_model(&m);
            let ok = r["ok"].as_bool().unwrap_or(false);
            if json {
                println!("{}", flelink_core::gguf::py_json(&r));
            } else {
                println!("verify: {}", if ok { "ok" } else { "failed" });
                for c in r["checks"].as_array().unwrap() {
                    println!(
                        "[{}] {}: {}",
                        c["status"].as_str().unwrap(),
                        c["name"].as_str().unwrap(),
                        c["detail"].as_str().unwrap()
                    );
                    if let Some(h) = c["hint"].as_str().filter(|h| !h.is_empty()) {
                        println!("       -> {h}");
                    }
                }
                for w in r["warnings"].as_array().unwrap() {
                    println!("[warn] {}", w.as_str().unwrap());
                }
                for e in r["errors"].as_array().unwrap() {
                    println!("[fail] {}", e.as_str().unwrap());
                }
            }
            std::process::exit(if ok { 0 } else { 1 });
        }
        Some(Cmd::Mcp { home }) => {
            let explicit = home.is_some();
            mcp::serve(home.unwrap_or_else(flelink_api::run::data_dir), explicit)
        }
        Some(Cmd::Connect { tool, url, model }) => {
            // an address given: ask that one for its model; otherwise look on this machine
            let found = match &url {
                Some(u) => flelink_core::connect::model_at(u).map(|m| (u.clone(), m)),
                None => flelink_core::connect::find_running(),
            };
            let base = url
                .or_else(|| found.as_ref().map(|f| f.0.clone()))
                .unwrap_or_else(|| "http://127.0.0.1:8642".into());
            let name = model
                .or_else(|| found.as_ref().map(|f| f.1.clone()))
                .unwrap_or_else(|| "local".into());
            if found.is_none() {
                eprintln!(
                    "{}",
                    paint(
                        "33",
                        "No model is serving there yet: start one (`flelink run <recipe>` or the app), then run this again for the exact model name."
                    )
                );
            } else {
                eprintln!("{} {base}  (model {name})", paint("1;32", "Found:"));
            }
            let all = flelink_core::connect::snippets(&base, &name);
            let pick: Vec<_> = all
                .iter()
                .filter(|s| tool.as_deref().is_none_or(|t| t == s.id))
                .collect();
            if pick.is_empty() {
                let ids: Vec<_> = all.iter().map(|s| s.id).collect();
                anyhow::bail!("no such tool; one of: {}", ids.join(", "));
            }
            for s in pick {
                let place = if s.place == "shell" {
                    "run in the terminal".to_string()
                } else {
                    format!("in {}", s.place)
                };
                println!("\n{}  {}", paint("1;36", s.title), paint("2", &place));
                for l in s.text.lines() {
                    println!("  {l}");
                }
            }
            Ok(())
        }
        Some(Cmd::Run { model }) => {
            let mut srv = flow::ensure_and_start(&home, &model, &mut |e| print_event(&e))?;
            eprintln!("\nServing. Press Ctrl+C to stop.");
            // Ctrl+C, SIGTERM, SIGHUP (or closing the console on Windows) stop the engine with the program; an engine that
            // ends on its own ends the program
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = stop.clone();
            let _ =
                ctrlc::set_handler(move || flag.store(true, std::sync::atomic::Ordering::SeqCst));
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                if let Ok(Some(status)) = srv.child.try_wait() {
                    anyhow::bail!(
                        "the engine stopped by itself ({status}); its log: {}",
                        srv.log.display()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            eprintln!("Stopping the engine");
            drop(srv);
            Ok(())
        }
        Some(Cmd::Ask { model, question }) => {
            // the question before the model loads: a missing one should not cost a minute of loading
            let q = if question.is_empty() {
                use std::io::{IsTerminal, Read};
                let mut t = String::new();
                if !std::io::stdin().is_terminal() {
                    std::io::stdin().read_to_string(&mut t)?;
                }
                if t.trim().is_empty() {
                    return Err(flelink_core::FleError::new(
                        "No question given.",
                        "flelink ask MODEL \"your question\" (or pipe it in: echo … | flelink ask MODEL)",
                    )
                    .into());
                }
                t.trim().to_string()
            } else {
                question.join(" ")
            };
            let srv = flow::ensure_and_start(&home, &model, &mut |e| print_event(&e))?;
            eprintln!("\n> {q}\n");
            let st = chat::stream(
                &srv.base,
                &model,
                &[("user".into(), q)],
                &mut |reasoning, t| {
                    if reasoning {
                        eprint!("{t}")
                    } else {
                        print!("{t}");
                        let _ = std::io::stdout().flush();
                    }
                },
            )?;
            println!(
                "\n\n[{} tokens, {} tok/s decode, {} tok/s prompt]",
                st.tokens,
                st.decode_tps
                    .map(|v| format!("{v:.1}"))
                    .unwrap_or("-".into()),
                st.prefill_tps
                    .map(|v| format!("{v:.0}"))
                    .unwrap_or("-".into())
            );
            Ok(())
        }
    }
}
