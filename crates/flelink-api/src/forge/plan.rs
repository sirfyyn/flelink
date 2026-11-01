//! The recipe calculator: which source supplies which expert layer, and what the result needs on your machine. Port of
//! `fle_studio_forge/plan.py`.
//!
//! A hybrid keeps the metadata and every non-expert tensor of one base file and takes whole expert layers from the base or
//! from other quants of the same model. Which layer comes from which source is a multiple-choice knapsack (one pick per
//! layer, sum of sizes within the budget, sum of the predicted errors as small as possible), solved exactly by dynamic
//! programming over the bytes in steps that never round a size down. The error per layer and source is a table you
//! measured, or the stand-in 4^-bpw. Measure the result (KL divergence) before trusting either.
use super::ggufio::{self, Model};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

pub const GB: u64 = 1_000_000_000;
const GIB: u64 = 1 << 30;
const NEVER_QUANTIZED: [&str; 7] = [
    "_norm.weight",
    "ffn_gate_inp.weight",
    "ffn_gate_inp_shexp.weight",
    "ssm_conv1d.weight",
    "pos_embd",
    "token_types",
    "attn_rel_b.weight",
];
/// resolution of the knapsack: the expert budget is cut into at most this many steps
const MAX_STEPS: u64 = 20000;

#[derive(Debug)]
pub struct PlanError(pub String);

type R<T> = Result<T, PlanError>;

fn perr<T>(m: impl Into<String>) -> R<T> {
    Err(PlanError(m.into()))
}

fn expert_layer() -> Regex {
    Regex::new(r"^blk\.(\d+)\.ffn_(gate_up|gate|up|down)_exps\.weight$").unwrap()
}

fn padded(n: u64, a: u64) -> u64 {
    n.div_ceil(a) * a
}

/// What the calculator needs to know about one file (or split file) of a model.
pub struct Source {
    pub id: String,
    pub model: Model,
    pub layer_bytes: BTreeMap<u64, u64>,
    pub layer_elems: BTreeMap<u64, u64>,
    pub layer_types: BTreeMap<u64, BTreeMap<String, String>>,
    pub layer_shapes: BTreeMap<u64, Vec<(String, Vec<u64>)>>,
}

impl Source {
    pub fn load(id: &str, path: &str) -> Result<Source, String> {
        let m = ggufio::open_model(&ggufio::path_buf(path)).map_err(|e| e.0)?;
        Ok(Self::from_model(id, m))
    }

    pub fn from_model(id: &str, model: Model) -> Source {
        let re = expert_layer();
        let a = model.alignment();
        let mut s = Source {
            id: id.into(),
            layer_bytes: BTreeMap::new(),
            layer_elems: BTreeMap::new(),
            layer_types: BTreeMap::new(),
            layer_shapes: BTreeMap::new(),
            model: model.clone(),
        };
        for name in &model.order {
            let Some(m) = re.captures(name) else { continue };
            let layer: u64 = m[1].parse().unwrap_or(0);
            let t = &model.tensors[name];
            *s.layer_bytes.entry(layer).or_default() += padded(t.nbytes, a);
            *s.layer_elems.entry(layer).or_default() += t.n_elements();
            s.layer_types
                .entry(layer)
                .or_default()
                .insert(m[2].to_string(), t.type_name());
            s.layer_shapes
                .entry(layer)
                .or_default()
                .push((name.clone(), t.shape.clone()));
        }
        s
    }

    pub fn layers(&self) -> Vec<u64> {
        self.layer_bytes.keys().copied().collect()
    }

    pub fn rest_bytes(&self) -> u64 {
        let re = expert_layer();
        let a = self.model.alignment();
        self.model
            .tensors
            .iter()
            .filter(|(n, _)| !re.is_match(n))
            .map(|(_, t)| padded(t.nbytes, a))
            .sum()
    }

    pub fn bpw(&self, layer: u64) -> f64 {
        self.layer_bytes[&layer] as f64 * 8.0 / self.layer_elems[&layer].max(1) as f64
    }

