//! The speed test against the running engine (the Experiments page). Port of `fle_studio/bench.py`.
//!
//! Three requests per run: a short answer, a longer piece of code and a long prompt (about 6,000 tokens) that measures
//! prefill. Each run records prefill and decode speed, time to first token and MTP acceptance from the server's own
//! timings, with the launch profile and the engine, so runs with different settings can be compared. Results are files in
//! `<data>/bench/`, newest first.
use crate::engine::Engine;
use crate::stats;
use serde_json::{json, Map, Value};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct Test {
    pub name: &'static str,
    pub max_tokens: u32,
    pub prompt: fn() -> String,
}

fn long_prompt() -> String {
    let rows: Vec<String> = (1..241)
        .map(|i| {
            let level = (i * 37) % 389 + 120;
            let temp = 4 + (i * 13) % 17;
            format!(
                "Station {i:03} on the river records a water level of {level} centimetres, a water temperature of \
                 {temp} degrees and {} boats passing before noon.",
                i % 7
            )
        })
        .collect();
    format!(
        "Here is a log of measuring stations.\n\n{}\n\nWhich station recorded the highest water level? Answer with the \
         station number and the level.",
        rows.join("\n")
    )
}

pub const TESTS: [Test; 3] = [
    Test {
        name: "Short answer",
        max_tokens: 256,
        prompt: || {
            "Explain in a few sentences why the sky is blue, for a curious ten-year-old.".into()
        },
    },
    Test {
        name: "Code",
        max_tokens: 768,
        prompt: || {
            "Write a Python function that parses an ISO 8601 duration like P3DT4H12M into seconds, with a docstring \
             and three doctest examples. Then explain the edge cases."
                .into()
        },
    },
    Test {
        name: "Long prompt",
        max_tokens: 128,
        prompt: long_prompt,
    },
];
const GEN_TESTS: [&str; 2] = ["Short answer", "Code"];
const PREFILL_TESTS: [&str; 1] = ["Long prompt"];
/// a decode speed this far below the last run of the same setting is flagged
const REGRESSION_WARN_PCT: f64 = 10.0;
const SERIES_LENGTH: usize = 12;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The command without the port (a free one is picked at every start) and without line continuations.
fn normalize_command(c: &str) -> String {
    let t = regex::Regex::new(r"\\\r?\n").unwrap().replace_all(c, " ");
    let t = regex::Regex::new(r"--port(?:=|\s+)\S+")
        .unwrap()
        .replace_all(&t, "");
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Python's `str(x or "")`.
fn s(v: &Value) -> String {
    if crate::settings::py_truthy(v) {
        crate::settings::py_str(v)
    } else {
        String::new()
    }
}

/// What makes two speed tests "the same setting": the model, the context and the exact command (profile, engine and
/// build when the command is not known, for a server that was already running).
pub fn setting_key(ctx: &Value) -> String {
    let command = normalize_command(&s(&ctx["command"]));
    let mut parts = vec![s(&ctx["model"]), s(&ctx["n_ctx"])];
    if command.is_empty() {
        parts.extend([s(&ctx["profile"]), s(&ctx["engine"]), s(&ctx["build"])]);
    } else {
        parts.push(command);
    }
    parts.join("\n")
}

/// Decode speeds per request name, only from requests that ended by themselves and carry `truncated`.
fn comparable_decode(tests: &[Value]) -> Vec<(String, Vec<f64>)> {
    let mut out: Vec<(String, Vec<f64>)> = vec![];
    for t in tests {
        let name = t["name"].as_str().unwrap_or("");
        let Some(d) = t["decode"].as_f64().filter(|d| *d != 0.0) else {
            continue;
        };
        if GEN_TESTS.contains(&name) && t.get("truncated") == Some(&json!(false)) {
            match out.iter_mut().find(|(n, _)| n == name) {
                Some((_, v)) => v.push(d),
                None => out.push((name.to_string(), vec![d])),
            }
        }
    }
    out
}

fn tests_of(v: &Value) -> Vec<Value> {
    v["tests"].as_array().cloned().unwrap_or_default()
}

/// How the decode speed of `new` differs from `old`, a result of the same setting.
pub fn compare_decode(new: &Value, old: &Value) -> Value {
    let (a, b) = (
        comparable_decode(&tests_of(new)),
        comparable_decode(&tests_of(old)),
    );
    let get =
        |m: &[(String, Vec<f64>)], n: &str| m.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    let names: Vec<&str> = GEN_TESTS
        .iter()
        .copied()
        .filter(|n| get(&a, n).is_some() && get(&b, n).is_some())
        .collect();
    let mut out = Map::new();
    let reference = |o: &mut Map<String, Value>| {
        o.insert(
            "previous".into(),
            old.get("id").cloned().unwrap_or(Value::Null),
        );
        o.insert(
            "previous_started".into(),
            old.get("started").cloned().unwrap_or(Value::Null),
        );
        o.insert("previous_label".into(), json!(s(&old["label"])));
    };
    if names.is_empty() {
        out.insert("comparable".into(), json!(false));
        out.insert(
            "reason".into(),
            json!("No request finished by itself in both runs (cut off at the token limit)"),
        );
        reference(&mut out);
        return Value::Object(out);
    }
    let all = |m: &[(String, Vec<f64>)]| -> Vec<f64> {
        names.iter().flat_map(|n| get(m, n).unwrap()).collect()
    };
    let (new_med, old_med) = (stats::median(&all(&a)), stats::median(&all(&b)));
    if old_med <= 0.0 {
        out.insert("comparable".into(), json!(false));
        out.insert(
            "reason".into(),
            json!("The earlier run has no decode speed"),
        );
        reference(&mut out);
        return Value::Object(out);
    }
    let change = (new_med - old_med) / old_med * 100.0;
    out.insert("comparable".into(), json!(true));
    out.insert("tests".into(), json!(names));
    out.insert("new".into(), json!(stats::round_to(new_med, 2)));
    out.insert("old".into(), json!(stats::round_to(old_med, 2)));
    out.insert("change_pct".into(), json!(stats::round_to(change, 1)));
    out.insert("warn".into(), json!(change < -REGRESSION_WARN_PCT));
    reference(&mut out);
    Value::Object(out)
}

/// The decode speed of the last runs of the same setting as `record`, oldest first, ending with `record`.
pub fn decode_series(record: &Value, history: &[Value]) -> Vec<Value> {
    let key = setting_key(&record["context"]);
    let mut runs = vec![record.clone()];
    for old in history {
        if runs.len() >= SERIES_LENGTH {
            break;
        }
        if old.get("id") == record.get("id") || old["state"] != "done" || tests_of(old).is_empty() {
            continue;
        }
        if setting_key(&old["context"]) == key {
            runs.push(old.clone());
        }
    }
    runs.iter()
        .rev()
        .map(|run| {
            let values: Vec<f64> = comparable_decode(&tests_of(run))
                .into_iter()
                .flat_map(|(_, v)| v)
                .collect();
            json!({"id": run.get("id").cloned().unwrap_or(Value::Null),
                   "started": run.get("started").cloned().unwrap_or(Value::Null),
                   "decode": if values.is_empty() { Value::Null } else { json!(stats::round_to(stats::median(&values), 2)) }})
        })
        .collect()
}

/// Compare `record` with the newest earlier result of the same setting; None when there is none.
pub fn compare_with_history(record: &Value, history: &[Value]) -> Option<Value> {
    let key = setting_key(&record["context"]);
    for old in history {
        if old.get("id") == record.get("id") || old["state"] != "done" || tests_of(old).is_empty() {
            continue;
        }
        if setting_key(&old["context"]) == key {
            let mut c = compare_decode(record, old);
            c["series"] = json!(decode_series(record, history));
            return Some(c);
        }
    }
    None
}

/// One value per run: the median of `key` over that run's requests named in `names`.
fn per_run(tests: &[Value], key: &str, names: &[&str]) -> Vec<f64> {
    let mut by_run: std::collections::BTreeMap<i64, Vec<f64>> = Default::default();
    for t in tests {
        if let Some(v) = t[key].as_f64() {
            if names.contains(&t["name"].as_str().unwrap_or("")) {
                by_run
                    .entry(t["run"].as_i64().unwrap_or(0))
                    .or_default()
                    .push(v);
            }
        }
    }
    by_run
        .values()
        .map(|v| stats::round_to(stats::median(v), 4))
        .collect()
}

fn summary(tests: &[Value]) -> Value {
    let med = |key: &str, names: &[&str]| -> Value {
        let vals: Vec<f64> = tests
            .iter()
            .filter(|t| names.contains(&t["name"].as_str().unwrap_or("")))
            .filter_map(|t| t[key].as_f64())
            .collect();
        if vals.is_empty() {
            Value::Null
        } else {
            json!(stats::round_to(stats::median(&vals), 2))
        }
    };
    let mut out = json!({"decode": med("decode", &GEN_TESTS), "prefill": med("prefill", &PREFILL_TESTS),
           "ttft_ms": med("ttft_ms", &GEN_TESTS), "accept": med("accept", &GEN_TESTS), "n": tests.len()});
    for (k, v) in run_stats(tests).as_object().unwrap() {
        out[k] = v.clone();
    }
    out
}

/// Why a measurement ended early: Cancel was pressed, or the engine failed (the message says how).
#[derive(Debug)]
pub enum Stop {
    Cancelled,
    Failed(String),
}

/// One decode and one prefill speed per run, with the median and 95 % interval (`run_stats` in bench.py).
pub fn run_stats(tests: &[Value]) -> Value {
    let decode = per_run(tests, "decode", &GEN_TESTS);
    let prefill = per_run(tests, "prefill", &PREFILL_TESTS);
    json!({"decode_runs": decode, "prefill_runs": prefill,
           "decode_ci": stats::summarize(&decode), "prefill_ci": stats::summarize(&prefill)})
}

/// What the tuner and the A/B/A comparison run per measurement: no 768-token code answer, which takes the most time.
pub fn quick_tests() -> Vec<&'static Test> {
    TESTS.iter().filter(|t| t.name != "Code").collect()
}

