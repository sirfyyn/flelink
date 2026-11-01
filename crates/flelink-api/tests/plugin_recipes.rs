//! A recipe that prefers a plugin engine starts from the Studio: a template engine with its own readiness path, and a
//! llama-server compatible engine from a path that gets the automatic arguments plus the plugin's own.
#![cfg(unix)]
mod common;
use common::{kv, tensors, write};
use flelink_api::run::start_embedded;
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

const FAKE: &str = r#"#!/usr/bin/env python3
import sys, json, threading, time, signal, os
from http.server import BaseHTTPRequestHandler, HTTPServer
a = sys.argv[1:]
if "--version" in a:
    print("version: 1234 (abcdef0)"); sys.exit(0)
if "--help" in a:
    print("--log-verbosity N"); sys.exit(0)
port = int(a[a.index("--port") + 1])
ready = os.environ.get("FAKE_READY", "/health")
print("args: " + " ".join(a), flush=True)
class H(BaseHTTPRequestHandler):
    def log_message(self, *x): pass
    def do_GET(self):
        code = 200 if self.path in (ready, "/props") else 404
        b = json.dumps({"ok": code == 200}).encode()
        self.send_response(code); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
import socket; socket.getfqdn = lambda *a: "127.0.0.1"  # HTTPServer looks its name up while binding; on macOS CI that hangs
srv = HTTPServer(("127.0.0.1", port), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
signal.signal(signal.SIGTERM, lambda *_: os._exit(0))
while True:
    time.sleep(0.2)
"#;

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn put(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn seed(home: &Path) {
    let fake = home.join("fake-server");
    put(&fake, FAKE);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let model = home.join("models/test__tiny/tiny.gguf");
    std::fs::create_dir_all(model.parent().unwrap()).unwrap();
    write(&model, &kv("qwen3", "Tiny", false), &tensors(false));
    let f = fake.to_string_lossy();
    put(
        &home.join("plugins/tpl/plugin.toml"),
        &format!(
            r#"id = "tpl"
title = "Template"
[[engine]]
id = "tpl-engine"
label = "Template engine"
path = "{f}"
command = ["{{binary}}", "--model", "{{model}}", "--port", "{{port}}", "--ctx", "{{ctx}}", "--name", "{{alias}}"]
health = "/ready"
env = {{ FAKE_READY = "/ready" }}
"#
        ),
    );
    put(
        &home.join("plugins/tpl/recipes/t.toml"),
        r#"id = "tiny-tpl"
title = "Tiny via template"
[model]
repo = "test/tiny"
files = ["tiny.gguf"]
[engine]
prefer = ["tpl-engine"]
strict = true
[run]
context = 3072
"#,
    );
    put(
        &home.join("plugins/fork/plugin.toml"),
        &format!(
            r#"id = "fork"
title = "A fork"
[[engine]]
id = "fork-engine"
label = "Fork"
path = "{f}"
args = ["--fork-flag"]
"#
        ),
    );
    put(
        &home.join("plugins/fork/recipes/f.toml"),
        r#"id = "tiny-fork"
title = "Tiny via fork"
[model]
repo = "test/tiny"
files = ["tiny.gguf"]
[engine]
prefer = ["fork-engine"]
strict = true
[run]
context = 2048
"#,
    );
    put(
        &home.join("recipes/ghost.toml"),
        r#"id = "tiny-ghost"
title = "Tiny via a missing plugin"
[model]
repo = "test/tiny"
files = ["tiny.gguf"]
[engine]
prefer = ["ghost-engine"]
strict = true
"#,
    );
}

struct Api {
    agent: ureq::Agent,
    url: String,
    token: String,
}

impl Api {
    fn call(&self, method: &str, path: &str) -> (u16, Value) {
        let r = self
            .agent
            .request(method, &format!("{}{path}", self.url))
            .set("X-FLE-Token", &self.token)
            .set("Content-Type", "application/json");
        let r = if method == "POST" {
            r.send_string("{}")
        } else {
            r.call()
        };
        let (code, body) = match r {
            Ok(r) => (r.status(), r.into_string().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap()),
            Err(e) => panic!("{e}"),
        };
        (
            code,
            serde_json::from_str(&body).unwrap_or(json!({"raw": body})),
        )
    }

    fn wait(&self, want: &str) -> Value {
        let t = Instant::now();
        loop {
            let (_, s) = self.call("GET", "api/engine");
            if s["state"] == want {
                return s;
            }
            assert!(
                t.elapsed() < Duration::from_secs(30) && s["state"] != "crashed",
                "still {} ({}); engine log:\n{}",
                s["state"],
                s["error"],
                self.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn log(&self) -> String {
        let (_, v) = self.call("GET", "api/engine/log?since=0");
        v["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["text"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[test]
fn plugin_recipes_start_from_the_studio() {
    let home =
        Tmp(std::env::temp_dir().join(format!("flelink-plugin-recipes-{}", std::process::id())));
    seed(&home.0);
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
    let api = Api {
        agent,
        url: s.url.clone(),
        token,
    };

    // a strict recipe whose plugin is not there says so
    let (code, v) = api.call("POST", "api/recipes/tiny-ghost/start");
    assert_eq!(code, 409, "{v}");
    assert!(v.to_string().contains("not installed"), "{v}");

    // the template: its own arguments, its own readiness path
    let (code, v) = api.call("POST", "api/recipes/tiny-tpl/start");
    assert_eq!(code, 200, "{v}");
    api.wait("ready");
    let log = api.log();
    let args = log.lines().find(|l| l.starts_with("args: ")).unwrap_or("");
    assert!(
        args.contains("--ctx 3072") && args.contains("--name tiny-tpl"),
        "{log}"
    );
    assert!(
        args.contains("tiny.gguf") && !args.contains(" -c "),
        "{log}"
    );
    api.call("POST", "api/engine/stop");
    api.wait("stopped");

    // the fork: automatic arguments with the recipe's context, and the plugin's flag
    let (code, v) = api.call("POST", "api/recipes/tiny-fork/start");
    assert_eq!(code, 200, "{v}");
    api.wait("ready");
    let log = api.log();
    let args = log.lines().rfind(|l| l.starts_with("args: ")).unwrap_or("");
    assert!(
        args.contains("-m ") && args.contains("--fork-flag"),
        "{log}"
    );
    assert!(
        args.contains("-c 2048") || args.contains("--ctx-size 2048"),
        "{log}"
    );
    api.call("POST", "api/engine/stop");
    api.wait("stopped");
    s.stop();
}
