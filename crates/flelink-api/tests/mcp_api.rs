//! MCP servers end to end: a stdio server (a Python script) and a Streamable HTTP server connect in the background, their
//! tools appear in the tool list as `server__tool`, calls ask for approval unless the server is trusted, results come back
//! as text, a server that exited starts again on the next call, and the servers end with the Studio.
#![cfg(unix)]
use flelink_api::run::start_embedded;
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const STDIO: &str = r#"
import sys, json, os
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    meth, p = m["method"], m.get("params") or {}
    if meth == "initialize":
        r = {"protocolVersion": p["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "calc", "version": "1"}}
    elif meth == "tools/list":
        r = {"tools": [{"name": "add", "description": "Add two numbers", "inputSchema": {"$schema": "x", "type": "object",
              "properties": {"a": {"type": "number"}, "b": {"type": "number"}}, "required": ["a", "b"]}},
             {"name": "pid", "description": "The process id"},
             {"name": "quit", "description": "Exit"},
             {"name": "app_only", "description": "x", "_meta": {"ui": {"visibility": ["app"]}}},
             {"name": "chart", "description": "A chart", "_meta": {"ui": {"resourceUri": "ui://calc/chart"}}}]}
    elif meth == "resources/read":
        r = {"contents": [{"uri": p["uri"], "mimeType": "text/html;profile=mcp-app", "text": "<p>chart</p>",
              "_meta": {"ui": {"csp": {"connectDomains": ["https://api.example.com", "https://x;y"]}, "prefersBorder": True}}}]}
    elif meth == "tools/call":
        a = p["arguments"]
        if p["name"] == "add":
            r = {"content": [{"type": "text", "text": str(a["a"] + a["b"])}]}
        elif p["name"] in ("chart", "app_only"):
            r = {"content": [{"type": "text", "text": p["name"] + " done"}], "structuredContent": {"points": [1, 2]}}
        elif p["name"] == "quit":
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": {"content": [{"type": "text", "text": "bye"}]}}) + "\n"); sys.stdout.flush()
            sys.exit(0)
        else:
            r = {"content": [{"type": "text", "text": str(os.getpid())}]}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32601, "message": "no"}}), flush=True); continue
    print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)
"#;

const HTTP: &str = r#"
import sys, json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *x): pass
    def do_DELETE(self):
        self.send_response(200); self.send_header("Content-Length", "0"); self.end_headers()
    def do_POST(self):
        if self.headers.get("Authorization") != "Bearer t0k":
            self.send_response(401); self.send_header("Content-Length", "0"); self.end_headers(); return
        m = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if "id" not in m:
            self.send_response(202); self.send_header("Content-Length", "0"); self.end_headers(); return
        meth, p = m["method"], m.get("params") or {}
        if meth == "initialize":
            r = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "remote-echo"}}
        elif meth == "tools/list":
            r = {"tools": [{"name": "echo", "title": "Echo", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}
        else:
            ok = self.headers.get("Mcp-Session-Id") == "s1" and self.headers.get("MCP-Protocol-Version") == "2025-06-18"
            r = {"content": [{"type": "text", "text": ("echo: " if ok else "no session: ") + p["arguments"]["text"]}]}
        body = json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r})
        sse = meth == "tools/call"  # the call answers as an event stream, the others as JSON
        data = ("event: message\ndata: " + body + "\n\n").encode() if sse else body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream" if sse else "application/json")
        self.send_header("Mcp-Session-Id", "s1")
        self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
S = ThreadingHTTPServer(("127.0.0.1", 0), H)
print(S.server_address[1], flush=True)
S.serve_forever()
"#;

/// A server of the 2024-11-05 revision: GET /sse opens the event stream and names the message address; a POST to /sse
/// (what a Streamable HTTP client tries first) is refused with 405.
const OLD_SSE: &str = r#"
import sys, json, queue, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
Q = queue.Queue()
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *x): pass
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.end_headers()
        self.wfile.write(b"event: endpoint\ndata: /messages?sid=1\n\n"); self.wfile.flush()
        while True:
            m = Q.get()
            self.wfile.write(("event: message\ndata: " + json.dumps(m) + "\n\n").encode()); self.wfile.flush()
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        if not self.path.startswith("/messages"):
            self.send_response(405); self.send_header("Content-Length", "0"); self.end_headers(); return
        self.send_response(202); self.send_header("Content-Length", "0"); self.end_headers()
        m = json.loads(body)
        if "id" not in m: return
        meth = m["method"]
        if meth == "initialize": r = {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "old"}}
        elif meth == "tools/list": r = {"tools": [{"name": "hello", "inputSchema": {"type": "object"}}]}
        else: r = {"content": [{"type": "text", "text": "hello from the old transport"}]}
        Q.put({"jsonrpc": "2.0", "id": m["id"], "result": r})
