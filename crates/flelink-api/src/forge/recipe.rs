//! The Forge recipe: one JSON document that says how a model is built, so that anyone can build it again. Port of
//! `fle_studio_forge/recipe.py` (schema 1, see docs/FORGE.md).
//!
//! A shared recipe carries file names but no folders (`portable`); whoever loads it points each source at a file of their
//! own. A recipe never contains a command line: the Forge builds every command itself from the fields, so a recipe from
//! someone else cannot make FLE Link run anything but its own steps.
use regex::Regex;
use serde_json::{json, Map, Value};

pub const SCHEMA: i64 = 1;
pub const KIND: &str = "fle-forge-recipe";
pub const MODES: [&str; 2] = ["hybrid", "quantize"];
pub const ROLES: [&str; 3] = ["base", "donor", "reference"];
pub const REAP_SCORES: [&str; 5] = ["reap", "freq", "mass", "man", "energy"];

#[derive(Debug)]
pub struct RecipeError(pub String);

type R<T> = Result<T, RecipeError>;

fn rerr<T>(m: impl Into<String>) -> R<T> {
    Err(RecipeError(m.into()))
}

pub fn defaults() -> Value {
    json!({
        "schema": SCHEMA, "kind": KIND, "name": "my-quant", "created": "", "studio": "", "notes": "", "mode": "hybrid",
        "sources": {},
        "hybrid": {"base": "", "layers": {}, "rules": [], "budget_gb": 0.0, "objective": "bpw", "errors": {}},
        "quantize": {"source": "", "type": "iq4_nl", "rules": [], "output_type": "", "embedding_type": "",
            "allow_requantize": false, "imatrix": true},
        "imatrix": {"corpus": "", "file": "", "chunks": 200, "context": 2048, "gpu_layers": 0, "threads": 0},
        "reap": {"enabled": false, "keep": 0, "score": "reap", "table": "", "chunks": 256, "gpu_layers": 0, "threads": 0},
        "verify": {"enabled": true, "reference": "", "text": "", "context": 2048, "chunks": 20, "gpu_layers": 0, "threads": 0,
            "base_logits": ""}
    })
}

/// Unknown keys are dropped; nested sections are merged, except the free-form tables.
fn merge(base: &Value, patch: &Value) -> Value {
    let mut out = base.clone();
    let (Some(o), Some(p)) = (out.as_object_mut(), patch.as_object()) else {
        return out;
    };
    for (k, v) in p {
        let Some(cur) = o.get(k) else { continue };
        if cur.is_object() && !["sources", "layers", "errors"].contains(&k.as_str()) {
            if v.is_object() {
                let merged = merge(cur, v);
                o.insert(k.clone(), merged);
            }
        } else {
            o.insert(k.clone(), v.clone());
        }
    }
    out
}

pub fn clean_name(name: &str) -> String {
    let re = Regex::new(r"[^\w.-]+").unwrap();
    let s = re.replace_all(name.trim(), "-").to_string();
    let s: String = s
        .trim_matches(|c| c == '-' || c == '.')
        .chars()
        .take(80)
        .collect();
    if s.is_empty() {
        "my-quant".into()
    } else {
        s
    }
}

fn num(v: &Value, what: &str, low: f64, high: f64) -> R<f64> {
    let x = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    };
    let Some(x) = x else {
        return rerr(format!("{what} must be a number"));
    };
    if !(low..=high).contains(&x) {
        return rerr(format!(
            "{what} must be between {} and {}",
            crate::launch::fmt_g(low),
            crate::launch::fmt_g(high)
        ));
    }
    Ok(x)
}

fn truthy(v: &Value) -> bool {
    crate::settings::py_truthy(v)
}

fn s(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        v if !truthy(v) => String::new(),
        v => crate::settings::py_str(v),
    }
}

/// "0-3,7" -> {0, 1, 2, 3, 7}
pub fn parse_layer_set(text: &str) -> R<std::collections::BTreeSet<u64>> {
    let mut out = std::collections::BTreeSet::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (lo, hi) = part.split_once('-').unwrap_or((part, ""));
        let bad = || RecipeError(format!("not a layer list: {text:?}"));
        let a: i64 = lo.trim().parse().map_err(|_| bad())?;
        let b: i64 = if hi.is_empty() {
            a
        } else {
            hi.trim().parse().map_err(|_| bad())?
        };
        if a < 0 || b < a || b > 4096 {
            return Err(bad());
        }
        out.extend(a as u64..=b as u64);
    }
    Ok(out)
}

