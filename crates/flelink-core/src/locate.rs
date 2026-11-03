//! Models that are already on this machine: before a recipe downloads a file, look for it where models usually are, and
//! link a copy that is the same file instead of downloading it again.
//!
//! Places, in this order: the models folder, the folders in `FLE_MODEL_DIRS` (separated like PATH), the Hugging Face
//! cache, LM Studio's folders, and Ollama's blobs (named by their SHA-256). A file counts as the same when its name matches
//! (the name in the repo or the name the recipe stores it under) and, when the recipe gives a SHA-256, its contents match
//! too: that check reads the file once, and the result is kept in `<data>/cache/checked-files.json` by path, size and
//! modification time. Without a SHA-256 the name has to do (and the size, when the recipe gives one). A found file is
//! linked (a symbolic link; on Windows a hard link, which needs the same drive), never moved or changed.
use crate::recipe::{Fetch, Recipe};
use crate::Event;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 7;

/// The folders searched besides `models`, in order.
pub fn search_roots(models: &Path) -> Vec<PathBuf> {
    let mut out = vec![models.to_path_buf()];
    if let Some(v) = std::env::var_os("FLE_MODEL_DIRS") {
        out.extend(std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()));
    }
    let home = dirs::home_dir().unwrap_or_default();
    let hf = std::env::var_os("HF_HUB_CACHE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HF_HOME").map(|h| PathBuf::from(h).join("hub")))
        .unwrap_or_else(|| home.join(".cache/huggingface/hub"));
    out.push(hf);
    out.push(home.join(".lmstudio/models"));
    out.push(home.join(".cache/lm-studio/models"));
    out.retain(|p| p.is_dir());
    out.dedup();
    out
}

fn ollama_blob(sha: &str) -> Option<PathBuf> {
    let base = std::env::var_os("OLLAMA_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".ollama/models"));
    let p = base.join("blobs").join(format!("sha256-{sha}"));
    p.is_file().then_some(p)
}

/// Every `.gguf` below the roots by file name (symbolic links to files count; linked folders are not entered, so a
/// cycle cannot trap the search).
fn index(roots: &[PathBuf]) -> HashMap<String, Vec<PathBuf>> {
    let mut out: HashMap<String, Vec<PathBuf>> = HashMap::new();
    fn walk(dir: &Path, depth: usize, out: &mut HashMap<String, Vec<PathBuf>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&p, depth + 1, out);
                }
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name.to_lowercase().ends_with(".gguf") && p.is_file() {
                out.entry(name).or_default().push(p);
            }
        }
    }
    for r in roots {
        walk(r, 0, &mut out);
    }
    out
}

/// SHA-256 of a file, remembered by path, size and modification time.
struct Checked {
    file: PathBuf,
    map: Map<String, Value>,
}

impl Checked {
    fn open(home: &Path) -> Checked {
        let file = home.join("cache").join("checked-files.json");
        let map = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        Checked { file, map }
    }

    fn key(p: &Path) -> Option<String> {
        let m = std::fs::metadata(p).ok()?;
        let t = m
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(format!(
            "{}|{}|{t}",
            p.canonicalize().ok()?.display(),
            m.len()
        ))
    }

    /// A file already read once with this checksum and unchanged since (same size and time), whatever its name.
    fn known(&self, sha: &str) -> Option<PathBuf> {
        self.map.iter().find_map(|(k, v)| {
            if v.as_str() != Some(sha) {
                return None;
            }
            let path = PathBuf::from(k.rsplitn(3, '|').nth(2)?);
            (Self::key(&path).as_deref() == Some(k.as_str())).then_some(path)
        })
    }

