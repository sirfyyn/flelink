//! Writing a GGUF from pieces of other GGUFs: the engine behind the hybrid build and the expert pruning. Port of
//! `fle_studio_forge/build.py`.
//!
//! Nothing is quantised here; bytes are copied. Every output tensor is a list of segments (file, offset, length) of its
//! sources. The output is written to `NAME.part` next to a small state file; a cancelled or crashed build starts again at
//! the first tensor that was not finished, after the header and the finished prefix were checked against what would be
//! written now.
use super::ggufio::{self, Kv};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// bytes compared at both ends of every tensor when a finished file is checked
const EDGE: u64 = 65536;

#[derive(Debug, Clone)]
pub struct OutTensor {
    pub name: String,
    pub shape: Vec<u64>,
    pub ttype: u32,
    pub nbytes: u64,
    pub segments: Vec<(String, u64, u64)>,
}

/// Why a write stopped: the user cancelled, or something failed.
#[derive(Debug)]
pub enum Stop {
    Cancelled,
    Failed(String),
}

impl From<String> for Stop {
    fn from(s: String) -> Self {
        Stop::Failed(s)
    }
}

pub type Progress<'a> = &'a mut dyn FnMut(f64, &str);

fn read_at(path: &str, offset: u64, n: u64) -> Result<Vec<u8>, String> {
    let mut f = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut b = vec![0u8; n as usize];
    f.read_exact(&mut b).map_err(|e| format!("{path}: {e}"))?;
    Ok(b)
}

/// The first and last EDGE bytes of the tensor the segments describe.
fn edges(segments: &[(String, u64, u64)], total: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
    if total <= 2 * EDGE {
        let mut all = vec![];
        for (p, o, l) in segments {
            all.extend(read_at(p, *o, *l)?);
        }
        return Ok((all, vec![]));
    }
    let mut head = vec![];
    for (p, o, l) in segments {
        let take = (*l).min(EDGE - head.len() as u64);
        head.extend(read_at(p, *o, take)?);
        if head.len() as u64 >= EDGE {
            break;
        }
    }
    let mut tail: Vec<u8> = vec![];
    for (p, o, l) in segments.iter().rev() {
        let take = (*l).min(EDGE - tail.len() as u64);
        let mut part = read_at(p, o + l - take, take)?;
        part.extend(tail);
        tail = part;
        if tail.len() as u64 >= EDGE {
            break;
        }
    }
    Ok((head, tail))
}