fn pattern_ok(p: &str) -> bool {
    Regex::new(r"^[\w.*+?|()\[\]\\-]{1,120}$")
        .unwrap()
        .is_match(p)
}

/// Fill in defaults, drop unknown keys and check every value.
pub fn normalize(data: &Value) -> R<Value> {
    if !data.is_object() {
        return rerr("a recipe is a JSON object");
    }
    if data.get("kind").is_some_and(|k| k != KIND) {
        return rerr("this is not a Forge recipe");
    }
    if let Some(schema) = data.get("schema").filter(|s| **s != json!(SCHEMA)) {
        return rerr(format!(
            "recipe schema {schema} is not supported (this FLE Link reads schema {SCHEMA})"
        ));
    }
    let mut r = merge(&defaults(), data);
    r["name"] = json!(clean_name(&s(&r["name"])));
    r["notes"] = json!(s(&r["notes"]).chars().take(2000).collect::<String>());
    if !MODES.contains(&r["mode"].as_str().unwrap_or("")) {
        return rerr(format!("mode must be one of {}", MODES.join(", ")));
    }
    let sid_re = Regex::new(r"^[A-Za-z][A-Za-z0-9_]{0,15}$").unwrap();
    let hex64 = Regex::new(r"^[0-9a-f]{64}$").unwrap();
    let mut sources = Map::new();
    for (sid, src) in r["sources"].as_object().cloned().unwrap_or_default() {
        if !sid_re.is_match(&sid) {
            return rerr(format!(
                "source id {sid:?} must be a letter followed by letters, digits or _ (16 at most)"
            ));
        }
        if !src.is_object() {
            return rerr(format!("source {sid} must be an object"));
        }
        let role = if truthy(&src["role"]) {
            s(&src["role"])
        } else {
            "donor".into()
        };
        if !ROLES.contains(&role.as_str()) {
            return rerr(format!(
                "source {sid}: role must be one of {}",
                ROLES.join(", ")
            ));
        }
        let sha = s(&src["sha256"]).to_lowercase();
        if !sha.is_empty() && !hex64.is_match(&sha) {
            return rerr(format!("source {sid}: sha256 is not a hex digest"));
        }
        let path = s(&src["path"]);
        let file = if truthy(&src["file"]) {
            s(&src["file"])
        } else {
            path.replace('\\', "/")
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string()
        };
        sources.insert(
            sid,
            json!({"file": file, "path": path, "role": role,
                "size": src["size"].as_i64().or_else(|| src["size"].as_f64().map(|f| f as i64)).unwrap_or(0),
                "sha256": sha, "origin": s(&src["origin"]).chars().take(300).collect::<String>()}),
        );
    }
    r["sources"] = Value::Object(sources);

    let h = &mut r["hybrid"];
    h["budget_gb"] = json!(num(&h["budget_gb"], "hybrid.budget_gb", 0.0, 100000.0)?);
    if !["bpw", "errors"].contains(&h["objective"].as_str().unwrap_or("")) {
        return rerr("hybrid.objective must be bpw or errors");
    }
    let mut layers = Map::new();
    for (layer, sid) in h["layers"].as_object().cloned().unwrap_or_default() {
        let n: i64 = layer
            .trim()
            .parse()
            .map_err(|_| RecipeError(format!("hybrid.layers: {layer:?} is not a layer number")))?;
        if !(0..=4096).contains(&n) {
            return rerr(format!("hybrid.layers: layer {n} is out of range"));
        }
        layers.insert(n.to_string(), json!(s(&sid)));
    }
    h["layers"] = Value::Object(layers);
    let mut rules = vec![];
    for rule in h["rules"].as_array().cloned().unwrap_or_default() {
        let pattern = s(&rule["pattern"]);
        if !pattern_ok(&pattern) {
            return rerr(format!("hybrid.rules: bad pattern {pattern:?}"));
        }
        let lt = s(&rule["layers"]);
        parse_layer_set(&lt)?;
        rules.push(json!({"source": s(&rule["source"]), "pattern": pattern, "layers": lt}));
    }
    h["rules"] = json!(rules);
    let mut errors = Map::new();
    for (sid, table) in h["errors"].as_object().cloned().unwrap_or_default() {
        let mut t = Map::new();
        for (k, v) in table.as_object().cloned().unwrap_or_default() {
            let n: i64 = k
                .trim()
                .parse()
                .map_err(|_| RecipeError(format!("hybrid.errors: bad layer {k:?}")))?;
            t.insert(
                n.to_string(),
                json!(num(&v, "hybrid.errors", f64::MIN, f64::MAX)?),
            );
        }
        errors.insert(sid, Value::Object(t));
    }
    h["errors"] = Value::Object(errors);

    let targets = super::ggufio::quantize_targets();
    let ftypes = super::ggufio::quantize_ftypes();
    let q = &mut r["quantize"];
    for key in ["type", "output_type", "embedding_type"] {
        let v = s(&q[key]).to_lowercase();
        let allowed = if key == "type" { &ftypes } else { &targets };
        if !v.is_empty() && !allowed.contains(&v) {
            return rerr(format!("quantize.{key}: unknown type {v:?}"));
        }
        q[key] = json!(v);
    }
    if q["type"] == "" {
        return rerr("quantize.type is required");
    }
    let mut qrules = vec![];
    for rule in q["rules"].as_array().cloned().unwrap_or_default() {
        let (pattern, t) = (s(&rule["pattern"]), s(&rule["type"]).to_lowercase());
        if !pattern_ok(&pattern) {
            return rerr(format!("quantize.rules: bad pattern {pattern:?}"));
        }
        if !targets.contains(&t) {
            return rerr(format!("quantize.rules: unknown type {t:?}"));
        }
        qrules.push(json!({"pattern": pattern, "type": t}));
    }
    q["rules"] = json!(qrules);
    q["allow_requantize"] = json!(truthy(&q["allow_requantize"]));
    q["imatrix"] = json!(truthy(&q["imatrix"]));

    let int = |v: &Value, what: &str, lo: f64, hi: f64| -> R<Value> {
        Ok(json!(num(v, what, lo, hi)? as i64))
    };
    let im = &mut r["imatrix"];
    im["chunks"] = int(&im["chunks"], "imatrix.chunks", 0.0, 1e6)?;
    im["context"] = int(&im["context"], "imatrix.context", 64.0, 1_048_576.0)?;
    im["gpu_layers"] = int(&im["gpu_layers"], "imatrix.gpu_layers", 0.0, 1000.0)?;
    im["threads"] = int(&im["threads"], "imatrix.threads", 0.0, 1024.0)?;
    im["corpus"] = json!(s(&im["corpus"]));
    im["file"] = json!(s(&im["file"]));

    let rp = &mut r["reap"];
    rp["enabled"] = json!(truthy(&rp["enabled"]));
    rp["keep"] = int(&rp["keep"], "reap.keep", 0.0, 100000.0)?;
    if !REAP_SCORES.contains(&rp["score"].as_str().unwrap_or("")) {
        return rerr(format!(
            "reap.score must be one of {}",
            REAP_SCORES.join(", ")
        ));
    }
    rp["chunks"] = int(&rp["chunks"], "reap.chunks", 0.0, 1e6)?;
    rp["gpu_layers"] = int(&rp["gpu_layers"], "reap.gpu_layers", 0.0, 1000.0)?;
    rp["threads"] = int(&rp["threads"], "reap.threads", 0.0, 1024.0)?;
    rp["table"] = json!(s(&rp["table"]));
    if rp["enabled"] == true && rp["keep"].as_i64().unwrap_or(0) < 1 {
        return rerr("reap.keep must be at least 1 when pruning is on");
    }

    let v = &mut r["verify"];
    v["enabled"] = json!(truthy(&v["enabled"]));
    v["context"] = int(&v["context"], "verify.context", 64.0, 1_048_576.0)?;
    v["chunks"] = int(&v["chunks"], "verify.chunks", 0.0, 1e6)?;
    v["gpu_layers"] = int(&v["gpu_layers"], "verify.gpu_layers", 0.0, 1000.0)?;
    v["threads"] = int(&v["threads"], "verify.threads", 0.0, 1024.0)?;
    for k in ["reference", "text", "base_logits"] {
        v[k] = json!(s(&v[k]));
    }
    Ok(r)
}