    fn remember(&mut self, p: &Path, sha: &str) {
        if let Some(key) = Self::key(p) {
            self.map.insert(key, json!(sha));
            if let Some(d) = self.file.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&self.file, Value::Object(self.map.clone()).to_string());
        }
    }

    fn sha(&mut self, p: &Path, say: &mut dyn FnMut(Event)) -> Option<String> {
        let key = Self::key(p)?;
        if let Some(s) = self.map.get(&key).and_then(Value::as_str) {
            return Some(s.to_string());
        }
        say(Event::Step(format!(
            "Checking {} (read once, then remembered)",
            p.display()
        )));
        let mut f = std::fs::File::open(p).ok()?;
        let total = f.metadata().map(|m| m.len()).unwrap_or(0);
        let label = format!(
            "checking {}",
            p.file_name().unwrap_or_default().to_string_lossy()
        );
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 8 << 20];
        let (mut done, mut shown) = (0u64, std::time::Instant::now());
        loop {
            let n = f.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            done += n as u64;
            // a model of 100 GB takes minutes to read: the bar shows that something happens
            if shown.elapsed().as_millis() >= 250 {
                shown = std::time::Instant::now();
                say(Event::Progress {
                    label: label.clone(),
                    done,
                    total,
                });
            }
        }
        say(Event::Progress { label, done, total });
        let s = hex::encode(h.finalize());
        self.map.insert(key, json!(s));
        if let Some(d) = self.file.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&self.file, Value::Object(self.map.clone()).to_string());
        Some(s)
    }
}

/// A copy of one file of the recipe that is already on this machine.
fn find(
    x: &Fetch,
    names: &[String],
    idx: &HashMap<String, Vec<PathBuf>>,
    dest: &Path,
    checked: &mut Checked,
    say: &mut dyn FnMut(Event),
) -> Option<PathBuf> {
    if let Some(sha) = &x.sha256 {
        if let Some(b) = ollama_blob(sha) {
            return Some(b);
        }
    }
    // the checksum decides, not the name: a file read once with this SHA-256 (another recipe's copy) is the same file
    if let Some(sha) = &x.sha256 {
        let dest_real = dest.canonicalize().ok();
        if let Some(p) = checked
            .known(sha)
            .filter(|p| p.canonicalize().ok() != dest_real)
        {
            return Some(p);
        }
    }
    let dest_real = dest.canonicalize().ok();
    for name in names {
        for p in idx.get(name).into_iter().flatten() {
            if p.canonicalize().ok() == dest_real && dest_real.is_some() {
                continue;
            }
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            if x.size > 0 && size != x.size {
                continue;
            }
            match &x.sha256 {
                Some(want) => {
                    if checked.sha(p, say).as_deref() == Some(want.as_str()) {
                        return Some(p.clone());
                    }
                }
                None => return Some(p.clone()),
            }
        }
    }
    None
}

fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(from, to)
    }
    #[cfg(not(unix))]
    {
        std::fs::hard_link(from, to)
    }
}

/// Link every file of the recipe that is missing in `models` but already elsewhere on this machine; what was linked.
pub fn adopt(
    r: &Recipe,
    home: &Path,
    models: &Path,
    say: &mut dyn FnMut(Event),
) -> Vec<(String, PathBuf)> {
    adopt_fetches(r.fetches(), home, models, say)
}

/// The same for a list of files, for a caller that knows more (the Studio fills in the checksums Hugging Face lists).
pub fn adopt_fetches(
    fetches: Vec<Fetch>,
    home: &Path,
    models: &Path,
    say: &mut dyn FnMut(Event),
) -> Vec<(String, PathBuf)> {
    let missing: Vec<Fetch> = fetches
        .into_iter()
        .filter(|x| !models.join(&x.local).exists())
        .collect();
    if missing.is_empty() {
        return vec![];
    }
    let roots = search_roots(models);
    let idx = index(&roots);
    let mut checked = Checked::open(home);
    let mut out = vec![];
    for x in missing {
        let dest = models.join(&x.local);
        let repo_name = Path::new(&x.file)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let local_name = x
            .local
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut names = vec![repo_name];
        if !names.contains(&local_name) {
            names.push(local_name);
        }
        // the same file (same SHA-256) that another recipe keeps under its own name, e.g. a part two quants share
        if let Some(sha) = &x.sha256 {
            for other in crate::recipe::builtin().iter().flat_map(|r| r.fetches()) {
                if other.sha256.as_deref() == Some(sha.as_str()) {
                    let n = other
                        .local
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if !names.contains(&n) {
                        names.push(n);
                    }
                }
            }
        }
        if let Some(found) = find(&x, &names, &idx, &dest, &mut checked, say) {
            match link(&found, &dest) {
                Ok(()) => {
                    say(Event::Step(format!(
                        "Using {} (already on this machine)",
                        found.display()
                    )));
                    out.push((x.file.clone(), found));
                }
                Err(e) => say(Event::Step(format!(
                    "Found {} but could not link it ({e}); downloading instead",
                    found.display()
                ))),
            }
        }
    }
    out
}

