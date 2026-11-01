//! A host reached through an SSH tunnel, end to end: a stand-in for `ssh` (FLE_SSH) that really forwards `-L`, a host
//! Studio and a client Studio. Pairing goes through the tunnel, requests under /h/<id>/ reach the host, ssh gets the
//! options that keep it from asking for a password, a dead ssh is started again, and the tunnel ends with the client.
#![cfg(unix)]
use flelink_api::run::start_embedded;
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// Forwards 127.0.0.1:<local> to 127.0.0.1:<remote> as `ssh -N -L 127.0.0.1:L:127.0.0.1:R` would, logs its arguments
/// and its pid.
const FAKE_SSH: &str = r#"
import os, socket, sys, threading
log = os.environ["FAKE_SSH_LOG"]
with open(log, "a") as f:
    f.write(str(os.getpid()) + " " + " ".join(sys.argv[1:]) + "\n")
spec = sys.argv[sys.argv.index("-L") + 1].split(":")
local, remote = int(spec[1]), int(spec[3])
srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", local)); srv.listen(32)
def pipe(a, b):
    try:
        while True:
            d = a.recv(65536)
            if not d: break
            b.sendall(d)
    except OSError:
        pass
    finally:
        for s in (a, b):
            try: s.shutdown(socket.SHUT_RDWR)
            except OSError: pass
while True:
    c, _ = srv.accept()
    r = socket.create_connection(("127.0.0.1", remote))
    threading.Thread(target=pipe, args=(c, r), daemon=True).start()
    threading.Thread(target=pipe, args=(r, c), daemon=True).start()
"#;

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Api {
    agent: ureq::Agent,
    base: String,
    token: String,
}

impl Api {
    fn new(url: &str) -> Api {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        let page = agent.get(url).call().unwrap().into_string().unwrap();
        let token = page
            .split("name=\"fle-token\" content=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string();
        Api {
            agent,
            base: url.trim_end_matches('/').to_string(),
            token,
        }
    }

    fn call(&self, m: &str, p: &str, b: Value) -> (u16, Value) {
        let req = self
            .agent
            .request(m, &format!("{}{p}", self.base))
            .set("X-FLE-Token", &self.token)
            .set("Content-Type", "application/json");
        let res = if m == "POST" {
            req.send_string(&b.to_string())
        } else {
            req.call()
        };
        let (c, t) = match res {
            Ok(r) => (r.status(), r.into_string().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap()),
            Err(e) => panic!("{e}"),
        };
        (c, serde_json::from_str(&t).unwrap_or(json!({"raw": t})))
    }
}

fn ssh_calls(log: &Path) -> Vec<(u32, String)> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (pid, args) = l.split_once(' ')?;
            Some((pid.parse().ok()?, args.to_string()))
        })
        .collect()
}

#[test]
fn a_host_behind_an_ssh_tunnel() {
    let tmp = Tmp(std::env::temp_dir().join(format!("flelink-ssh-{}", std::process::id())));
    std::fs::create_dir_all(&tmp.0).unwrap();
    let script = tmp.0.join("fake_ssh.py");
    std::fs::write(&script, FAKE_SSH).unwrap();
    let log = tmp.0.join("ssh.log");
    // the client starts ssh from its own threads: the stand-in has to be known before it starts
    std::env::set_var(
        "FLE_SSH",
        json!(["python3", script.to_string_lossy()]).to_string(),
    );
    std::env::set_var("FAKE_SSH_LOG", &log);

    let host = start_embedded(Some(tmp.0.join("host")), None).expect("host");
    let host_port = host.port;
    let h = Api::new(&host.url);
    let mut client = start_embedded(Some(tmp.0.join("client")), None).expect("client");
    let c = Api::new(&client.url);

    let (_, hosts) = c.call("GET", "/api/hosts", json!(null));
    assert_eq!(hosts["ssh"], true, "{hosts}");

    // a target that could smuggle an option is refused before ssh runs
    let (code, v) = c.call(
        "POST",
        "/api/hosts",
        json!({"kind": "ssh", "target": "-oProxyCommand=x", "code": "ABCD2345"}),
    );
    assert_eq!(code, 400, "{v}");
    assert!(ssh_calls(&log).is_empty());

    let (code, pairing) = h.call("POST", "/api/remote/code", json!({}));
    assert_eq!(code, 200, "{pairing}");
    let (code, added) = c.call(
        "POST",
        "/api/hosts",
        json!({"kind": "ssh", "target": "tester@hostbox", "port": 2222,
        "remote_port": host_port, "code": pairing["code"], "name": "Box"}),
    );
    assert_eq!(code, 200, "{added}");
    assert_eq!(added["probe"]["ok"], true, "{added}");
    let id = added["host"]["id"].as_str().unwrap().to_string();

    let calls = ssh_calls(&log);
    assert_eq!(calls.len(), 1, "{calls:?}");
    let args = &calls[0].1;
    for want in [
        "-N",
        "-p 2222",
        "BatchMode=yes",
        "ExitOnForwardFailure=yes",
        "ServerAliveInterval=15",
        "tester@hostbox",
    ] {
        assert!(args.contains(want), "{want} missing in: {args}");
    }
    assert!(args.contains(&format!(":127.0.0.1:{host_port}")), "{args}");

    // through the tunnel: the host's own answer, under the client's /h/<id>/
    let (code, who) = c.call("GET", &format!("/h/{id}/api/remote/whoami"), json!(null));
    assert_eq!(code, 403, "pairing paths stay on the client: {who}");
    let (code, state) = c.call("GET", &format!("/h/{id}/api/engine"), json!(null));
    assert_eq!(code, 200, "{state}");
    assert_eq!(state["state"], "stopped", "{state}");

    // ssh dies: the next request starts it again
    let pid = calls[0].0;
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    let t = Instant::now();
    loop {
        let (code, _) = c.call("GET", &format!("/h/{id}/api/engine"), json!(null));
        if code == 200 && ssh_calls(&log).len() >= 2 {
            break;
        }
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "the tunnel did not come back: {:?}",
            ssh_calls(&log)
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    let second = ssh_calls(&log).last().unwrap().0;
    assert_ne!(second, pid);

    // the client ends: so does its ssh
    client.stop();
    let t = Instant::now();
    while unsafe { libc::kill(second as i32, 0) } == 0 {
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "ssh {second} outlived the client"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
