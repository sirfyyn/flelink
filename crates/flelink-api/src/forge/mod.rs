//! Forge: build your own quant with the FLE recipe, from inside FLE Link. Port of the `fle_studio_forge` plugin.
//!
//! ggufio   reading and writing GGUF files
//! recipe   the recipe JSON (schema 1)
//! tools    finding llama-imatrix, llama-quantize, llama-perplexity, llama-fle-reap
//! plan     the recipe calculator and the memory preview
//! build    writing a model from pieces of other models (hybrid build), resumable
//! reap     expert pruning
//! verify   KL divergence results
//! corpus   calibration text
//! jobs     background jobs with steps (steps.py lives here too), log, cancel and resume
//! this     the Forge object the /api/forge routes call
pub mod build;
pub mod corpus;
pub mod ggufio;
pub mod jobs;
pub mod plan;
pub mod reap;
pub mod recipe;
pub mod tools;
pub mod usage;
pub mod verify;

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use jobs::ForgeJobs;

/// An error the route turns into a status and a message.
pub struct ForgeError(pub u16, pub String);

impl From<String> for ForgeError {
    fn from(m: String) -> Self {
        ForgeError(400, m)
    }
}

type R<T> = Result<T, ForgeError>;

fn bad<T>(m: impl Into<String>) -> R<T> {
    Err(ForgeError(400, m.into()))
}

pub struct Forge {
    data_dir: PathBuf,
    found: Mutex<(Option<Instant>, HashMap<String, tools::Tool>)>,
    pub jobs: Arc<ForgeJobs>,
}

