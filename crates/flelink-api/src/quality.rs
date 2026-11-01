//! Quality next to speed: how far a quantization drifts from its reference, and whether it still knows things. Port of
//! `fle_studio/quality.py`.
//!
//! * **KL divergence** against a reference model (usually the BF16 or a Q8 file of the same model), computed by
//!   `llama-perplexity`: a first run with the reference saves its output probabilities for a short free text
//!   (`--kl-divergence-base`), a second run with the candidate compares against them (`--kl-divergence`). The mean KLD is
//!   the number the model cards of the FLE files quote; lower is better, 0 is identical.
//! * **A knowledge probe**: short factual questions sent to the running engine, answers checked against a list of accepted
//!   words. The built-in set is 48 questions in English and German written for FLE (CC0). A harder set can be imported
//!   from MKQA on request; that data is not shipped because its license asks for attribution and share-alike.
//!
//! Results are JSON files in `<data>/quality/`, shown on the Experiments page next to the speed.
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::Digest;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// tokens per chunk of the KL run; the built-in text has about 1,400 tokens, so about 5 chunks
pub const KLD_CTX: i64 = 256;
const KLD_TEXT: &str = "kld_text.txt";
const KLD_TEXT_DATA: &[u8] = include_bytes!("quality_data/kld_text.txt");
const KNOWLEDGE: &str = include_str!("quality_data/knowledge_cc0.json");
pub const BUILTIN: &str = "builtin";
const MKQA_URL: &str = "https://datasets-server.huggingface.co/rows";
pub const MKQA_LICENSE: &str =
    "CC BY-SA 3.0 (Apple ml-mkqa; the Hugging Face card says CC BY 3.0). Attribution and share-alike apply.";
const SYSTEM_PROMPT: &str =
    "Answer with the shortest correct answer only: a name, a number or a few words. No explanation.";

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn round(x: f64, n: i32) -> f64 {
    crate::stats::round_to(x, n)
}

// ------------------------------------------------------------------------------------------------ KL divergence

const NUM: &str = r"(-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?)";

fn pm() -> String {
    format!(r"\s*(?:±|\+/-|\+-)\s*{NUM}") // "±" in the tool's output, ASCII in some builds
}

fn first(text: &str, pattern: &str) -> Option<(f64, Option<f64>)> {
    let m = Regex::new(pattern).ok()?.captures(text)?;
    let a = m.get(1)?.as_str().parse().ok()?;
    let b = m.get(2).and_then(|x| x.as_str().parse().ok());
    Some((a, b))
}

/// Read the report of `llama-perplexity --kl-divergence`; Err when there is no KL divergence (the run did not finish).
pub fn parse_kld(text: &str) -> Result<Value, String> {
    let pm = pm();
    let mut out = Map::new();
    let Some((mean, unc)) = first(text, &format!(r"Mean\s+KLD:\s*{NUM}(?:{pm})?")) else {
        return Err("The output has no KL divergence statistics; the run did not finish.".into());
    };
    out.insert("mean_kld".into(), json!(mean));
    out.insert("mean_kld_unc".into(), json!(unc));
    for (key, label) in [
        ("max_kld", r"Maximum\s+KLD:"),
        ("kld_p999", r"99\.9%\s+KLD:"),
        ("kld_p99", r"99\.0%\s+KLD:"),
        ("kld_p95", r"95\.0%\s+KLD:"),
        ("kld_p90", r"90\.0%\s+KLD:"),
        ("median_kld", r"Median\s+KLD:"),
        ("kld_p10", r"10\.0%\s+KLD:"),
        ("kld_p5", r"5\.0%\s+KLD:"),
        ("kld_p1", r"1\.0%\s+KLD:"),
        ("kld_p01", r"0\.1%\s+KLD:"),
        ("min_kld", r"Minimum\s+KLD:"),
    ] {
        // "5.0%" must not match inside "95.0%" (Python uses a look-behind; here the character before must not be one)
        let p = format!(r"(?:^|[^\d.]){label}\s*{NUM}");
        if let Some((v, _)) = first(text, &p) {
            out.insert(key.into(), json!(v));
        }
    }
    for (key, label) in [
        ("ppl_q", r"Mean\s+PPL\(Q\)\s*:"),
        ("ppl_base", r"Mean\s+PPL\(base\)\s*:"),
    ] {
        if let Some((v, u)) = first(text, &format!(r"{label}\s*{NUM}(?:{pm})?")) {
            out.insert(key.into(), json!(v));
            out.insert(format!("{key}_unc"), json!(u));
        }
    }
    if let Some((v, u)) = first(
        text,
        &format!(r"Mean\s+PPL\(Q\)/PPL\(base\)\s*:\s*{NUM}(?:{pm})?"),
    ) {
        out.insert("ppl_ratio".into(), json!(v));
        out.insert("ppl_ratio_unc".into(), json!(u));
    }
    if let Some((v, _)) = first(
        text,
        &format!(r"Cor\(ln\(PPL\(Q\)\),\s*ln\(PPL\(base\)\)\):\s*{NUM}\s*%"),
    ) {
        out.insert("ppl_correlation".into(), json!(v));
    }
    for (key, label) in [
        ("same_top_p", r"Same\s+top\s+p:\s*"),
        ("rms_dp", r"RMS\s+Δp\s*:\s*"),
        ("mean_dp", r"Mean\s+Δp:\s*"),
    ] {
        if let Some((v, u)) = first(text, &format!(r"{label}{NUM}(?:{pm})?\s*%")) {
            out.insert(key.into(), json!(v));
            out.insert(format!("{key}_unc"), json!(u));
        }
    }
    Ok(Value::Object(out))
}

