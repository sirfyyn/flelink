//! The tuner and the A/B/A comparison end to end: the Studio starts a fake llama-server once per setting (more threads are
//! faster there), measures it, keeps the faster setting, saves it as the tuned profile, and compares two settings A/B/A.
#![cfg(unix)]
mod common;
use common::{kv, tensors, write};
use flelink_api::run::start_embedded;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const FAKE: &str = r#"#!/usr/bin/env python3
import sys, json, threading, time, signal, os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
a = sys.argv[1:]
if "--version" in a:
    print("version: 6001 (abc123f)"); sys.exit(0)
if "--help" in a:
    print("-ngl N\n--jinja\n--log-verbosity N"); sys.exit(0)
port = int(a[a.index("--port") + 1])
threads = int(a[a.index("-t") + 1]) if "-t" in a else 2
print("args: " + " ".join(a), flush=True)
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *x): pass
    def _json(self, code, body):
        b = json.dumps(body).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path == "/health": self._json(200, {"status": "ok"})
        elif self.path == "/props": self._json(200, {"model_path": "m", "default_generation_settings": {"n_ctx": 4096}})
        else: self._json(404, {})
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.send_header("Connection", "close"); self.end_headers()
        def send(o): self.wfile.write(("data: " + json.dumps(o) + "\n\n").encode()); self.wfile.flush()
        send({"choices": [{"delta": {"content": "Hi"}}]})
        wobble = (body["seed"] % 3) * 0.1
        send({"choices": [{"delta": {}, "finish_reason": "stop"}], "timings": {"prompt_n": 100, "prompt_per_second": 500.0 + wobble,
              "predicted_n": 20, "predicted_per_second": 5.0 * threads + wobble}})
        self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
