//! Finding the llama.cpp command-line tools the Forge drives, and asking each what it can do. Port of
//! `fle_studio_forge/tools.py`.
//!
//! The Forge carries no quantiser of its own: it uses `llama-imatrix`, `llama-quantize`, `llama-perplexity` and, in the FLE
//! builds, `llama-fle-reap`. They are searched in the folder set in the settings, the folders of the installed engines and
//! of a llama-server of your own, then the PATH. Each hit is asked for its version and the options the Forge relies on.
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const TOOL_NAMES: [&str; 4] = [
    "llama-imatrix",
    "llama-quantize",
    "llama-perplexity",
    "llama-fle-reap",
];

fn features_of(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "llama-quantize" => &[
            ("--tensor-type", "tensor_type"),
            ("--imatrix", "imatrix"),
            ("--allow-requantize", "allow_requantize"),
            ("--output-tensor-type", "output_type"),
            ("--token-embedding-type", "embedding_type"),
        ],
        "llama-imatrix" => &[
            ("--chunks", "chunks"),
            ("--in-file", "in_file"),
            ("-ngl", "gpu_layers"),
        ],
        "llama-perplexity" => &[
            ("--kl-divergence", "kl_divergence"),
            ("--kl-divergence-base", "kl_base"),
            ("--chunks", "chunks"),
            ("-ngl", "gpu_layers"),
        ],
        "llama-fle-reap" => &[("--chunks", "chunks"), ("-cmoe", "cmoe"), ("-fit", "fit")],
        _ => &[],
    }
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub path: String,
    /// where it was found: settings | engine | custom engine | PATH
    pub source: String,
    pub version: String,
    pub features: Map<String, Value>,
    pub problem: String,
}

impl Tool {
    pub fn ok(&self) -> bool {
        self.problem.is_empty()
    }
    pub fn public(&self) -> Value {
        json!({"name": self.name, "path": self.path, "source": self.source, "version": self.version,
            "features": self.features, "problem": self.problem, "ok": self.ok()})
    }
    pub fn feature(&self, f: &str) -> bool {
        self.features.get(f).is_none_or(|v| v == true)
    }
}

/// A script (the test stand-ins are scripts) runs with python3; a real tool runs directly.
pub fn command_for(path: &str) -> (String, Vec<String>) {
    if path.to_lowercase().ends_with(".py") {
        ("python3".into(), vec![path.into()])
    } else {
        (path.into(), vec![])
    }
}

/// The environment of a tool: the libraries of an engine build sit next to its tools.
pub fn configure(cmd: &mut std::process::Command, path: &str) {
    let here = Path::new(path)
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    if cfg!(target_os = "linux") {
        let old = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        cmd.env(
            "LD_LIBRARY_PATH",
            if old.is_empty() {
                here.display().to_string()
            } else {
                format!("{}:{old}", here.display())
            },
        );
    } else if cfg!(windows) {
        cmd.env(
            "PATH",
            format!(
                "{};{}",
                here.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    }
    cmd.env("PYTHONUTF8", "1").current_dir(here);
}

fn run(path: &str, arg: &str) -> Result<String, String> {
    let (prog, mut args) = command_for(path);
    args.push(arg.into());
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).stdin(std::process::Stdio::null());
    configure(&mut cmd, path);
    let out = cmd.output().map_err(|e| e.to_string())?;
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

type Probed = HashMap<(String, u64, u64), Tool>;
static CACHE: Mutex<Option<Probed>> = Mutex::new(None);

pub fn probe(path: &str, name: &str, source: &str) -> Tool {
    let meta = std::fs::metadata(path).ok();
    let key = (
        path.to_string(),
        meta.as_ref().map_or(0, |m| m.len()),
        meta.and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs()),
    );
    if let Some(hit) = CACHE
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|c| c.get(&key).cloned())
    {
        return Tool {
            source: source.into(),
            ..hit
        };
    }
    let mut tool = Tool {
        name: name.into(),
        path: path.into(),
        source: source.into(),
        version: String::new(),
        features: Map::new(),
        problem: String::new(),
    };
    match (run(path, "--version"), run(path, "--help")) {
        (Ok(ver), Ok(help)) => {
            let re = Regex::new(r"(?i)version:\s*(\S+)(?:\s*\(([0-9a-f]+)\))?").unwrap();
            let m = re.captures(&ver);
            tool.version = m.as_ref().map_or(String::new(), |m| {
                m[1].to_string()
                    + &m.get(2)
                        .map_or(String::new(), |c| format!(" ({})", c.as_str()))
            });
            for (opt, feat) in features_of(name) {
                tool.features
                    .insert(feat.to_string(), json!(help.contains(opt)));
            }
            if m.is_none() && help.trim().is_empty() {
                tool.problem = "does not answer --version or --help".into();
            }
        }
        (Err(e), _) | (_, Err(e)) => tool.problem = e.chars().take(300).collect(),
    }
    CACHE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(key, tool.clone());
    tool
}

/// (label, folder) in the order they are searched; `engine_paths` are (label, path of a llama-server).
pub fn search_dirs(configured: &str, engine_paths: &[(String, String)]) -> Vec<(String, PathBuf)> {
    let mut out = vec![];
    if !configured.trim().is_empty() {
        out.push((
            "settings".to_string(),
            super::ggufio::path_buf(configured.trim()),
        ));
    }
    for (label, server) in engine_paths {
        if !server.is_empty() {
            if let Some(d) = super::ggufio::path_buf(server).parent() {
                out.push((label.clone(), d.to_path_buf()));
            }
        }
    }
    out
}

/// Every tool that can be found, probed; a missing tool is absent from the result.
pub fn find(dirs: &[(String, PathBuf)]) -> HashMap<String, Tool> {
    let mut found: HashMap<String, Tool> = HashMap::new();
    for name in TOOL_NAMES {
        let mut candidates: Vec<(String, String)> = vec![];
        let exe = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        for (label, folder) in dirs {
            for f in [exe.clone(), format!("{name}.py")] {
                let p = folder.join(&f);
                if p.is_file() {
                    candidates.push((label.clone(), p.to_string_lossy().to_string()));
                    break;
                }
            }
        }
        if let Some(p) = flelink_core::launch::which(name) {
            candidates.push(("PATH".into(), p.to_string_lossy().to_string()));
        }
        for (label, cand) in candidates {
            let t = probe(&cand, name, &label);
            if t.ok() {
                found.insert(name.into(), t);
                break;
            }
            found.entry(name.into()).or_insert(t); // shown with its problem unless a later candidate works
        }
    }
    found
}

pub fn missing_for(tools: &HashMap<String, Tool>, needs: &[&str]) -> Vec<String> {
    needs
        .iter()
        .filter(|n| tools.get(**n).is_none_or(|t| !t.ok()))
        .map(|n| n.to_string())
        .collect()
}