/// The text the KL run reads: the built-in one (copied into the data folder) or a file of the user's own.
pub fn kld_text_path(data_dir: &Path, custom: &str) -> Result<PathBuf, String> {
    let custom = custom.trim();
    if !custom.is_empty() {
        let p = match custom.strip_prefix("~/") {
            Some(r) => dirs::home_dir().unwrap_or_default().join(r),
            None => PathBuf::from(custom),
        };
        if !p.is_file() {
            return Err(format!("No text file at {}", p.display()));
        }
        return Ok(p);
    }
    let dir = data_dir.join("quality");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(KLD_TEXT);
    if std::fs::read(&dest).ok().as_deref() != Some(KLD_TEXT_DATA) {
        std::fs::write(&dest, KLD_TEXT_DATA).map_err(|e| e.to_string())?;
    }
    Ok(dest)
}

/// Where the reference run keeps its output; the name holds everything that changes it.
fn base_file(
    data_dir: &Path,
    reference: &Path,
    text: &Path,
    ctx: i64,
    chunks: i64,
) -> Result<PathBuf, String> {
    let st = std::fs::metadata(reference).map_err(|e| e.to_string())?;
    let mtime = st
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let text_digest = hex::encode(sha2::Sha256::digest(
        std::fs::read(text).map_err(|e| e.to_string())?,
    ));
    let resolved = std::fs::canonicalize(reference).unwrap_or_else(|_| reference.to_path_buf());
    let key = format!(
        "{}|{}|{mtime}|{text_digest}|{ctx}|{chunks}",
        resolved.display(),
        st.len()
    );
    let digest = hex::encode(sha2::Sha256::digest(key.as_bytes()));
    let dir = data_dir.join("quality").join("base");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem: String = reference
        .file_stem()
        .map(|s| s.to_string_lossy().chars().take(40).collect())
        .unwrap_or_default();
    Ok(dir.join(format!("{stem}-{}.kld", &digest[..16])))
}

#[allow(clippy::too_many_arguments)]
fn kld_args(
    model: &Path,
    text: &Path,
    base: &Path,
    ctx: i64,
    chunks: i64,
    reference: bool,
    threads: i64,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-m".into(),
        model.to_string_lossy().into(),
        "-f".into(),
        text.to_string_lossy().into(),
        "--kl-divergence-base".into(),
        base.to_string_lossy().into(),
        "-c".into(),
        ctx.to_string(),
    ];
    if !reference {
        a.push("--kl-divergence".into());
    }
    if chunks > 0 {
        a.extend(["--chunks".into(), chunks.to_string()]);
    }
    if threads > 0 {
        a.extend(["-t".into(), threads.to_string()]);
    }
    a
}

