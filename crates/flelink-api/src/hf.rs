//! Hugging Face Hub: a repo's files, the GGUF models in it, and a search. Port of `fle_studio/hf.py`. Public API; a token is
//! only needed for gated or private repos.
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

pub const HF: &str = "https://huggingface.co";

/// Python's `urllib.parse.quote(text, safe=...)`.
fn quote(text: &str, safe: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) || safe.as_bytes().contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub fn resolve_url(repo: &str, path: &str, revision: &str) -> String {
    format!(
        "{HF}/{repo}/resolve/{}/{}",
        quote(revision, ""),
        quote(path, "/")
    )
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("regex"))
}

static REPO_RE: OnceLock<Regex> = OnceLock::new();
static URL_RE: OnceLock<Regex> = OnceLock::new();
static PATH_RE: OnceLock<Regex> = OnceLock::new();
static QUANT_RE: OnceLock<Regex> = OnceLock::new();
static SPLIT_RE: OnceLock<Regex> = OnceLock::new();
static NEXT_RE: OnceLock<Regex> = OnceLock::new();

fn split_re() -> &'static Regex {
    re(
        &SPLIT_RE,
        r"(?i)^(?P<base>.+)-(?P<no>\d{5})-of-(?P<count>\d{5})\.gguf$",
    )
}

/// (repo, revision, subfolder or quant hint) from `owner/name`, a huggingface.co URL or `hf.co/owner/name:Q4_K_M`.
pub fn parse_repo(text: &str) -> Result<(String, String, String), String> {
    let t = text.trim().trim_matches('/');
    let t = re(&URL_RE, r"^(https?://)?(www\.)?(huggingface\.co|hf\.co)/")
        .replace(t, "")
        .to_string();
    let (mut t, mut hint) = (t.clone(), String::new());
    if let Some((a, b)) = t.split_once(':') {
        if a.contains('/') {
            hint = b.to_string();
            t = a.to_string();
        }
    }
    let m = re(
        &PATH_RE,
        r"^([^/]+/[^/]+)(?:/(?:tree|blob|resolve)/([^/]+)(?:/(.*))?)?$",
    )
    .captures(&t);
    let bad = || "Enter a Hugging Face repo like owner/name or its URL".to_string();
    let Some(m) = m else { return Err(bad()) };
    let repo = m.get(1).map(|x| x.as_str()).unwrap_or("");
    if !re(&REPO_RE, r"^[A-Za-z0-9][\w.-]*/[\w.-]+$").is_match(repo) {
        return Err(bad());
    }
    let revision = m.get(2).map(|x| x.as_str()).unwrap_or("main").to_string();
    if hint.is_empty() {
        hint = m.get(3).map(|x| x.as_str()).unwrap_or("").to_string();
    }
    Ok((repo.to_string(), revision, hint))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(20))
        .timeout_read(std::time::Duration::from_secs(30))
        .user_agent(&format!("flelink/{}", env!("CARGO_PKG_VERSION")))
        .build()
}