/// Problems that make the recipe unbuildable as it stands. Empty = fine.
pub fn validate_sources(r: &Value) -> Vec<String> {
    let mut problems: Vec<String> = vec![];
    let src = r["sources"].as_object().cloned().unwrap_or_default();
    let need = |sid: &str, what: &str, problems: &mut Vec<String>| {
        if sid.is_empty() {
            problems.push(format!("{what} is not set"));
        } else if !src.contains_key(sid) {
            problems.push(format!(
                "{what} names the source {sid:?}, which the recipe does not list"
            ));
        }
    };
    if r["mode"] == "hybrid" {
        let h = &r["hybrid"];
        need(
            h["base"].as_str().unwrap_or(""),
            "hybrid.base",
            &mut problems,
        );
        for (layer, sid) in h["layers"].as_object().into_iter().flatten() {
            let sid = sid.as_str().unwrap_or("");
            if !src.contains_key(sid) {
                problems.push(format!(
                    "layer {layer} comes from {sid:?}, which the recipe does not list"
                ));
            }
        }
        for rule in h["rules"].as_array().into_iter().flatten() {
            need(
                rule["source"].as_str().unwrap_or(""),
                "a hybrid rule's source",
                &mut problems,
            );
        }
    } else {
        need(
            r["quantize"]["source"].as_str().unwrap_or(""),
            "quantize.source",
            &mut problems,
        );
    }
    if r["verify"]["enabled"] == true {
        need(
            r["verify"]["reference"].as_str().unwrap_or(""),
            "verify.reference",
            &mut problems,
        );
    }
    for (sid, s) in &src {
        if s["path"].as_str().unwrap_or("").is_empty() {
            let f = s["file"]
                .as_str()
                .filter(|f| !f.is_empty())
                .unwrap_or("no file name");
            problems.push(format!(
                "source {sid} ({f}) has no file on this computer yet"
            ));
        }
    }
    if r["reap"]["enabled"] == true && r["reap"]["keep"].as_i64().unwrap_or(0) < 1 {
        problems.push("reap.keep is not set".into());
    }
    let mut seen = std::collections::HashSet::new();
    problems.retain(|p| seen.insert(p.clone()));
    problems
}