    pub fn describe(&self) -> Value {
        let m = &self.model;
        let mut groups: Vec<(String, Vec<u64>)> = vec![];
        for layer in self.layers() {
            let combo = self.layer_types[&layer]
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(" / ");
            match groups.iter_mut().find(|(c, _)| *c == combo) {
                Some((_, v)) => v.push(layer),
                None => groups.push((combo, vec![layer])),
            }
        }
        groups.sort_by_key(|(_, v)| v[0]);
        let bytes: u64 = self.layer_bytes.values().sum();
        let elems: u64 = self.layer_elems.values().sum();
        json!({"id": self.id, "path": m.files[0].path, "parts": m.files.len(), "size": m.total_bytes(), "arch": m.arch(),
            "name": m.meta.get("general.name").and_then(|v| v.as_str()).unwrap_or(""),
            "layers": m.number("block_count"), "experts": m.number("expert_count"), "experts_used": m.number("expert_used_count"),
            "expert_layers": self.layer_bytes.len(), "expert_bytes": bytes, "rest_bytes": self.rest_bytes(),
            "types": groups.iter().map(|(k, v)| json!({"types": k, "layers": v})).collect::<Vec<_>>(),
            "bpw": if self.layer_bytes.is_empty() { Value::Null } else { json!(crate::stats::round_to(bytes as f64 * 8.0 / elems.max(1) as f64, 3)) }})
    }
}

/// A donor can replace an expert layer only if the layer has the same tensors with the same shapes.
fn check_compatible(base: &Source, other: &Source) -> R<()> {
    if base.model.arch() != other.model.arch() {
        return perr(format!(
            "{} is a {:?} model, {} is {:?}",
            other.id,
            other.model.arch(),
            base.id,
            base.model.arch()
        ));
    }
    if base.layers() != other.layers() {
        return perr(format!(
            "{} has other expert layers than {}",
            other.id, base.id
        ));
    }
    for layer in base.layers() {
        if base.layer_shapes[&layer] != other.layer_shapes[&layer] {
            return perr(format!("layer {layer}: the expert tensors of {} and {} differ in name or shape; they are not quants of the same model", other.id, base.id));
        }
    }
    Ok(())
}

pub struct Selection {
    pub layers: BTreeMap<u64, String>,
    pub feasible: bool,
    pub bytes: u64,
    pub min_bytes: u64,
    pub max_bytes: u64,
    pub error: f64,
    pub error_base: f64,
    pub note: String,
}

impl Selection {
    pub fn public(&self) -> Value {
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for s in self.layers.values() {
            *counts.entry(s.clone()).or_default() += 1;
        }
        json!({"layers": self.layers.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
            "feasible": self.feasible, "bytes": self.bytes, "min_bytes": self.min_bytes, "max_bytes": self.max_bytes,
            "error": self.error, "error_base": self.error_base, "counts": counts, "note": self.note})
    }
}

fn layer_errors(
    sources: &BTreeMap<String, Source>,
    errors: Option<&Value>,
) -> R<HashMap<String, HashMap<u64, f64>>> {
    let mut out = HashMap::new();
    for (sid, src) in sources {
        let mut m = HashMap::new();
        if let Some(e) = errors.filter(|e| e.as_object().is_some_and(|o| !o.is_empty())) {
            let Some(table) = e.get(sid).and_then(|t| t.as_object()) else {
                return perr(format!("the error table has no entry for source {sid}"));
            };
            let missing: Vec<u64> = src
                .layers()
                .into_iter()
                .filter(|l| !table.contains_key(&l.to_string()))
                .collect();
            if !missing.is_empty() {
                return perr(format!(
                    "the error table of {sid} lacks layer(s) {:?}",
                    &missing[..missing.len().min(5)]
                ));
            }
            for l in src.layers() {
                m.insert(l, table[&l.to_string()].as_f64().unwrap_or(0.0));
            }
        } else {
            for l in src.layers() {
                m.insert(l, 4f64.powf(-src.bpw(l)));
            }
        }
        out.insert(sid.clone(), m);
    }
    Ok(out)
}