/// `name` (e.g. llama-perplexity) from the folder of the engine, or None if the build does not have it.
pub fn find_tool(engine_path: &str, name: &str) -> Option<PathBuf> {
    let server = Path::new(engine_path);
    let dir = server.parent()?;
    let c = dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    c.is_file().then_some(c)
}

/// Run a tool to the end: (exit code, all output with stderr merged in). `on_text` gets the output as it comes; Cancel
/// stops the tool.
fn run_tool(
    binary: &Path,
    args: &[String],
    cancel: &AtomicBool,
    on_text: &dyn Fn(&str),
) -> Result<(i32, String), Stop> {
    let dir = binary.parent().unwrap_or(Path::new("."));
    let mut cmd = Command::new(binary);
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cfg!(target_os = "linux") {
        let old = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        cmd.env(
            "LD_LIBRARY_PATH",
            if old.is_empty() {
                dir.display().to_string()
            } else {
                format!("{}:{old}", dir.display())
            },
        );
    } else if cfg!(windows) {
        cmd.env(
            "PATH",
            format!(
                "{};{}",
                dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| Stop::Failed(format!("Could not start {}: {e}", binary.display())))?;
    let out = Arc::new(Mutex::new(String::new()));
    let mut readers = vec![];
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let out = out.clone();
        readers.push(std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut buf = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                out.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }));
    }
    let mut seen = 0;
    let rc = loop {
        {
            let o = out.lock().unwrap();
            if o.len() > seen {
                on_text(&o[seen..]);
                seen = o.len();
            }
        }
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Stop::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(Stop::Failed(e.to_string())),
        }
    };
    for r in readers {
        let _ = r.join();
    }
    let text = out.lock().unwrap().clone();
    Ok((rc, text))
}

/// `[12]`: the chunk llama-perplexity is at
fn chunk_re() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| Regex::new(r"\[(\d+)\]").unwrap())
}

fn tail(output: &str) -> String {
    let rows: Vec<&str> = output
        .trim()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let t: String = rows[rows.len().saturating_sub(3)..]
        .join(" | ")
        .chars()
        .take(400)
        .collect();
    if t.is_empty() {
        "no output".into()
    } else {
        t
    }
}

// ------------------------------------------------------------------------------------------------ knowledge probe