srv = ThreadingHTTPServer(("127.0.0.1", port), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
signal.signal(signal.SIGTERM, lambda *_: os._exit(0))
print("MoE expert cache enabled: 4 layers x 8 slots, 1 inserts/step, 100.0 MiB device memory", flush=True)
print("moe-cache: steps=512 hits=300 misses=100 hit-rate=75.0%", flush=True)
print("moe-layer: steps=512 layer=2 hits=90 misses=10 filled=6 slots=8", flush=True)
print("srv main: listening on http://127.0.0.1:%d" % port, flush=True)
while True:
    time.sleep(0.2)
"#;

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_tuner_keeps_what_is_faster_and_aba_compares() {
    let home = Tmp(std::env::temp_dir().join(format!("flelink-tuner-{}", std::process::id())));
    let models = home.0.join("models");
    std::fs::create_dir_all(&models).unwrap();
    write(
        &models.join("tiny.gguf"),
        &kv("qwen3", "Tiny", false),
        &tensors(false),
    );
    let fake = home.0.join("fake-llama-server");
    std::fs::write(&fake, FAKE).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut s = start_embedded(Some(home.0.clone()), None).expect("start");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build();
    let page = agent.get(&s.url).call().unwrap().into_string().unwrap();
    let token = page
        .split("name=\"fle-token\" content=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    let call = |m: &str, p: &str, b: Value| -> (u16, Value) {
        let r = agent
            .request(m, &format!("{}{p}", s.url))
            .set("X-FLE-Token", &token)
            .set("Content-Type", "application/json");
        let r = if m == "POST" {
            r.send_string(&b.to_string())
        } else {
            r.call()
        };
        let (c, t) = match r {
            Ok(r) => (r.status(), r.into_string().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap()),
            Err(e) => panic!("{e}"),
        };
        (c, serde_json::from_str(&t).unwrap_or(json!({"raw": t})))
    };
    let (c, v) = call(
        "POST",
        "api/settings",
        json!({"engine": {"choice": "custom", "custom_path": fake.to_string_lossy()},
               "launch": {"fallback_on_failure": false}}),
    );
    assert_eq!(c, 200, "{v}");
    let (_, models_v) = call("GET", "api/models", json!({}));
    let model = models_v["models"][0]["id"].as_str().unwrap().to_string();

    let (c, v) = call("GET", "api/tuning", json!({}));
    assert_eq!(c, 200);
    assert_eq!(v["defaults"]["runs"], 3);
    assert_eq!(v["params"].as_array().unwrap().len(), 5);
    let (c, v) = call(
        "POST",
        "api/tuning/aba",
        json!({"model": model, "a": "auto"}),
    );
    assert_eq!(c, 400, "{v}");

    let wait = || -> Value {
        let t = Instant::now();
        loop {
            let (_, v) = call("GET", &format!("api/tuning?model={model}"), json!({}));
            if v["current"]["state"] != "running" {
                return v;
            }
            assert!(
                t.elapsed() < Duration::from_secs(120),
                "still running: {}",
                v["current"]
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    };

    // the tuner: threads only, so the run is short; more threads are faster in the fake
    let (c, v) = call(
        "POST",
        "api/tuning/start",
        json!({"model": model, "params": ["threads"], "runs": 2, "budget_min": 10}),
    );
    assert_eq!(c, 200, "{v}");
    let (c, _) = call("POST", "api/tuning/start", json!({"model": model}));
    assert_eq!(c, 409, "a second run while one runs");
    let v = wait();
    let cur = &v["current"];
    assert_eq!(cur["state"], "done", "{cur}");
    assert_eq!(cur["probe"]["ok"], true);
    let trials = cur["trials"].as_array().unwrap();
    assert!(!trials.is_empty(), "{cur}");
    assert!(
        cur["best"]["label"].as_str().unwrap().ends_with("threads"),
        "{cur}"
    );
    assert!(trials.iter().any(|t| t["kept"] == true), "{cur}");
    assert_eq!(cur["profile"]["id"], format!("tuned-{model}"));
    assert!(
        v["best"]["id"].is_string(),
        "the tuned profile is the best for this machine: {}",
        v["best"]
    );
    let (_, e) = call("GET", "api/engine", json!({}));
    assert_eq!(e["state"], "stopped", "the engine is stopped at the end");

    // A/B/A: automatic settings against the tuned profile, which is faster
    let tuned = cur["profile"]["id"].as_str().unwrap().to_string();
    let (c, v) = call(
        "POST",
        "api/tuning/aba",
        json!({"model": model, "a": "auto", "b": tuned, "runs": 3, "label": "tuned vs auto"}),
    );
    assert_eq!(c, 200, "{v}");
    let v = wait();
    let cur = &v["current"];
    assert_eq!(cur["state"], "done", "{cur}");
    assert_eq!(cur["trials"].as_array().unwrap().len(), 3);
    assert_eq!(
        cur["result"]["decode"]["verdict"], "better",
        "{}",
        cur["result"]
    );
    assert_eq!(v["results"].as_array().unwrap().len(), 2);

    // the expert cache view follows the engine log
    let (c, started) = call("POST", "api/engine/start", json!({"model": model}));
    assert_eq!(c, 200, "{started}");
    let t = Instant::now();
    let cache = loop {
        let (_, c) = call("GET", "api/cache?layer=2", json!({}));
        if c["available"]["layers"] == true || t.elapsed() > Duration::from_secs(20) {
            break c;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(cache["enabled"]["layers"], 4, "{cache}");
    assert_eq!(cache["overall"]["hit_rate"], 75.0);
    assert_eq!(cache["layers"][0]["fill"], 0.75);
    assert_eq!(cache["layer_series"]["series"].as_array().unwrap().len(), 1);
    assert_eq!(
        cache["missing"].as_array().unwrap().len(),
        1,
        "only the hot experts are missing"
    );
    call("POST", "api/engine/stop", json!({}));

    let id = v["results"][0]["id"].as_str().unwrap();
    assert_eq!(
        call("POST", &format!("api/tuning/{id}/delete"), json!({})).0,
        200
    );
    assert_eq!(
        call("GET", "api/tuning", json!({})).1["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    s.stop();
}
