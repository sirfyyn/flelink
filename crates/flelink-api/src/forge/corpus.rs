//! Calibration text for the importance matrix, the router statistics and the KL-divergence check. Port of
//! `fle_studio_forge/corpus.py`.
//!
//! The Forge mixes what you give it: text files and folders, cut into paragraphs and drawn at random (fixed seed, so the
//! same inputs give the same file) up to a character budget, with a per-source report. A small mixed text ships with FLE
//! Link (`bundled`, about 1 MB: English and German prose, code, dialogue, numbers; origin and license of every file in
//! corpus/SOURCES.md). The shuffles use Python's generator, so the files are the ones the Python Studio writes.
use crate::stats::PyRandom;
use regex::Regex;
use serde_json::{json, Value};
use sha2::Digest;
use std::path::{Path, PathBuf};

const TEXT_SUFFIXES: [&str; 18] = [
    "txt", "md", "rst", "py", "c", "h", "cpp", "js", "ts", "json", "sh", "html", "css", "yaml",
    "yml", "toml", "tex", "csv",
];
const HELD_OUT_PERCENT: u64 = 15;

static BUNDLED: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/forge/corpus");

fn unit_split(text: &str) -> Vec<String> {
    Regex::new(r"\n\s*\n")
        .unwrap()
        .split(text)
        .map(String::from)
        .collect()
}

/// `random.shuffle` of CPython: from the back, a random index in front of each place.
fn shuffle<T>(rng: &mut PyRandom, v: &mut [T]) {
    for i in (1..v.len()).rev() {
        let j = rng.randrange(i + 1);
        v.swap(i, j);
    }
}

fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python's `len(str)`: characters, not bytes.
fn clen(s: &str) -> usize {
    s.chars().count()
}

fn files(inputs: &[String]) -> Result<Vec<(String, Vec<PathBuf>)>, String> {
    let mut out = vec![];
    for item in inputs {
        let p = super::ggufio::path_buf(item);
        if p.is_file() {
            out.push((p.to_string_lossy().to_string(), vec![p]));
        } else if p.is_dir() {
            let mut found = vec![];
            let mut stack = vec![p.clone()];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    let path = e.path();
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.extension().is_some_and(|x| {
                        TEXT_SUFFIXES.contains(&x.to_string_lossy().to_lowercase().as_str())
                    }) {
                        found.push(path);
                    }
                }
            }
            found.sort();
            out.push((p.to_string_lossy().to_string(), found));
        } else {
            return Err(format!("{item} is neither a file nor a folder"));
        }
    }
    Ok(out)
}

