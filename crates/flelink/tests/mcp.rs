//! `flelink mcp` as an agent uses it: a Studio announces itself in `studio.json`, the MCP server finds it there, answers
//! the handshake and the tool list, and its tools go through the Studio's API. The file goes away with the Studio.
#![cfg(unix)]
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

extern "C" {
    #[link_name = "kill"]
    fn libc_kill_raw(pid: i32, sig: i32) -> i32;
}

#[test]
fn the_mcp_server_finds_the_studio_and_drives_it_through_its_api() {
    let home = std::env::temp_dir().join(format!("fle-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut studio = Command::new(env!("CARGO_BIN_EXE_flelink"))
        .args(["studio", "--no-browser", "--home"])
        .arg(&home)
        .args(["--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let said = home.join("studio.json");
    let t = Instant::now();
    while !said.exists() {
        assert!(t.elapsed() < Duration::from_secs(20), "no studio.json");
        std::thread::sleep(Duration::from_millis(100));
    }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&said).unwrap()).unwrap();
    assert_eq!(v["url"], format!("http://127.0.0.1:{port}/"));
    assert!(!v["token"].as_str().unwrap().is_empty());
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&said).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "only this user reads the token");
    }

    let mut mcp = Command::new(env!("CARGO_BIN_EXE_flelink"))
        .args(["mcp", "--home"])
        .arg(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = mcp.stdin.take().unwrap();
    let mut output = BufReader::new(mcp.stdout.take().unwrap());
    let mut n = 0;
    let mut rpc = |method: &str, params: Value| -> Value {
        n += 1;
        writeln!(
            input,
            "{}",
            json!({"jsonrpc": "2.0", "id": n, "method": method, "params": params})
        )
        .unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = rpc(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "flelink");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    let tools = rpc("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for want in ["status", "recipes", "load", "stop", "connect", "ask"] {
        assert!(names.contains(&want), "{names:?}");
    }
    let text = |r: &Value| {
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let st = rpc("tools/call", json!({"name": "status", "arguments": {}}));
    let st: Value = serde_json::from_str(&text(&st)).unwrap();
    assert_eq!(st["state"], "stopped");
    assert_eq!(st["api"]["openai"], format!("http://127.0.0.1:{port}/v1"));
    // a changing request: it carries the page token from studio.json
    let set = rpc(
        "tools/call",
        json!({"name": "settings", "arguments": {"changes": {"theme": "dark"}}}),
    );
    assert!(set["result"]["isError"].is_null(), "{}", text(&set));
    let got = rpc("tools/call", json!({"name": "settings", "arguments": {}}));
    assert!(text(&got).contains("\"theme\": \"dark\""), "{}", text(&got));
    let bad = rpc("tools/call", json!({"name": "nope", "arguments": {}}));
    assert_eq!(bad["result"]["isError"], true);
    drop(input);
    let _ = mcp.wait();

    unsafe {
        libc_kill_raw(studio.id() as i32, 15);
    }
    let t = Instant::now();
    while studio.try_wait().unwrap().is_none() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "the Studio did not stop"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!said.exists(), "studio.json goes with the Studio");
    let _ = std::fs::remove_dir_all(&home);
}
