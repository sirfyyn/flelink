//! Expert pruning in the style of REAP: keep the K most useful routed experts of every layer, drop the rest. Port of
//! `fle_studio_forge/reap.py`.
//!
//! Input is a table of router statistics per layer and expert (`layer expert count sum_gate sum_saliency sum_norm`, as the
//! FLE engine's `llama-fle-reap` writes it, or derived from an importance matrix). Scores (higher is kept): reap
//! (saliency / count), man (norm / count), freq (count), mass (sum_gate), energy (saliency). The expert axis is the slowest
//! axis of every per-expert tensor, so the kept experts are contiguous slices; the router loses the rows of the dropped
//! experts, and `<arch>.expert_count` becomes K. Models with grouped routing are refused; nothing is re-trained.
use super::build::{self, OutTensor, Progress, Stop};
use super::ggufio;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub const SCORES: [&str; 5] = ["reap", "freq", "mass", "man", "energy"];
/// count, sum_gate, sum_saliency, sum_norm
pub type Entry = (u64, f64, f64, f64);
pub type Table = BTreeMap<u64, BTreeMap<u64, Entry>>;

fn per_expert() -> Regex {
    Regex::new(r"^blk\.(\d+)\.(ffn_(gate|up|down|gate_up)_exps(\.(weight|bias|scale))?|ffn_(gate|up|down)_exps_s.*|ffn_gate_inp(\.weight)?|exp_probs_b(\.bias)?)$").unwrap()
}

pub fn load_table(path: &Path) -> Result<Table, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().unwrap_or("").split('\t').collect();
    let col = |n: &str| head.iter().position(|h| *h == n);
    let (Some(l), Some(e), Some(c)) = (col("layer"), col("expert"), col("count")) else {
        return Err("the table needs the columns layer, expert and count (tab separated)".into());
    };
    let (g, s, n) = (col("sum_gate"), col("sum_saliency"), col("sum_norm"));
    let mut table = Table::new();
    for line in lines.filter(|x| !x.trim().is_empty()) {
        let f: Vec<&str> = line.split('\t').collect();
        let get = |i: Option<usize>| {
            i.and_then(|i| f.get(i))
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        let layer = get(Some(l)) as u64;
        let expert = get(Some(e)) as u64;
        table
            .entry(layer)
            .or_default()
            .insert(expert, (get(Some(c)) as u64, get(g), get(s), get(n)));
    }
    if table.is_empty() {
        return Err("the table is empty".into());
    }
    Ok(table)
}

pub fn write_table(table: &Table, path: &Path) -> std::io::Result<()> {
    let mut out = String::from("layer\texpert\tcount\tsum_gate\tsum_saliency\tsum_norm\n");
    for (l, row) in table {
        for (e, (c, g, s, n)) in row {
            out += &format!("{l}\t{e}\t{c}\t{g:?}\t{s:?}\t{n:?}\n");
        }
    }
    std::fs::write(path, out)
}

/// Router counts and input energy per expert from an importance-matrix GGUF (the newer file format).
pub fn table_from_imatrix(path: &Path) -> Result<Table, String> {
    let g = ggufio::read_gguf(path, 0).map_err(|e| e.0)?;
    let re = Regex::new(r"^blk\.(\d+)\.ffn_gate_exps\.weight\.(counts|in_sum2)$").unwrap();
    let mut counts: BTreeMap<u64, Vec<f32>> = BTreeMap::new();
    let mut energy: BTreeMap<u64, Vec<f32>> = BTreeMap::new();
    let p = path.to_string_lossy().to_string();
    for t in &g.tensors {
        let Some(m) = re.captures(&t.name) else {
            continue;
        };
        if t.type_name() != "F32" {
            continue;
        }
        let mut raw = vec![];
        ggufio::for_chunks(&p, g.data_start + t.offset, t.n_elements() * 4, &mut |b| {
            raw.extend_from_slice(b);
            Ok(())
        })?;
        let vals: Vec<f32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        let layer: u64 = m[1].parse().unwrap_or(0);
        if &m[2] == "counts" {
            counts.insert(layer, vals);
        } else {
            energy.insert(layer, vals);
        }
    }
    if counts.is_empty() {
        return Err("the importance matrix has no per-expert counts (it must be a GGUF written by a recent llama-imatrix, not the old .dat format)".into());
    }
    let mut table = Table::new();
    for (layer, c) in counts {
        let e = energy.get(&layer);
        let n = c.len();
        let per = e
            .filter(|e| n > 0 && e.len() % n == 0)
            .map_or(0, |e| e.len() / n);
        let row = table.entry(layer).or_default();
        #[allow(clippy::needless_range_loop)]
        for x in 0..n {
            let en = if per > 0 {
                e.unwrap()[x * per..(x + 1) * per]
                    .iter()
                    .map(|v| *v as f64)
                    .sum()
            } else {
                c[x] as f64
            };
            row.insert(x as u64, (c[x] as u64, c[x] as f64, en, en));
        }
    }
    Ok(table)
}

