//! Plugins: folders in `<data>/plugins/<id>/` that bring engines and recipes. A plugin is data, not code that runs inside
//! FLE Link: `plugin.toml` names engines (where their server comes from, how it is started) and `recipes/` holds recipes.
//! See docs/PLUGINS.md.
//!
//! An engine from a plugin must speak like `llama-server` (`-m`, `-c`, `--host`, `--port`, `/health`, `/v1`), which covers
//! llama.cpp forks. Engines with their own front end need a built-in adapter (crate::adapter).
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub engine: Vec<PluginEngine>,
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEngine {
    /// The id recipes use in `engine.prefer`. Must not be one of the built-in ids.
    pub id: String,
    pub label: String,
    /// A server the user built: an absolute path.
    #[serde(default)]
    pub path: Option<String>,
    /// Or a GitHub release FLE Link downloads (SHA-256 checked) and unpacks into `<data>/engines/<id>-<tag>/`.
    #[serde(default)]
    pub release: Option<ReleaseSpec>,
    /// Arguments appended to the automatic ones, and environment for the server.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSpec {
    /// `owner/name` on GitHub.
    pub repo: String,
    /// A tag; empty or missing: the latest release.
    #[serde(default)]
    pub tag: String,
    /// Per system (`linux-x64`, `linux-arm64`, `windows-x64`, `macos-arm64`, `macos-x64`): words the archive name must
    /// contain. A system without an entry cannot install this engine.
    pub assets: BTreeMap<String, Vec<String>>,
    /// The server inside the archive.
    #[serde(default = "llama_server")]
    pub binary: String,
}

fn llama_server() -> String {
    "llama-server".into()
}

const BUILTIN_ENGINES: [&str; 3] = ["fle", "upstream", "custom"];

/// A plain name: starts with a letter or digit (so never `.` or `..`), then letters, digits, `.`, `_`, `-`.
fn slug(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

impl Manifest {
    pub fn parse(text: &str, dir: &Path) -> Result<Manifest> {
        let mut m: Manifest = toml::from_str(text)
            .with_context(|| format!("not a valid plugin.toml in {}", dir.display()))?;
        m.dir = dir.to_path_buf();
        let folder = dir.file_name().unwrap_or_default().to_string_lossy();
        if !slug(&m.id) || m.id != folder {
            bail!(
                "plugin id '{}' must be a plain name and match its folder '{folder}'",
                m.id
            );
        }
        for e in &m.engine {
            if !slug(&e.id) || BUILTIN_ENGINES.contains(&e.id.as_str()) {
                bail!("plugin {}: engine id '{}' is not allowed (plain name, not fle/upstream/custom)", m.id, e.id);
            }
            match (&e.path, &e.release) {
                (Some(p), None) if Path::new(p).is_absolute() => {}
                (Some(_), None) => bail!("plugin {}: engine {} path must be absolute", m.id, e.id),
                (None, Some(r)) => {
                    let ok = r
                        .repo
                        .split_once('/')
                        .is_some_and(|(a, b)| slug(a) && slug(b));
                    if !ok || r.assets.is_empty() || !slug(&r.binary) {
                        bail!("plugin {}: engine {} release needs repo owner/name, assets and a plain binary name", m.id, e.id);
                    }
                }
                _ => bail!(
                    "plugin {}: engine {} needs exactly one of path or release",
                    m.id,
                    e.id
                ),
            }
            for k in e.env.keys() {
                if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    bail!("plugin {}: environment name '{k}' is not allowed", m.id);
                }
            }
        }
        Ok(m)
    }
}

/// Every plugin in `<data>/plugins/`, and the folders that could not be read (shown, never fatal). An engine id that two
/// plugins define is kept from the first (sorted by folder) and reported.
pub fn load_all(home: &Path) -> (Vec<Manifest>, Vec<String>) {
    let mut out: Vec<Manifest> = vec![];
    let mut problems = vec![];
    let Ok(rd) = std::fs::read_dir(home.join("plugins")) else {
        return (out, problems);
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        let f = d.join("plugin.toml");
        if !f.exists() {
            continue; // a folder with recipes only is fine
        }
        match std::fs::read_to_string(&f)
            .map_err(anyhow::Error::from)
            .and_then(|t| Manifest::parse(&t, &d))
        {
            Ok(mut m) => {
                m.engine.retain(|e| {
                    let taken = out.iter().any(|o| o.engine.iter().any(|x| x.id == e.id));
                    if taken {
                        problems.push(format!(
                            "plugin {}: engine {} is already defined by another plugin",
                            m.id, e.id
                        ));
                    }
                    !taken
                });
                out.push(m);
            }
            Err(e) => problems.push(format!("{e:#}")),
        }
    }
    (out, problems)
}

/// The engine with this id from any plugin.
pub fn engine(home: &Path, id: &str) -> Option<(Manifest, PluginEngine)> {
    load_all(home).0.into_iter().find_map(|m| {
        m.engine
            .iter()
            .find(|e| e.id == id)
            .cloned()
            .map(|e| (m.clone(), e))
    })
}

/// The server of a plugin engine: the user's path, or the release installed (downloading it the first time).
pub fn server(
    home: &Path,
    e: &PluginEngine,
    os: &str,
    arch: &str,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<PathBuf> {
    if let Some(p) = &e.path {
        let p = PathBuf::from(p);
        if !p.is_file() {
            bail!("engine {}: {} does not exist", e.id, p.display());
        }
        return Ok(p);
    }
    let r = e
        .release
        .as_ref()
        .context("engine has neither path nor release")?;
    let key = format!("{os}-{arch}");
    let tokens = r
        .assets
        .get(&key)
        .with_context(|| format!("engine {} has no build for {key}", e.id))?;
    let tag = (!r.tag.is_empty()).then_some(r.tag.as_str());
    crate::engines::install_from_release(home, &e.id, &r.repo, tag, tokens, &r.binary, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("fle-plugin-{}-{name}", std::process::id()))
            .join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn manifest_checks() {
        let d = dir("ik");
        let ok = r#"
id = "ik"
title = "ik_llama.cpp"
[[engine]]
id = "ik-llama"
label = "ik_llama.cpp"
args = ["-fmoe"]
[engine.release]
repo = "owner/ik_llama.cpp"
assets = { "linux-x64" = ["ubuntu", "x64"] }
"#;
        let m = Manifest::parse(ok, &d).unwrap();
        assert_eq!(m.engine[0].release.as_ref().unwrap().binary, "llama-server");
        let bad = |from: &str, to: &str| Manifest::parse(&ok.replace(from, to), &d).is_err();
        assert!(
            bad("id = \"ik\"", "id = \"other\""),
            "id must match the folder"
        );
        assert!(
            bad("id = \"ik-llama\"", "id = \"fle\""),
            "built-in engine id"
        );
        assert!(bad("repo = \"owner/ik_llama.cpp\"", "repo = \"../x\""));
        assert!(
            bad(
                "[engine.release]",
                "path = \"relative/llama-server\"\n[engine.release]"
            ),
            "both path and release"
        );
        let _ = std::fs::remove_dir_all(d.parent().unwrap());
    }
}