/// Run `tests` `runs` times against the engine at `base`, one after the other (blocking). Used by the speed test and
/// by the tuner and the A/B/A comparison, which restart the engine between their measurements.
#[allow(clippy::too_many_arguments)]
pub fn measure(
    base: &str,
    key: &str,
    runs: usize,
    tests: &[&Test],
    cancel: &AtomicBool,
    run_offset: usize,
    on_step: &mut dyn FnMut(usize, &Test),
    on_result: &mut dyn FnMut(&Value),
) -> Result<Vec<Value>, Stop> {
    let mut out = vec![];
    for run in 0..runs {
        for (i, test) in tests.iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                return Err(Stop::Cancelled);
            }
            on_step(run, test);
            let mut res = one(
                base,
                key,
                test,
                (1000 * (run + run_offset) + i) as u64,
                cancel,
            )?;
            res["run"] = json!(run + run_offset + 1);
            res["name"] = json!(test.name);
            on_result(&res);
            out.push(res);
        }
    }
    Ok(out)
}

#[derive(Clone)]
pub struct Bench {
    engine: Engine,
    dir: PathBuf,
    current: Arc<Mutex<Option<Value>>>,
    cancel: Arc<AtomicBool>,
}

impl Bench {
    pub fn new(engine: Engine, data_dir: &Path) -> Bench {
        let dir = data_dir.join("bench");
        let _ = std::fs::create_dir_all(&dir);
        Bench {
            engine,
            dir,
            current: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn test_names() -> Vec<&'static str> {
        TESTS.iter().map(|t| t.name).collect()
    }

    /// The stored results, newest first (at most 200).
    pub fn results(&self) -> Vec<Value> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map(|r| {
                r.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "json"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files.reverse();
        files
            .iter()
            .take(200)
            .filter_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
            .collect()
    }

    pub fn delete(&self, id: &str) {
        if let Ok(r) = std::fs::read_dir(&self.dir) {
            for e in r.flatten() {
                if e.file_name()
                    .to_string_lossy()
                    .ends_with(&format!("-{id}.json"))
                {
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

    /// Start the speed test in a thread; Err is the message of a 409. `probe`: (question set, data folder) of the knowledge
    /// probe that runs after the speed requests and is stored in the same record.
    pub fn start(
        &self,
        runs: i64,
        label: &str,
        context: Value,
        probe: Option<(String, PathBuf)>,
    ) -> Result<Value, String> {
        let mut cur = self.current.lock().unwrap();
        if cur.as_ref().is_some_and(|c| c["state"] == "running") {
            return Err("A speed test is already running".into());
        }
        let (state, base, key) = self.engine.target();
        if !matches!(state.as_str(), "ready" | "attached") || base.is_empty() {
            return Err("Load a model first".into());
        }
        let runs = runs.clamp(1, 10);
        self.cancel.store(false, Ordering::SeqCst);
        let mut id = [0u8; 4];
        getrandom::getrandom(&mut id).expect("random bytes");
        let label: String = label.trim().chars().take(80).collect();
        let rec = json!({"id": hex::encode(id), "state": "running", "label": label, "started": now(), "runs": runs,
            "done": 0, "total": runs as usize * TESTS.len(), "step": "", "tests": [], "context": context});
        *cur = Some(rec.clone());
        let me = self.clone();
        std::thread::Builder::new()
            .name("bench".into())
            .spawn(move || me.run(runs as usize, &base, &key, probe))
            .map_err(|e| e.to_string())?;
        Ok(rec)
    }

    fn set(&self, f: impl FnOnce(&mut Value)) {
        if let Some(c) = self.current.lock().unwrap().as_mut() {
            f(c);
        }
    }

    fn run(&self, runs: usize, base: &str, key: &str, probe: Option<(String, PathBuf)>) {
        let result = (|| -> Result<(), Stop> {
            let all: Vec<&Test> = TESTS.iter().collect();
            measure(
                base,
                key,
                runs,
                &all,
                &self.cancel,
                0,
                &mut |run, test| {
                    let step = format!("Run {}/{runs}: {}", run + 1, test.name);
                    self.set(|c| c["step"] = json!(step));
                },
                &mut |res| {
                    self.set(|c| {
                        c["tests"].as_array_mut().unwrap().push(res.clone());
                        c["done"] = json!(c["done"].as_i64().unwrap_or(0) + 1);
                    })
                },
            )?;
            if let Some((source, data_dir)) = &probe {
                if self.cancel.load(Ordering::SeqCst) {
                    return Err(Stop::Cancelled);
                }
                let say = |t: &str| -> Result<(), crate::quality::Stop> {
                    if self.cancel.load(Ordering::SeqCst) {
                        return Err(crate::quality::Stop::Cancelled);
                    }
                    self.set(|c| c["step"] = json!(t));
                    Ok(())
                };
                say("Knowledge probe").map_err(|_| Stop::Cancelled)?;
                let q = crate::quality::run_probe(base, key, &say, source, data_dir, "", 0)
                    .map_err(|e| match e {
                        crate::quality::Stop::Cancelled => Stop::Cancelled,
                        crate::quality::Stop::Failed(m) => Stop::Failed(m),
                    })?;
                self.set(|c| c["quality"] = q);
            }
            // summary and comparison first, then "done": the page reads them as soon as it sees the state change
            let (id, context, tests) = {
                let c = self.status();
                (c["id"].clone(), c["context"].clone(), tests_of(&c))
            };
            let sum = summary(&tests);
            let compare = if tests.is_empty() {
                None
            } else {
                compare_with_history(
                    &json!({"id": id, "context": context, "tests": tests}),
                    &self.results(),
                )
            };
            self.set(|c| {
                c["summary"] = sum;
                if let Some(cmp) = compare {
                    c["compare"] = cmp;
                }
                c["state"] = json!("done");
            });
            Ok(())
        })();
        match result {
            Ok(()) => {}
            Err(Stop::Cancelled) => self.set(|c| c["state"] = json!("cancelled")),
            Err(Stop::Failed(e)) => self.set(|c| {
                c["state"] = json!("failed");
                c["error"] = json!(e);
            }),
        }
        let mut record = Value::Null;
        self.set(|c| {
            c["finished"] = json!(now());
            c["summary"] = summary(&tests_of(c));
            c["step"] = json!("");
            record = c.clone();
        });
        if !tests_of(&record).is_empty() {
            let started = record["started"].as_f64().unwrap_or(0.0);
            let t = chrono::DateTime::from_timestamp(started as i64, 0)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%Y%m%d-%H%M%S")
                        .to_string()
                })
                .unwrap_or_default();
            let name = format!("{t}-{}.json", s(&record["id"]));
            let _ = std::fs::write(
                self.dir.join(name),
                serde_json::to_string_pretty(&record).unwrap_or_default(),
            );
        }
    }
}

/// One streamed request; the speeds come from the server's own timings.
fn one(base: &str, key: &str, test: &Test, seed: u64, cancel: &AtomicBool) -> Result<Value, Stop> {
    {
        let body = json!({"messages": [{"role": "user", "content": (test.prompt)()}], "max_tokens": test.max_tokens,
            "temperature": 0, "stream": true, "seed": seed, "cache_prompt": false, "timings_per_token": false});
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(600))
            .build();
        let mut req = agent
            .post(&format!("{base}/v1/chat/completions"))
            .set("Content-Type", "application/json");
        if !key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let t0 = Instant::now();
        let resp = req
            .send_string(&body.to_string())
            .map_err(|e| Stop::Failed(e.to_string()))?;
        let mut first: Option<Instant> = None;
        let mut timings = json!({});
        let mut finish_reason = String::new();
        for line in std::io::BufReader::new(resp.into_reader()).lines() {
            if cancel.load(Ordering::SeqCst) {
                return Err(Stop::Cancelled);
            }
            let line = line.map_err(|e| Stop::Failed(e.to_string()))?;
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let choice = &chunk["choices"][0];
            let delta = &choice["delta"];
            let has_text = |k: &str| delta[k].as_str().is_some_and(|t| !t.is_empty());
            if first.is_none() && (has_text("content") || has_text("reasoning_content")) {
                first = Some(Instant::now());
            }
            if let Some(f) = choice["finish_reason"].as_str().filter(|f| !f.is_empty()) {
                finish_reason = f.to_string();
            }
            if chunk["timings"].as_object().is_some_and(|t| !t.is_empty()) {
                timings = chunk["timings"].clone();
            }
        }
        let wall = t0.elapsed().as_secs_f64();
        let draft_n = timings["draft_n"].as_f64().unwrap_or(0.0);
        let accept = if draft_n > 0.0 {
            json!(stats::round_to(
                timings["draft_n_accepted"].as_f64().unwrap_or(0.0) / draft_n,
                4
            ))
        } else {
            Value::Null
        };
        let t = |k: &str| timings.get(k).cloned().unwrap_or(Value::Null);
        Ok(
            json!({"prompt_n": t("prompt_n"), "prefill": t("prompt_per_second"), "generated": t("predicted_n"),
            "decode": t("predicted_per_second"),
            "ttft_ms": first.map(|f| (f - t0).as_millis() as i64),
            "accept": accept, "wall_s": stats::round_to(wall, 2), "finish_reason": finish_reason,
            "truncated": finish_reason == "length"}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, decode: &[f64], cmd: &str) -> Value {
        let tests: Vec<Value> = decode
            .iter()
            .enumerate()
            .map(|(i, d)| json!({"name": "Short answer", "run": i + 1, "decode": d, "truncated": false}))
            .collect();
        json!({"id": id, "state": "done", "context": {"model": "m", "n_ctx": 8192, "command": cmd}, "tests": tests})
    }

    #[test]
    fn the_port_does_not_split_settings() {
        assert_eq!(
            setting_key(&json!({"model": "m", "command": "llama-server --port 8080 -c 4096"})),
            setting_key(&json!({"model": "m", "command": "llama-server \\\n --port=9000 -c 4096"}))
        );
    }

    #[test]
    fn a_slower_run_of_the_same_setting_is_flagged() {
        let old = rec("a", &[20.0, 20.0], "x -c 1");
        let new = rec("b", &[17.0, 17.0], "x -c 1");
        let c = compare_with_history(&new, std::slice::from_ref(&old)).unwrap();
        assert_eq!(c["comparable"], true);
        assert_eq!(c["change_pct"], -15.0);
        assert_eq!(c["warn"], true);
        assert_eq!(c["series"].as_array().unwrap().len(), 2);
        assert!(compare_with_history(&rec("c", &[1.0], "other"), &[old]).is_none());
    }

    #[test]
    fn the_prompts_are_the_python_studios() {
        use sha2::Digest;
        // sha256 of fle_studio.bench.TESTS[i]["prompt"] (Python Studio 055f4aa)
        let want = [
            "060d7afe21cdeace05475c2ee9280b56a482467a650fc4a22c5daa17ce6326b3",
            "8c8b4e113fdfdd8b95fb22ef7df677d6177a4539d10899ed7920eb39e3aff3ad",
            "e578a15fdfb93576881a2afb90d9ba4f65d2769747b374dc2c5a20eb266759bb",
        ];
        for (t, w) in TESTS.iter().zip(want) {
            assert_eq!(
                hex::encode(sha2::Sha256::digest((t.prompt)())),
                w,
                "{}",
                t.name
            );
        }
    }
}