pub fn score(e: &Entry, kind: &str) -> f64 {
    let (count, gate, sal, norm) = *e;
    match kind {
        "freq" => count as f64,
        "mass" => gate,
        "man" => {
            if count > 0 {
                norm / count as f64
            } else {
                0.0
            }
        }
        "energy" => sal,
        _ => {
            if count > 0 {
                sal / count as f64
            } else {
                0.0
            }
        }
    }
}

/// Per layer the K best experts, in their original order. Never-routed experts score 0 and go first.
pub fn select(
    table: &Table,
    layers: &[u64],
    n_expert: u64,
    keep: u64,
    kind: &str,
) -> Result<BTreeMap<u64, Vec<u64>>, String> {
    if !SCORES.contains(&kind) {
        return Err(format!("unknown score {kind:?}"));
    }
    let missing: Vec<u64> = layers
        .iter()
        .filter(|l| !table.contains_key(l))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "the table has no data for layer(s) {:?}; run the router statistics on this model",
            &missing[..missing.len().min(8)]
        ));
    }
    let mut chosen = BTreeMap::new();
    for l in layers {
        let stats = &table[l];
        let mut ranked: Vec<u64> = (0..n_expert).collect();
        // highest score first; among equal scores the lower id first
        ranked.sort_by(|a, b| {
            let (sa, sb) = (
                score(stats.get(a).unwrap_or(&(0, 0.0, 0.0, 0.0)), kind),
                score(stats.get(b).unwrap_or(&(0, 0.0, 0.0, 0.0)), kind),
            );
            sb.total_cmp(&sa).then(a.cmp(b))
        });
        let mut k: Vec<u64> = ranked.into_iter().take(keep as usize).collect();
        k.sort();
        chosen.insert(*l, k);
    }
    Ok(chosen)
}

/// [2, 3, 4, 9] -> [(2, 3), (9, 1)]
fn runs(ids: &[u64]) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = vec![];
    for i in ids {
        match out.last_mut() {
            Some(last) if last.0 + last.1 == *i => last.1 += 1,
            _ => out.push((*i, 1)),
        }
    }
    out
}

