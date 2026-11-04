//! Models already on this machine, for the first-run view (Max 04.11.2026: "klicke hier um deinen modellordner
//! festzulegen"): GGUF files in the places FLE Link searches anyway (the models folder, `FLE_MODEL_DIRS`, the Hugging Face
//! cache, LM Studio), in Ollama's store (named through its manifests), and in folders the person adds. Folders added here
//! are kept in `<data>/model-dirs.json`, not in the settings, and are searched from then on.
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 6;
const MAX_FILES: usize = 200;

fn dirs_file(home: &Path) -> PathBuf {
    home.join("model-dirs.json")
}

/// The folders the person added (missing ones are kept: a drive may be unplugged for now).
pub fn extra_dirs(home: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(dirs_file(home))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// Add a folder to search (`~/` allowed). Refused when it is not a folder.
pub fn add_dir(home: &Path, text: &str) -> Result<PathBuf, String> {
    let t = text.trim().trim_matches('"');
    let p = match t.strip_prefix("~/") {
        Some(r) => dirs::home_dir().unwrap_or_default().join(r),
        None => PathBuf::from(t),
    };
    if !p.is_dir() {
        return Err(format!("No folder at {}", p.display()));
    }
    let p = p.canonicalize().unwrap_or(p);
    let mut all = extra_dirs(home);
    if !all.contains(&p) {
        all.push(p.clone());
    }
    let list: Vec<String> = all.iter().map(|d| d.display().to_string()).collect();
    std::fs::write(
        dirs_file(home),
        serde_json::to_string_pretty(&list).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(p)
}

fn label(root: &Path, models_dir: &Path) -> String {
    let s = root.to_string_lossy();
    if root == models_dir {
        "the models folder".into()
    } else if s.contains("huggingface") {
        "the Hugging Face cache".into()
    } else if s.contains("lmstudio") || s.contains("lm-studio") {
        "LM Studio".into()
    } else {
        root.display().to_string()
    }
}

/// `name-00002-of-00004.gguf` → Some(("name", 2, 4))
fn split_part(name: &str) -> Option<(&str, u32, u32)> {
    let stem = name.strip_suffix(".gguf")?;
    let (base, tail) = stem.rsplit_once('-')?;
    let count: u32 = tail.parse().ok()?;
    let (base, mid) = base.rsplit_once('-')?;
    if mid != "of" {
        return None;
    }
    let (base, no) = base.rsplit_once('-')?;
    Some((base, no.parse().ok()?, count))
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            let n = e.file_name().to_string_lossy().to_string();
            if depth < MAX_DEPTH && !n.starts_with('.') && n != "_downloads" {
                walk(&p, depth + 1, out);
            }
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("gguf"))
        {
            out.push(p);
        }
    }
}

/// Ollama keeps weights as `blobs/sha256-…` and names them in `manifests/<registry>/<ns>/<model>/<tag>`.
fn ollama() -> Vec<(String, PathBuf)> {
    let base = std::env::var_os("OLLAMA_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".ollama/models"));
    let mut tags = vec![];
    walk_manifests(&base.join("manifests"), 0, &mut tags);
    tags.into_iter()
        .filter_map(|m| {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&m).ok()?).ok()?;
            let layer = v["layers"]
                .as_array()?
                .iter()
                .find(|l| l["mediaType"] == "application/vnd.ollama.image.model")?;
            let blob = base
                .join("blobs")
                .join(layer["digest"].as_str()?.replace(':', "-"));
            let tag = m.file_name()?.to_string_lossy().to_string();
            let model = m.parent()?.file_name()?.to_string_lossy().to_string();
            blob.is_file().then(|| (format!("{model}:{tag}"), blob))
        })
        .collect()
}