/// Lower case, accents and punctuation gone, one space between words: `Zürich!` and `zurich` are the same.
pub fn normalize(text: &str) -> String {
    use unicode_normalization::char::is_combining_mark;
    use unicode_normalization::UnicodeNormalization;
    // `casefold`: lower case, and ß becomes ss
    let folded = text.to_lowercase().replace('ß', "ss");
    let plain: String = folded.nfkd().filter(|c| !is_combining_mark(*c)).collect();
    let re = Regex::new(r"[^\w]+").unwrap();
    re.replace_all(&plain, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when the answer contains one of the accepted answers as whole words.
pub fn is_correct(answer: &str, accepted: &[String]) -> bool {
    let hay = format!(" {} ", normalize(answer));
    accepted
        .iter()
        .map(|a| normalize(a))
        .filter(|n| !n.is_empty())
        .any(|n| hay.contains(&format!(" {n} ")))
}

/// 95 % interval of a share (Wilson score): honest for small n and for shares near 0 or 1.
pub fn wilson(correct: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let z = 1.96f64;
    let (nf, p) = (n as f64, correct as f64 / n as f64);
    let denom = 1.0 + z * z / nf;
    let centre = (p + z * z / (2.0 * nf)) / denom;
    let half = z * (p * (1.0 - p) / nf + z * z / (4.0 * nf * nf)).sqrt() / denom;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// (description, questions) of a question set: "builtin", "mkqa" (imported into the data folder) or a JSON file.
pub fn load_questions(
    source: &str,
    data_dir: &Path,
    lang: &str,
    limit: usize,
) -> Result<(Value, Vec<Value>), String> {
    let raw: Value = if source == BUILTIN || source.is_empty() {
        serde_json::from_str(KNOWLEDGE).map_err(|e| e.to_string())?
    } else {
        let file = if source == "mkqa" {
            let f = data_dir.join("quality").join("knowledge_mkqa.json");
            if !f.is_file() {
                return Err("Import the MKQA questions first".into());
            }
            f
        } else {
            let f = match source.strip_prefix("~/") {
                Some(r) => dirs::home_dir().unwrap_or_default().join(r),
                None => PathBuf::from(source),
            };
            if !f.is_file() {
                return Err(format!("No question file at {}", f.display()));
            }
            f
        };
        serde_json::from_str(&std::fs::read_to_string(&file).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{} is not valid JSON: {e}", file.display()))?
    };
    let truthy = crate::settings::py_truthy;
    let mut questions: Vec<Value> = raw["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|q| truthy(&q["q"]) && truthy(&q["a"]))
        .cloned()
        .collect();
    if !lang.is_empty() {
        questions.retain(|q| q["lang"] == lang);
    }
    if limit > 0 {
        questions.truncate(limit);
    }
    if questions.is_empty() {
        return Err(format!(
            "The question set has no questions{}",
            if lang.is_empty() {
                String::new()
            } else {
                format!(" in language {lang}")
            }
        ));
    }
    let mut meta = Map::new();
    for k in ["name", "license", "source", "note"] {
        meta.insert(k.into(), raw.get(k).cloned().unwrap_or(json!("")));
    }
    let mut langs: Vec<String> = questions
        .iter()
        .map(|q| q["lang"].as_str().unwrap_or("").to_string())
        .collect();
    langs.sort();
    langs.dedup();
    meta.insert("languages".into(), json!(langs));
    Ok((Value::Object(meta), questions))
}

fn ask(base: &str, key: &str, question: &str, seed: usize) -> Result<String, String> {
    let body = json!({"messages": [{"role": "system", "content": SYSTEM_PROMPT}, {"role": "user", "content": question}],
        "max_tokens": 64, "temperature": 0, "seed": seed, "stream": false, "cache_prompt": false,
        "chat_template_kwargs": {"enable_thinking": false}});
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(120))
        .build();
    let mut req = agent
        .post(&format!("{base}/v1/chat/completions"))
        .set("Content-Type", "application/json");
    if !key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let text = req
        .send_string(&body.to_string())
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    let data: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(data["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

/// Ask the running engine the questions one by one and score the answers.
pub fn run_probe(
    base: &str,
    key: &str,
    progress: &dyn Fn(&str) -> Result<(), Stop>,
    source: &str,
    data_dir: &Path,
    lang: &str,
    limit: usize,
) -> Result<Value, Stop> {
    let (meta, questions) = load_questions(source, data_dir, lang, limit).map_err(Stop::Failed)?;
    let t0 = Instant::now();
    let mut items: Vec<Value> = vec![];
    for (i, q) in questions.iter().enumerate() {
        progress(&format!("Knowledge probe {}/{}", i + 1, questions.len()))?;
        let answer = ask(base, key, q["q"].as_str().unwrap_or(""), i).map_err(Stop::Failed)?;
        let accepted: Vec<String> = q["a"]
            .as_array()
            .into_iter()
            .flatten()
            .map(crate::settings::py_str)
            .collect();
        items.push(json!({"id": q.get("id").cloned().unwrap_or(json!("")), "lang": q.get("lang").cloned().unwrap_or(json!("")),
            "q": q["q"], "expected": accepted.first().cloned().unwrap_or_default(),
            "answer": answer.trim().chars().take(200).collect::<String>(), "ok": is_correct(&answer, &accepted)}));
    }
    let correct = items.iter().filter(|x| x["ok"] == true).count();
    let (lo, hi) = wilson(correct, items.len());
    let mut by_lang = Map::new();
    for code in meta["languages"].as_array().into_iter().flatten() {
        let sub: Vec<&Value> = items.iter().filter(|x| &x["lang"] == code).collect();
        let c = sub.iter().filter(|x| x["ok"] == true).count();
        by_lang.insert(
            crate::settings::py_str(code),
            json!({"n": sub.len(), "correct": c, "accuracy": round(c as f64 / sub.len() as f64, 4)}),
        );
    }
    Ok(
        json!({"kind": "probe", "set": meta, "n": items.len(), "correct": correct,
        "accuracy": round(correct as f64 / items.len() as f64, 4), "ci_lo": round(lo, 4), "ci_hi": round(hi, 4),
        "by_lang": by_lang, "items": items, "seconds": round(t0.elapsed().as_secs_f64(), 1)}),
    )
}

/// Download `count` MKQA questions in `lang` (via the Hugging Face dataset viewer) into the data folder. Only questions with
/// a short answer are kept. The data stays in the user's folder; it is not shipped with FLE Link.
pub fn import_mkqa(
    data_dir: &Path,
    lang: &str,
    count: usize,
    offset: usize,
) -> Result<Value, String> {
    if !Regex::new(r"^[a-z]{2}(?:_[a-z]{2})?$")
        .unwrap()
        .is_match(lang)
    {
        return Err("lang must be a language code like en or de".into());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build();
    let mut kept: Vec<Value> = vec![];
    let mut scanned = 0;
    while kept.len() < count && scanned < 600 {
        let url = format!(
            "{MKQA_URL}?dataset=apple%2Fmkqa&config=mkqa&split=train&offset={}&length=100",
            offset + scanned
        );
        let doc: Value = agent
            .get(&url)
            .call()
            .map_err(|e| format!("Could not reach the dataset viewer: {e}"))?
            .into_string()
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let rows = doc["rows"].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        scanned += rows.len();
        for entry in rows {
            let row = &entry["row"];
            let Some(text) = row["queries"][lang].as_str().filter(|t| !t.is_empty()) else {
                continue;
            };
            let mut accepted: Vec<String> = vec![];
            for ans in row["answers"][lang].as_array().into_iter().flatten() {
                if matches!(
                    ans["type"].as_str(),
                    Some("entity" | "date" | "number" | "number_with_unit" | "short_phrase")
                ) {
                    accepted.push(crate::settings::py_str(&ans["text"]));
                    accepted.extend(
                        ans["aliases"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(crate::settings::py_str),
                    );
                }
            }
            let mut seen = std::collections::HashSet::new();
            accepted.retain(|a| !a.trim().is_empty() && a != "None" && seen.insert(a.clone()));
            if !accepted.is_empty() {
                kept.push(json!({"id": crate::settings::py_str(&row["example_id"]), "lang": lang, "q": text, "a": accepted}));
                if kept.len() >= count {
                    break;
                }
            }
        }
    }
    if kept.is_empty() {
        return Err("The dataset viewer returned no usable MKQA questions".into());
    }
    let doc = json!({"name": format!("MKQA ({lang}, {} questions)", kept.len()), "license": MKQA_LICENSE,
        "source": "https://huggingface.co/datasets/apple/mkqa (Longpre, Lu, Daiber: MKQA, 2020)",
        "note": "Imported on request; not part of FLE Link.", "questions": kept});
    let dir = data_dir.join("quality");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join("knowledge_mkqa.json");
    std::fs::write(
        &file,
        serde_json::to_string_pretty(&doc).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(
        json!({"count": doc["questions"].as_array().map_or(0, Vec::len), "lang": lang, "path": file.to_string_lossy(), "license": MKQA_LICENSE}),
    )
}

// ------------------------------------------------------------------------------------------------ the jobs

#[derive(Debug)]
pub enum Stop {
    Cancelled,
    Failed(String),
}

/// Runs KL divergence jobs and standalone probes in a thread, keeps their results in the data folder.
pub struct Quality {
    data_dir: PathBuf,
    dir: PathBuf,
    current: Arc<Mutex<Option<Value>>>,
    cancel: Arc<AtomicBool>,
}

impl Quality {
    pub fn new(data_dir: &Path) -> Quality {
        let dir = data_dir.join("quality");
        let _ = std::fs::create_dir_all(&dir);
        Quality {
            data_dir: data_dir.to_path_buf(),
            dir,
            current: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn results(&self) -> Vec<Value> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map(|r| {
                r.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name().is_some_and(|n| {
                            let n = n.to_string_lossy();
                            n.starts_with("run-") && n.ends_with(".json")
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files.reverse();
        let mut out: Vec<Value> = files
            .iter()
            .take(200)
            .filter_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
            .collect();
        // file names only resolve to the second; runs started in the same second keep their real order
        out.sort_by(|a, b| {
            b["started"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&a["started"].as_f64().unwrap_or(0.0))
        });
        out
    }

    pub fn delete(&self, id: &str) {
        if let Ok(r) = std::fs::read_dir(&self.dir) {
            for e in r.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with("run-") && n.ends_with(&format!("-{id}.json")) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    pub fn status(&self) -> Value {
        self.current.lock().unwrap().clone().unwrap_or(Value::Null)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn save(&self, record: &Value) {
        let started = record["started"].as_f64().unwrap_or(0.0);
        let stamp = chrono::DateTime::from_timestamp(started as i64, 0)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("run-%Y%m%d-%H%M%S")
                    .to_string()
            })
            .unwrap_or_default();
        let _ = std::fs::write(
            self.dir.join(format!(
                "{stamp}-{}.json",
                crate::settings::py_str(&record["id"])
            )),
            serde_json::to_string_pretty(record).unwrap_or_default(),
        );
    }

    /// The newest finished KL run per candidate model name, for the column next to the speed.
    pub fn latest_kld_by_model(&self) -> Value {
        let mut out = Map::new();
        for r in self.results().iter().rev() {
            if r["kind"] == "kld" && r["state"] == "done" && r["result"].is_object() {
                out.insert(
                    crate::settings::py_str(&r["candidate"]["name"]),
                    json!({"id": r["id"], "mean_kld": r["result"]["mean_kld"], "reference": r["reference"]["name"],
                        "finished": r.get("finished").cloned().unwrap_or(Value::Null)}),
                );
            }
        }
        Value::Object(out)
    }

    fn begin(&self, record: Value) -> Result<Value, String> {
        let mut cur = self.current.lock().unwrap();
        if cur.as_ref().is_some_and(|c| c["state"] == "running") {
            return Err("A quality measurement is already running".into());
        }
        self.cancel.store(false, Ordering::SeqCst);
        *cur = Some(record.clone());
        Ok(record)
    }

    fn new_id() -> String {
        let mut b = [0u8; 4];
        getrandom::getrandom(&mut b).expect("random bytes");
        hex::encode(b)
    }

    fn set(current: &Mutex<Option<Value>>, f: impl FnOnce(&mut Value)) {
        if let Some(c) = current.lock().unwrap().as_mut() {
            f(c);
        }
    }

    /// Start the two llama-perplexity runs; `reference` and `candidate` are library models ({name, path}).
    /// Err((409, …)) when the engine has no llama-perplexity or a run is going, Err((400, …)) for a bad request.
    #[allow(clippy::too_many_arguments)]
    pub fn start_kld(
        &self,
        reference: &Value,
        candidate: &Value,
        engine_path: &str,
        text: &str,
        ctx: i64,
        chunks: i64,
        threads: i64,
    ) -> Result<Value, (u16, String)> {
        let binary = find_tool(engine_path, "llama-perplexity").ok_or((409, "This engine has no llama-perplexity next to llama-server. Install a llama.cpp release (Engine page) or put llama-perplexity beside your own build.".to_string()))?;
        if reference["path"] == candidate["path"] {
            return Err((
                400,
                "Pick two different models: the reference and the one to measure".into(),
            ));
        }
        let text_file = kld_text_path(&self.data_dir, text).map_err(|e| (400, e))?;
        let (ctx, chunks) = (ctx.clamp(64, 8192), chunks.clamp(0, 1000));
        let chars = std::fs::metadata(&text_file).map_or(0, |m| m.len());
        let rec = self
            .begin(json!({"id": Self::new_id(), "kind": "kld", "state": "running", "started": now(), "step": "Starting",
                "reference": {"name": reference["name"], "path": reference["path"]},
                "candidate": {"name": candidate["name"], "path": candidate["path"]},
                "text": {"name": text_file.file_name().map(|n| n.to_string_lossy().to_string()), "chars": chars},
                "ctx": ctx, "chunks": chunks, "progress": 0.0, "commands": []}))
            .map_err(|e| (409, e))?;
        let (current, cancel, data_dir) = (
            self.current.clone(),
            self.cancel.clone(),
            self.data_dir.clone(),
        );
        let me = Quality {
            data_dir: data_dir.clone(),
            dir: self.dir.clone(),
            current: current.clone(),
            cancel: cancel.clone(),
        };
        std::thread::Builder::new()
            .name("quality-kld".into())
            .spawn(move || me.run_kld(&binary, &text_file, threads))
            .map_err(|e| (500, e.to_string()))?;
        Ok(rec)
    }

    fn run_kld(&self, binary: &Path, text: &Path, threads: i64) {
        let cur = self.status();
        let r = (|| -> Result<(), Stop> {
            let ref_path = PathBuf::from(crate::settings::py_str(&cur["reference"]["path"]));
            let cand_path = PathBuf::from(crate::settings::py_str(&cur["candidate"]["path"]));
            let (ctx, chunks) = (
                cur["ctx"].as_i64().unwrap_or(KLD_CTX),
                cur["chunks"].as_i64().unwrap_or(0),
            );
            let base =
                base_file(&self.data_dir, &ref_path, text, ctx, chunks).map_err(Stop::Failed)?;
            let mut steps = vec![("candidate", cand_path, false)];
            if !base.is_file() {
                steps.insert(0, ("reference", ref_path, true));
            }
            let n = steps.len();
            for (i, (what, model, is_reference)) in steps.into_iter().enumerate() {
                let args = kld_args(&model, text, &base, ctx, chunks, is_reference, threads);
                let name = model
                    .file_name()
                    .map_or(String::new(), |n| n.to_string_lossy().to_string());
                let command = std::iter::once(
                    binary
                        .file_name()
                        .map_or(String::new(), |n| n.to_string_lossy().to_string()),
                )
                .chain(args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" ");
                Self::set(&self.current, |c| {
                    c["step"] = json!(format!("Step {} of {n}: {what} model {name}", i + 1));
                    c["commands"].as_array_mut().unwrap().push(json!(command));
                    c["progress"] = json!(i as f64 / n as f64);
                });
                let current = self.current.clone();
                let on_text = move |piece: &str| {
                    if let Some(m) = chunk_re().captures_iter(piece).last() {
                        let k: i64 = m[1].parse().unwrap_or(0);
                        Self::set(&current, |c| c["chunk"] = json!(k));
                    }
                };
                let (rc, output) = run_tool(binary, &args, &self.cancel, &on_text)?;
                if is_reference {
                    if rc != 0 || !base.is_file() {
                        let _ = std::fs::remove_file(&base);
                        return Err(Stop::Failed(format!(
                            "The reference run failed (exit code {rc}): {}",
                            tail(&output)
                        )));
                    }
                    continue;
                }
                let result = parse_kld(&output)
                    .map_err(|e| Stop::Failed(format!("{e} Exit code {rc}: {}", tail(&output))))?;
                Self::set(&self.current, |c| c["result"] = result);
            }
            Self::set(&self.current, |c| {
                c["state"] = json!("done");
                c["progress"] = json!(1.0);
            });
            Ok(())
        })();
        self.finish(r);
    }

    fn finish(&self, r: Result<(), Stop>) {
        let mut record = Value::Null;
        Self::set(&self.current, |c| {
            match r {
                Ok(()) => {}
                Err(Stop::Cancelled) => c["state"] = json!("cancelled"),
                Err(Stop::Failed(e)) => {
                    c["state"] = json!("failed");
                    c["error"] = json!(e);
                }
            }
            c["finished"] = json!(now());
            c["step"] = json!("");
            record = c.clone();
        });
        self.save(&record);
    }

    /// A probe of the running engine on its own.
    #[allow(clippy::too_many_arguments)]
    pub fn start_probe(
        &self,
        base: &str,
        key: &str,
        model: &str,
        source: &str,
        lang: &str,
        limit: usize,
        label: &str,
    ) -> Result<Value, (u16, String)> {
        load_questions(source, &self.data_dir, lang, limit).map_err(|e| (400, e))?; // fail early for a bad set
        let label: String = label.chars().take(80).collect();
        let rec = self
            .begin(json!({"id": Self::new_id(), "kind": "probe", "state": "running", "started": now(), "step": "Starting",
                "candidate": {"name": model, "path": ""}, "label": label}))
            .map_err(|e| (409, e))?;
        let me = Quality {
            data_dir: self.data_dir.clone(),
            dir: self.dir.clone(),
            current: self.current.clone(),
            cancel: self.cancel.clone(),
        };
        let (base, key, source, lang) = (
            base.to_string(),
            key.to_string(),
            source.to_string(),
            lang.to_string(),
        );
        std::thread::Builder::new()
            .name("quality-probe".into())
            .spawn(move || {
                let say = |t: &str| -> Result<(), Stop> {
                    if me.cancel.load(Ordering::SeqCst) {
                        return Err(Stop::Cancelled);
                    }
                    Self::set(&me.current, |c| c["step"] = json!(t));
                    Ok(())
                };
                let r = run_probe(&base, &key, &say, &source, &me.data_dir, &lang, limit).map(
                    |result| {
                        Self::set(&me.current, |c| {
                            c["result"] = result;
                            c["state"] = json!("done");
                        })
                    },
                );
                me.finish(r);
            })
            .map_err(|e| (500, e.to_string()))?;
        Ok(rec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "====== Perplexity statistics ======
Mean PPL(Q)                   :   9.123456 ±   0.123456
Mean PPL(base)                :   8.987654 ±   0.120000
Cor(ln(PPL(Q)), ln(PPL(base))):  99.12%
Mean PPL(Q)/PPL(base)         :   1.015110 ±   0.002000

====== KL divergence statistics ======
Mean    KLD:   0.012345 ±   0.000456
Maximum KLD:   1.234567
99.9%   KLD:   0.876543
99.0%   KLD:   0.345678
95.0%   KLD:   0.098765
90.0%   KLD:   0.045678
Median  KLD:   0.005432
10.0%   KLD:   0.000123
 5.0%   KLD:   0.000045
 1.0%   KLD:   0.000004
 0.1%   KLD:  -0.000001
Minimum KLD:  -0.000010

====== Token probability statistics ======
Mean    Δp: -0.123 ± 0.045 %
RMS Δp    :  3.456 ± 0.123 %
Same top p: 95.432 ± 0.567 %";

    #[test]
    fn the_report_is_read_like_the_python_studio_does() {
        let r = parse_kld(REPORT).unwrap();
        // fle_studio.quality.parse_kld of the same text (Python Studio 055f4aa)
        let want: Value = serde_json::from_str(PY_PARSE).unwrap();
        assert_eq!(r, want);
        assert!(parse_kld("llama_model_load: error").is_err());
    }

    #[test]
    fn answers_are_compared_like_the_python_studio_does() {
        assert_eq!(normalize("Zürich!  Straße"), "zurich strasse");
        assert!(is_correct("It is Zürich.", &["Zurich".into()]));
        assert!(!is_correct("Bern", &["Zurich".into()]));
        assert!(!is_correct("Parisian", &["Paris".into()]));
        let (lo, hi) = wilson(40, 48);
        assert_eq!((round(lo, 4), round(hi, 4)), (0.7042, 0.913));
        let (meta, q) = load_questions(BUILTIN, Path::new("/nonexistent"), "de", 0).unwrap();
        assert_eq!(meta["license"], "CC0-1.0");
        assert!(q.iter().all(|x| x["lang"] == "de") && !q.is_empty());
    }

    const PY_PARSE: &str = r#"{"mean_kld": 0.012345, "mean_kld_unc": 0.000456, "max_kld": 1.234567, "kld_p999": 0.876543, "kld_p99": 0.345678, "kld_p95": 0.098765, "kld_p90": 0.045678, "median_kld": 0.005432, "kld_p10": 0.000123, "kld_p5": 4.5e-05, "kld_p1": 4e-06, "kld_p01": -1e-06, "min_kld": -1e-05, "ppl_q": 9.123456, "ppl_q_unc": 0.123456, "ppl_base": 8.987654, "ppl_base_unc": 0.12, "ppl_ratio": 1.01511, "ppl_ratio_unc": 0.002, "ppl_correlation": 99.12, "same_top_p": 95.432, "same_top_p_unc": 0.567, "rms_dp": 3.456, "rms_dp_unc": 0.123, "mean_dp": -0.123, "mean_dp_unc": 0.045}"#;
}
