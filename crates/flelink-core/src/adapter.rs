//! Engine adapters: what FLE Link needs to know about an inference engine, in one place.
//!
//! Outside this file nothing should compare an engine id with a string. See docs/ENGINE-ADAPTER.md for the plan: this is its
//! first step (identity, naming, which rules apply, the health address and the capabilities); installing, building the
//! command and reading the log follow, adapter by adapter, with the comparison tests staying green.

/// What an engine can do. The defaults are what plain `llama-server` offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Multi-token prediction heads (`--spec-type mtp`).
    pub mtp: bool,
    /// The FLE policy variables (expert cache, pager, overlap) mean something to this engine.
    pub fle_policy: bool,
    /// Draft-free n-gram speculation.
    pub ngram: bool,
    /// LoRA adapters (`--lora`).
    pub lora: bool,
}

/// Where FLE Link downloads an engine from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Release {
    /// GitHub `owner/name`
    pub repo: &'static str,
    /// The tag to install when the user names none; `None` is the latest release.
    pub default_tag: Option<&'static str>,
    /// Look for a newer release once a day and install it (only for an engine that follows the latest release).
    pub self_updates: bool,
}

pub const FLE_REPO: &str = "sirfyyn/FastLocalExperts";
pub const UPSTREAM_REPO: &str = "ggml-org/llama.cpp";
/// The llama.cpp build the upstream engine is pinned to (tested with it).
pub const LLAMA_TAG: &str = "b11540";

/// Log lines of llama-server (and its forks) that mark a loading phase (checked in order, first match wins).
pub const LLAMA_PHASES: [(&str, &str); 9] = [
    ("listening on", "Ready"),
    ("model loaded", "Starting the server"),
    ("warming up", "Warming up"),
    (
        "moe expert cache enabled",
        "Setting up the GPU expert cache",
    ),
    ("llama_kv_cache", "Allocating the KV cache"),
    ("llama_context:", "Creating the context"),
    ("load_tensors:", "Loading weights"),
    ("llama_params_fit", "Fitting the model into GPU memory"),
    ("llama_model_loader: loaded meta data", "Reading the model"),
];

/// Error lines of llama-server and what to tell the user about them (regular expressions).
pub const LLAMA_HINTS: [(&str, &str); 6] = [
    (
        r"(?i)out of memory|cudaMalloc failed|failed to allocate .*buffer|ErrorOutOfDeviceMemory",
        "The GPU ran out of memory. Lower the context or the GPU expert cache on the Engine page.",
    ),
    (
        r"(?i)error while loading shared libraries|cannot open shared object|\.dll was not found|Library not loaded|image not found",
        "The engine misses a library. Reinstall the engine on the Engine page.",
    ),
    (
        r"(?i)CUDA driver version is insufficient|no CUDA-capable device|cudaGetDeviceCount",
        "CUDA could not start. Update the NVIDIA driver, or use the Vulkan or CPU engine.",
    ),
    (
        r"(?i)failed to load model|error loading model|unable to load model",
        "The engine could not load the model. Check that the download finished (Models page).",
    ),
    (
        r"(?i)couldn't bind|address already in use|bind failed|only one usage of each socket address",
        "The engine port is taken. Set the port to 0 (automatic) on the Engine page.",
    ),
    (
        r"(?i)error: (invalid|unknown) argument|unknown option|invalid value",
        "The engine does not know one of the arguments. Is this profile meant for another engine?",
    ),
];

pub trait EngineAdapter: Sync {
    /// Stable id: stored in profiles and settings, shown in the plan.
    fn id(&self) -> &'static str;
    /// Name for people ("FLE", "Upstream llama.cpp").
    fn label(&self) -> &'static str;
    /// Short name inside a sentence ("Install the FLE engine first").
    fn short(&self) -> &'static str;
    /// Where FLE Link downloads it; `None` for an engine that is always the user's own binary.
    fn release(&self) -> Option<Release>;
    /// Can FLE Link download this engine for the user?
    fn installable(&self) -> bool {
        self.release().is_some()
    }
    /// The address that answers once the model is loaded.
    fn health_path(&self) -> &'static str {
        "/health"
    }
    fn capabilities(&self) -> Capabilities;
    /// Log lines that mark a loading phase: (text in the lower-cased line, phase shown). First match wins.
    fn log_phases(&self) -> &'static [(&'static str, &'static str)] {
        &LLAMA_PHASES
    }
    /// Error lines (regular expressions) and the hint for the user. First match wins.
    fn log_hints(&self) -> &'static [(&'static str, &'static str)] {
        &LLAMA_HINTS
    }
}