fn walk_manifests(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 4 {
            walk_manifests(&p, depth + 1, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

/// Every model found, one entry per model (split files as their first part with the summed size), newest first; files the
/// library already lists (`known`, canonical paths) are left out.
pub fn found(home: &Path, models_dir: &Path, known: &HashSet<PathBuf>) -> Vec<Value> {
    let extra = extra_dirs(home);
    let mut roots = flelink_core::locate::search_roots(models_dir);
    roots.extend(extra.iter().filter(|d| d.is_dir()).cloned());
    roots.dedup();
    let mut out = vec![];
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in &roots {
        let mut files = vec![];
        walk(root, 0, &mut files);
        for f in files {
            let name = f
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let lower = name.to_lowercase();
            if lower.contains("mmproj") || lower.contains("-mtp") || lower.contains("draft") {
                continue; // helpers, not models to chat with
            }
            let size = match split_part(&name) {
                Some((_, n, _)) if n != 1 => continue,
                Some((base, _, count)) => (1..=count)
                    .map(|i| f.with_file_name(format!("{base}-{i:05}-of-{count:05}.gguf")))
                    .filter_map(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len())
                    .sum(),
                None => std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0),
            };
            let canon = f.canonicalize().unwrap_or(f.clone());
            if known.contains(&canon) || !seen.insert(canon) {
                continue;
            }
            let modified = std::fs::metadata(&f).and_then(|m| m.modified()).ok();
            out.push((modified, json!({"name": name.trim_end_matches(".gguf"), "path": f.display().to_string(), "size": size,
                "where": label(root, models_dir)})));
        }
    }
    for (name, blob) in ollama() {
        let canon = blob.canonicalize().unwrap_or(blob.clone());
        if known.contains(&canon) || !seen.insert(canon) {
            continue;
        }
        let size = std::fs::metadata(&blob).map(|m| m.len()).unwrap_or(0);
        out.push((std::fs::metadata(&blob).and_then(|m| m.modified()).ok(),
            json!({"name": name, "path": blob.display().to_string(), "size": size, "where": "Ollama"})));
    }
    out.sort_by_key(|x| std::cmp::Reverse(x.0));
    out.into_iter().map(|(_, v)| v).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_models_count_once_and_added_folders_are_searched() {
        let tmp = std::env::temp_dir().join(format!("fle-found-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (home, models, mine) = (tmp.join("home"), tmp.join("models"), tmp.join("my models"));
        for d in [&home, &models, &mine] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(mine.join("big-00001-of-00002.gguf"), vec![0u8; 10]).unwrap();
        std::fs::write(mine.join("big-00002-of-00002.gguf"), vec![0u8; 5]).unwrap();
        std::fs::write(mine.join("small.gguf"), vec![0u8; 3]).unwrap();
        std::fs::write(mine.join("mmproj-small.gguf"), vec![0u8; 3]).unwrap();
        assert!(found(&home, &models, &HashSet::new())
            .iter()
            .all(|v| !v["path"].as_str().unwrap().contains("my models")));
        assert!(add_dir(&home, "/definitely/not/here").is_err());
        add_dir(&home, mine.to_str().unwrap()).unwrap();
        add_dir(&home, mine.to_str().unwrap()).unwrap();
        assert_eq!(extra_dirs(&home).len(), 1, "added once");
        let f: Vec<Value> = found(&home, &models, &HashSet::new())
            .into_iter()
            .filter(|v| v["path"].as_str().unwrap().contains("my models"))
            .collect();
        let mut names: Vec<&str> = f.iter().map(|v| v["name"].as_str().unwrap()).collect();
        names.sort();
        assert_eq!(names, vec!["big-00001-of-00002", "small"]);
        assert_eq!(
            f.iter()
                .find(|v| v["name"] == "big-00001-of-00002")
                .unwrap()["size"],
            15
        );
        let known: HashSet<PathBuf> = [mine.join("small.gguf").canonicalize().unwrap()]
            .into_iter()
            .collect();
        assert!(
            found(&home, &models, &known)
                .iter()
                .all(|v| v["name"] != "small"),
            "already in the library"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
