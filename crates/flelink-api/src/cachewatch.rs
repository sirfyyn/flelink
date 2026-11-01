//! A look into the FLE expert cache: how it fills, the hit rate over time and per layer, the hot experts. Port of
//! `fle_studio/cachewatch.py`.
//!
//! The data comes from the lines the FLE engine prints to its log (format: docs/EXPERT-CACHE-DATA.md in FastLocalExperts):
//!
//! ```text
//! MoE expert cache enabled: 40 layers x 96 slots, 1 inserts/step, 14984.0 MiB device memory
//! moe-cache: steps=512 hits=123 misses=45 hit-rate=73.2%
//! moe-layer: steps=512 layer=7 hits=1234 misses=210 filled=88 slots=96
//! moe-hot: steps=512 layer=7 top=311:812,27:790,402:655
//! ```
//!
//! Counts are cumulative since the engine started, so the view subtracts neighbours for the rate of an interval. A new
//! "cache enabled" line, a new start (`$ command`), or steps that go backwards start a new run. Each log line costs one
//! substring check unless it is a cache line.
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock};

/// per series; older points drop out
const MAX_POINTS: usize = 400;

struct Rx {
    enabled: Regex,
    size: Regex,
    overall: Regex,
    layer: Regex,
    hot: Regex,
}

fn rx() -> &'static Rx {
    static R: OnceLock<Rx> = OnceLock::new();
    R.get_or_init(|| Rx {
        enabled: Regex::new(r"(?i)MoE expert cache enabled[:,]?\s*(?P<text>.*)").unwrap(),
        size: Regex::new(r"(?i)(\d+)\s+layers\s*x\s*(\d+)\s+slots").unwrap(),
        overall: Regex::new(
            r"moe-cache:.*?(?:steps=(\d+).*?)?hits=(\d+).*?misses=(\d+).*?hit-rate=([\d.]+)\s*%",
        )
        .unwrap(),
        layer: Regex::new(
            r"moe-layer:\s+steps=(\d+)\s+layer=(\d+)\s+hits=(\d+)\s+misses=(\d+)(?:\s+filled=(\d+))?(?:\s+slots=(\d+))?",
        )
        .unwrap(),
        hot: Regex::new(r"moe-hot:\s+steps=(\d+)\s+layer=(\d+)\s+top=([\d:,]*)").unwrap(),
    })
}

fn rate(hits: i64, misses: i64) -> Value {
    let total = hits + misses;
    if total > 0 {
        json!(crate::stats::round_to(
            100.0 * hits as f64 / total as f64,
            2
        ))
    } else {
        Value::Null
    }
}

#[derive(Default)]
struct State {
    layers_total: i64,
    slots_per_layer: i64,
    enabled_text: String,
    steps: i64,
    overall: VecDeque<Value>,
    layer_points: BTreeMap<i64, VecDeque<Value>>,
    hot: BTreeMap<i64, Vec<(i64, i64)>>,
    hot_steps: i64,
    lines_seen: i64,
}

#[derive(Default)]
pub struct CacheWatch {
    st: Mutex<State>,
}