/// Pick the source of every expert layer for the smallest summed error within `budget` bytes of file size.
pub fn choose_layers(
    sources: &BTreeMap<String, Source>,
    base: &str,
    budget: u64,
    errors: Option<&Value>,
    header_bytes: u64,
) -> R<Selection> {
    let Some(b) = sources.get(base) else {
        return perr(format!("base source {base:?} is not among the sources"));
    };
    for s in sources.values() {
        check_compatible(b, s)?;
    }
    let layers = b.layers();
    if layers.is_empty() {
        return perr("the base model has no expert layers (is it a mixture-of-experts model?)");
    }
    let err = layer_errors(sources, errors)?;
    let ids: Vec<&String> = sources.keys().collect();
    let rest = b.rest_bytes() + header_bytes;
    let size = |s: &str, l: u64| sources[s].layer_bytes[&l];
    let cheapest: HashMap<u64, u64> = layers
        .iter()
        .map(|l| (*l, ids.iter().map(|s| size(s, *l)).min().unwrap()))
        .collect();
    let dearest: HashMap<u64, u64> = layers
        .iter()
        .map(|l| (*l, ids.iter().map(|s| size(s, *l)).max().unwrap()))
        .collect();
    let min_total = rest + cheapest.values().sum::<u64>();
    let max_total = rest + dearest.values().sum::<u64>();
    let base_err: f64 = layers.iter().map(|l| err[base][l]).sum();
    let base_total = rest + b.layer_bytes.values().sum::<u64>();
    if budget < min_total {
        let pick: BTreeMap<u64, String> = layers
            .iter()
            .map(|l| {
                (
                    *l,
                    (*ids.iter().min_by_key(|s| size(s, *l)).unwrap()).clone(),
                )
            })
            .collect();
        let e = layers.iter().map(|l| err[&pick[l]][l]).sum();
        return Ok(Selection {
            layers: pick,
            feasible: false,
            bytes: min_total,
            min_bytes: min_total,
            max_bytes: max_total,
            error: e,
            error_base: base_err,
            note: format!(
                "The budget is below the smallest possible result ({:.2} GB).",
                min_total as f64 / GB as f64
            ),
        });
    }
    let avail = budget - min_total;
    let step = if avail > 0 {
        avail.div_ceil(MAX_STEPS).max(1)
    } else {
        1
    };
    let cap = avail / step;
    // state: extra units over the cheapest pick -> (error, back pointer, source)
    let mut tables: Vec<BTreeMap<u64, (f64, u64, String)>> = vec![];
    let mut states: BTreeMap<u64, (f64, u64, String)> =
        BTreeMap::from([(0, (0.0, u64::MAX, String::new()))]);
    for l in &layers {
        let mut nxt: BTreeMap<u64, (f64, u64, String)> = BTreeMap::new();
        for (used, (e0, _, _)) in &states {
            for sid in &ids {
                let extra = (size(sid, *l) - cheapest[l]).div_ceil(step);
                let u = used + extra;
                if u > cap {
                    continue;
                }
                let e1 = e0 + err[*sid][l];
                if nxt.get(&u).is_none_or(|x| e1 < x.0) {
                    nxt.insert(u, (e1, *used, (*sid).clone()));
                }
            }
        }
        // keep only states better than every state with fewer bytes
        let mut front = BTreeMap::new();
        let mut best = f64::INFINITY;
        for (u, v) in nxt {
            if v.0 < best {
                best = v.0;
                front.insert(u, v);
            }
        }
        tables.push(front.clone());
        states = front;
    }
    let mut used = *states
        .iter()
        .min_by(|a, b| a.1 .0.total_cmp(&b.1 .0))
        .map(|(u, _)| u)
        .unwrap();
    let mut pick = BTreeMap::new();
    for (l, table) in layers.iter().rev().zip(tables.iter().rev()) {
        let (_, prev, sid) = &table[&used];
        pick.insert(*l, sid.clone());
        used = *prev;
    }
    let total = rest + layers.iter().map(|l| size(&pick[l], *l)).sum::<u64>();
    let error = layers.iter().map(|l| err[&pick[l]][l]).sum();
    let note = if total > base_total {
        "The result is larger than the base file."
    } else {
        ""
    };
    Ok(Selection {
        layers: pick,
        feasible: true,
        bytes: total,
        min_bytes: min_total,
        max_bytes: max_total,
        error,
        error_base: base_err,
        note: note.into(),
    })
}

