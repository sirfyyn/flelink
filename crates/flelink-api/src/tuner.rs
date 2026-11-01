//! Set the engine up by measuring instead of guessing: the tuner and the A/B/A comparison. Port of `fle_studio/tuner.py`
//! and `trials.py`.
//!
//! Both start the engine themselves, one setting after the other, send the quick speed requests several times and stop it
//! again, so the engine must be idle when they begin and is stopped when they end.
//!
//! **The tuner** changes one thing at a time from the settings FLE Link would use anyway: a probe of the starting settings
//! (if they do not work, nothing else is tried), then for each setting that applies (threads, the FLE expert cache or
//! `--n-cpu-moe`, batch size, optionally context) a few values, each a full trial. A value replaces the best only if it is
//! faster with a 95 % interval that excludes 0 (`stats::compare`). The time budget is checked before every trial. The
//! winner is saved as the launch profile `Tuned: <model>`, which remembers the machine it was measured on.
//!
//! **A/B/A** measures A, B, then A again and says whether B is faster, slower or within noise (`stats::aba`).
//!
//! Not ported: the `llama-bench` screening of the Python tuner (DEVIATIONS.md D14); every value gets a full trial.
use crate::bench::{self, Stop};
use crate::launch::Plan;
use crate::server::{ApiError, Ctx};
use crate::settings::{py_str, py_truthy};
use crate::stats;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const CACHE_ENV: &str = "FLE_MOE_CACHE_BUDGET_MIB";
pub const PARAM_LABELS: [(&str, &str); 5] = [
    ("threads", "CPU threads"),
    ("cache", "GPU expert cache"),
    ("ncmoe", "Experts kept on the CPU (--n-cpu-moe)"),
    ("batch", "Batch size"),
    ("context", "Context size"),
];
pub const DEFAULT_PARAMS: [&str; 4] = ["threads", "cache", "ncmoe", "batch"];
pub const MIN_RUNS: i64 = 2;
pub const MAX_RUNS: i64 = 10;
const READY_TIMEOUT: Duration = Duration::from_secs(900);
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn label_of(key: &str) -> &'static str {
    PARAM_LABELS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or("", |(_, l)| l)
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ------------------------------------------------------------------------------------------------ plan changes

/// Every spelling of a flag and whether it takes a value (the flag help's table; unknown flags take one).
fn names_of(name: &str) -> (Vec<String>, bool) {
    let takes = crate::flaghelp::lookup(name).is_none_or(|(_, t, _)| t);
    (crate::flaghelp::names_of(name), takes)
}

/// A flag value to set: a value, a switch without value, or remove the flag.
#[derive(Clone, Debug)]
pub enum FlagValue {
    Set(String),
    Switch,
    Remove,
}

/// `args` with the flag `name` set (any spelling replaced in place, extra copies dropped).
pub fn set_flag(args: &[String], name: &str, value: &FlagValue) -> Vec<String> {
    let (names, takes) = names_of(name);
    let mut out = vec![];
    let mut done = false;
    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        let (base, eq) = match token.strip_prefix("--").and(token.split_once('=')) {
            Some((b, _)) => (b, true),
            None => (token.as_str(), false),
        };
        if !names.iter().any(|n| n == base) {
            out.push(token.clone());
            i += 1;
            continue;
        }
        let next_is_value = args.get(i + 1).is_some_and(|n| {
            !n.starts_with('-') || n[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
        });
        let consumed = if !eq && takes && next_is_value { 2 } else { 1 };
        if !done {
            match value {
                FlagValue::Switch => out.push(base.to_string()),
                FlagValue::Set(v) if eq => out.push(format!("{base}={v}")),
                FlagValue::Set(v) => out.extend([base.to_string(), v.clone()]),
                FlagValue::Remove => {}
            }
        }
        done = done || !matches!(value, FlagValue::Remove);
        i += consumed;
    }
    if !done {
        match value {
            FlagValue::Switch => out.push(name.to_string()),
            FlagValue::Set(v) => out.extend([name.to_string(), v.clone()]),
            FlagValue::Remove => {}
        }
    }
    out
}