pub struct Fle;
pub struct Upstream;
pub struct Custom;

impl EngineAdapter for Fle {
    fn id(&self) -> &'static str {
        "fle"
    }
    fn label(&self) -> &'static str {
        "FLE"
    }
    fn short(&self) -> &'static str {
        "FLE"
    }
    fn release(&self) -> Option<Release> {
        Some(Release {
            repo: FLE_REPO,
            default_tag: None,
            self_updates: true,
        })
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            mtp: true,
            fle_policy: true,
            ngram: true,
            lora: true,
        }
    }
}

impl EngineAdapter for Upstream {
    fn id(&self) -> &'static str {
        "upstream"
    }
    fn label(&self) -> &'static str {
        "Upstream llama.cpp"
    }
    fn short(&self) -> &'static str {
        "upstream"
    }
    fn release(&self) -> Option<Release> {
        Some(Release {
            repo: UPSTREAM_REPO,
            default_tag: Some(LLAMA_TAG),
            self_updates: false,
        })
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            mtp: false,
            fle_policy: false,
            ngram: true,
            lora: true,
        }
    }
}

impl EngineAdapter for Custom {
    fn id(&self) -> &'static str {
        "custom"
    }
    fn label(&self) -> &'static str {
        "Custom engine"
    }
    fn short(&self) -> &'static str {
        "custom"
    }
    fn release(&self) -> Option<Release> {
        None
    }
    /// A custom engine is assumed to behave like plain llama-server until its profile says more.
    fn capabilities(&self) -> Capabilities {
        Upstream.capabilities()
    }
}

static ALL: [&dyn EngineAdapter; 3] = [&Fle, &Upstream, &Custom];

/// Every adapter, in the order they are offered.
pub fn all() -> &'static [&'static dyn EngineAdapter] {
    &ALL
}

/// The adapter with this id.
pub fn get(id: &str) -> Option<&'static dyn EngineAdapter> {
    ALL.iter().copied().find(|a| a.id() == id)
}

/// The ids of the engines FLE Link installs itself ("fle", "upstream"), in the order they are offered.
pub fn installable_ids() -> Vec<&'static str> {
    ALL.iter()
        .filter(|a| a.installable())
        .map(|a| a.id())
        .collect()
}

/// Is this an engine FLE Link installs itself?
pub fn is_installable(id: &str) -> bool {
    get(id).is_some_and(|a| a.installable())
}

/// Where to download the engine with this id, if FLE Link installs it.
pub fn release_of(id: &str) -> Option<Release> {
    get(id).and_then(|a| a.release())
}

/// Do the FLE rules (policy variables, expert cache, `-fit on`) mean something to this engine?
pub fn applies_fle_policy(id: &str) -> bool {
    get(id).is_some_and(|a| a.capabilities().fle_policy)
}

/// Which engine a binary is, from its `--help` text: the FLE engine has options no other llama-server has.
pub fn detect_flavor(help: &str) -> &'static str {
    if help.contains("--moe-expert-cache-inserts") {
        Fle.id()
    } else {
        Upstream.id()
    }
}

/// "fle, upstream or custom": the ids for an error message.
pub fn id_list() -> String {
    let ids: Vec<&str> = ALL.iter().map(|a| a.id()).collect();
    match ids.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => ids.join(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_found() {
        assert_eq!(id_list(), "fle, upstream or custom");
        for a in all() {
            assert_eq!(get(a.id()).unwrap().id(), a.id());
        }
        assert!(get("vllm").is_none());
    }

    #[test]
    fn the_flavor_comes_from_the_help_text() {
        assert_eq!(
            detect_flavor("-ngl N\n--moe-expert-cache-inserts N\n"),
            "fle"
        );
        assert_eq!(detect_flavor("-ngl N\n--jinja\n"), "upstream");
        assert!(applies_fle_policy("fle"));
        assert!(!applies_fle_policy("upstream"));
        assert!(!applies_fle_policy("anything else"));
    }

    #[test]
    fn only_fle_has_the_fle_rules_and_mtp() {
        assert!(get("fle").unwrap().capabilities().fle_policy);
        assert!(!get("upstream").unwrap().capabilities().fle_policy);
        assert!(!get("custom").unwrap().capabilities().mtp);
        assert!(!get("custom").unwrap().installable());
        assert_eq!(installable_ids(), ["fle", "upstream"]);
        assert!(release_of("fle").unwrap().self_updates);
        assert_eq!(release_of("upstream").unwrap().default_tag, Some(LLAMA_TAG));
        assert!(release_of("custom").is_none() && !is_installable("vllm"));
    }
}
