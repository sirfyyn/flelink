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

pub trait EngineAdapter: Sync {
    /// Stable id: stored in profiles and settings, shown in the plan.
    fn id(&self) -> &'static str;
    /// Name for people ("FLE", "Upstream llama.cpp").
    fn label(&self) -> &'static str;
    /// Short name inside a sentence ("Install the FLE engine first").
    fn short(&self) -> &'static str;
    /// Can FLE Link download this engine for the user? A custom engine is always the user's own binary.
    fn installable(&self) -> bool;
    /// The address that answers once the model is loaded.
    fn health_path(&self) -> &'static str {
        "/health"
    }
    fn capabilities(&self) -> Capabilities;
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
    fn installable(&self) -> bool {
        true
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
    fn installable(&self) -> bool {
        true
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
    fn installable(&self) -> bool {
        false
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
    }
}
