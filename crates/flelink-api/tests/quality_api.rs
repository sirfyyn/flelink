//! Quality measurements end to end: KL divergence with a fake llama-perplexity next to the engine (the reference run saves
//! its base file once, later runs reuse it), the knowledge probe against a fake server that knows some answers, and the
//! probe after a speed test.
#![cfg(unix)]
mod common;
use common::{kv, tensors, write};
use flelink_api::run::start_embedded;
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PERPLEXITY: &str = r#"#!/usr/bin/env python3
import sys
a = sys.argv[1:]
base = a[a.index("--kl-divergence-base") + 1]
if "--kl-divergence" not in a:
    open(base, "w").write("logits")
    print("[1]0.1,[2]0.2,[3]0.3"); sys.exit(0)
if open(base).read() != "logits":
    sys.exit(2)
print("""[1]0.01,[2]0.02
====== KL divergence statistics ======
Mean    KLD:   0.012345 ±   0.000456
Maximum KLD:   1.234567
 5.0%   KLD:   0.000045
====== Token probability statistics ======
Same top p: 95.432 ± 0.567 %""")
"#;

const SERVER: &str = r#"#!/usr/bin/env python3
import sys
if "--version" in sys.argv: print("version: 6001 (abc123f)"); sys.exit(0)
if "--help" in sys.argv: print("-ngl N"); sys.exit(0)
"#;

/// Knows the capital of Australia and the boiling point of water, nothing else; streams for the speed test.
const CHAT: &str = r#"
import sys, json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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
        if body.get("stream"):
            self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.send_header("Connection", "close"); self.end_headers()
            self.wfile.write(b'data: {"choices": [{"delta": {"content": "x"}}]}\n\n')
            self.wfile.write(b'data: {"choices": [{"delta": {}, "finish_reason": "stop"}], "timings": {"predicted_per_second": 20.0, "prompt_per_second": 400.0}}\n\ndata: [DONE]\n\n')
            return
        q = body["messages"][-1]["content"].lower()
        a = "Canberra." if "australia" in q or "australien" in q else "100 degrees" if "boil" in q or "siede" in q else "I do not know"
        self._json(200, {"choices": [{"message": {"content": a}}]})
