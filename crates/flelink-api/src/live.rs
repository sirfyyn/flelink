//! Live inference data, sampled about once a second. Port of `fle_studio/metrics.py`.
//!
//! Speeds come from the counters in llama-server's /metrics: tokens generated per second of generation time, prompt tokens
//! per second of prompt time, and the share of drafted (MTP) tokens that were accepted, each over the last interval. A server
//! started without --metrics has no such counters; then the timings of the last answer stand in.
use crate::engine::{read_and_memory, Engine};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const P: &str = "llamacpp:";

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Sum of the values per metric name (labels are dropped).
pub fn parse_prometheus(text: &str) -> HashMap<String, f64> {
    let mut out: HashMap<String, f64> = HashMap::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.rsplit_once(' ') else {
            continue;
        };
        let name = name.split('{').next().unwrap_or("").trim().to_string();
        if let Ok(v) = value.trim().parse::<f64>() {
            *out.entry(name).or_insert(0.0) += v;
        }
    }
    out
}

fn round(x: f64, digits: i32) -> f64 {
    format!("{:.*}", digits as usize, x).parse().unwrap_or(x)
}

/// What changed between two readings of the counters (`_rates`). `None` for `prev` is a first reading.
pub fn rates(
    prev: Option<&HashMap<String, f64>>,
    cur: &HashMap<String, f64>,
) -> Map<String, Value> {
    let g = |m: &HashMap<String, f64>, k: &str| m.get(&format!("{P}{k}")).copied().unwrap_or(0.0);
    let mut out = Map::new();
    out.insert("busy".into(), json!(g(cur, "requests_processing") > 0.0));
    out.insert("deferred".into(), json!(g(cur, "requests_deferred") as i64));
    let Some(prev) = prev else { return out };
    let d = |k: &str| g(cur, k) - g(prev, k);
    if d("tokens_predicted_total") < 0.0 || d("prompt_tokens_total") < 0.0 {
        return out; // the server restarted
    }
    let (gen_n, gen_s) = (
        d("tokens_predicted_total"),
        d("tokens_predicted_seconds_total"),
    );
    let (pp_n, pp_s) = (d("prompt_tokens_total"), d("prompt_seconds_total"));
    let (drafted, accepted) = (
        d("spec_decode_num_draft_tokens_total"),
        d("spec_decode_num_accepted_tokens_total"),
    );
    if gen_n > 0.0 && gen_s > 0.0 {
        out.insert("decode".into(), json!(round(gen_n / gen_s, 2)));
        out.insert("generated".into(), json!(gen_n as i64));
    }
    if pp_n > 0.0 && pp_s > 0.0 {
        out.insert("prefill".into(), json!(round(pp_n / pp_s, 1)));
        out.insert("prompt_tokens".into(), json!(pp_n as i64));
    }
    if drafted > 0.0 {
        out.insert("accept".into(), json!(round(accepted / drafted, 4)));
    }
    out
}

struct Inner {
    /// time of the last look at the live data (a page asked), `None` before the first
    seen: Option<Instant>,
    /// the last time a page (not only an answer through the proxy) asked: only then the GPU is read
    seen_page: Option<Instant>,
    history: VecDeque<Value>,
    responses: VecDeque<Value>,
    metrics_available: Option<bool>,
    prev: Option<HashMap<String, f64>>,
    prev_url: String,
    gpus: Vec<Value>,
    totals: Value,
    /// one `nvidia-smi -l` process while somebody watches (None otherwise): see system::GpuStream
    gpu_stream: Option<crate::system::GpuStream>,
    /// a response is running (from the last /metrics), so the engine is asked less often
    busy: bool,
    last_rates: Map<String, Value>,
    last_slots: Map<String, Value>,
}

#[derive(Clone)]
pub struct Live {
    engine: Engine,
    inner: Arc<Mutex<Inner>>,
    wake: Arc<std::sync::Condvar>,
    watch_seconds: f64,
}