/// Mix the inputs into one text file; every input gets an equal share of the budget, what it cannot fill goes to the others.
pub fn build(
    inputs: &[String],
    out: &Path,
    chars: usize,
    seed: u32,
    max_unit: usize,
) -> Result<Value, String> {
    if chars < 1000 {
        return Err("a corpus of fewer than 1000 characters is of no use".into());
    }
    let groups = files(inputs)?;
    let mut units: Vec<(String, Vec<String>)> = vec![];
    for (key, fs) in &groups {
        let mut found = vec![];
        for f in fs {
            let Ok(bytes) = std::fs::read(f) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            for u in unit_split(&text) {
                let t = u.trim();
                if clen(t) >= 80 {
                    found.push(truncate_chars(t, max_unit));
                }
            }
        }
        if found.is_empty() {
            return Err(format!(
                "{key} holds no usable text (looked at {} file(s))",
                fs.len()
            ));
        }
        units.push((key.clone(), found));
    }
    let mut rng = PyRandom::new(seed);
    for (_, u) in units.iter_mut() {
        shuffle(&mut rng, u);
    }
    let n = units.len() as f64;
    let mut budget: Vec<f64> = vec![chars as f64 / n; units.len()];
    let mut chosen: Vec<Vec<String>> = vec![vec![]; units.len()];
    for _ in 0..4 {
        let mut spare = 0.0;
        for (i, (_, u)) in units.iter_mut().enumerate() {
            while budget[i] > 0.0 {
                let Some(unit) = u.pop() else { break };
                budget[i] -= (clen(&unit) + 2) as f64;
                chosen[i].push(unit);
            }
            if budget[i] > 0.0 {
                spare += budget[i];
                budget[i] = 0.0;
            }
        }
        let open: Vec<usize> = (0..units.len())
            .filter(|i| !units[*i].1.is_empty())
            .collect();
        if spare <= 0.0 || open.is_empty() {
            break;
        }
        for i in &open {
            budget[*i] += spare / open.len() as f64;
        }
    }
    let mut everything: Vec<String> = chosen.iter().flatten().cloned().collect();
    shuffle(&mut rng, &mut everything);
    let text = everything.join("\n\n");
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(out, &text).map_err(|e| e.to_string())?;
    let total = chosen
        .iter()
        .flatten()
        .map(|x| clen(x))
        .sum::<usize>()
        .max(1);
    let report: Vec<Value> = units
        .iter()
        .zip(&chosen)
        .map(|((k, _), c)| {
            let ch: usize = c.iter().map(|x| clen(x)).sum();
            json!({"input": k, "units": c.len(), "chars": ch, "share": ch as f64 / total as f64})
        })
        .collect();
    let largest = report
        .iter()
        .filter_map(|r| r["share"].as_f64())
        .fold(0.0, f64::max);
    Ok(
        json!({"path": out.to_string_lossy(), "chars": clen(&text), "words": text.split_whitespace().count(),
        "sources": report, "largest_share": largest}),
    )
}

fn held_out(unit: &str) -> bool {
    // int(sha256 hex, 16) % 100: the remainder of the 256-bit number, computed digit by digit
    let d = sha2::Sha256::digest(unit.as_bytes());
    let r = d.iter().fold(0u64, |acc, b| (acc * 256 + *b as u64) % 100);
    r < HELD_OUT_PERCENT
}

/// The text that ships with FLE Link: "calibration" (about 85 % of its paragraphs) or "evaluation" (the other 15 %), split
/// by a hash of the paragraph so the two never share one.
pub fn bundled(out: &Path, part: &str, seed: u32, max_unit: usize) -> Result<Value, String> {
    if part != "calibration" && part != "evaluation" {
        return Err("part must be calibration or evaluation".into());
    }
    let mut files: Vec<&include_dir::File> = BUNDLED
        .files()
        .filter(|f| f.path().extension().is_some_and(|x| x == "txt"))
        .collect();
    files.sort_by_key(|f| f.path().to_path_buf());
    let mut units: Vec<String> = vec![];
    let mut per_file: Vec<(String, usize)> = vec![];
    for f in files {
        let before = units.len();
        let text = String::from_utf8_lossy(f.contents());
        for u in unit_split(&text) {
            let t = truncate_chars(u.trim(), max_unit);
            if clen(&t) >= 80 && held_out(&t) == (part == "evaluation") {
                units.push(t);
            }
        }
        let name = f
            .path()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        per_file.push((name, units[before..].iter().map(|x| clen(x)).sum()));
    }
    shuffle(&mut PyRandom::new(seed), &mut units);
    let text = units.join("\n\n");
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(out, &text).map_err(|e| e.to_string())?;
    let total = per_file.iter().map(|x| x.1).sum::<usize>().max(1);
    let report: Vec<Value> = per_file
        .iter()
        .map(|(k, v)| json!({"input": k, "chars": v, "share": *v as f64 / total as f64}))
        .collect();
    let largest = report
        .iter()
        .filter_map(|r| r["share"].as_f64())
        .fold(0.0, f64::max);
    Ok(
        json!({"path": out.to_string_lossy(), "chars": clen(&text), "words": text.split_whitespace().count(),
        "sources": report, "largest_share": largest, "part": part}),
    )
}