import socket; socket.getfqdn = lambda *a: "127.0.0.1"  # HTTPServer looks its name up while binding; on macOS CI that hangs
SRV = ThreadingHTTPServer(("127.0.0.1", 0), H)
print(SRV.server_address[1], flush=True)
SRV.serve_forever()
"#;

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn quality_measurements() {
    let home = Tmp(std::env::temp_dir().join(format!("flelink-quality-{}", std::process::id())));
    let models = home.0.join("models");
    std::fs::create_dir_all(&models).unwrap();
    write(
        &models.join("ref.gguf"),
        &kv("qwen3", "Ref", false),
        &tensors(false),
    );
    write(
        &models.join("cand.gguf"),
        &kv("qwen3", "Cand", false),
        &tensors(false),
    );
    let eng = home.0.join("engine");
    std::fs::create_dir_all(&eng).unwrap();
    use std::os::unix::fs::PermissionsExt;
    for (name, text) in [("llama-server", SERVER), ("llama-perplexity", PERPLEXITY)] {
        std::fs::write(eng.join(name), text).unwrap();
        std::fs::set_permissions(eng.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // the fake takes a free port itself and prints it (no race for a port picked here)
    let err = home.0.join("chat.err");
    let mut child = Command::new("python3")
        .args(["-c", CHAT])
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&err).unwrap())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut line = String::new();
        let _ = std::io::BufReader::new(out).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(60)).unwrap_or_default();
    let port: u16 = line.trim().parse().unwrap_or_else(|_| {
        panic!(
            "the fake server did not start ({line:?}, exit {:?}): {}",
            child.try_wait().ok().flatten(),
            std::fs::read_to_string(&err).unwrap_or_default()
        )
    });
    let _chat = Kill(child);
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
    let wait = || -> Value {
        let t = Instant::now();
        loop {
            let (_, v) = call("GET", "api/quality", json!({}));
            if v["current"]["state"] != "running" {
                return v;
            }
            assert!(t.elapsed() < Duration::from_secs(60), "still running: {v}");
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    call(
        "POST",
        "api/settings",
        json!({"engine": {"choice": "custom", "custom_path": eng.join("llama-server").to_string_lossy()}}),
    );
    let (_, ms) = call("GET", "api/models", json!({}));
    let id_of = |name: &str| -> String {
        ms["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["path"].as_str().unwrap_or("").ends_with(name))
            .map(|m| m["id"].as_str().unwrap().to_string())
            .unwrap()
    };
    let (rf, cd) = (id_of("ref.gguf"), id_of("cand.gguf"));
    let (_, v) = call("GET", "api/quality", json!({}));
    assert_eq!(v["builtin"]["license"], "CC0-1.0", "{v}");
    assert_eq!(v["kld_ctx"], 256);

    // KL divergence: the same model twice is refused; two runs, the second reuses the reference's base file
    let (c, _) = call(
        "POST",
        "api/quality/kld",
        json!({"reference": rf, "candidate": rf}),
    );
    assert_eq!(c, 400);
    let (c, v) = call(
        "POST",
        "api/quality/kld",
        json!({"reference": rf, "candidate": cd}),
    );
    assert_eq!(c, 200, "{v}");
    let v = wait();
    let cur = &v["current"];
    assert_eq!(cur["state"], "done", "{cur}");
    assert_eq!(cur["result"]["mean_kld"], 0.012345);
    assert_eq!(cur["result"]["kld_p5"], 0.000045);
    assert_eq!(cur["result"]["same_top_p"], 95.432);
    assert_eq!(
        cur["commands"].as_array().unwrap().len(),
        2,
        "reference and candidate"
    );
    assert!(
        cur["commands"][1]
            .as_str()
            .unwrap()
            .ends_with(" --kl-divergence"),
        "{cur}"
    );
    call(
        "POST",
        "api/quality/kld",
        json!({"reference": rf, "candidate": cd}),
    );
    let v = wait();
    assert_eq!(
        v["current"]["commands"].as_array().unwrap().len(),
        1,
        "the base file is reused"
    );
    assert_eq!(
        v["kld_by_model"]["Cand"]["mean_kld"], 0.012345,
        "{}",
        v["kld_by_model"]
    );

    // the knowledge probe asks the running engine
    let (c, _) = call("POST", "api/quality/probe", json!({}));
    assert_eq!(c, 400, "no engine yet");
    let (c, v) = call(
        "POST",
        "api/engine/attach",
        json!({"url": format!("http://127.0.0.1:{port}")}),
    );
    assert_eq!(c, 200, "{v}");
    let (c, v) = call(
        "POST",
        "api/quality/probe",
        json!({"lang": "en", "label": "fake"}),
    );
    assert_eq!(c, 200, "{v}");
    let v = wait();
    let r = &v["current"]["result"];
    assert_eq!(v["current"]["state"], "done", "{}", v["current"]);
    let n = r["n"].as_u64().unwrap();
    let correct = r["correct"].as_u64().unwrap();
    assert!(n > 0 && correct >= 1 && correct < n, "{r}");
    assert!(r["ci_lo"].as_f64().unwrap() <= r["accuracy"].as_f64().unwrap());
    assert!(
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["ok"] == true && i["answer"] == "Canberra."),
        "{r}"
    );
    assert_eq!(v["results"].as_array().unwrap().len(), 3);

    // the probe after a speed test, in the same record
    let (c, v) = call(
        "POST",
        "api/bench/start",
        json!({"runs": 1, "knowledge": true}),
    );
    assert_eq!(c, 200, "{v}");
    let t = Instant::now();
    let b = loop {
        let (_, b) = call("GET", "api/bench", json!({}));
        if b["current"]["state"] != "running" {
            break b;
        }
        assert!(t.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(b["current"]["state"], "done", "{}", b["current"]);
    assert_eq!(b["current"]["context"]["knowledge_set"], "builtin");
    assert!(b["current"]["quality"]["n"].as_u64().unwrap() > 0);
    let (c, _) = call(
        "POST",
        "api/bench/start",
        json!({"runs": 1, "knowledge": true, "knowledge_set": "/nope.json"}),
    );
    assert_eq!(c, 400, "a bad question set fails before the speed requests");

    let id = v["id"].as_str().unwrap_or("").to_string();
    let _ = id;
    let (_, q) = call("GET", "api/quality", json!({}));
    let first = q["results"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        call("POST", &format!("api/quality/{first}/delete"), json!({})).0,
        200
    );
    assert_eq!(
        call("GET", "api/quality", json!({})).1["results"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    s.stop();
}
