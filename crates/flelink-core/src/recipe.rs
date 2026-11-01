//! Recipes: a model, the engine it wants, its start settings and chat defaults, as one file people can share.
//!
//! A recipe is TOML (see docs/RECIPES.md). FLE Link finds them in four places, later ones win on equal ids:
//! built in (the catalog), `<data>/recipes/*.toml`, `<data>/plugins/<plugin>/recipes/*.toml`, and a file or URL given
//! on the command line. A recipe never runs code: it only names files to download and values to pass to the engine.
use crate::catalog;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    /// Stable name, also the command argument: `flelink run <id>`. Letters, digits, `.`, `_`, `-`.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    pub model: ModelSpec,
    #[serde(default, skip_serializing_if = "EngineSpec::is_default")]
    pub engine: EngineSpec,
    #[serde(default, skip_serializing_if = "RunSpec::is_default")]
    pub run: RunSpec,
    #[serde(default, skip_serializing_if = "Requires::is_default")]
    pub requires: Requires,
    #[serde(default, skip_serializing_if = "ChatSpec::is_default")]
    pub chat: ChatSpec,
    /// Where it came from: "builtin", a file path, a plugin id. Filled in by the loader, never read from the file.
    #[serde(skip)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    /// Hugging Face repo, `owner/name`.
    pub repo: String,
    #[serde(default = "main_rev", skip_serializing_if = "is_main")]
    pub revision: String,
    /// Every file of the model (all parts of a split GGUF), paths inside the repo; the first one is passed to the engine.
    pub files: Vec<String>,
    /// SHA-256 per file, when known (path -> hex). Files without one are checked by size only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sha256: BTreeMap<String, String>,
    /// Total bytes of all files, for the fit check before anything is downloaded.
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EngineSpec {
    /// Engine ids in order of preference (`fle`, `upstream`, or one a plugin brings). Empty: the best for this machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefer: Vec<String>,
    /// Only these engines may run it (for a model that needs FLE's expert offloading).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    /// Context in tokens; default: sized from the memory the model leaves free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
    /// Extra engine arguments, appended after the automatic ones (the engine takes the last value of a flag).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment for the engine; overrides the automatic values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_gib: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_gib: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChatSpec {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub system: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
}

fn main_rev() -> String {
    "main".into()
}
fn is_main(s: &String) -> bool {
    s == "main"
}
impl EngineSpec {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
impl RunSpec {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
impl Requires {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
impl ChatSpec {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Recipe {
    /// Read and check a recipe from TOML text.
    pub fn parse(text: &str, source: &str) -> Result<Recipe> {
        let mut r: Recipe =
            toml::from_str(text).with_context(|| format!("not a valid recipe: {source}"))?;
        r.source = source.to_string();
        r.check()?;
        Ok(r)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    fn check(&self) -> Result<()> {
        let slug = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        };
        if !slug(&self.id) {
            bail!(
                "recipe id '{}' may only use letters, digits, '.', '_' and '-'",
                self.id
            );
        }
        let repo_ok = self
            .model
            .repo
            .split_once('/')
            .is_some_and(|(a, b)| slug(a) && slug(b));
        if !repo_ok {
            bail!("recipe {}: model.repo must look like owner/name", self.id);
        }
        if self.model.files.is_empty() {
            bail!("recipe {}: model.files lists no file", self.id);
        }
        for f in &self.model.files {
            let p = Path::new(f);
            let normal = p
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
            if !normal || !f.to_lowercase().ends_with(".gguf") {
                bail!(
                    "recipe {}: '{f}' is not a .gguf path inside the repo",
                    self.id
                );
            }
        }
        for k in self.run.env.keys() {
            if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                bail!("recipe {}: environment name '{k}' is not allowed", self.id);
            }
        }
        if let Some(c) = self.run.context {
            if !(512..=1_048_576).contains(&c) {
                bail!(
                    "recipe {}: run.context {c} is outside 512 to 1048576",
                    self.id
                );
            }
        }
        Ok(())
    }

