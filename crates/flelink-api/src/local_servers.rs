//! Inference servers that already run on this machine: a llama-server (llama.cpp or FLE) that something else started,
//! a systemd service or a terminal. FLE Link finds them by their processes and connects by itself, instead of
//! saying "no model loaded" next to a model that answers.
use serde_json::{json, Value};
use std::time::Duration;

/// Whether FLE Link connects to a server it finds by itself. On unless `FLE_ATTACH_FOUND` is 0, off, false or no,
/// or the settings file has `attach_found: false` (no default there: the settings stay those of the Python Studio).
pub fn wanted(setting: Option<bool>) -> bool {
    let off = std::env::var("FLE_ATTACH_FOUND")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            )
        })
        .unwrap_or(false);
    !off && setting.unwrap_or(true)
}

/// llama-server's own default port: asked even when no process shows up (a container, another user's process).
const DEFAULT_PORT: u16 = 8080;

/// The port in a llama-server command line (`--port N`, `--port=N`), 8080 without one. None: not a llama-server.
pub fn port_of(args: &[String]) -> Option<u16> {
    let first = args.first()?;
    let name = first.rsplit(['/', '\\']).next().unwrap_or(first);
    if !name.starts_with("llama-server") {
        return None;
    }
    let mut port = DEFAULT_PORT;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--port" {
            if let Some(p) = it.next().and_then(|v| v.parse().ok()) {
                port = p;
            }
        } else if let Some(v) = a.strip_prefix("--port=") {
            if let Ok(p) = v.parse() {
                port = p;
            }
        }
    }
    Some(port)
}

#[cfg(target_os = "linux")]
fn process_ports() -> Vec<u16> {
    let mut out = vec![];
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in dir.flatten() {
        if !e
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).to_string())
            .collect();
        if let Some(p) = port_of(&args) {
            out.push(p);
        }
    }
    out
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_ports() -> Vec<u16> {
    let Ok(o) = std::process::Command::new("ps")
        .args(["-axww", "-o", "command="])
        .output()
    else {
        return vec![];
    };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| port_of(&l.split_whitespace().map(String::from).collect::<Vec<_>>()))
        .collect()
}

#[cfg(not(unix))]
fn process_ports() -> Vec<u16> {
    vec![]
}

/// What answers on this port, if it is a llama-server: its address, model and context. `locked`: it wants a key.
fn probe(port: u16) -> Option<Value> {
    let base = format!("http://127.0.0.1:{port}");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(700))
        .build();
    let health = match agent.get(&format!("{base}/health")).call() {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(c, _)) => c,
        Err(_) => return None,
    };
    if health != 200 && health != 503 {
        return None;
    }
    let props = match agent.get(&format!("{base}/props")).call() {
        Ok(r) => r
            .into_string()
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok()),
        Err(ureq::Error::Status(401 | 403, _)) => {
            return Some(
                json!({"url": base, "port": port, "model": "", "n_ctx": null, "loading": false, "locked": true}),
            );
        }
        Err(_) => None,
    };
    let props = props.unwrap_or(json!({}));
    // a server that is still loading answers 503 without props; anything else must look like llama-server
    if health == 200
        && props.get("default_generation_settings").is_none()
        && props.get("model_path").is_none()
    {
        return None;
    }
    let path = props["model_path"].as_str().unwrap_or("");
    let model = props["model_alias"]
        .as_str()
        .filter(|n| !n.is_empty())
        .map(String::from)
        .unwrap_or_else(|| path.rsplit(['/', '\\']).next().unwrap_or("").to_string());
    let n_ctx = props["default_generation_settings"]["n_ctx"]
        .as_u64()
        .or(props["n_ctx"].as_u64());
    Some(
        json!({"url": base, "port": port, "model": model, "n_ctx": n_ctx, "loading": health == 503, "locked": false}),
    )
}

/// The inference servers on this machine, by port. `skip`: ports that are FLE Link's own (its page, its engine).
pub fn local_servers(skip: &[u16]) -> Vec<Value> {
    let mut ports = process_ports();
    ports.push(DEFAULT_PORT);
    ports.sort_unstable();
    ports.dedup();
    ports
        .into_iter()
        .filter(|p| !skip.contains(p))
        .filter_map(probe)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn port_from_the_command_line() {
        assert_eq!(
            port_of(&args(
                "/opt/fle/bin/llama-server -m a.gguf --host 0.0.0.0 --port 18420 --alias katy"
            )),
            Some(18420)
        );
        assert_eq!(
            port_of(&args("llama-server --port=9001 -m a.gguf")),
            Some(9001)
        );
        assert_eq!(
            port_of(&args("C:\\fle\\llama-server.exe -m a.gguf")),
            Some(8080)
        );
        assert_eq!(port_of(&args("python3 server.py --port 18421")), None);
        assert_eq!(port_of(&args("/usr/bin/flelink studio --port 8643")), None);
        assert_eq!(port_of(&[]), None);
    }

    #[test]
    fn nothing_answers_on_a_free_port() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        assert!(probe(port).is_none());
    }
}