fn state_path(out: &Path) -> PathBuf {
    out.with_file_name(format!(
        "{}.part.json",
        out.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn write_state(out: &Path, state: &Value) {
    let p = state_path(out);
    let tmp = p.with_extension("tmp");
    if std::fs::write(&tmp, state.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// Write the model to `out` (atomically: NAME.part is renamed at the end); its size and SHA-256. Resumes a partial
/// NAME.part with the same header.
pub fn write_model(
    out: &Path,
    kv: &[Kv],
    tensors: &[OutTensor],
    alignment: u64,
    progress: Progress,
    cancelled: &dyn Fn() -> bool,
    log: &dyn Fn(&str),
) -> Result<Value, Stop> {
    let infos: Vec<ggufio::Info> = tensors
        .iter()
        .map(|t| (t.name.clone(), t.shape.clone(), t.ttype, t.nbytes))
        .collect();
    let (header, offsets) = ggufio::build_header(kv, &infos, alignment);
    let header_sha = hex::encode(sha2::Sha256::digest(&header));
    let end_of = |i: usize| {
        header.len() as u64 + offsets[i] + tensors[i].nbytes.div_ceil(alignment) * alignment
    };
    let total = if tensors.is_empty() {
        header.len() as u64
    } else {
        end_of(tensors.len() - 1)
    };
    let part = out.with_file_name(format!(
        "{}.part",
        out.file_name().unwrap_or_default().to_string_lossy()
    ));
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let mut hasher = sha2::Sha256::new();
    let (mut done, mut written) = (0usize, 0u64);
    let state: Option<Value> = (part.exists() && state_path(out).exists())
        .then(|| {
            std::fs::read_to_string(state_path(out))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
        })
        .flatten();
    let part_len = std::fs::metadata(&part).map_or(0, |m| m.len());
    match &state {
        Some(s)
            if s["header_sha"] == json!(header_sha)
                && s["offset"].as_u64().is_some_and(|o| o > 0 && o <= part_len)
                && s["done"]
                    .as_u64()
                    .is_some_and(|d| d as usize <= tensors.len()) =>
        {
            done = s["done"].as_u64().unwrap() as usize;
            written = s["offset"].as_u64().unwrap();
            log(&format!(
                "Resuming {}: {done} of {} tensors are already written",
                out.file_name().unwrap_or_default().to_string_lossy(),
                tensors.len()
            ));
            ggufio::for_chunks(&part.to_string_lossy(), 0, written, &mut |b| {
                hasher.update(b);
                Ok(())
            })?;
        }
        _ if part.exists() => {
            log("A partial file from a different recipe was found; starting from the beginning")
        }
        _ => {}
    }
    let mut f = if written > 0 {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&part)
            .map_err(|e| e.to_string())?;
        f.set_len(written).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(written))
            .map_err(|e| e.to_string())?;
        f
    } else {
        let mut f = File::create(&part).map_err(|e| e.to_string())?;
        f.write_all(&header).map_err(|e| e.to_string())?;
        hasher.update(&header);
        written = header.len() as u64;
        f
    };
    let t0 = Instant::now();
    let started_at = written;
    let mut last_state = Instant::now() - std::time::Duration::from_secs(10);
    let result = (|| -> Result<(), Stop> {
        let start = done;
        for i in start..tensors.len() {
            let t = &tensors[i];
            for (p, o, l) in &t.segments {
                ggufio::for_chunks(p, *o, *l, &mut |b| {
                    if cancelled() {
                        return Err("\u{0}cancelled".into());
                    }
                    f.write_all(b).map_err(|e| e.to_string())?;
                    hasher.update(b);
                    written += b.len() as u64;
                    Ok(())
                })
                .map_err(|e| {
                    if e == "\u{0}cancelled" {
                        Stop::Cancelled
                    } else {
                        Stop::Failed(e)
                    }
                })?;
            }
            let pad = (alignment - written % alignment) % alignment;
            if pad > 0 {
                let z = vec![0u8; pad as usize];
                f.write_all(&z).map_err(|e| e.to_string())?;
                hasher.update(&z);
                written += pad;
            }
            done = i + 1;
            if last_state.elapsed().as_secs_f64() > 1.0 || done == tensors.len() {
                f.flush().map_err(|e| e.to_string())?;
                write_state(
                    out,
                    &json!({"header_sha": header_sha, "done": done, "offset": written}),
                );
                last_state = Instant::now();
            }
            let rate = (written - started_at) as f64 / t0.elapsed().as_secs_f64().max(1e-6);
            progress(
                written as f64 / total.max(1) as f64,
                &format!(
                    "{done} of {} tensors, {:.1} of {:.1} GB{}",
                    tensors.len(),
                    written as f64 / 1e9,
                    total as f64 / 1e9,
                    if rate > 0.0 {
                        format!(", {:.0} MB/s", rate / 1e6)
                    } else {
                        String::new()
                    }
                ),
            );
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = f.flush();
        // the state records the end of the last finished tensor; bytes of an unfinished one are cut on resume
        if done > 0 {
            write_state(
                out,
                &json!({"header_sha": header_sha, "done": done, "offset": end_of(done - 1)}),
            );
        }
        return Err(e);
    }
    drop(f);
    if written != total {
        return Err(Stop::Failed(format!(
            "wrote {written} bytes, expected {total}"
        )));
    }
    let problems = verify_file(&part, kv, tensors);
    if !problems.is_empty() {
        return Err(Stop::Failed(format!(
            "the new file does not match its sources: {}",
            problems[..problems.len().min(5)].join("; ")
        )));
    }
    std::fs::rename(&part, out).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(state_path(out));
    Ok(
        json!({"path": out.to_string_lossy(), "size": total, "sha256": hex::encode(hasher.finalize()), "tensors": tensors.len()}),
    )
}

/// Header, tensor table and the first and last bytes of every tensor of a finished file, against the plan.
pub fn verify_file(path: &Path, kv: &[Kv], tensors: &[OutTensor]) -> Vec<String> {
    let got = match ggufio::read_gguf(path, 0) {
        Ok(g) => g,
        Err(e) => return vec![format!("unreadable: {e}")],
    };
    let mut problems = vec![];
    if got.kv.iter().map(|k| &k.key).collect::<Vec<_>>()
        != kv.iter().map(|k| &k.key).collect::<Vec<_>>()
    {
        problems.push("metadata keys differ".to_string());
    }
    if got.tensors.iter().map(|t| &t.name).collect::<Vec<_>>()
        != tensors.iter().map(|t| &t.name).collect::<Vec<_>>()
    {
        problems.push("tensor names or order differ".into());
        return problems;
    }
    let p = path.to_string_lossy().to_string();
    for (have, want) in got.tensors.iter().zip(tensors) {
        if have.shape != want.shape || have.ttype != want.ttype {
            problems.push(format!("type or shape: {}", want.name));
            continue;
        }
        let Ok((head, tail)) = edges(&want.segments, want.nbytes) else {
            problems.push(format!("source unreadable: {}", want.name));
            continue;
        };
        let start = got.data_start + have.offset;
        if read_at(&p, start, head.len() as u64).ok().as_deref() != Some(head.as_slice()) {
            problems.push(format!("first bytes: {}", want.name));
            continue;
        }
        if !tail.is_empty()
            && read_at(
                &p,
                start + want.nbytes - tail.len() as u64,
                tail.len() as u64,
            )
            .ok()
            .as_deref()
                != Some(tail.as_slice())
        {
            problems.push(format!("last bytes: {}", want.name));
        }
    }
    problems
}

/// Assemble the hybrid a recipe describes. Metadata and all tensors outside the chosen layers come from the base.
pub fn build_hybrid(
    recipe: &Value,
    out: &Path,
    progress: Progress,
    cancelled: &dyn Fn() -> bool,
    log: &dyn Fn(&str),
) -> Result<Value, Stop> {
    let h = &recipe["hybrid"];
    let base_id = h["base"].as_str().unwrap_or("").to_string();
    let used: std::collections::HashSet<String> = std::iter::once(base_id.clone())
        .chain(
            h["layers"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(_, v)| v.as_str().map(String::from)),
        )
        .chain(
            h["rules"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["source"].as_str().map(String::from)),
        )
        .collect();
    let mut models: HashMap<String, ggufio::Model> = HashMap::new();
    for (sid, s) in recipe["sources"].as_object().into_iter().flatten() {
        if s["role"] == "reference" || !used.contains(sid) {
            continue;
        }
        let m = ggufio::open_model(&ggufio::path_buf(s["path"].as_str().unwrap_or("")))
            .map_err(|e| e.0)?;
        log(&format!(
            "Source {sid}: {}, {} tensors",
            Path::new(s["path"].as_str().unwrap_or(""))
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            m.order.len()
        ));
        models.insert(sid.clone(), m);
    }
    let base = models
        .get(&base_id)
        .ok_or(Stop::Failed(format!("the base {base_id} is not open")))?
        .clone();
    let pick: BTreeMap<u64, String> = h["layers"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_string())))
        .collect();
    let rules = super::plan::parse_rules(&h["rules"]).map_err(|e| e.0)?;
    let plan = super::plan::assemble(&base, &models, &pick, &base_id, &rules).map_err(|e| e.0)?;
    let tensors: Vec<OutTensor> = base
        .order
        .iter()
        .map(|n| {
            let m = &models[&plan[n]];
            let t = &m.tensors[n];
            OutTensor {
                name: n.clone(),
                shape: t.shape.clone(),
                ttype: t.ttype,
                nbytes: t.nbytes,
                segments: vec![m.location(n)],
            }
        })
        .collect();
    let mut origin: BTreeMap<String, u64> = BTreeMap::new();
    for sid in plan.values() {
        *origin.entry(sid.clone()).or_default() += 1;
    }
    log(&format!(
        "Tensors per source: {}",
        origin
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let mut result = write_model(
        out,
        &base.kv,
        &tensors,
        base.alignment(),
        progress,
        cancelled,
        log,
    )?;
    result["origin"] = json!(origin);
    result["layers"] = json!(pick
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect::<serde_json::Map<_, _>>());
    Ok(result)
}