pub fn plan_prune(
    model: &ggufio::Model,
    table: &Table,
    keep: u64,
    kind: &str,
) -> Result<(Vec<OutTensor>, Vec<ggufio::Kv>, Value), String> {
    let n_expert = model
        .number("expert_count")
        .filter(|n| *n > 0)
        .ok_or("this is not a mixture-of-experts model (no expert_count)")?
        as u64;
    if model.number("expert_group_count").unwrap_or(1) > 1 {
        return Err("models with grouped expert routing are not supported".into());
    }
    let used = model.number("expert_used_count").unwrap_or(0) as u64;
    if !(0 < keep && keep < n_expert) {
        return Err(format!("keep must be between 1 and {}", n_expert - 1));
    }
    if keep < used {
        return Err(format!(
            "the model activates {used} experts per token; keep at least that many"
        ));
    }
    let re = per_expert();
    let mut layers: Vec<u64> = model
        .order
        .iter()
        .filter_map(|n| re.captures(n).and_then(|m| m[1].parse().ok()))
        .collect();
    layers.sort();
    layers.dedup();
    if layers.is_empty() {
        return Err("no expert tensors found".into());
    }
    let chosen = select(table, &layers, n_expert, keep, kind)?;
    let mut tensors = vec![];
    for name in &model.order {
        let t = &model.tensors[name];
        let (path, offset, nbytes) = model.location(name);
        let Some(m) = re.captures(name) else {
            tensors.push(OutTensor {
                name: name.clone(),
                shape: t.shape.clone(),
                ttype: t.ttype,
                nbytes: t.nbytes,
                segments: vec![(path, offset, nbytes)],
            });
            continue;
        };
        if t.shape.last() != Some(&n_expert) || nbytes % n_expert != 0 {
            return Err(format!(
                "{name}: expected {n_expert} experts on the last axis, shape is {:?}",
                t.shape
            ));
        }
        let ids = &chosen[&m[1].parse::<u64>().unwrap_or(0)];
        let each = nbytes / n_expert;
        let segments = runs(ids)
            .iter()
            .map(|(first, count)| (path.clone(), offset + first * each, count * each))
            .collect();
        let mut shape = t.shape.clone();
        *shape.last_mut().unwrap() = keep;
        tensors.push(OutTensor {
            name: name.clone(),
            shape,
            ttype: t.ttype,
            nbytes: each * keep,
            segments,
        });
    }
    let kv = ggufio::with_value(
        &model.kv,
        &format!("{}.expert_count", model.arch()),
        keep as i64,
    )
    .map_err(|e| e.0)?;
    let mut kept_mass = vec![];
    for l in &layers {
        let total: f64 = table[l].values().map(|v| v.1).sum::<f64>();
        let total = if total == 0.0 { 1.0 } else { total };
        kept_mass.push(
            chosen[l]
                .iter()
                .map(|e| table[l].get(e).map_or(0.0, |v| v.1))
                .sum::<f64>()
                / total,
        );
    }
    let (min_i, min) = kept_mass
        .iter()
        .enumerate()
        .fold(
            (0, f64::INFINITY),
            |acc, (i, v)| if *v < acc.1 { (i, *v) } else { acc },
        );
    let info = json!({"n_expert": n_expert, "keep": keep, "score": kind,
        "layers": chosen.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
        "routing_mass_kept": {"mean": kept_mass.iter().sum::<f64>() / kept_mass.len() as f64, "min": min, "min_layer": layers[min_i]}});
    Ok((tensors, kv, info))
}

#[allow(clippy::too_many_arguments)]
pub fn prune(
    model_path: &Path,
    out: &Path,
    table: &Table,
    keep: u64,
    kind: &str,
    progress: Progress,
    cancelled: &dyn Fn() -> bool,
    log: &dyn Fn(&str),
) -> Result<Value, Stop> {
    let model = ggufio::open_model(model_path).map_err(|e| e.0)?;
    let (tensors, kv, info) = plan_prune(&model, table, keep, kind)?;
    let rm = &info["routing_mass_kept"];
    log(&format!(
        "{} -> {keep} experts in {} layers, score {kind}; router mass kept: mean {:.4}, lowest {:.4} (layer {})",
        model.number("expert_count").unwrap_or(0),
        info["layers"].as_object().map_or(0, |o| o.len()),
        rm["mean"].as_f64().unwrap_or(0.0),
        rm["min"].as_f64().unwrap_or(0.0),
        rm["min_layer"]
    ));
    let mut result = build::write_model(
        out,
        &kv,
        &tensors,
        model.alignment(),
        progress,
        cancelled,
        log,
    )?;
    let mut manifest =
        json!({"model": model_path.file_name().unwrap_or_default().to_string_lossy()});
    for (k, v) in info.as_object().unwrap() {
        manifest[k] = v.clone();
    }
    manifest["sha256"] = result["sha256"].clone();
    let _ = std::fs::write(
        format!("{}.reap.json", out.display()),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    );
    result["keep"] = json!(keep);
    result["n_expert"] = info["n_expert"].clone();
    result["routing_mass_kept"] = rm.clone();
    Ok(result)
}

/// Size of the pruned file from the headers alone.
pub fn predicted_prune_size(model: &ggufio::Model, keep: u64) -> u64 {
    let n_expert = model.number("expert_count").unwrap_or(0) as u64;
    let re = per_expert();
    let infos: Vec<ggufio::Info> = model
        .order
        .iter()
        .map(|n| {
            let t = &model.tensors[n];
            if re.is_match(n) && n_expert > 0 && t.shape.last() == Some(&n_expert) {
                let mut s = t.shape.clone();
                *s.last_mut().unwrap() = keep;
                (n.clone(), s, t.ttype, t.nbytes / n_expert * keep)
            } else {
                (n.clone(), t.shape.clone(), t.ttype, t.nbytes)
            }
        })
        .collect();
    let (header, _) = ggufio::build_header(&model.kv, &infos, model.alignment());
    header.len() as u64 + ggufio::layout(&infos, model.alignment()).1
}