impl Live {
    pub fn new(engine: Engine, interval: f64) -> Live {
        let l = Live {
            engine,
            inner: Arc::new(Mutex::new(Inner {
                seen: None,
                seen_page: None,
                history: VecDeque::new(),
                responses: VecDeque::new(),
                metrics_available: None,
                prev: None,
                prev_url: String::new(),
                gpus: vec![],
                totals: json!({}),
                gpu_stream: None,
                busy: false,
                last_rates: Map::new(),
                last_slots: Map::new(),
            })),
            wake: Arc::new(std::sync::Condvar::new()),
            watch_seconds: 20.0,
        };
        let me = l.clone();
        std::thread::Builder::new()
            .name("live-sampler".into())
            .spawn(move || {
                let n_gpus = crate::system::nvidia().len();
                let mut tick = 0u64;
                loop {
                    // The sampler runs only while somebody watches: a page that asks for the live data keeps it going for
                    // `watch_seconds` after its last look. With no page open nothing is sampled, so an idle Studio next to a
                    // busy engine sends the engine no /metrics and /slots requests.
                    if !me.watched() {
                        me.inner.lock().unwrap().gpu_stream = None; // ends the nvidia-smi process
                        let guard = me.inner.lock().unwrap();
                        let _unused = me
                            .wake
                            .wait_while(guard, |i| {
                                !i.seen
                                    .is_some_and(|t| t.elapsed().as_secs_f64() < me.watch_seconds)
                            })
                            .unwrap();
                        tick = 0; // the counters kept running meanwhile; the first rate covers the time not watched
                        continue;
                    }
                    // An answer through the proxy (Claude Code, an agent, a script) keeps the engine's /metrics and /slots going, but
                    // nobody looks at the GPU then: without a page open there is no nvidia-smi, which cost this 16-thread engine about
                    // 5 % (47.1 against 49.7 tokens/s measured on 6 Nov 2026).
                    let page = me.page_watched();
                    if !page {
                        me.inner.lock().unwrap().gpu_stream = None;
                    }
                    let t0 = Instant::now();
                    me.sample(tick, if page { n_gpus } else { 0 });
                    tick += 1;
                    let spent = t0.elapsed().as_secs_f64();
                    std::thread::sleep(Duration::from_secs_f64(
                        (interval - spent).max(0.2).min(interval.max(0.2)),
                    ));
                }
            })
            .expect("live thread");
        l
    }

    /// A page asked for the live data: the sampler runs, GPU included.
    pub fn touch(&self) {
        let mut i = self.inner.lock().unwrap();
        let t = Some(Instant::now());
        i.seen = t;
        i.seen_page = t;
        drop(i);
        self.wake.notify_all();
    }

    /// An answer went through the proxy: the sampler runs for the engine's counters, without the GPU.
    pub fn touch_api(&self) {
        self.inner.lock().unwrap().seen = Some(Instant::now());
        self.wake.notify_all();
    }

    pub fn page_watched(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .seen_page
            .is_some_and(|t| t.elapsed().as_secs_f64() < self.watch_seconds)
    }

    pub fn watched(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .seen
            .is_some_and(|t| t.elapsed().as_secs_f64() < self.watch_seconds)
    }

    /// Called by the chat proxy with the `timings` of every finished answer.
    pub fn record_response(&self, timings: &Value, usage: Option<&Value>) {
        self.record_request(timings, usage, Map::new());
    }

    /// The same with what the proxy knows about the request: which API (`api`), which program sent it (`client`), how
    /// long it took (`seconds`). Answers without engine timings (the Anthropic and Responses formats) count their tokens
    /// from `usage`, and their speed is estimated from the wall clock (`estimated`).
    pub fn record_request(
        &self,
        timings: &Value,
        usage: Option<&Value>,
        extra: Map<String, Value>,
    ) {
        let mut e = Map::new();
        e.insert("t".into(), json!(now()));
        for k in [
            "prompt_n",
            "prompt_ms",
            "prompt_per_second",
            "predicted_n",
            "predicted_ms",
            "predicted_per_second",
            "draft_n",
            "draft_n_accepted",
            "cache_n",
        ] {
            e.insert(k.into(), timings.get(k).cloned().unwrap_or(Value::Null));
        }
        if let Some(u) = usage.filter(|u| crate::settings::py_truthy(u)) {
            e.insert("usage".into(), u.clone());
            let num = |k: &str| u.get(k).and_then(Value::as_f64);
            if e["prompt_n"].is_null() {
                if let Some(p) = num("input_tokens").or(num("prompt_tokens")) {
                    e.insert("prompt_n".into(), json!(p));
                }
            }
            if e["predicted_n"].is_null() {
                if let Some(o) = num("output_tokens").or(num("completion_tokens")) {
                    e.insert("predicted_n".into(), json!(o));
                    let secs = extra.get("seconds").and_then(Value::as_f64).unwrap_or(0.0);
                    if secs > 0.0 && o > 0.0 {
                        e.insert("predicted_per_second".into(), json!(o / secs));
                        e.insert("estimated".into(), json!(true));
                    }
                }
            }
        }
        e.extend(extra);
        let mut i = self.inner.lock().unwrap();
        i.responses.push_back(Value::Object(e));
        if i.responses.len() > 100 {
            i.responses.pop_front();
        }
    }