/// (source, pattern, layers) of a rule that takes further tensors from a donor.
pub type Rule = (String, Regex, Option<std::collections::BTreeSet<u64>>);

pub fn parse_rules(rules: &Value) -> R<Vec<Rule>> {
    let mut out = vec![];
    for r in rules.as_array().into_iter().flatten() {
        let lt = r["layers"].as_str().unwrap_or("");
        let layers = if lt.is_empty() {
            None
        } else {
            Some(super::recipe::parse_layer_set(lt).map_err(|e| PlanError(e.0))?)
        };
        let p = r["pattern"].as_str().unwrap_or("");
        let re = Regex::new(p).map_err(|e| PlanError(format!("bad pattern {p:?}: {e}")))?;
        out.push((r["source"].as_str().unwrap_or("").to_string(), re, layers));
    }
    Ok(out)
}

/// For every tensor of the base model, the source id it is taken from (checked: it must exist and have the same shape).
pub fn assemble(
    model: &Model,
    donors: &HashMap<String, Model>,
    pick: &BTreeMap<u64, String>,
    base: &str,
    rules: &[Rule],
) -> R<HashMap<String, String>> {
    let re = expert_layer();
    let blk = Regex::new(r"^blk\.(\d+)\.").unwrap();
    let mut plan = HashMap::new();
    for name in &model.order {
        let mut sid = base.to_string();
        if let Some(m) = re.captures(name) {
            if let Some(s) = pick.get(&m[1].parse::<u64>().unwrap_or(0)) {
                sid = s.clone();
            }
        }
        let layer: Option<u64> = blk.captures(name).and_then(|m| m[1].parse().ok());
        for (rs, pattern, layers) in rules {
            if pattern.is_match(name)
                && layers
                    .as_ref()
                    .is_none_or(|ls| layer.is_some_and(|l| ls.contains(&l)))
            {
                if sid != base && *rs != sid {
                    return perr(format!("{name}: claimed by two sources ({sid}, {rs})"));
                }
                sid = rs.clone();
            }
        }
        if sid != base {
            let Some(donor) = donors.get(&sid) else {
                return perr(format!("source {sid} is not open"));
            };
            let Some(t) = donor.tensors.get(name) else {
                return perr(format!("source {sid} has no tensor {name}"));
            };
            if t.shape != model.tensors[name].shape {
                return perr(format!(
                    "{name}: shape {:?} in {sid}, {:?} in the base",
                    t.shape, model.tensors[name].shape
                ));
            }
        }
        plan.insert(name.clone(), sid);
    }
    Ok(plan)
}

pub fn result_tensors(
    model: &Model,
    models: &HashMap<String, Model>,
    plan: &HashMap<String, String>,
) -> Vec<ggufio::Info> {
    model
        .order
        .iter()
        .map(|n| {
            let t = &models[&plan[n]].tensors[n];
            (n.clone(), t.shape.clone(), t.ttype, t.nbytes)
        })
        .collect()
}

/// The exact size of the file a build will write, computed from headers only.
pub fn predicted_file_size(
    model: &Model,
    models: &HashMap<String, Model>,
    plan: &HashMap<String, String>,
) -> u64 {
    let infos = result_tensors(model, models, plan);
    let (header, _) = ggufio::build_header(&model.kv, &infos, model.alignment());
    header.len() as u64 + ggufio::layout(&infos, model.alignment()).1
}