/// GET a JSON document; returns it with the `Link` header (pagination).
fn get_json(url: &str, token: &str) -> Result<(Value, String), String> {
    let mut req = agent().get(url).set("Accept", "application/json");
    if !token.is_empty() {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    match req.call() {
        Ok(r) => {
            let link = r.header("Link").unwrap_or("").to_string();
            let body = r.into_string().map_err(|e| e.to_string())?;
            serde_json::from_str(&body)
                .map(|v| (v, link))
                .map_err(|e| e.to_string())
        }
        Err(ureq::Error::Status(c, _)) => Err(match c {
            401 => "Unauthorized (gated or private: add a Hugging Face token in Settings)".into(),
            403 => "Forbidden".into(),
            404 => "Not Found".into(),
            429 => "Too Many Requests".into(),
            c => format!("HTTP {c}"),
        }),
        Err(e) => Err(e.to_string()),
    }
}

/// All files of a repo (recursive), following the API's pagination.
pub fn tree(repo: &str, revision: &str, token: &str) -> Result<Vec<Value>, String> {
    let mut url = format!(
        "{HF}/api/models/{repo}/tree/{}?recursive=true&expand=false",
        quote(revision, "")
    );
    let mut files = vec![];
    for _ in 0..50 {
        let (data, link) = get_json(&url, token)?;
        files.extend(
            data.as_array()
                .into_iter()
                .flatten()
                .filter(|f| f["type"] == "file")
                .cloned(),
        );
        match re(&NEXT_RE, r#"<([^>]+)>;\s*rel="next""#).captures(&link) {
            Some(m) => url = m[1].to_string(),
            None => break,
        }
    }
    Ok(files)
}

pub fn quant_label(filename: &str) -> String {
    let stem = split_re().replace(filename, "${base}").to_string();
    let stem = Regex::new(r"(?i)\.gguf$")
        .unwrap()
        .replace(&stem, "")
        .to_string();
    // the regex crate has no lookahead: match the separator after the label as well, then keep group 1
    let q = re(
        &QUANT_RE,
        r"(?i)(?:^|[-_.])((?:UD-)?(?:I?Q\d(?:_[A-Z0-9]+)*|BF16|F16|F32|MXFP4(?:_MOE)?|TQ\d_\d)(?:-REAP\d+)?)(?:$|[-_.])",
    );
    let mut last = None;
    let mut at = 0;
    while let Some(c) = q.captures_at(&stem, at) {
        let g = c.get(1).unwrap();
        last = Some(g.as_str().to_uppercase());
        at = g.end(); // the separator after it may start the next label
        if at >= stem.len() {
            break;
        }
    }
    last.unwrap_or(stem)
}

/// The downloadable GGUF units of a repo: split parts belong together; projectors and MTP heads are marked.
pub fn gguf_groups(files: &[Value], hint: &str) -> Vec<Value> {
    let mut order: Vec<String> = vec![];
    let mut groups: Map<String, Value> = Map::new();
    for f in files {
        let path = f["path"].as_str().unwrap_or("");
        let name = path.rsplit('/').next().unwrap_or(path);
        if !name.to_lowercase().ends_with(".gguf") {
            continue;
        }
        let folder = path.rsplit_once('/').map(|(a, _)| a).unwrap_or("");
        let key = match split_re().captures(name) {
            Some(m) => format!("{folder}/{}", &m["base"]),
            None => path.to_string(),
        };
        let low = name.to_lowercase();
        let kind = if low.contains("mmproj") {
            "mmproj"
        } else if low.starts_with("mtp-") {
            "draft"
        } else {
            "model"
        };
        if !groups.contains_key(&key) {
            order.push(key.clone());
            groups.insert(key.clone(), json!({"key": key, "name": quant_label(name), "kind": kind, "files": [], "size": 0}));
        }
        let lfs = &f["lfs"];
        let size = f["size"]
            .as_i64()
            .or_else(|| lfs["size"].as_i64())
            .unwrap_or(0);
        let g = groups.get_mut(&key).unwrap();
        g["files"].as_array_mut().unwrap().push(json!({"path": path, "size": size, "sha256": lfs.get("oid").cloned().unwrap_or(Value::Null)}));
        g["size"] = json!(g["size"].as_i64().unwrap_or(0) + f["size"].as_i64().unwrap_or(0));
    }
    let mut out: Vec<Value> = order
        .into_iter()
        .filter_map(|k| groups.remove(&k))
        .collect();
    out.sort_by_key(|g| (g["kind"] != "model", g["size"].as_i64().unwrap_or(0)));
    for g in out.iter_mut() {
        g["files"]
            .as_array_mut()
            .unwrap()
            .sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        g["first"] = g["files"][0]["path"].clone();
        g["parts"] = json!(g["files"].as_array().unwrap().len());
    }
    if !hint.is_empty() {
        let h = hint.to_lowercase();
        out.sort_by_key(|g| {
            let hit = g["key"].as_str().unwrap_or("").to_lowercase().contains(&h)
                || g["name"].as_str().unwrap_or("").to_lowercase().contains(&h);
            (!hit, g["kind"] != "model")
        });
    }
    out
}

pub fn repo_info(repo: &str, token: &str) -> Result<Value, String> {
    let (d, _) = get_json(&format!("{HF}/api/models/{repo}"), token)?;
    let card = &d["cardData"];
    let license = card["license"]
        .as_str()
        .map(String::from)
        .or_else(|| {
            d["tags"].as_array().into_iter().flatten().find_map(|t| {
                t.as_str()
                    .and_then(|s| s.strip_prefix("license:"))
                    .map(String::from)
            })
        })
        .unwrap_or_default();
    Ok(json!({
        "id": d.get("id").cloned().unwrap_or(json!(repo)),
        "gated": crate::settings::py_truthy(&d["gated"]),
        "private": crate::settings::py_truthy(&d["private"]),
        "downloads": d["downloads"], "likes": d["likes"], "license": license, "updated": d["lastModified"],
    }))
}

pub fn search(query: &str, limit: usize, token: &str) -> Result<Vec<Value>, String> {
    let params: String = form_urlencoded::Serializer::new(String::new())
        .append_pair("search", query)
        .append_pair("filter", "gguf")
        .append_pair("sort", "downloads")
        .append_pair("direction", "-1")
        .append_pair("limit", &limit.to_string())
        .finish();
    let (data, _) = get_json(&format!("{HF}/api/models?{params}"), token)?;
    Ok(data
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| json!({"id": d["id"], "downloads": d["downloads"], "likes": d["likes"], "updated": d["lastModified"]}))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_forms() {
        let ok = |t: &str| parse_repo(t).unwrap();
        assert_eq!(
            ok("owner/name"),
            ("owner/name".into(), "main".into(), "".into())
        );
        assert_eq!(
            ok("https://huggingface.co/owner/name/tree/dev/sub"),
            ("owner/name".into(), "dev".into(), "sub".into())
        );
        assert_eq!(
            ok("hf.co/owner/name:Q4_K_M"),
            ("owner/name".into(), "main".into(), "Q4_K_M".into())
        );
        assert!(parse_repo("not a repo").is_err());
    }

    #[test]
    fn labels_and_groups() {
        assert_eq!(
            quant_label("Model-UD-IQ2_XXS-00001-of-00003.gguf"),
            "UD-IQ2_XXS"
        );
        assert_eq!(quant_label("Llama-3.2-1B-Instruct-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(quant_label("mmproj-model-f16.gguf"), "F16");
        let files = vec![
            json!({"path": "Q2/m-Q2_K-00002-of-00002.gguf", "size": 5, "lfs": {"oid": "b", "size": 5}}),
            json!({"path": "Q2/m-Q2_K-00001-of-00002.gguf", "size": 4, "lfs": {"oid": "a", "size": 4}}),
            json!({"path": "m-Q8_0.gguf", "size": 20}),
            json!({"path": "mmproj-m-f16.gguf", "size": 1}),
            json!({"path": "README.md", "size": 1}),
        ];
        let g = gguf_groups(&files, "");
        assert_eq!(g.len(), 3);
        assert_eq!(g[0]["name"], "Q2_K");
        assert_eq!(g[0]["parts"], 2);
        assert_eq!(g[0]["size"], 9);
        assert_eq!(g[0]["first"], "Q2/m-Q2_K-00001-of-00002.gguf");
        assert_eq!(g[2]["kind"], "mmproj");
        assert_eq!(gguf_groups(&files, "q8")[0]["name"], "Q8_0");
    }
}
