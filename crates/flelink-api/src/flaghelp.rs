//! One short sentence for every flag and environment variable in the engine command. Port of `fle_studio/flaghelp.py`;
//! the sentences are its table, word for word (`flaghelp.json`, exported from the Python Studio).
//!
//! `explain` turns a command into `{kind, name, value, text, known}` lines (shown under the command), `table` gives the
//! same sentences by name for the tooltips. A flag that is not in the table gets `known: false` and no guess.
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

struct Flag {
    canon: String,
    aliases: Vec<String>,
    takes: bool,
    text: String,
}

struct Table {
    flags: Vec<Flag>,
    env: Map<String, Value>,
    fle_env_generic: String,
}

fn table_data() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let v: Value = serde_json::from_str(include_str!("flaghelp.json")).expect("flaghelp.json");
        let s = |x: &Value| x.as_str().unwrap_or("").to_string();
        Table {
            flags: v["flags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| Flag {
                    canon: s(&f[0]),
                    aliases: f[1].as_array().unwrap().iter().map(s).collect(),
                    takes: f[2].as_bool().unwrap_or(true),
                    text: s(&f[3]),
                })
                .collect(),
            env: v["env"].as_object().cloned().unwrap_or_default(),
            fle_env_generic: s(&v["fle_env_generic"]),
        }
    })
}

fn find(name: &str) -> Option<&'static Flag> {
    table_data()
        .flags
        .iter()
        .find(|f| f.canon == name || f.aliases.iter().any(|a| a == name))
}

/// (canonical name, takes a value, sentence) for a flag, or None if it is not in the table.
pub fn lookup(name: &str) -> Option<(&'static str, bool, &'static str)> {
    find(name).map(|f| (f.canon.as_str(), f.takes, f.text.as_str()))
}

/// Every spelling of a flag, canonical name first; just the name for one that is not known.
pub fn names_of(name: &str) -> Vec<String> {
    match find(name) {
        Some(f) => std::iter::once(f.canon.clone())
            .chain(f.aliases.iter().cloned())
            .collect(),
        None => vec![name.to_string()],
    }
}

pub fn env_text(name: &str) -> Option<String> {
    let t = table_data();
    if let Some(v) = t.env.get(name) {
        return v.as_str().map(String::from);
    }
    if name.starts_with("FLE_") {
        return Some(t.fle_env_generic.clone());
    }
    if name.starts_with("LLAMA_ARG_") {
        return Some(
            "The same as the command line flag of that name (llama.cpp reads it from the environment).".into(),
        );
    }
    None
}

/// `GET /api/flags`: every known name (with its aliases) and sentence, for the tooltips.
pub fn table() -> Value {
    let t = table_data();
    let mut flags = Map::new();
    for f in &t.flags {
        for n in std::iter::once(&f.canon).chain(f.aliases.iter()) {
            flags.insert(
                n.clone(),
                json!({"text": f.text, "value": f.takes, "canonical": f.canon}),
            );
        }
    }
    json!({"flags": flags, "env": t.env, "fle_env_generic": t.fle_env_generic})
}

/// `-5`, `-0.5`: a negative number, not a flag.
fn is_number(t: &str) -> bool {
    t.strip_prefix('-')
        .and_then(|r| r.chars().next())
        .is_some_and(|c| c.is_ascii_digit())
}