    fn get(&self, base: &str, path: &str, key: &str) -> (i64, String) {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(2))
            .build();
        let mut req = agent.get(&format!("{base}{path}"));
        if !key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        match req.call() {
            Ok(r) => {
                let c = i64::from(r.status());
                (c, r.into_string().unwrap_or_default())
            }
            Err(ureq::Error::Status(c, _)) => (i64::from(c), String::new()),
            Err(_) => (0, String::new()),
        }
    }

    fn sample(&self, tick: u64, n_gpus: usize) {
        let bound = self.engine.bound();
        if bound == "url" && tick % 5 == 0 {
            self.engine.refresh_attached();
        }
        let (state, base, key) = self.engine.target();
        let mut s = Map::new();
        s.insert("t".into(), json!(round(now(), 2)));
        s.insert("state".into(), json!(state));
        let mem = crate::system::memory();
        let (total, avail) = (
            mem["total"].as_u64().unwrap_or(0),
            mem["available"].as_u64().unwrap_or(0),
        );
        s.insert("ram_used".into(), json!(total as i64 - avail as i64));
        s.insert("ram_total".into(), json!(total));
        if n_gpus > 0 {
            // one long-lived nvidia-smi while somebody watches; a process per reading only when the stream gives nothing
            let streamed = {
                let mut i = self.inner.lock().unwrap();
                if i.gpu_stream.is_none() {
                    i.gpu_stream = crate::system::GpuStream::start(n_gpus, 3);
                }
                i.gpu_stream
                    .as_ref()
                    .map(|g| g.latest())
                    .unwrap_or_default()
            };
            let raw = if !streamed.is_empty() {
                Some(streamed)
            } else if tick % 2 == 0 || self.inner.lock().unwrap().gpus.is_empty() {
                Some(crate::system::nvidia())
            } else {
                None
            };
            if let Some(raw) = raw {
                let gpus: Vec<Value> = raw
                    .into_iter()
                    .map(|g| {
                        Value::Object(
                            [
                                "index",
                                "name",
                                "utilization",
                                "memory_used",
                                "memory_total",
                                "power_w",
                                "temperature_c",
                            ]
                            .iter()
                            .map(|k| (k.to_string(), g.get(*k).cloned().unwrap_or(Value::Null)))
                            .collect(),
                        )
                    })
                    .collect();
                self.inner.lock().unwrap().gpus = gpus;
            }
        }
        s.insert(
            "gpus".into(),
            Value::Array(self.inner.lock().unwrap().gpus.clone()),
        );
        if let Some(pid) = self.engine.proc_pid() {
            let rss = read_and_memory(pid).1;
            s.insert("engine_mem".into(), json!(rss));
            self.engine.note_memory(rss);
        }
        let live = state == "ready" || state == "attached";
        if live && !base.is_empty() {
            {
                let mut i = self.inner.lock().unwrap();
                if base != i.prev_url {
                    // An engine Studio started counts from zero, so even an answer that finished before the first sample shows
                    // up. A server it attached to has run before: its first sample is the baseline.
                    i.prev = if state == "ready" {
                        Some(HashMap::new())
                    } else {
                        None
                    };
                    i.prev_url = base.clone();
                    i.metrics_available = None;
                }
            }
            // while a response runs the engine is asked for /metrics every second tick and for /slots every sixth (the rates
            // cover the longer interval; the counters keep counting), so the sampler does not interrupt its decode loop
            let busy = self.inner.lock().unwrap().busy;
            let ask_metrics = !busy || tick % 2 == 0;
            let (code, text) = if ask_metrics {
                self.get(&base, "/metrics", &key)
            } else {
                (-1, String::new())
            };
            if code == -1 {
                let i = self.inner.lock().unwrap();
                for (k, v) in &i.last_rates {
                    s.insert(k.clone(), v.clone());
                }
            } else if code == 200 && !text.is_empty() {
                let cur = parse_prometheus(&text);
                let mut i = self.inner.lock().unwrap();
                i.metrics_available = Some(true);
                let r = rates(i.prev.as_ref(), &cur);
                i.prev = Some(cur.clone());
                i.busy = r.get("busy").and_then(Value::as_bool).unwrap_or(false);
                i.last_rates = r.clone();
                for (k, v) in r {
                    s.insert(k, v);
                }
                let g = |k: &str| cur.get(&format!("{P}{k}")).copied().unwrap_or(0.0);
                i.totals = json!({"generated": g("tokens_predicted_total"), "prompt": g("prompt_tokens_total"), "decode_calls": g("n_decode_total")});
            } else if code == 404 || code == 501 {
                self.inner.lock().unwrap().metrics_available = Some(false);
            }
            let ask_slots = if busy { tick % 6 == 0 } else { tick % 2 == 0 };
            if !ask_slots {
                let i = self.inner.lock().unwrap();
                for (k, v) in &i.last_slots {
                    s.insert(k.clone(), v.clone());
                }
            } else {
                let (code, text) = self.get(&base, "/slots", &key);
                if code == 200 && text.starts_with('[') {
                    if let Ok(Value::Array(slots)) = serde_json::from_str::<Value>(&text) {
                        let busy_n = slots
                            .iter()
                            .filter(|x| crate::settings::py_truthy(&x["is_processing"]))
                            .count();
                        s.insert("slots_busy".into(), json!(busy_n));
                        s.insert("slots".into(), json!(slots.len()));
                        let mut i = self.inner.lock().unwrap();
                        i.last_slots.insert("slots_busy".into(), json!(busy_n));
                        i.last_slots.insert("slots".into(), json!(slots.len()));
                    }
                }
            }
        } else {
            let mut i = self.inner.lock().unwrap();
            i.prev = None; // the next start (same port or not) begins a new count
            i.prev_url.clear();
        }
        if let Some(h) = self
            .engine
            .cache_snapshot()
            .get("hit_rate")
            .filter(|h| !h.is_null())
        {
            s.insert("cache_hit".into(), h.clone());
        }
        let mut i = self.inner.lock().unwrap();
        if i.metrics_available == Some(false) {
            if let Some(last) = i.responses.back().cloned() {
                s.insert("last_answer".into(), json!(true));
                s.insert("decode".into(), last["predicted_per_second"].clone());
                s.insert("prefill".into(), last["prompt_per_second"].clone());
                if crate::settings::py_truthy(&last["draft_n"]) {
                    s.insert(
                        "accept".into(),
                        json!(
                            last["draft_n_accepted"].as_f64().unwrap_or(0.0)
                                / last["draft_n"].as_f64().unwrap_or(1.0)
                        ),
                    );
                }
            }
        }
        i.history.push_back(Value::Object(s));
        if i.history.len() > 900 {
            i.history.pop_front();
        }
    }

    /// The last answers, newest first (the Engine page's list of requests).
    pub fn requests(&self) -> Value {
        let i = self.inner.lock().unwrap();
        json!({"requests": i.responses.iter().rev().cloned().collect::<Vec<_>>()})
    }

    pub fn snapshot(&self, since: f64) -> Value {
        self.touch();
        let i = self.inner.lock().unwrap();
        let hist: Vec<Value> = if since != 0.0 {
            i.history
                .iter()
                .filter(|h| h["t"].as_f64().unwrap_or(0.0) > since)
                .cloned()
                .collect()
        } else {
            i.history.iter().cloned().collect()
        };
        json!({"now": now(), "history": hist, "metrics_available": i.metrics_available, "last_response": last_timed(&i.responses),
            "totals": i.totals, "cache": self.engine.cache_snapshot(), "accept_logged": self.engine.accept_logged()})
    }
}

/// The last answer with engine timings, in the shape the Python Studio reports (the request list keeps the rest).
fn last_timed(r: &std::collections::VecDeque<Value>) -> Option<Value> {
    let mut v = r.iter().rev().find(|x| !x["prompt_ms"].is_null())?.clone();
    if let Some(m) = v.as_object_mut() {
        for k in ["api", "client", "seconds", "status", "estimated"] {
            m.remove(k);
        }
    }
    Some(v)
}
