//! `flelink run` ends its engine when only the program gets a signal (a service manager, `kill`), not just on Ctrl+C in a
//! terminal, which reaches the whole process group. Found on titan: after `kill -INT` the engine kept running.
#![cfg(unix)]
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// An engine that answers /health, writes its pid and ignores SIGINT (so only FLE Link can end it here).
const FAKE: &str = r#"#!/usr/bin/env python3
import sys, os, signal, socket, threading, time
socket.getfqdn = lambda *a: "127.0.0.1"
from http.server import BaseHTTPRequestHandler, HTTPServer
a = sys.argv[1:]
if "--version" in a: print("version: 1 (abc)"); sys.exit(0)
if "--help" in a: print("--port N"); sys.exit(0)
signal.signal(signal.SIGINT, signal.SIG_IGN)
open(os.environ["FAKE_PID"], "w").write(str(os.getpid()))
port = int(a[a.index("--port") + 1])
class H(BaseHTTPRequestHandler):
    def log_message(self, *x): pass
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Length", "2"); self.end_headers(); self.wfile.write(b"{}")
srv = HTTPServer(("127.0.0.1", port), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
signal.signal(signal.SIGTERM, lambda *_: os._exit(0))
while True: time.sleep(0.2)
"#;

fn put(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn a_signal_to_the_program_alone_stops_the_engine() {
    let home = std::env::temp_dir().join(format!("flelink-signals-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let fake = home.join("fake-server");
    put(&fake, FAKE);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // a model file the plugin recipe names (its header is never read by the fake engine)
    put(&home.join("models/test__tiny/tiny.gguf"), "GGUF");
    put(
        &home.join("plugins/fork/plugin.toml"),
        &format!("id = \"fork\"\ntitle = \"A fork\"\n[[engine]]\nid = \"fork-engine\"\nlabel = \"Fork\"\npath = \"{}\"\n", fake.display()),
    );
    put(
        &home.join("plugins/fork/recipes/f.toml"),
        "id = \"tiny-fork\"\ntitle = \"Tiny via fork\"\n[model]\nrepo = \"test/tiny\"\nfiles = [\"tiny.gguf\"]\n[engine]\nprefer = [\"fork-engine\"]\nstrict = true\n[run]\ncontext = 2048\n",
    );
    let pid_file = home.join("engine.pid");
    let mut child = Command::new(env!("CARGO_BIN_EXE_flelink"))
        .args(["run", "tiny-fork"])
        .env("FLE_HOME", &home)
        .env("FLE_NO_UPDATE", "1")
        .env("FAKE_PID", &pid_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // its own process group: a signal to it does not reach the engine the way Ctrl+C in a terminal would
    let err = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    let t = Instant::now();
    let mut seen = vec![];
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(l) => {
                let serving = l.contains("Serving");
                seen.push(l);
                if serving {
                    break;
                }
            }
            Err(_) => panic!("flelink run did not start: {seen:?}"),
        }
        assert!(t.elapsed() < Duration::from_secs(60), "{seen:?}");
    }
    let engine: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(engine));
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let status = child.wait().unwrap();
    let t = Instant::now();
    while alive(engine) {
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "the engine {engine} outlived flelink run ({status})"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&home);
}