/// Note the checksum of a file FLE Link just downloaded and verified, so a later recipe finds it by its SHA-256 under
/// any name (two quants that share a part).
pub fn remember_checked(home: &Path, path: &Path, sha: &str) {
    Checked::open(home).remember(path, sha);
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_file_checked_once_is_found_by_its_checksum_under_any_name() {
        let dir = std::env::temp_dir().join(format!("flelink-sha-{}", std::process::id()));
        let home = dir.join("home");
        let models = home.join("models");
        let other = models.join("q-a").join("shared-part-00006.gguf");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, b"the same bytes").unwrap();
        let sha = hex::encode(sha2::Sha256::digest(b"the same bytes"));
        super::remember_checked(&home, &other, &sha);
        let x = crate::recipe::Fetch {
            repo: "o/r".into(),
            revision: "main".into(),
            file: "Engram.gguf".into(),
            local: PathBuf::from("q-b/other-name-00006.gguf"),
            sha256: Some(sha.clone()),
            size: 14,
        };
        let got = adopt_fetches(vec![x], &home, &models, &mut |_| {});
        assert_eq!(got.len(), 1, "linked although the name differs");
        assert_eq!(
            std::fs::read(models.join("q-b/other-name-00006.gguf")).unwrap(),
            b"the same bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    #[test]
    fn copies_elsewhere_are_linked_when_name_and_checksum_match() {
        let tmp = std::env::temp_dir().join(format!("fle-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (home, models, other) = (tmp.join("home"), tmp.join("models"), tmp.join("elsewhere"));
        std::fs::create_dir_all(other.join("sub")).unwrap();
        std::fs::create_dir_all(&models).unwrap();
        let good = b"the model".to_vec();
        let sha = hex::encode(Sha256::digest(&good));
        // same name, other contents: not taken; same name and contents one folder down: taken
        std::fs::write(other.join("m-00001-of-00002.gguf"), b"something else").unwrap();
        std::fs::write(other.join("sub/m-00001-of-00002.gguf"), &good).unwrap();
        // the second part only under its repo name, no checksum in the recipe: the name decides
        std::fs::write(other.join("odd-name.gguf"), b"part two").unwrap();
        let text = format!(
            "id = \"t\"\ntitle = \"t\"\n[model]\nrepo = \"a/b\"\nfiles = [\"m-00001-of-00002.gguf\", \"odd-name.gguf\"]\n\
             [model.save_as]\n\"odd-name.gguf\" = \"m-00002-of-00002.gguf\"\n[model.sha256]\n\"m-00001-of-00002.gguf\" = \"{sha}\"\n"
        );
        let r = Recipe::parse(&text, "t").unwrap();
        std::env::set_var("FLE_MODEL_DIRS", &other);
        let mut said = vec![];
        let got = adopt(&r, &home, &models, &mut |e| {
            if let Event::Step(s) = e {
                said.push(s)
            }
        });
        std::env::remove_var("FLE_MODEL_DIRS");
        assert_eq!(got.len(), 2, "{said:?}");
        let first = models.join("a__b/m-00001-of-00002.gguf");
        assert_eq!(std::fs::read(&first).unwrap(), good);
        assert_eq!(
            std::fs::read(models.join("a__b/m-00002-of-00002.gguf")).unwrap(),
            b"part two"
        );
        assert_eq!(r.missing_bytes(&models), 0);
        // the check was remembered: a second look reads nothing again
        let c = Checked::open(&home);
        assert!(!c.map.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
