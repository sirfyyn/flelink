//! The speed test end to end: the Studio attaches to a fake server that streams answers with llama-server's timings, runs
//! the three requests, stores the result and compares a second run of the same setting with the first.
#![cfg(unix)]
use flelink_api::run::start_embedded;
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FAKE: &str = r#"
import sys, json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
port = int(sys.argv[1])
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *x): pass
    def _json(self, code, body):
        b = json.dumps(body).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path == "/health": self._json(200, {"status": "ok"})
        elif self.path == "/props": self._json(200, {"model_path": "/m/fake.gguf", "build_info": "b1-x", "default_generation_settings": {"n_ctx": 4096}})
        elif self.path == "/v1/models": self._json(200, {"data": [{"id": "fake"}]})
        else: self._json(404, {})
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        long = body["max_tokens"] == 128
        self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.send_header("Connection", "close"); self.end_headers()
        def send(o): self.wfile.write(("data: " + json.dumps(o) + "\n\n").encode()); self.wfile.flush()
        send({"choices": [{"delta": {"content": "Hi"}}]})
        send({"choices": [{"delta": {}, "finish_reason": "stop"}], "timings": {"prompt_n": 6000 if long else 30,
              "prompt_per_second": 900.0 if long else 300.0, "predicted_n": 40, "predicted_per_second": 25.0 + body["seed"] % 7,
              "draft_n": 10, "draft_n_accepted": 7}})
        self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
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

fn call(agent: &ureq::Agent, token: &str, method: &str, url: &str, body: Value) -> (u16, Value) {
    let r = agent
        .request(method, url)
        .set("X-FLE-Token", token)
        .set("Content-Type", "application/json");
    let r = if method == "POST" {
        r.send_string(&body.to_string())
    } else {
        r.call()
    };
    let (code, text) = match r {
        Ok(r) => (r.status(), r.into_string().unwrap()),
        Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap()),
        Err(e) => panic!("{e}"),
    };
    (
        code,
        serde_json::from_str(&text).unwrap_or(json!({"raw": text})),
    )
}

#[test]
fn the_speed_test_measures_stores_and_compares() {
    let home = Tmp(std::env::temp_dir().join(format!("flelink-bench-{}", std::process::id())));
    std::fs::create_dir_all(&home.0).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let _fake = Kill(
        Command::new("python3")
            .args(["-c", FAKE, &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let t = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "the fake server did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut s = start_embedded(Some(home.0.clone()), None).expect("start");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
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
    let api = |m: &str, p: &str, b: Value| call(&agent, &token, m, &format!("{}{p}", s.url), b);

    let (code, v) = api("POST", "api/bench/start", json!({"runs": 1}));
    assert_eq!(code, 409, "no engine yet: {v}");
    let (code, v) = api(
        "POST",
        "api/engine/attach",
        json!({"url": format!("http://127.0.0.1:{port}")}),
    );
    assert_eq!(code, 200, "{v}");
    let (code, v) = api(
        "POST",
        "api/bench/start",
        json!({"runs": 1, "knowledge": true}),
    );
    assert_eq!(code, 501, "{v}");

    let run = |label: &str| -> Value {
        let (code, v) = api(
            "POST",
            "api/bench/start",
            json!({"runs": 2, "label": label}),
        );
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["total"], 6);
        let t = Instant::now();
        loop {
            let (_, b) = api("GET", "api/bench", json!({}));
            if b["current"]["state"] != "running" {
                return b;
            }
            assert!(t.elapsed() < Duration::from_secs(30), "still running: {b}");
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let b = run("first");
    let cur = &b["current"];
    assert_eq!(cur["state"], "done", "{cur}");
    assert_eq!(cur["tests"].as_array().unwrap().len(), 6);
    assert_eq!(cur["context"]["profile"], "attached");
    assert_eq!(cur["summary"]["prefill"], 900.0);
    assert_eq!(cur["summary"]["accept"], 0.7);
    assert_eq!(cur["summary"]["decode_runs"].as_array().unwrap().len(), 2);
    assert!(cur["summary"]["ttft_ms"].is_number());
    assert!(cur.get("compare").is_none(), "nothing to compare with yet");
    assert_eq!(b["results"].as_array().unwrap().len(), 1);
    assert_eq!(b["tests"], json!(["Short answer", "Code", "Long prompt"]));

    let b = run("second");
    let cmp = &b["current"]["compare"];
    assert_eq!(cmp["comparable"], true, "{cmp}");
    assert_eq!(cmp["change_pct"], 0.0);
    assert_eq!(cmp["previous_label"], "first");
    assert_eq!(cmp["series"].as_array().unwrap().len(), 2);
    let results = b["results"].as_array().unwrap().clone();
    assert_eq!(results.len(), 2);

    let id = results[0]["id"].as_str().unwrap();
    let (code, _) = api("POST", &format!("api/bench/{id}/delete"), json!({}));
    assert_eq!(code, 200);
    let (_, b) = api("GET", "api/bench", json!({}));
    assert_eq!(b["results"].as_array().unwrap().len(), 1);
    s.stop();
}