S = ThreadingHTTPServer(("127.0.0.1", 0), H)
print(S.server_address[1], flush=True)
S.serve_forever()
"#;

/// Start a fake server; its first line is the port it listens on.
fn serve(code: &str, err: &std::path::Path) -> (Kill, u16) {
    use std::io::BufRead;
    let mut child = Command::new("python3")
        .args(["-c", code])
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(err).unwrap())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::BufReader::new(out).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(60)).unwrap_or_default();
    let port = line.trim().parse().unwrap_or_else(|_| {
        panic!(
            "a fake server did not start ({:?}, exit {:?}): {}",
            line,
            child.try_wait().ok().flatten(),
            std::fs::read_to_string(err).unwrap_or_default()
        )
    });
    (Kill(child), port)
}

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
fn mcp_servers_become_tools() {
    let home = Tmp(std::env::temp_dir().join(format!("flelink-mcp-{}", std::process::id())));
    std::fs::create_dir_all(&home.0).unwrap();
    let script = home.0.join("calc_server.py");
    std::fs::write(&script, STDIO).unwrap();
    // each fake server takes a free port itself and prints it (no race for a port picked here)
    let (_http, port) = serve(HTTP, &home.0.join("http.err"));
    let (_old, old_port) = serve(OLD_SSE, &home.0.join("sse.err"));
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
    let servers = json!({"mcpServers": {
        "calc": {"command": "python3", "args": [script.to_string_lossy()]},
        "remote": {"url": format!("http://127.0.0.1:{port}/mcp"), "headers": {"Authorization": "Bearer t0k"}},
        "locked": {"url": format!("http://127.0.0.1:{port}/mcp")},
        "old": {"url": format!("http://127.0.0.1:{old_port}/sse")},
        "missing": {"command": "definitely-not-a-program-here"}}});
    let (c, v) = call(
        "POST",
        "api/settings",
        json!({"tools": {"enabled": true, "mcp_enabled": true, "mcp_json": servers.to_string(), "mcp_trusted": ["remote"]}}),
    );
    assert_eq!(c, 200, "{v}");

    let t = Instant::now();
    let tools = loop {
        let (_, v) = call("GET", "api/tools", json!({}));
        if v["connecting"] == false || t.elapsed() > Duration::from_secs(30) {
            break v;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let server = |n: &str| {
        tools["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == n)
            .cloned()
            .unwrap()
    };
    assert_eq!(server("calc")["status"], "ready", "{tools}");
    assert_eq!(server("calc")["info"], "calc");
    assert_eq!(server("calc")["tools"], 5);
    assert_eq!(server("remote")["status"], "ready", "{tools}");
    assert_eq!(server("remote")["trusted"], true);
    assert_eq!(server("locked")["status"], "auth_required");
    assert_eq!(server("old")["status"], "ready", "{tools}");
    assert_eq!(server("old")["legacy_sse"], true);
    assert_eq!(server("missing")["status"], "error");
    assert!(server("missing")["error"]
        .as_str()
        .unwrap()
        .contains("was not found"));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"calc__add") && names.contains(&"remote__echo"),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("app_only")),
        "a tool only an app may call is not the model's"
    );
    let add = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "calc__add")
        .unwrap();
    assert_eq!(add["source"], "mcp");
    assert_eq!(add["confirm"], true);
    assert!(add["schema"]["function"]["parameters"]
        .get("$schema")
        .is_none());
    assert_eq!(add["description"], "[calc] Add two numbers");

    // an untrusted server asks first
    let (c, _) = call(
        "POST",
        "api/tools/call",
        json!({"name": "calc__add", "arguments": {"a": 2, "b": 3}}),
    );
    assert_eq!(c, 409);
    let (c, v) = call(
        "POST",
        "api/tools/call",
        json!({"name": "calc__add", "arguments": {"a": 2, "b": 3}, "approved": true}),
    );
    assert_eq!(c, 200);
    assert_eq!(v["content"], "5");
    assert_eq!(v["is_error"], false);
    // a trusted HTTP server: no question, session and protocol version carried, an event-stream answer read
    let (c, v) = call(
        "POST",
        "api/tools/call",
        json!({"name": "remote__echo", "arguments": "{\"text\": \"hi\"}"}),
    );
    assert_eq!(c, 200, "{v}");
    assert_eq!(v["content"], "echo: hi");

    // an MCP App: the tool's result carries the whole result for the app, the frame serves its document under its own
    // policy, and the app may call only its own server's app tools, with the same approval as the model
    let (_, v) = call(
        "POST",
        "api/tools/call",
        json!({"name": "calc__chart", "approved": true}),
    );
    assert_eq!(v["content"], "chart done");
    assert_eq!(v["app"]["uri"], "ui://calc/chart");
    assert_eq!(
        v["app"]["result"]["structuredContent"]["points"],
        json!([1, 2])
    );
    let (c, app) = call("POST", "api/apps/prepare", json!({"name": "calc__chart"}));
    assert_eq!(c, 200, "{app}");
    assert_eq!(app["prefers_border"], true);
    assert_eq!(app["trusted"], false);
    let frame = agent
        .get(&format!(
            "{}{}",
            s.url,
            app["url"].as_str().unwrap().trim_start_matches('/')
        ))
        .call()
        .unwrap();
    let csp = frame.header("Content-Security-Policy").unwrap().to_string();
    assert!(
        csp.contains("connect-src https://api.example.com;") && !csp.contains("x;y"),
        "{csp}"
    );
    assert!(
        csp.contains("frame-src 'none'") && csp.contains("default-src 'none'"),
        "{csp}"
    );
    assert_eq!(frame.into_string().unwrap(), "<p>chart</p>");
    let f = app["frame"].clone();
    let (c, _) = call(
        "POST",
        "api/apps/call",
        json!({"frame": f, "name": "app_only"}),
    );
    assert_eq!(c, 409);
    let (c, v) = call(
        "POST",
        "api/apps/call",
        json!({"frame": f, "name": "app_only", "approved": true}),
    );
    assert_eq!(c, 200);
    assert_eq!(v["result"]["content"][0]["text"], "app_only done");
    let (c, _) = call(
        "POST",
        "api/apps/call",
        json!({"frame": f, "name": "nope", "approved": true}),
    );
    assert_eq!(c, 404, "a tool the server does not have");
    let (c, _) = call("POST", "api/apps/prepare", json!({"name": "calc__add"}));
    assert_eq!(c, 404);
    assert!(agent
        .get(&format!("{}app-frame/{}", s.url, "x".repeat(24)))
        .call()
        .is_err());

    // the older HTTP+SSE server, reached after the POST to its address was refused
    let (_, v) = call(
        "POST",
        "api/tools/call",
        json!({"name": "old__hello", "approved": true}),
    );
    assert_eq!(v["content"], "hello from the old transport", "{v}");

    // the stdio server exits; the next call starts it again
    let (_, v) = call(
        "POST",
        "api/tools/call",
        json!({"name": "calc__pid", "approved": true}),
    );
    let pid1 = v["content"].as_str().unwrap().to_string();
    let (_, v) = call(
        "POST",
        "api/tools/call",
        json!({"name": "calc__quit", "approved": true}),
    );
    assert_eq!(v["content"], "bye");
    std::thread::sleep(Duration::from_millis(300));
    let (_, v) = call(
        "POST",
        "api/tools/call",
        json!({"name": "calc__pid", "approved": true}),
    );
    let pid2 = v["content"].as_str().unwrap().to_string();
    assert_ne!(pid1, pid2, "{v}");
    assert!(pid2.parse::<u32>().is_ok(), "{v}");

    // the last tool lists are remembered for the next start
    let known: Value =
        serde_json::from_str(&std::fs::read_to_string(home.0.join("mcp-tools.json")).unwrap())
            .unwrap();
    assert_eq!(known["calc"]["tools"].as_array().unwrap().len(), 5);

    // turning MCP off ends the servers
    call(
        "POST",
        "api/settings",
        json!({"tools": {"mcp_enabled": false}}),
    );
    let (_, v) = call("GET", "api/tools", json!({}));
    assert_eq!(v["servers"], json!([]));
    std::thread::sleep(Duration::from_secs(2));
    let alive = Command::new("kill")
        .args(["-0", &pid2])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the stdio server ended");
    s.stop();
}