    /// Where the files go: `<data>/models/<owner>__<name>/<path>`, like the catalog.
    pub fn file_path(&self, home: &Path, file: &str) -> PathBuf {
        home.join("models")
            .join(self.model.repo.replace('/', "__"))
            .join(file)
    }

    /// The file passed to the engine (the first part of a split model).
    pub fn main_file(&self, home: &Path) -> PathBuf {
        self.file_path(home, &self.model.files[0])
    }

    pub fn needs_fle(&self) -> bool {
        self.engine.strict && self.engine.prefer.iter().all(|e| e == "fle")
    }

    /// Put the recipe's start values over an automatic plan.
    pub fn apply(&self, plan: &mut crate::plan::Plan) {
        if let Some(c) = self.run.context {
            plan.ctx = c;
            if let Some(i) = plan.args.iter().position(|a| a == "-c") {
                if i + 1 < plan.args.len() {
                    plan.args[i + 1] = c.to_string();
                }
            }
        }
        plan.args.extend(self.run.args.iter().cloned());
        for (k, v) in &self.run.env {
            match plan.env.iter_mut().find(|(a, _)| a == k) {
                Some(e) => e.1 = v.clone(),
                None => plan.env.push((k.clone(), v.clone())),
            }
        }
    }
}

/// The built-in catalog as recipes.
pub fn builtin() -> Vec<Recipe> {
    catalog::builtin()
        .into_iter()
        .map(|m| {
            let mut sha = BTreeMap::new();
            if let Some(s) = m.sha256 {
                sha.insert(m.file.to_string(), s.to_string());
            }
            Recipe {
                id: m.id.into(),
                title: m.title.into(),
                note: m.note.into(),
                model: ModelSpec {
                    repo: m.repo.into(),
                    revision: "main".into(),
                    files: m.files.iter().map(|f| f.to_string()).collect(),
                    sha256: sha,
                    size: m.size,
                },
                engine: EngineSpec {
                    prefer: if m.fle_only {
                        vec!["fle".into()]
                    } else {
                        vec![]
                    },
                    strict: m.fle_only,
                },
                run: RunSpec::default(),
                requires: Requires::default(),
                chat: ChatSpec::default(),
                source: "builtin".into(),
            }
        })
        .collect()
}

fn read_dir_recipes(dir: &Path, source: &str, out: &mut Vec<Recipe>, problems: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    for p in files {
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let src = if source.is_empty() {
            p.to_string_lossy().to_string()
        } else {
            format!(
                "{source}: {}",
                p.file_name().unwrap_or_default().to_string_lossy()
            )
        };
        match Recipe::parse(&text, &src) {
            Ok(r) => out.push(r),
            Err(e) => problems.push(format!("{e:#}")),
        }
    }
}

/// Every recipe this data folder knows, later sources replacing earlier ones with the same id, and the files that could
/// not be read (shown, never fatal).
pub fn load_all(home: &Path) -> (Vec<Recipe>, Vec<String>) {
    let mut all = builtin();
    let mut problems = vec![];
    read_dir_recipes(&home.join("recipes"), "", &mut all, &mut problems);
    if let Ok(rd) = std::fs::read_dir(home.join("plugins")) {
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for d in dirs {
            let id = d
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            read_dir_recipes(
                &d.join("recipes"),
                &format!("plugin {id}"),
                &mut all,
                &mut problems,
            );
        }
    }
    let mut out: Vec<Recipe> = vec![];
    for r in all {
        match out.iter_mut().find(|x| x.id == r.id) {
            Some(x) => *x = r,
            None => out.push(r),
        }
    }
    (out, problems)
}

/// A recipe by id, or from a `.toml` file or an `https://` URL.
pub fn find(home: &Path, what: &str) -> Result<Recipe> {
    if what.starts_with("https://") {
        let text = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?
            .get(what)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("could not fetch {what}"))?
            .text()?;
        return Recipe::parse(&text, what);
    }
    if what.ends_with(".toml") {
        let text =
            std::fs::read_to_string(what).with_context(|| format!("could not read {what}"))?;
        return Recipe::parse(&text, what);
    }
    let (all, _) = load_all(home);
    all.into_iter()
        .find(|r| r.id == what)
        .with_context(|| format!("no recipe '{what}' (try: flelink recipes)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
id = "qwen-coder-long"
title = "Qwen coder with a long context"
[model]
repo = "owner/Qwen-Coder-GGUF"
files = ["Q4/qwen-Q4_K_M-00001-of-00002.gguf", "Q4/qwen-Q4_K_M-00002-of-00002.gguf"]
size = 20000000000
[engine]
prefer = ["fle", "upstream"]
[run]
context = 65536
args = ["--cache-reuse", "256"]
env = { FLE_MOE_CACHE_BIAS = "0.002" }
[chat]
thinking = false
temperature = 0.2
"#;

    #[test]
    fn parse_check_and_round_trip() {
        let r = Recipe::parse(EXAMPLE, "test").unwrap();
        assert_eq!(r.model.files.len(), 2);
        assert_eq!(r.run.context, Some(65536));
        let again = Recipe::parse(&r.to_toml(), "again").unwrap();
        assert_eq!(again.model, r.model);
        assert_eq!(again.run, r.run);
        assert_eq!(again.chat, r.chat);
    }

    #[test]
    fn refuses_bad_values() {
        let bad = |from: &str, to: &str| Recipe::parse(&EXAMPLE.replace(from, to), "t").is_err();
        assert!(bad("qwen-coder-long", "../evil"));
        assert!(bad(
            "Q4/qwen-Q4_K_M-00001-of-00002.gguf",
            "../../etc/passwd.gguf"
        ));
        assert!(bad("owner/Qwen-Coder-GGUF", "not a repo"));
        assert!(bad("FLE_MOE_CACHE_BIAS", "\"BAD NAME\""));
        assert!(bad("context = 65536", "context = 10"));
        assert!(bad("temperature = 0.2", "temperature = 0.2\nunknown = 1"));
    }

    #[test]
    fn apply_overrides_the_plan() {
        let r = Recipe::parse(EXAMPLE, "test").unwrap();
        let mut p = crate::plan::Plan {
            env: vec![("FLE_MOE_CACHE_BIAS".into(), "0.001".into())],
            args: vec!["-c".into(), "8192".into(), "--jinja".into()],
            port: 8080,
            ctx: 8192,
        };
        r.apply(&mut p);
        assert_eq!(p.ctx, 65536);
        assert_eq!(p.args, ["-c", "65536", "--jinja", "--cache-reuse", "256"]);
        assert_eq!(
            p.env,
            [("FLE_MOE_CACHE_BIAS".to_string(), "0.002".to_string())]
        );
    }

    #[test]
    fn builtin_split_model_lists_every_part() {
        let fle = builtin().into_iter().find(|r| r.id == "fle-iq3-m").unwrap();
        assert!(fle.model.files.len() > 1, "{:?}", fle.model.files);
        assert!(fle.needs_fle());
    }

    #[test]
    fn user_recipe_replaces_builtin_and_bad_files_are_reported() {
        let home = std::env::temp_dir().join(format!("fle-recipes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("recipes")).unwrap();
        std::fs::write(
            home.join("recipes/a.toml"),
            EXAMPLE.replace("qwen-coder-long", "llama-3.2-1b"),
        )
        .unwrap();
        std::fs::write(home.join("recipes/b.toml"), "id = 'x'").unwrap();
        let (all, problems) = load_all(&home);
        let l = all.iter().find(|r| r.id == "llama-3.2-1b").unwrap();
        assert!(l.source.ends_with("a.toml"));
        assert_eq!(problems.len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }
}