fn num(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn file(path: &Value) -> R<String> {
    let text = path.as_str().unwrap_or("").trim().trim_matches('"');
    let p = ggufio::path_buf(text);
    if !p.is_file() {
        return bad(format!("No file at {}", p.display()));
    }
    Ok(p.to_string_lossy().to_string())
}

impl Forge {
    pub fn new(data_dir: &Path) -> Forge {
        Forge {
            data_dir: data_dir.to_path_buf(),
            found: Mutex::new((None, HashMap::new())),
            jobs: Arc::new(ForgeJobs::new(data_dir)),
        }
    }

    /// The tools, probed at most every 10 seconds unless `refresh`. `engine_paths` are (label, llama-server path).
    pub fn tools(
        &self,
        refresh: bool,
        tools_dir: &str,
        engine_paths: &[(String, String)],
    ) -> HashMap<String, tools::Tool> {
        let mut f = self.found.lock().unwrap();
        if refresh || f.0.is_none_or(|t| t.elapsed().as_secs() > 10) || f.1.is_empty() {
            *f = (
                Some(Instant::now()),
                tools::find(&tools::search_dirs(tools_dir, engine_paths)),
            );
        }
        f.1.clone()
    }

    pub fn overview(
        &self,
        found: &HashMap<String, tools::Tool>,
        tools_dir: &str,
        models_dir: &Path,
    ) -> Value {
        let list: Vec<Value> = tools::TOOL_NAMES
            .iter()
            .map(|n| match found.get(*n) {
                Some(t) => t.public(),
                None => json!({"name": n, "ok": false, "path": "", "source": "", "version": "", "features": {}, "problem": "not found"}),
            })
            .collect();
        json!({"tools": list, "tools_dir": tools_dir, "jobs": self.jobs.list(),
            "defaults": recipe::normalize(&json!({})).unwrap_or(Value::Null),
            "types": ggufio::quantize_targets(), "ftypes": ggufio::quantize_ftypes(), "scores": reap::SCORES,
            "output_dir": models_dir.join("forge").to_string_lossy(), "references": verify::reference_cards()})
    }

    pub fn inspect(&self, path: &str) -> R<Value> {
        let f = file(&json!(path))?;
        Ok(plan::Source::load("S", &f)?.describe())
    }

    /// The calculator: the layer choice for a budget (hybrid), or the size estimate for a quantisation.
    pub fn calculate(&self, body: &Value) -> R<Value> {
        let mode = body["mode"]
            .as_str()
            .filter(|m| !m.is_empty())
            .unwrap_or("hybrid");
        let gb = plan::GB as f64;
        let ram = (num(&body["ram_gb"]) * gb) as u64;
        let vram = (num(&body["vram_gb"]) * gb) as u64;
        let context = match num(&body["context"]) as u64 {
            0 => 32768,
            c => c,
        };
        if mode == "quantize" {
            let q = recipe::normalize(&json!({"quantize": if body["quantize"].is_object() { body["quantize"].clone() } else { json!({}) }})).map_err(|e| e.0)?["quantize"].clone();
            let model =
                ggufio::open_model(Path::new(&file(&body["source_path"])?)).map_err(|e| e.0)?;
            let est = plan::estimate_quantize(
                &model,
                q["type"].as_str().unwrap_or(""),
                &q["rules"],
                q["output_type"].as_str().unwrap_or(""),
                q["embedding_type"].as_str().unwrap_or(""),
            );
            return Ok(
                json!({"mode": mode, "estimate": est, "model": plan::Source::from_model("S", model).describe()}),
            );
        }
        let mut sources: BTreeMap<String, plan::Source> = BTreeMap::new();
        for (sid, p) in body["sources"].as_object().into_iter().flatten() {
            sources.insert(sid.clone(), plan::Source::load(sid, &file(p)?)?);
        }
        let base = body["base"].as_str().unwrap_or("").to_string();
        let Some(base_src) = sources.get(&base) else {
            return bad("Choose which of the files is the base");
        };
        let base_model = base_src.model.clone();
        let infos: Vec<ggufio::Info> = base_model
            .order
            .iter()
            .map(|n| {
                let t = &base_model.tensors[n];
                (n.clone(), t.shape.clone(), t.ttype, t.nbytes)
            })
            .collect();
        let header_bytes = ggufio::build_header(&base_model.kv, &infos, base_model.alignment())
            .0
            .len() as u64;
        let budget = (num(&body["budget_gb"]) * gb) as u64;
        let given = body["layers"].as_object().filter(|o| !o.is_empty());
        let (pick, sel): (BTreeMap<u64, String>, Option<Value>) = match given {
            Some(layers) => (
                layers
                    .iter()
                    .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_string())))
                    .collect(),
                None,
            ),
            None => {
                if budget == 0 {
                    return bad("Enter a size budget in GB");
                }
                let errors = (body["objective"] == "errors").then(|| &body["errors"]);
                let s = plan::choose_layers(&sources, &base, budget, errors, header_bytes)
                    .map_err(|e| e.0)?;
                (s.layers.clone(), Some(s.public()))
            }
        };
        let models: HashMap<String, ggufio::Model> = sources
            .iter()
            .map(|(k, s)| (k.clone(), s.model.clone()))
            .collect();
        let rules = plan::parse_rules(if body["rules"].is_array() {
            &body["rules"]
        } else {
            &Value::Null
        })
        .map_err(|e| e.0)?;
        let assigned =
            plan::assemble(&base_model, &models, &pick, &base, &rules).map_err(|e| e.0)?;
        let size = plan::predicted_file_size(&base_model, &models, &assigned);
        let mut out = json!({"mode": mode, "selection": sel,
            "layers": pick.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
            "file_bytes": size, "sources": sources.values().map(|s| s.describe()).collect::<Vec<_>>(),
            "preview": plan::memory_preview(&base_model, &models, &assigned, ram, vram, context)});
        let keep = num(&body["reap_keep"]) as u64;
        if keep > 0 {
            out["pruned_bytes"] = json!(reap::predicted_prune_size(&base_model, keep));
        }
        Ok(out)
    }

    pub fn make_corpus(&self, body: &Value) -> R<Value> {
        let dir = self.data_dir.join("forge").join("corpus");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let out = dir.join(format!("corpus-{stamp}.txt"));
        let chars = match num(&body["chars"]) as usize {
            0 => 1_000_000,
            c => c,
        };
        if body["bundled"].as_bool() == Some(true)
            || body["bundled"].as_i64().is_some_and(|x| x != 0)
        {
            let part = if body["part"] == "evaluation" {
                "evaluation"
            } else {
                "calibration"
            };
            return Ok(corpus::bundled(
                &dir.join(format!("bundled-{part}.txt")),
                part,
                1,
                4000,
            )?);
        }
        if body["stdlib"] == true {
            return bad("FLE Link carries no Python library to sample; use the bundled text or your own files");
        }
        let inputs: Vec<String> = body["inputs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(crate::settings::py_str)
            .collect();
        if inputs.is_empty() {
            return bad("Name at least one text file or folder");
        }
        Ok(corpus::build(&inputs, &out, chars, 1, 4000)?)
    }

    pub fn import_recipe(&self, text: &str) -> R<Value> {
        let r = recipe::loads(text).map_err(|e| e.0)?;
        let needs: Vec<Value> = r["sources"].as_object().into_iter().flatten()
            .map(|(sid, s)| json!({"id": sid, "file": s["file"], "role": s["role"], "size": s["size"], "origin": s["origin"]}))
            .collect();
        Ok(json!({"recipe": r, "needs": needs}))
    }

    pub fn recipe_of(&self, id: &str) -> R<Value> {
        let job = self
            .jobs
            .get_job(id)
            .ok_or(ForgeError(404, "Not found".into()))?;
        let r = job.data.lock().unwrap()["recipe"].clone();
        Ok(recipe::portable(&r).map_err(|e| e.0)?)
    }

    /// The start arguments and notes of the profile for a finished job (the caller adds the model to the library and
    /// saves the profile).
    pub fn publish_plan(
        &self,
        id: &str,
        gpu: bool,
        unified: bool,
    ) -> R<(String, String, Vec<String>, String)> {
        let job = self
            .jobs
            .get_job(id)
            .ok_or(ForgeError(404, "Not found".into()))?;
        if job.state() != "done" {
            return Err(ForgeError(
                409,
                "Only a finished job can be published".into(),
            ));
        }
        let d = job.data.lock().unwrap();
        let final_ = d["artifacts"]["final"].as_str().unwrap_or("").to_string();
        let name = d["name"].as_str().unwrap_or("").to_string();
        let mut args: Vec<String> = ["-m", "{model}", "--jinja", "-c", "{context}"]
            .map(String::from)
            .to_vec();
        if gpu && !unified {
            args.extend(["-ngl", "99", "-cmoe"].map(String::from)); // dense weights on the GPU, routed experts in RAM
        } else if unified {
            args.extend(["-ngl", "99"].map(String::from));
        }
        let mut notes = format!("Built with Forge from recipe {name}.");
        let card = &d["result"]["card"];
        if let Some(k) = card["kld"].as_f64() {
            let reference = card["reference"]
                .as_str()
                .filter(|r| !r.is_empty())
                .unwrap_or("the reference");
            notes += &format!(" KL divergence {k:.4} against {reference}.");
        }
        Ok((final_, name, args, notes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_needs_a_budget_or_layers() {
        let dir = std::env::temp_dir().join(format!("forge-t-{}", std::process::id()));
        let f = Forge::new(&dir);
        let e = f
            .calculate(&json!({"mode": "hybrid", "sources": {}, "base": "A"}))
            .err()
            .unwrap();
        assert_eq!(e.1, "Choose which of the files is the base");
        let _ = std::fs::remove_dir_all(dir);
    }
}
