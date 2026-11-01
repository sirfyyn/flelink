//! flelink-core: everything `fle` (terminal) and, later, the Tauri app need to take a person from "nothing installed"
//! to "model answering": hardware, engines, model catalog, resumable downloads, start plan, server, chat.
//! No Python, no uv, no git. One static binary on top of this.

pub mod adapter;
pub mod assets;
pub mod bench;
pub mod catalog;
pub mod chat;
pub mod config;
pub mod doctor;
pub mod download;
pub mod engines;
pub mod gguf;
pub mod launch;
pub mod orphans;
pub mod plan;
pub mod plugin;
pub mod policy;
pub mod recipe;
pub mod report;
pub mod serve;
pub mod server;
pub mod sidecars;
pub mod system;

use std::path::PathBuf;

/// The one folder the terminal, the Studio and the app share for engines, models, chats and logs.
/// `--home`, FLE_HOME (or FLE_STUDIO_HOME) override it. Before FLE Link, the terminal used `FLE` and the Studio
/// `FLE Studio`: while the new folder does not exist yet, an old one is used as it is, so nothing is moved or lost.
pub fn data_dir() -> PathBuf {
    for var in ["FLE_HOME", "FLE_STUDIO_HOME"] {
        if let Some(p) = std::env::var_os(var).filter(|p| !p.is_empty()) {
            let p = PathBuf::from(p);
            return match p.strip_prefix("~") {
                Ok(rest) => dirs::home_dir().unwrap_or_default().join(rest),
                Err(_) => p,
            };
        }
    }
    let (new, old): (PathBuf, [PathBuf; 2]) = {
        #[cfg(target_os = "macos")]
        {
            let base = dirs::home_dir()
                .unwrap_or_default()
                .join("Library/Application Support");
            (
                base.join("FLE Link"),
                [base.join("FLE Studio"), base.join("FLE")],
            )
        }
        #[cfg(target_os = "windows")]
        {
            let base = dirs::data_local_dir().unwrap_or_default();
            (
                base.join("FLE Link"),
                [base.join("FLE Studio"), base.join("FLE")],
            )
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let base = dirs::data_local_dir().unwrap_or_default();
            (
                base.join("flelink"),
                [base.join("fle-studio"), base.join("fle")],
            )
        }
    };
    if new.exists() {
        return new;
    }
    old.into_iter().find(|p| p.is_dir()).unwrap_or(new)
}

/// Progress events every long step reports, so a TUI, a CLI and a GUI can show them their own way.
#[derive(Debug, Clone)]
pub enum Event {
    Step(String),
    Progress {
        label: String,
        done: u64,
        total: u64,
    },
    Log(String),
    Ready {
        base: String,
        engine: String,
        model: String,
        command: String,
    },
}

pub mod flow;

/// A problem the user can fix: the message is the whole story, the hint adds the next step (like `FleError` in tools/fle).
#[derive(Debug, Clone)]
pub struct FleError {
    pub message: String,
    pub hint: String,
}
impl FleError {
    pub fn new(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hint: hint.into(),
        }
    }
}
impl std::fmt::Display for FleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for FleError {}