fn base_name(v: &str) -> String {
    v.replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// The recipe as it is shared: no folders, no paths of this computer.
pub fn portable(r: &Value) -> R<Value> {
    let mut out = normalize(r)?;
    if let Some(src) = out["sources"].as_object_mut() {
        for s in src.values_mut() {
            s["path"] = json!("");
        }
    }
    for (section, key) in [
        ("imatrix", "corpus"),
        ("imatrix", "file"),
        ("reap", "table"),
        ("verify", "text"),
        ("verify", "base_logits"),
    ] {
        let v = out[section][key].as_str().unwrap_or("").to_string();
        out[section][key] = json!(if v.is_empty() {
            String::new()
        } else {
            base_name(&v)
        });
    }
    if out["studio"] == "" {
        out["studio"] = json!(env!("CARGO_PKG_VERSION"));
    }
    Ok(out)
}

pub fn dumps(r: &Value) -> String {
    serde_json::to_string_pretty(r).unwrap_or_default() + "\n"
}

pub fn loads(text: &str) -> R<Value> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| RecipeError(format!("not valid JSON: {e}")))?;
    normalize(&v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recipe_is_normalized_like_the_python_studio_does() {
        let r = normalize(&json!({"name": "My Quant!", "mode": "quantize", "unknown": 1,
            "sources": {"B": {"path": "/x/y/model.gguf", "role": "base"}},
            "quantize": {"source": "B", "type": "Q4_K_M", "rules": [{"pattern": "ffn_.*_exps", "type": "IQ3_S"}]},
            "imatrix": {"chunks": "50"}}))
        .unwrap();
        assert_eq!(r["name"], "My-Quant");
        assert!(r.get("unknown").is_none());
        assert_eq!(r["sources"]["B"]["file"], "model.gguf");
        assert_eq!(r["quantize"]["type"], "q4_k_m");
        assert_eq!(r["quantize"]["rules"][0]["type"], "iq3_s");
        assert_eq!(r["imatrix"]["chunks"], 50);
        assert!(normalize(&json!({"reap": {"enabled": true}})).is_err());
        assert!(normalize(&json!({"schema": 2})).is_err());
        assert!(normalize(&json!({"quantize": {"type": "q9_z"}})).is_err());
        assert_eq!(portable(&r).unwrap()["sources"]["B"]["path"], "");
        assert_eq!(parse_layer_set("0-3,7").unwrap().len(), 5);
        assert!(parse_layer_set("5-2").is_err());
        assert!(!validate_sources(&r).is_empty()); // verify is on and has no reference
    }
}