/// What the result needs, split the way llama.cpp places it, and whether it fits the given RAM and VRAM.
pub fn memory_preview(
    model: &Model,
    models: &HashMap<String, Model>,
    plan: &HashMap<String, String>,
    ram: u64,
    vram: u64,
    context: u64,
) -> Value {
    let exp = Regex::new(r"\.ffn_(?:gate_up|gate|up|down)_exps\.").unwrap();
    let (mut experts, mut inputs, mut other) = (0u64, 0u64, 0u64);
    for n in &model.order {
        let t = &models[&plan[n]].tensors[n];
        if exp.is_match(n) {
            experts += t.nbytes;
        } else if n.starts_with("token_embd.") || n.starts_with("per_layer_token_embd.") {
            inputs += t.nbytes;
        } else {
            other += t.nbytes;
        }
    }
    let kv_per_token = crate::gguf::inspect(std::path::Path::new(&model.files[0].path))
        .map(|i| i.kv_bytes_per_token("q8_0", "q8_0"))
        .unwrap_or(0.0);
    let kv = (kv_per_token * context as f64) as u64;
    let (rt_ram, rt_gpu) = (2 * GIB, GIB + GIB / 2);
    let split_ram = experts + inputs + rt_ram;
    let split_vram = other + kv + rt_gpu;
    let split_fits = split_ram <= ram && (vram == 0 || split_vram <= vram);
    let gpu_ram = inputs + rt_ram;
    let gpu_vram = experts + other + kv + rt_gpu;
    let gpu_fits = vram != 0 && gpu_ram <= ram && gpu_vram <= vram;
    let uni_ram = experts + inputs + other + kv + rt_ram;
    let uni_fits = uni_ram <= ram;
    let verdict = if vram == 0 {
        if uni_fits {
            "cpu_only"
        } else {
            "too_big"
        }
    } else if gpu_fits {
        "all_gpu"
    } else if split_fits {
        "experts_cpu"
    } else if uni_fits {
        "cpu_only"
    } else {
        "too_big"
    };
    json!({"experts": experts, "inputs": inputs, "dense": other, "kv": kv, "context": context,
        "experts_on_cpu": {"ram": split_ram, "vram": split_vram, "gpu_cache": if vram != 0 { vram.saturating_sub(split_vram) } else { 0 }, "fits": split_fits},
        "all_gpu": {"ram": gpu_ram, "vram": gpu_vram, "fits": gpu_fits},
        "cpu_only": {"ram": uni_ram, "vram": 0, "fits": uni_fits},
        "verdict": verdict, "ram_bytes": ram, "vram_bytes": vram})
}

/// An estimate of what llama-quantize will write (marked as an estimate wherever it is shown).
pub fn estimate_quantize(
    model: &Model,
    default_type: &str,
    rules: &Value,
    output_type: &str,
    embedding_type: &str,
) -> Value {
    let default_type = ggufio::ftype_base(default_type);
    let rules: Vec<(Regex, String)> = rules
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some((
                Regex::new(r["pattern"].as_str()?).ok()?,
                r["type"].as_str()?.to_string(),
            ))
        })
        .collect();
    let (mut infos, mut changed, mut kept) = (vec![], 0, 0);
    for n in &model.order {
        let t = &model.tensors[n];
        let mut want: Option<String> = None;
        if t.shape.len() >= 2
            && n.ends_with(".weight")
            && !NEVER_QUANTIZED.iter().any(|x| n.contains(x))
        {
            want = Some(if n == "output.weight" && !output_type.is_empty() {
                output_type.to_string()
            } else if n == "token_embd.weight" && !embedding_type.is_empty() {
                embedding_type.to_string()
            } else {
                rules
                    .iter()
                    .find(|(re, _)| re.is_match(n))
                    .map_or(default_type.to_string(), |(_, t)| t.clone())
            });
        }
        let tid = want.as_deref().and_then(ggufio::type_id);
        match tid.and_then(|id| ggufio::tensor_nbytes(id, &t.shape).map(|b| (id, b))) {
            Some((id, b)) => {
                infos.push((n.clone(), t.shape.clone(), id, b));
                changed += 1;
            }
            None => {
                infos.push((n.clone(), t.shape.clone(), t.ttype, t.nbytes));
                kept += 1;
            }
        }
    }
    let (header, _) = ggufio::build_header(&model.kv, &infos, model.alignment());
    let size = header.len() as u64 + ggufio::layout(&infos, model.alignment()).1;
    let params: u64 = model.tensors.values().map(|t| t.n_elements()).sum();
    json!({"estimate_bytes": size, "bpw": crate::stats::round_to(size as f64 * 8.0 / params.max(1) as f64, 3),
        "tensors_changed": changed, "tensors_kept": kept, "estimate": true})
}