/// The value of a flag in the plan, whichever way it is spelled.
pub fn flag_value(args: &[String], name: &str) -> Option<String> {
    let (names, _) = names_of(name);
    for (i, a) in args.iter().enumerate() {
        if names.iter().any(|n| n == a) && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        for n in names.iter().filter(|n| n.starts_with("--")) {
            if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// One setting to try: flags and environment variables (None removes one).
#[derive(Clone, Debug)]
pub struct Change {
    pub label: String,
    pub flags: Vec<(String, FlagValue)>,
    pub env: Vec<(String, Option<String>)>,
}

pub fn change_plan(plan: &Plan, c: &Change) -> Plan {
    let mut new = plan.clone();
    for (name, value) in &c.flags {
        new.args = set_flag(&new.args, name, value);
    }
    for (k, v) in &c.env {
        new.env.retain(|(a, _)| a != k);
        if let Some(v) = v {
            // in place, as Python's dict keeps the key where it was
            match plan.env.iter().position(|(a, _)| a == k) {
                Some(p) if p <= new.env.len() => new.env.insert(p, (k.clone(), v.clone())),
                _ => new.env.push((k.clone(), v.clone())),
            }
        }
    }
    new
}

pub fn describe(c: &Change) -> String {
    let mut parts: Vec<String> = c
        .flags
        .iter()
        .filter_map(|(k, v)| match v {
            FlagValue::Set(v) => Some(format!("{k} {v}")),
            FlagValue::Switch => Some(k.clone()),
            FlagValue::Remove => None,
        })
        .collect();
    parts.extend(
        c.env
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={v}"))),
    );
    parts.join(", ")
}

fn env_get<'a>(plan: &'a Plan, k: &str) -> Option<&'a str> {
    plan.env
        .iter()
        .find(|(a, _)| a == k)
        .map(|(_, v)| v.as_str())
}

// ------------------------------------------------------------------------------------------------ what to try

pub struct Param {
    pub key: &'static str,
    /// decode | prefill: which speed decides
    pub objective: &'static str,
    /// speed: must be faster; capacity: bigger is better as long as it is not slower
    pub rule: &'static str,
    pub base: String,
    pub values: Vec<Change>,
}

fn round_step(n: f64) -> i64 {
    256.max((n / 256.0).floor() as i64 * 256)
}

fn change(label: String, flags: &[(&str, String)]) -> Change {
    Change {
        label,
        flags: flags
            .iter()
            .map(|(k, v)| (k.to_string(), FlagValue::Set(v.clone())))
            .collect(),
        env: vec![],
    }
}

/// The settings worth trying for this plan on this machine, in the order they are tried.
pub fn candidates(plan: &Plan, sysinfo: &Value, keys: &[&str]) -> Vec<Param> {
    let info = &plan.model["info"];
    let physical = sysinfo["cpu"]["physical"].as_i64().unwrap_or(1).max(1);
    let fle = env_get(plan, CACHE_ENV).is_some();
    let has_gpu = sysinfo["gpus"]
        .as_array()
        .is_some_and(|g| g.iter().any(|g| g["vendor"] != "none"));
    let mut out = vec![];
    for &key in keys {
        match key {
            "threads" if physical > 1 => {
                let base = flag_value(&plan.args, "-t");
                let b: i64 = base.as_deref().and_then(|b| b.parse().ok()).unwrap_or(-1);
                let mut vals: Vec<i64> = [(physical - 1).max(1), physical, (physical / 2).max(1)]
                    .into_iter()
                    .filter(|v| *v != b)
                    .collect();
                vals.sort();
                vals.dedup();
                let b0 = base.as_deref().and_then(|b| b.parse().ok()).unwrap_or(0);
                out.push(Param {
                    key: "threads",
                    objective: "decode",
                    rule: "speed",
                    base: base.clone().unwrap_or_else(|| "default".into()),
                    values: vals
                        .into_iter()
                        .filter(|v| *v != b0)
                        .map(|v| change(format!("{v} threads"), &[("-t", v.to_string())]))
                        .collect(),
                });
            }
            "cache" if fle => {
                let budget: i64 = env_get(plan, CACHE_ENV)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                if budget >= 1024 {
                    let vals = [
                        round_step(budget as f64 * 0.8),
                        round_step(budget as f64 * 1.1),
                    ];
                    out.push(Param {
                        key: "cache",
                        objective: "decode",
                        rule: "speed",
                        base: format!("{budget} MiB"),
                        values: vals
                            .into_iter()
                            .filter(|v| *v != budget)
                            .map(|v| Change {
                                label: format!("expert cache {v} MiB"),
                                flags: vec![],
                                env: vec![(CACHE_ENV.into(), Some(v.to_string()))],
                            })
                            .collect(),
                    });
                }
            }
            "ncmoe"
                if !fle
                    && has_gpu
                    && py_truthy(&info["moe"])
                    && py_truthy(&info["layers"])
                    && flag_value(&plan.args, "--n-cpu-moe").is_none()
                    && !plan.args.iter().any(|a| a == "--cpu-moe") =>
            {
                let layers = info["layers"].as_i64().unwrap_or(0);
                let mut vals: Vec<i64> = [layers / 4, layers / 2, (3 * layers) / 4]
                    .into_iter()
                    .filter(|v| *v != 0)
                    .collect();
                vals.sort();
                vals.dedup();
                out.push(Param {
                    key: "ncmoe",
                    objective: "decode",
                    rule: "speed",
                    base: "chosen by -fit".into(),
                    values: vals
                        .into_iter()
                        .map(|v| {
                            change(
                                format!("{v} layers of experts on the CPU"),
                                &[("--n-cpu-moe", v.to_string())],
                            )
                        })
                        .collect(),
                });
            }
            "batch" => {
                let base = flag_value(&plan.args, "-b");
                let mut vals: Vec<i64> = [512, 1024, 2048, 4096]
                    .into_iter()
                    .filter(|v| Some(v.to_string()) != base)
                    .collect();
                if plan.args.iter().any(|a| a == "-ub" || a == "--ubatch-size") {
                    let ub = flag_value(&plan.args, "-ub");
                    vals.retain(|v| Some(v.to_string()) != ub);
                }
                out.push(Param {
                    key: "batch",
                    objective: "prefill",
                    rule: "speed",
                    base: base.unwrap_or_else(|| "default".into()),
                    values: vals
                        .into_iter()
                        .map(|v| {
                            change(
                                format!("batch {v}"),
                                &[("-b", v.to_string()), ("-ub", v.to_string())],
                            )
                        })
                        .collect(),
                });
            }
            "context" => {
                let ctx: i64 = flag_value(&plan.args, "-c")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let limit = info["context_length"].as_i64().unwrap_or(0);
                let vals: Vec<i64> = [ctx * 2, ctx * 4]
                    .into_iter()
                    .filter(|v| ctx != 0 && (limit == 0 || *v <= limit))
                    .collect();
                out.push(Param {
                    key: "context",
                    objective: "decode",
                    rule: "capacity",
                    base: format!("{} tokens", crate::launch::commas(ctx)),
                    values: vals
                        .into_iter()
                        .map(|v| {
                            change(
                                format!("context {} tokens", crate::launch::commas(v)),
                                &[("-c", v.to_string())],
                            )
                        })
                        .collect(),
                });
            }
            _ => {}
        }
    }
    out.retain(|p| !p.values.is_empty());
    out
}

// ------------------------------------------------------------------------------------------------ profiles

/// A short fingerprint of the hardware the settings were measured on.
pub fn machine_key(sysinfo: &Value) -> String {
    use sha2::Digest;
    let cpu = &sysinfo["cpu"];
    let int_gib = |v: &Value| (v.as_f64().unwrap_or(0.0).trunc() / GIB).floor() as i64;
    let mut gpus: Vec<String> = sysinfo["gpus"]
        .as_array()
        .map(|g| {
            g.iter()
                .map(|g| {
                    format!(
                        "{}:{}:{}",
                        py_str(&g["vendor"]),
                        py_str(&g["name"]),
                        int_gib(&g["memory_total"])
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    gpus.sort();
    let or_empty = |v: &Value| {
        if py_truthy(v) {
            py_str(v)
        } else {
            String::new()
        }
    };
    let mut parts = vec![
        or_empty(&cpu["name"]),
        or_empty(&cpu["physical"]),
        int_gib(&sysinfo["memory"]["total"]).to_string(),
    ];
    parts.extend(gpus);
    hex::encode(sha2::Sha256::digest(parts.join("|").as_bytes()))[..12].to_string()
}

/// A profile that starts exactly like `plan`, with the model and MTP paths back as placeholders and without the
/// arguments FLE Link adds on its own (host, port, metrics, alias, log level).
pub fn profile_from_plan(
    plan: &Plan,
    id: &str,
    name: &str,
    notes: &str,
    meta: Value,
) -> Map<String, Value> {
    let model = &plan.model;
    let swap: Vec<(String, &str)> = [
        (py_str(&model["path"]), "{model}"),
        (py_str(&model["mtp"]["head"]), "{mtp_head}"),
        (py_str(&model["mtp"]["vocab"]), "{mtp_vocab}"),
    ]
    .into_iter()
    .filter(|(k, _)| !k.is_empty() && k != "None")
    .collect();
    let back = |t: &str| {
        swap.iter()
            .find(|(k, _)| k == t)
            .map_or(t.to_string(), |(_, v)| v.to_string())
    };
    let mut args = plan.args.clone();
    for f in ["--host", "--port", "-a", "-lv", "--metrics"] {
        args = set_flag(&args, f, &FlagValue::Remove);
    }
    let args: Vec<String> = args.iter().map(|a| back(a)).collect();
    let env: Map<String, Value> = plan
        .env
        .iter()
        .map(|(k, v)| {
            let v = if k == "FLE_MOE_PROFILE" {
                "{expert_profile}".to_string()
            } else {
                back(v)
            };
            (k.clone(), json!(v))
        })
        .collect();
    let custom = plan.engine == "custom";
    let mut m = Map::new();
    m.insert("id".into(), json!(id));
    m.insert("name".into(), json!(name));
    m.insert("engine".into(), json!(plan.engine));
    m.insert(
        "binary".into(),
        json!(if custom { plan.binary.as_str() } else { "" }),
    );
    m.insert("args".into(), json!(args));
    m.insert("env".into(), Value::Object(env));
    m.insert("notes".into(), json!(notes));
    m.insert("meta".into(), meta);
    m
}

// ------------------------------------------------------------------------------------------------ one trial

/// How a trial ended without a result.
#[derive(Debug)]
pub enum TrialErr {
    /// the engine did not start or did not answer with this plan
    Failed(String),
    Cancelled,
    /// the engine could not be started at all (another one runs)
    Busy(String),
}

impl From<Stop> for TrialErr {
    fn from(s: Stop) -> Self {
        match s {
            Stop::Cancelled => TrialErr::Cancelled,
            Stop::Failed(e) => TrialErr::Failed(format!("The engine stopped answering: {e}")),
        }
    }
}

/// Sees the results of the first run before the others start (the probe gives up early with it).
pub type FirstRun = dyn Fn(&[Value]) -> Result<(), TrialErr>;

/// Start the engine with `plan`, wait until it answers, measure `runs` times, stop it (in every case).
pub fn trial(
    st: &Ctx,
    cancel: &AtomicBool,
    plan: &Plan,
    runs: usize,
    on_first_run: Option<&FirstRun>,
) -> Result<Value, TrialErr> {
    let t0 = Instant::now();
    if let Err(e) = st.engine.start(plan.clone()) {
        return Err(match e {
            crate::engine::EngineErr::Busy(m) => TrialErr::Busy(m),
            crate::engine::EngineErr::Invalid(m) => TrialErr::Failed(m),
        });
    }
    let r = (|| {
        wait_ready(st, cancel)?;
        let load_s = stats::round_to(t0.elapsed().as_secs_f64(), 1);
        let (_, base, key) = st.engine.target();
        let quick = bench::quick_tests();
        let mut results: Vec<Value> = vec![];
        for run in 0..runs {
            if cancel.load(Ordering::SeqCst) {
                return Err(TrialErr::Cancelled);
            }
            let part = bench::measure(
                &base,
                &key,
                1,
                &quick,
                cancel,
                run,
                &mut |_, _| {},
                &mut |_| {},
            )?;
            if run == 0 {
                if let Some(f) = on_first_run {
                    f(&part)?;
                }
            }
            results.extend(part);
        }
        let mut out = json!({"load_s": load_s, "seconds": stats::round_to(t0.elapsed().as_secs_f64(), 1),
            "tests": results});
        for (k, v) in bench::run_stats(&results).as_object().unwrap() {
            out[k] = v.clone();
        }
        Ok(out)
    })();
    stop_engine(st);
    r
}

fn wait_ready(st: &Ctx, cancel: &AtomicBool) -> Result<(), TrialErr> {
    let t = Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(TrialErr::Cancelled);
        }
        let s = st.engine.status();
        match s["state"].as_str().unwrap_or("") {
            "ready" | "attached" => return Ok(()),
            "crashed" | "stopped" => {
                let error = Some(py_str(&s["error"]))
                    .filter(|e| !e.is_empty() && e != "None")
                    .unwrap_or_else(|| "The engine stopped while starting".into());
                let hint = Some(py_str(&s["hint"]))
                    .filter(|h| !h.is_empty() && h != "None")
                    .map(|h| format!(" {h}"))
                    .unwrap_or_default();
                return Err(TrialErr::Failed(format!("{error}{hint}")));
            }
            _ => {}
        }
        if t.elapsed() > READY_TIMEOUT {
            return Err(TrialErr::Failed(format!(
                "The engine did not become ready within {} s",
                READY_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Stop the engine and wait until the process is gone, so the next trial gets all of its memory.
fn stop_engine(st: &Ctx) {
    st.engine.stop();
    let t = Instant::now();
    while matches!(
        st.engine.status()["state"].as_str(),
        Some("starting" | "ready" | "stopping")
    ) && t.elapsed() < Duration::from_secs(60)
    {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A trial without the raw request list: what the page and the stored record keep.
fn slim(res: &Value) -> Value {
    json!({"load_s": res["load_s"], "seconds": res["seconds"], "decode_runs": res["decode_runs"],
        "prefill_runs": res["prefill_runs"], "decode": res["decode_ci"], "prefill": res["prefill_ci"]})
}

fn floats(v: &Value) -> Vec<f64> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

fn merge(target: &mut Value, extra: &Value) {
    if let (Some(t), Some(e)) = (target.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            t.insert(k.clone(), v.clone());
        }
    }
}

// ------------------------------------------------------------------------------------------------ the tuner

pub struct Tuner {
    dir: PathBuf,
    current: Mutex<Option<Value>>,
    cancel: AtomicBool,
}

impl Tuner {
    pub fn new(data_dir: &std::path::Path) -> Tuner {
        let dir = data_dir.join("tuning");
        let _ = std::fs::create_dir_all(&dir);
        Tuner {
            dir,
            current: Mutex::new(None),
            cancel: AtomicBool::new(false),
        }
    }

    /// The stored runs, newest first (at most 100).
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
            .take(100)
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

    fn set(&self, f: impl FnOnce(&mut Value)) {
        if let Some(c) = self.current.lock().unwrap().as_mut() {
            f(c);
        }
    }

    /// The profile the tuner saved for this model on this machine, or null.
    pub fn tuned_profile(st: &Ctx, model_id: &str) -> Value {
        let key = machine_key(&st.sysinfo());
        st.profiles
            .lock()
            .unwrap()
            .all()
            .into_iter()
            .find(|p| {
                let t = &p.meta["tuned"];
                t["model_id"] == json!(model_id) && t["machine"] == json!(key)
            })
            .map_or(Value::Null, |p| p.public())
    }

    fn begin(&self, st: &Ctx, kind: &str, fields: Value) -> Result<Value, ApiError> {
        let mut cur = self.current.lock().unwrap();
        if cur.as_ref().is_some_and(|c| c["state"] == "running") {
            return Err(ApiError(
                409,
                "A tuning run or comparison is already running".into(),
            ));
        }
        let state = st.engine.status()["state"].clone();
        if state != "stopped" && state != "crashed" {
            return Err(ApiError(409, "Stop the engine first (or disconnect from the server): the tuner starts the engine itself, once for every setting it tries, and stops it at the end.".into()));
        }
        self.cancel.store(false, Ordering::SeqCst);
        let mut id = [0u8; 4];
        getrandom::getrandom(&mut id).expect("random bytes");
        let mut rec = json!({"id": hex::encode(id), "kind": kind, "state": "running", "started": now(),
            "step": "Starting", "trials": [], "notes": []});
        merge(&mut rec, &fields);
        *cur = Some(rec.clone());
        Ok(rec)
    }

    fn finish(&self, state: &str, error: &str) {
        let mut record = Value::Null;
        self.set(|c| {
            let t = now();
            c["state"] = json!(state);
            c["finished"] = json!(t);
            c["step"] = json!("");
            c["elapsed"] = json!(stats::round_to(t - c["started"].as_f64().unwrap_or(t), 1));
            if !error.is_empty() {
                c["error"] = json!(error);
            }
            record = c.clone();
        });
        let started = record["started"].as_f64().unwrap_or(0.0);
        let stamp = chrono::DateTime::from_timestamp(started as i64, 0)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%Y%m%d-%H%M%S")
                    .to_string()
            })
            .unwrap_or_default();
        let _ = std::fs::write(
            self.dir
                .join(format!("{stamp}-{}.json", py_str(&record["id"]))),
            serde_json::to_string_pretty(&record).unwrap_or_default(),
        );
    }

    pub fn start_tune(
        st: &Ctx,
        model_id: &str,
        profile: &str,
        budget_s: i64,
        runs: i64,
        params: Vec<String>,
        screen: bool,
    ) -> Result<Value, ApiError> {
        let runs = runs.clamp(MIN_RUNS, MAX_RUNS);
        let budget_s = budget_s.clamp(60, 86400);
        let mut keys: Vec<String> = params
            .into_iter()
            .filter(|p| PARAM_LABELS.iter().any(|(k, _)| k == p))
            .collect();
        if keys.is_empty() {
            keys = DEFAULT_PARAMS.iter().map(|s| s.to_string()).collect();
        }
        let base_plan = st.make_plan(model_id, Some(&json!(profile)))?;
        let rec = st.tuner.begin(
            st,
            "tune",
            json!({"model": {"id": model_id, "name": base_plan.model.get("name").cloned().unwrap_or(json!(""))},
                "profile": profile, "budget_s": budget_s, "runs": runs, "params": keys, "screen": screen}),
        )?;
        let st2 = st.clone();
        std::thread::Builder::new()
            .name("tuner".into())
            .spawn(move || {
                st2.tuner
                    .run_tune(&st2, base_plan, keys, runs as usize, budget_s as f64)
            })
            .map_err(|e| ApiError(500, e.to_string()))?;
        Ok(rec)
    }

    pub fn start_aba(
        st: &Ctx,
        model_id: &str,
        a: &str,
        b: &str,
        runs: i64,
        label: &str,
    ) -> Result<Value, ApiError> {
        let runs = runs.clamp(MIN_RUNS, MAX_RUNS);
        let plan_a = st.make_plan(model_id, Some(&json!(a)))?;
        let plan_b = st.make_plan(model_id, Some(&json!(b)))?;
        let label: String = label.trim().chars().take(80).collect();
        let rec = st.tuner.begin(
            st,
            "aba",
            json!({"model": {"id": model_id, "name": plan_a.model.get("name").cloned().unwrap_or(json!(""))},
                "a": {"profile": a, "command": plan_a.command_line()},
                "b": {"profile": b, "command": plan_b.command_line()}, "runs": runs, "label": label}),
        )?;
        let st2 = st.clone();
        std::thread::Builder::new()
            .name("aba".into())
            .spawn(move || st2.tuner.run_aba(&st2, plan_a, plan_b, runs as usize))
            .map_err(|e| ApiError(500, e.to_string()))?;
        Ok(rec)
    }

    fn step(&self, what: &str) {
        self.set(|c| c["step"] = json!(what));
    }

    fn run_tune(&self, st: &Ctx, base_plan: Plan, keys: Vec<String>, runs: usize, budget_s: f64) {
        let restart = st.engine.restart_on_crash();
        st.engine.set_restart_on_crash(false); // a candidate that crashes is a result, not a reason to start it again
        let outcome = self.tune(st, &base_plan, &keys, runs, budget_s);
        st.engine.set_restart_on_crash(restart);
        stop_engine(st);
        match outcome {
            Ok(()) => self.finish("done", ""),
            Err(TrialErr::Cancelled) => self.finish("cancelled", ""),
            Err(TrialErr::Failed(e) | TrialErr::Busy(e)) => self.finish("failed", &e),
        }
    }

    fn tune(
        &self,
        st: &Ctx,
        base_plan: &Plan,
        keys: &[String],
        runs: usize,
        budget_s: f64,
    ) -> Result<(), TrialErr> {
        let sysinfo = st.sysinfo();
        let t0 = Instant::now();
        let left = || budget_s - t0.elapsed().as_secs_f64();
        // 1. the probe: the starting settings, first run checked at once
        let check = |part: &[Value]| -> Result<(), TrialErr> {
            if part.is_empty()
                || !part
                    .iter()
                    .any(|r| r["decode"].as_f64().unwrap_or(0.0) > 0.0)
            {
                return Err(TrialErr::Failed("The engine answered, but generated no tokens. Check the model and the engine log on the Engine page.".into()));
            }
            Ok(())
        };
        self.step("Probe: the starting settings");
        let first = match trial(st, &self.cancel, base_plan, runs, Some(&check)) {
            Ok(r) => r,
            Err(TrialErr::Failed(e)) => {
                self.set(|c| c["probe"] = json!({"ok": false, "error": e}));
                return Err(TrialErr::Failed(format!(
                    "The starting settings do not work, so nothing was tried: {e}"
                )));
            }
            Err(e) => return Err(e),
        };
        let mut best_plan = base_plan.clone();
        let mut best = first.clone();
        let mut longest = first["seconds"].as_f64().unwrap_or(0.0);
        let s1 = slim(&first);
        self.set(|c| {
            let mut probe = json!({"ok": true});
            merge(&mut probe, &s1);
            c["probe"] = probe;
            c["baseline"] = s1.clone();
            let mut b = json!({"label": "starting settings", "command": base_plan.command_line()});
            merge(&mut b, &s1);
            c["best"] = b;
            c["trial_seconds"] = json!(longest);
        });
        let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let params = candidates(base_plan, &sysinfo, &key_refs);
        let mut notes: Vec<String> = vec![];
        if params.is_empty() {
            notes.push(
                "Nothing to try for this model and engine; the starting settings stay.".into(),
            );
        }
        let mut stopped = "";
        'outer: for param in &params {
            let label = label_of(param.key);
            for ch in &param.values {
                if left() < longest * 1.1 {
                    stopped = "The time budget ran out";
                    break 'outer;
                }
                let plan = change_plan(&best_plan, ch);
                let n = self.status()["trials"].as_array().map_or(0, Vec::len) + 1;
                let mut entry = json!({"n": n, "param": param.key, "param_label": label, "label": ch.label,
                    "change": describe(ch)});
                self.step(&format!("{label}: {}", ch.label));
                let res = match trial(st, &self.cancel, &plan, runs, None) {
                    Ok(r) => r,
                    Err(TrialErr::Failed(e)) => {
                        merge(
                            &mut entry,
                            &json!({"state": "failed", "error": e, "kept": false}),
                        );
                        self.set(|c| c["trials"].as_array_mut().unwrap().push(entry));
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                longest = longest.max(res["seconds"].as_f64().unwrap_or(0.0));
                let k = if param.objective == "decode" {
                    "decode_runs"
                } else {
                    "prefill_runs"
                };
                let cmp = stats::compare(&floats(&best[k]), &floats(&res[k]), true, 0.0);
                let take = cmp["verdict"] == "better"
                    || (param.rule == "capacity" && cmp["verdict"] == "within noise");
                let vs: Map<String, Value> = ["verdict", "pct", "pct_lo", "pct_hi", "method"]
                    .iter()
                    .map(|f| (f.to_string(), cmp[*f].clone()))
                    .collect();
                merge(
                    &mut entry,
                    &json!({"state": "ok", "kept": take, "vs_best": vs}),
                );
                merge(&mut entry, &slim(&res));
                self.set(|c| c["trials"].as_array_mut().unwrap().push(entry));
                if take {
                    best_plan = plan;
                    best = res;
                    let mut b = json!({"label": ch.label, "command": best_plan.command_line()});
                    merge(&mut b, &slim(&best));
                    self.set(|c| c["best"] = b);
                }
            }
        }
        if !stopped.is_empty() {
            notes.push(format!(
                "{stopped}; the values not tried yet stay as they are."
            ));
        }
        self.set(|c| c["notes"] = json!(notes));
        let profile = self.save_profile(st, &best_plan, base_plan, &first, &best)?;
        let cmd = best_plan.command_line();
        self.set(|c| {
            c["profile"] = profile;
            c["best"]["command"] = json!(cmd);
        });
        Ok(())
    }

    fn save_profile(
        &self,
        st: &Ctx,
        best_plan: &Plan,
        base_plan: &Plan,
        first: &Value,
        best: &Value,
    ) -> Result<Value, TrialErr> {
        let cur = self.status();
        let model = &base_plan.model;
        let decode = &best["decode_ci"];
        let median = decode["median"].as_f64().unwrap_or(0.0);
        let base_median = first["decode_ci"]["median"].as_f64().unwrap_or(0.0);
        let gain = if best != first && base_median != 0.0 {
            format!(
                ", {:+.1} % against the starting settings",
                100.0 * (median / base_median - 1.0)
            )
        } else {
            String::new()
        };
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let trials = cur["trials"].as_array().map_or(0, Vec::len);
        let minutes = (now() - cur["started"].as_f64().unwrap_or(now())) / 60.0;
        let notes = format!(
            "Tuned by FLE Link on {today}: {} tok/s decode (median of {} runs{gain}). {trials} settings tried in {minutes:.0} min. Measured on this machine only.",
            crate::launch::fmt_g(median),
            decode["n"]
        );
        let model_id = py_str(&cur["model"]["id"]);
        let meta = json!({"tuned": {"model_id": model.get("id").cloned().unwrap_or(json!(model_id)),
            "model": model.get("name").cloned().unwrap_or(json!("")), "machine": machine_key(&st.sysinfo()),
            "date": today, "decode": decode, "base_decode": first["decode_ci"], "trials": trials}});
        let name = match model.get("name").filter(|n| py_truthy(n)) {
            Some(n) => py_str(n),
            None => model_id.clone(),
        };
        let data = profile_from_plan(
            best_plan,
            &format!("tuned-{model_id}"),
            &format!("Tuned: {name}"),
            &notes,
            meta,
        );
        let saved = st.profiles.lock().unwrap().save(&data).map_err(|e| {
            TrialErr::Failed(format!("The tuned profile could not be saved: {}", e.0))
        })?;
        Ok(json!({"id": saved.id, "name": saved.name}))
    }

    fn run_aba(&self, st: &Ctx, plan_a: Plan, plan_b: Plan, runs: usize) {
        let restart = st.engine.restart_on_crash();
        st.engine.set_restart_on_crash(false);
        let r = (|| -> Result<(), TrialErr> {
            let cur = self.status();
            let mut phases: Vec<Value> = vec![];
            for (name, plan) in [("A1", &plan_a), ("B", &plan_b), ("A2", &plan_a)] {
                let which = if name == "B" { "b" } else { "a" };
                self.step(&format!("{name}: {}", py_str(&cur[which]["profile"])));
                let res = trial(st, &self.cancel, plan, runs, None)?;
                let mut p = json!({"phase": name});
                merge(&mut p, &slim(&res));
                phases.push(p);
                let ph = phases.clone();
                self.set(|c| c["trials"] = json!(ph));
            }
            let by =
                |n: &str, k: &str| floats(&phases.iter().find(|p| p["phase"] == n).unwrap()[k]);
            let result = json!({
                "decode": stats::aba(&by("A1", "decode_runs"), &by("B", "decode_runs"), &by("A2", "decode_runs")),
                "prefill": stats::aba(&by("A1", "prefill_runs"), &by("B", "prefill_runs"), &by("A2", "prefill_runs"))});
            self.set(|c| c["result"] = result);
            Ok(())
        })();
        st.engine.set_restart_on_crash(restart);
        stop_engine(st);
        match r {
            Ok(()) => self.finish("done", ""),
            Err(TrialErr::Cancelled) => self.finish("cancelled", ""),
            Err(TrialErr::Failed(e)) => {
                let step = py_str(&self.status()["step"]);
                let step = if step.is_empty() {
                    "A trial".to_string()
                } else {
                    step
                };
                self.finish("failed", &format!("{step} failed: {e}"))
            }
            Err(TrialErr::Busy(e)) => self.finish("failed", &format!("EngineBusy: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn flags_are_replaced_in_place_whatever_their_spelling() {
        let a = s(&[
            "-m",
            "x",
            "--threads",
            "8",
            "-c",
            "4096",
            "--metrics",
            "-t",
            "4",
        ]);
        assert_eq!(
            set_flag(&a, "-t", &FlagValue::Set("12".into())),
            s(&["-m", "x", "--threads", "12", "-c", "4096", "--metrics"])
        );
        assert_eq!(
            set_flag(&a, "--metrics", &FlagValue::Remove),
            s(&["-m", "x", "--threads", "8", "-c", "4096", "-t", "4"])
        );
        assert_eq!(
            set_flag(
                &s(&["--ctx-size=2048"]),
                "-c",
                &FlagValue::Set("8192".into())
            ),
            s(&["--ctx-size=8192"])
        );
        assert_eq!(
            set_flag(&s(&["-m", "x"]), "-b", &FlagValue::Set("512".into())),
            s(&["-m", "x", "-b", "512"])
        );
        assert_eq!(
            flag_value(&s(&["--batch-size", "1024"]), "-b").as_deref(),
            Some("1024")
        );
    }

    #[test]
    fn the_machine_key_is_the_python_studios() {
        // trials.machine_key of this sysinfo (Python Studio 055f4aa)
        let info = json!({"cpu": {"name": "AMD Ryzen 9 9950X", "physical": 16},
            "memory": {"total": 201863897088u64},
            "gpus": [{"vendor": "nvidia", "name": "NVIDIA GeForce RTX 5090", "memory_total": 34190917632u64},
                     {"vendor": "nvidia", "name": "NVIDIA GeForce RTX 3090", "memory_total": 25769803776u64}]});
        assert_eq!(machine_key(&info), PY_MACHINE_KEY);
    }

    const PY_MACHINE_KEY: &str = "53c169c9f043";
}