fn push(q: &mut VecDeque<Value>, v: Value) {
    q.push_back(v);
    if q.len() > MAX_POINTS {
        q.pop_front();
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl CacheWatch {
    pub fn reset(&self) {
        *self.st.lock().unwrap() = State::default();
    }

    /// Read one engine log line; true when it was a cache line (or the start of a new run).
    pub fn feed(&self, line: &str) -> bool {
        self.feed_at(line, now())
    }

    pub fn feed_at(&self, line: &str, now: f64) -> bool {
        if line.starts_with("$ ") {
            self.reset();
            return true;
        }
        if !line.contains("moe-") && !line.contains("MoE expert cache") {
            return false;
        }
        let r = rx();
        let int = |m: &regex::Captures, i: usize| -> Option<i64> {
            m.get(i).and_then(|x| x.as_str().parse().ok())
        };
        if let Some(m) = r.enabled.captures(line) {
            let mut s = self.st.lock().unwrap();
            *s = State::default();
            s.enabled_text = m["text"].trim().to_string();
            if let Some(z) = r.size.captures(&s.enabled_text.clone()) {
                s.layers_total = z[1].parse().unwrap_or(0);
                s.slots_per_layer = z[2].parse().unwrap_or(0);
            }
            s.lines_seen += 1;
            return true;
        }
        if let Some(m) = r.overall.captures(line) {
            let (steps, hits, misses) = (
                int(&m, 1).unwrap_or(0),
                int(&m, 2).unwrap_or(0),
                int(&m, 3).unwrap_or(0),
            );
            let hr: f64 = m[4].parse().unwrap_or(0.0);
            let mut s = self.st.lock().unwrap();
            Self::restart_if_steps_went_back(&mut s, steps);
            let recent = match s.overall.back() {
                Some(p) => rate(
                    hits - p["hits"].as_i64().unwrap_or(0),
                    misses - p["misses"].as_i64().unwrap_or(0),
                ),
                None => json!(hr),
            };
            let point = json!({"t": crate::stats::round_to(now, 2), "steps": steps, "hits": hits, "misses": misses,
                "hit_rate": hr, "recent_rate": recent});
            push(&mut s.overall, point);
            s.steps = s.steps.max(steps);
            s.lines_seen += 1;
            return true;
        }
        if let Some(m) = r.layer.captures(line) {
            let (steps, layer, hits, misses) = (
                int(&m, 1).unwrap_or(0),
                int(&m, 2).unwrap_or(0),
                int(&m, 3).unwrap_or(0),
                int(&m, 4).unwrap_or(0),
            );
            let (filled, slots) = (int(&m, 5), int(&m, 6));
            let mut s = self.st.lock().unwrap();
            Self::restart_if_steps_went_back(&mut s, steps);
            let series = s.layer_points.entry(layer).or_default();
            let recent = match series.back() {
                Some(p) => rate(
                    hits - p["hits"].as_i64().unwrap_or(0),
                    misses - p["misses"].as_i64().unwrap_or(0),
                ),
                None => Value::Null,
            };
            push(
                series,
                json!({"t": crate::stats::round_to(now, 2), "steps": steps, "hits": hits, "misses": misses,
                    "filled": filled, "slots": slots, "hit_rate": rate(hits, misses), "recent_rate": recent}),
            );
            s.steps = s.steps.max(steps);
            s.lines_seen += 1;
            return true;
        }
        if let Some(m) = r.hot.captures(line) {
            let top: Vec<(i64, i64)> = m[3]
                .split(',')
                .filter_map(|p| {
                    let (e, c) = p.split_once(':').unwrap_or((p, ""));
                    Some((e.parse().ok()?, c.parse().ok()?))
                })
                .collect();
            let (steps, layer) = (int(&m, 1).unwrap_or(0), int(&m, 2).unwrap_or(0));
            let mut s = self.st.lock().unwrap();
            s.hot.insert(layer, top);
            s.hot_steps = s.hot_steps.max(steps);
            s.lines_seen += 1;
            return true;
        }
        false
    }

    fn restart_if_steps_went_back(s: &mut State, steps: i64) {
        if steps != 0 && steps < s.steps {
            let keep = (
                std::mem::take(&mut s.enabled_text),
                s.layers_total,
                s.slots_per_layer,
            );
            *s = State::default();
            (s.enabled_text, s.layers_total, s.slots_per_layer) = keep;
        }
    }

    /// `GET /api/cache`: everything the Cache page shows; `layer` adds that layer's time series.
    pub fn snapshot(&self, layer: Option<i64>) -> Value {
        let s = self.st.lock().unwrap();
        let overall: Vec<Value> = s.overall.iter().cloned().collect();
        let latest: Vec<(i64, Value)> = s
            .layer_points
            .iter()
            .filter_map(|(n, q)| q.back().map(|p| (*n, p.clone())))
            .collect();
        let hot: Map<String, Value> = s
            .hot
            .iter()
            .map(|(n, top)| {
                (
                    n.to_string(),
                    json!(top.iter().map(|(e, c)| json!([e, c])).collect::<Vec<_>>()),
                )
            })
            .collect();
        let fill = Self::fill_series(&s);
        let available = json!({"overall": !overall.is_empty(), "layers": !latest.is_empty(), "hot": !hot.is_empty()});
        let mut out = json!({
            "available": available,
            "enabled": if s.enabled_text.is_empty() { Value::Null } else {
                json!({"layers": s.layers_total, "slots": s.slots_per_layer, "text": s.enabled_text}) },
            "steps": s.steps,
            "lines": s.lines_seen,
        });
        out["overall"] = json!({"series": overall,
            "hit_rate": overall.last().map_or(Value::Null, |p| p["hit_rate"].clone()),
            "recent_rate": overall.last().map_or(Value::Null, |p| p["recent_rate"].clone())});
        let layers: Vec<Value> = latest
            .iter()
            .map(|(n, p)| {
                let slots = p["slots"]
                    .as_i64()
                    .filter(|v| *v != 0)
                    .or(Some(s.slots_per_layer).filter(|v| *v != 0));
                let fill = match (p["filled"].as_i64(), slots) {
                    (Some(f), Some(sl)) => json!(crate::stats::round_to(f as f64 / sl as f64, 4)),
                    _ => Value::Null,
                };
                json!({"layer": n, "hits": p["hits"], "misses": p["misses"], "hit_rate": p["hit_rate"],
                    "recent_rate": p["recent_rate"], "filled": p["filled"], "slots": slots, "fill": fill})
            })
            .collect();
        out["layers"] = json!(layers);
        out["fill"] =
            json!({"series": fill, "now": fill.last().map_or(Value::Null, |p| p["fill"].clone())});
        out["hot"] = json!({"steps": s.hot_steps, "layers": hot});
        if let Some(l) = layer {
            let series: Vec<Value> = s
                .layer_points
                .get(&l)
                .map(|q| q.iter().cloned().collect())
                .unwrap_or_default();
            out["layer_series"] = json!({"layer": l, "series": series});
        }
        out["missing"] = json!(Self::missing(&out["available"]));
        out
    }

    /// The share of cache slots in use over time, over all layers that reported at the same step count.
    fn fill_series(s: &State) -> Vec<Value> {
        let mut by_steps: BTreeMap<i64, Vec<(i64, i64, f64)>> = BTreeMap::new();
        for series in s.layer_points.values() {
            for p in series {
                let slots = p["slots"]
                    .as_i64()
                    .filter(|v| *v != 0)
                    .unwrap_or(s.slots_per_layer);
                if let Some(f) = p["filled"].as_i64() {
                    if slots != 0 {
                        by_steps
                            .entry(p["steps"].as_i64().unwrap_or(0))
                            .or_default()
                            .push((f, slots, p["t"].as_f64().unwrap_or(0.0)));
                    }
                }
            }
        }
        let out: Vec<Value> = by_steps
            .iter()
            .map(|(steps, rows)| {
                let total: i64 = rows.iter().map(|r| r.1).sum();
                let filled: i64 = rows.iter().map(|r| r.0).sum();
                json!({"steps": steps, "t": rows.iter().map(|r| r.2).fold(f64::MIN, f64::max),
                    "fill": crate::stats::round_to(filled as f64 / total as f64, 4), "layers": rows.len()})
            })
            .collect();
        let skip = out.len().saturating_sub(MAX_POINTS);
        out[skip..].to_vec()
    }

    /// What the engine does not print (yet), in words for the page.
    fn missing(available: &Value) -> Vec<&'static str> {
        let mut out = vec![];
        if available["overall"] != true {
            out.push("The overall hit rate (the `moe-cache:` line) has not appeared yet. The engine prints it every 512 decode steps, and only at log verbosity 4 (FLE Link starts it with `-lv 4`).");
        }
        if available["layers"] != true {
            out.push("Per-layer hit rate and fill need `moe-layer:` lines, which this engine does not print yet (format: docs/EXPERT-CACHE-DATA.md).");
        }
        if available["hot"] != true {
            out.push("The hot experts need `moe-hot:` lines, which this engine does not print yet (format: docs/EXPERT-CACHE-DATA.md).");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const LINES: [&str; 10] = [
        "$ llama-server -m x.gguf",
        "MoE expert cache enabled: 4 layers x 8 slots, 1 inserts/step, 100.0 MiB device memory",
        "moe-cache: steps=512 hits=100 misses=50 hit-rate=66.7%",
        "moe-layer: steps=512 layer=0 hits=40 misses=10 filled=6 slots=8",
        "moe-layer: steps=512 layer=1 hits=30 misses=20 filled=4",
        "moe-hot: steps=512 layer=0 top=3:40,1:22,7:5",
        "moe-cache: steps=1024 hits=260 misses=90 hit-rate=74.3%",
        "moe-layer: steps=1024 layer=0 hits=100 misses=20 filled=8 slots=8",
        "srv  log_server_r: request: POST /v1/chat/completions 200",
        "moe-layer: steps=1024 layer=1 hits=70 misses=30 filled=7",
    ];

    /// cachewatch.CacheWatch fed the lines above at t = 1000.0, 1001.0, ... (Python Studio 055f4aa)
    #[test]
    fn the_snapshot_is_the_python_studios() {
        let c = CacheWatch::default();
        let fed: Vec<bool> = LINES
            .iter()
            .enumerate()
            .map(|(i, l)| c.feed_at(l, 1000.0 + i as f64))
            .collect();
        assert_eq!(json!(fed), serde_json::from_str::<Value>(PY_FED).unwrap());
        let want: Value = serde_json::from_str(PY_SNAPSHOT).unwrap();
        assert_eq!(want["missing"], json!([]), "every kind of line was fed");
        assert_eq!(c.snapshot(Some(1)), want);
    }

    const PY_FED: &str = r#"[true, true, true, true, true, true, true, true, false, true]"#;
    const PY_SNAPSHOT: &str = r#"{"available": {"overall": true, "layers": true, "hot": true}, "enabled": {"layers": 4, "slots": 8, "text": "4 layers x 8 slots, 1 inserts/step, 100.0 MiB device memory"}, "steps": 1024, "lines": 8, "overall": {"series": [{"t": 1002.0, "steps": 512, "hits": 100, "misses": 50, "hit_rate": 66.7, "recent_rate": 66.7}, {"t": 1006.0, "steps": 1024, "hits": 260, "misses": 90, "hit_rate": 74.3, "recent_rate": 80.0}], "hit_rate": 74.3, "recent_rate": 80.0}, "layers": [{"layer": 0, "hits": 100, "misses": 20, "hit_rate": 83.33, "recent_rate": 85.71, "filled": 8, "slots": 8, "fill": 1.0}, {"layer": 1, "hits": 70, "misses": 30, "hit_rate": 70.0, "recent_rate": 80.0, "filled": 7, "slots": 8, "fill": 0.875}], "fill": {"series": [{"steps": 512, "t": 1004.0, "fill": 0.625, "layers": 2}, {"steps": 1024, "t": 1009.0, "fill": 0.9375, "layers": 2}], "now": 0.9375}, "hot": {"steps": 512, "layers": {"0": [[3, 40], [1, 22], [7, 5]]}}, "layer_series": {"layer": 1, "series": [{"t": 1004.0, "steps": 512, "hits": 30, "misses": 20, "filled": 4, "slots": null, "hit_rate": 60.0, "recent_rate": null}, {"t": 1009.0, "steps": 1024, "hits": 70, "misses": 30, "filled": 7, "slots": null, "hit_rate": 70.0, "recent_rate": 80.0}]}, "missing": []}"#;
}