/// The command's environment and arguments, one entry each.
pub fn explain(args: &[String], env: &[(String, String)]) -> Vec<Value> {
    let mut out = vec![];
    for (k, v) in env {
        let text = env_text(k);
        out.push(
            json!({"kind": "env", "name": k, "value": v, "text": text.clone().unwrap_or_default(),
            "known": text.is_some()}),
        );
    }
    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        i += 1;
        if !token.starts_with('-') || is_number(token) {
            continue;
        }
        let (name, eq, inline) = match token
            .starts_with("--")
            .then(|| token.split_once('='))
            .flatten()
        {
            Some((n, v)) => (n, true, v),
            None => (token.as_str(), false, ""),
        };
        let info = lookup(name);
        let takes = match info {
            Some((_, t, _)) => t,
            None => eq || (i < args.len() && !args[i].starts_with('-')),
        };
        let mut value = String::new();
        if eq {
            value = inline.to_string();
        } else if takes && i < args.len() && (!args[i].starts_with('-') || is_number(&args[i])) {
            value = args[i].clone();
            i += 1;
        }
        out.push(json!({"kind": "flag", "name": name, "value": value,
            "text": info.map_or("", |x| x.2), "known": info.is_some()}));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_explained_like_the_python_studio_does() {
        // fle_studio.flaghelp.explain(["-m","x.gguf","--threads=8","-ngl","99","--nope","1","-c","-5"],
        //                             {"FLE_MOE_CACHE_BUDGET_MIB": "1024", "CUDA_VISIBLE_DEVICES": "1"})
        let args: Vec<String> = [
            "-m",
            "x.gguf",
            "--threads=8",
            "-ngl",
            "99",
            "--nope",
            "1",
            "-c",
            "-5",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let env = vec![
            ("FLE_MOE_CACHE_BUDGET_MIB".to_string(), "1024".to_string()),
            ("CUDA_VISIBLE_DEVICES".to_string(), "1".to_string()),
        ];
        let want: Value = serde_json::from_str(PY_EXPLAIN).unwrap();
        assert_eq!(json!(explain(&args, &env)), want);
        assert_eq!(names_of("--threads")[0], "-t");
    }

    #[test]
    fn the_table_is_the_python_studios() {
        let want: Value = serde_json::from_str(PY_TABLE_KEYS).unwrap();
        let t = table();
        let got: Vec<&String> = t["flags"].as_object().unwrap().keys().collect();
        assert_eq!(json!(got), want);
    }

    const PY_EXPLAIN: &str = r#"[{"kind": "env", "name": "FLE_MOE_CACHE_BUDGET_MIB", "value": "1024", "text": "FLE engine: GPU memory in MiB for the expert cache, the experts it keeps ready on the GPU.", "known": true}, {"kind": "env", "name": "CUDA_VISIBLE_DEVICES", "value": "1", "text": "Which NVIDIA GPUs the engine may use, by number.", "known": true}, {"kind": "flag", "name": "-m", "value": "x.gguf", "text": "The model file to load. For a split model, the first part (…-00001-of-0000N.gguf).", "known": true}, {"kind": "flag", "name": "--threads", "value": "8", "text": "CPU threads for generating. Usually one per physical core; the FLE engine leaves a core free for its own helper threads on processors without hyper-threading.", "known": true}, {"kind": "flag", "name": "-ngl", "value": "99", "text": "How many of the model's layers run on the GPU. Without it the engine decides (see -fit).", "known": true}, {"kind": "flag", "name": "--nope", "value": "1", "text": "", "known": false}, {"kind": "flag", "name": "-c", "value": "-5", "text": "How many tokens the model can see at once (prompt plus answer), shared by all parallel requests. A larger value needs more memory.", "known": true}]"#;
    const PY_TABLE_KEYS: &str = r#"["-m", "--model", "-md", "--model-draft", "--spec-draft-model", "--spec-type", "--spec-draft-n-max", "--draft-max", "--draft", "--draft-n", "--spec-draft-p-min", "--draft-p-min", "--no-spec-draft-backend-sampling", "-cmoed", "--cpu-moe-draft", "-c", "--ctx-size", "--parallel", "-np", "--n-parallel", "--kv-unified", "-kvu", "--jinja", "-b", "--batch-size", "-ub", "--ubatch-size", "-ctk", "--cache-type-k", "-ctv", "--cache-type-v", "-fit", "--fit", "--fit-target", "-fitt", "--moe-expert-cache-inserts", "--cache-ram", "-cram", "--load-mode", "-t", "--threads", "-tb", "--threads-batch", "-C", "--cpu-mask", "--cpu-strict", "--temp", "--temperature", "--top-p", "--top_p", "--top-k", "--top_k", "--min-p", "--min_p", "--presence-penalty", "--host", "--port", "--metrics", "-a", "--alias", "-lv", "--verbosity", "--log-verbosity", "-ngl", "--n-gpu-layers", "--gpu-layers", "--n-cpu-moe", "-ncmoe", "--cpu-moe", "-cmoe", "-fa", "--flash-attn", "--no-mmap", "--mlock", "-sm", "--split-mode", "-ts", "--tensor-split", "--no-warmup", "--chat-template-kwargs", "--api-key"]"#;
}
