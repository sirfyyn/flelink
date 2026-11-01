//! Signing in to a remote MCP server end to end, against a fake server that is its own authorization server: the 401 with
//! its challenge, discovery, dynamic client registration, PKCE (the fake checks the verifier), the browser's redirect to
//! FLE Link's callback, the token on the requests, a refresh after the token was refused, and signing out.
#![cfg(unix)]
use flelink_api::run::start_embedded;
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FAKE: &str = r#"
import sys, json, hashlib, base64
from urllib.parse import urlparse, parse_qs, urlencode
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
P = int(sys.argv[1]); BASE = "http://127.0.0.1:%d" % P
S = {"valid": {"AT1"}, "codes": {}, "revoked": []}
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *x): pass
    def out(self, code, body=None, headers=()):
        data = json.dumps(body).encode() if body is not None else b""
        self.send_response(code)
        for k, v in headers: self.send_header(k, v)
        if body is not None: self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def form(self):
        return {k: v[0] for k, v in parse_qs(self.rfile.read(int(self.headers["Content-Length"])).decode()).items()}
    def do_GET(self):
        u = urlparse(self.path); q = {k: v[0] for k, v in parse_qs(u.query).items()}
        if u.path == "/.well-known/oauth-protected-resource/mcp":
            self.out(200, {"resource": BASE + "/mcp", "authorization_servers": [BASE], "scopes_supported": ["mcp"]})
        elif u.path == "/.well-known/oauth-authorization-server":
            self.out(200, {"issuer": BASE, "authorization_endpoint": BASE + "/authorize", "token_endpoint": BASE + "/token",
                           "registration_endpoint": BASE + "/register", "revocation_endpoint": BASE + "/revoke",
                           "code_challenge_methods_supported": ["S256"]})
        elif u.path == "/authorize":
            ok = q.get("code_challenge_method") == "S256" and q.get("client_id") == "c1" and q.get("resource") == BASE + "/mcp" and q.get("scope") == "mcp"
            S["codes"]["K1"] = q["code_challenge"]
            self.out(302, None, [("Location", q["redirect_uri"] + "?" + urlencode({"code": "K1" if ok else "", "state": q["state"]}))])
        elif u.path == "/expire":
            S["valid"].discard("AT1"); self.out(200, {})
        elif u.path == "/revoked":
            self.out(200, S["revoked"])
        else:
            self.out(404, {})
    def do_POST(self):
        u = urlparse(self.path)
        if u.path == "/register":
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            self.out(201 if body["token_endpoint_auth_method"] == "none" else 400, {"client_id": "c1"})
        elif u.path == "/token":
            f = self.form()
            if f["grant_type"] == "authorization_code":
                want = S["codes"].pop(f["code"], None)
                got = base64.urlsafe_b64encode(hashlib.sha256(f["code_verifier"].encode()).digest()).rstrip(b"=").decode()
                if want != got or f["resource"] != BASE + "/mcp":
                    self.out(400, {"error": "invalid_grant"}); return
                self.out(200, {"access_token": "AT1", "token_type": "Bearer", "expires_in": 3600, "refresh_token": "RT1", "scope": "mcp"})
            elif f["grant_type"] == "refresh_token" and f["refresh_token"] == "RT1":
                S["valid"].add("AT2")
                self.out(200, {"access_token": "AT2", "token_type": "bearer", "expires_in": 3600})
            else:
                self.out(400, {"error": "invalid_grant"})
        elif u.path == "/revoke":
            S["revoked"].append(self.form()["token_type_hint"]); self.out(200, {})
        elif u.path == "/mcp":
            tok = (self.headers.get("Authorization") or "").removeprefix("Bearer ")
            if tok not in S["valid"]:
                self.rfile.read(int(self.headers["Content-Length"]))
                self.out(401, {}, [("WWW-Authenticate", 'Bearer resource_metadata="%s/.well-known/oauth-protected-resource/mcp", scope="mcp"' % BASE)])
                return
            m = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if "id" not in m:
                self.out(202); return
            if m["method"] == "initialize":
                r = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "locked"}}
            elif m["method"] == "tools/list":
                r = {"tools": [{"name": "whoami", "inputSchema": {"type": "object"}}]}
            else:
                r = {"content": [{"type": "text", "text": "token " + tok}]}
            self.out(200, {"jsonrpc": "2.0", "id": m["id"], "result": r})
        else:
            self.out(404, {})
ThreadingHTTPServer(("127.0.0.1", P), H).serve_forever()
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
fn signing_in_to_a_remote_mcp_server() {
    let home = Tmp(std::env::temp_dir().join(format!("flelink-oauth-{}", std::process::id())));
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
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut s = start_embedded(Some(home.0.clone()), None).expect("start");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .redirects(0)
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
    let base = format!("http://127.0.0.1:{port}");
    let servers = json!({"mcpServers": {"locked": {"url": format!("{base}/mcp")}}});
    call(
        "POST",
        "api/settings",
        json!({"tools": {"enabled": true, "mcp_enabled": true, "mcp_json": servers.to_string(), "mcp_trusted": ["locked"]}}),
    );
    let wait_status = |want: &str| -> Value {
        let t = Instant::now();
        loop {
            let (_, v) = call("GET", "api/tools", json!({}));
            let sv = v["servers"][0].clone();
            if sv["status"] == want || t.elapsed() > Duration::from_secs(20) {
                return sv;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let sv = wait_status("auth_required");
    assert_eq!(sv["status"], "auth_required", "{sv}");
    assert_eq!(sv["auth"]["signed_in"], false);
    assert_eq!(sv["auth"]["required"], true);

    // sign in: discovery, registration, the authorization address
    let (c, v) = call("POST", "api/mcp/signin", json!({"server": "locked"}));
    assert_eq!(c, 200, "{v}");
    assert_eq!(v["issuer"], base);
    assert_eq!(v["scope"], "mcp");
    let auth_url = v["auth_url"].as_str().unwrap();
    assert!(
        auth_url.starts_with(&format!("{base}/authorize?")),
        "{auth_url}"
    );
    assert!(auth_url.contains(&format!(
        "redirect_uri=http%3A%2F%2F127.0.0.1%3A{}%2Foauth%2Fcallback",
        s.port
    )));
    // the browser: the server sends it back to FLE Link's callback
    let r = agent.get(auth_url).call().unwrap();
    assert_eq!(r.status(), 302);
    let back = r.header("Location").unwrap().to_string();
    let page = match agent.get(&back).call() {
        Ok(r) => r.into_string().unwrap(),
        Err(ureq::Error::Status(c, r)) => panic!("{c}: {}", r.into_string().unwrap()),
        Err(e) => panic!("{e}"),
    };
    assert!(
        page.contains("Signed in") && page.contains("locked is connected"),
        "{page}"
    );
    // the same answer twice is refused
    let again = agent.get(&back).call();
    assert!(matches!(again, Err(ureq::Error::Status(400, _))));

    let sv = wait_status("ready");
    assert_eq!(sv["status"], "ready", "{sv}");
    assert_eq!(sv["auth"]["signed_in"], true);
    assert_eq!(sv["auth"]["can_refresh"], true);
    assert!(
        sv.to_string().find("AT1").is_none(),
        "the page never sees a token"
    );
    let (_, v) = call("POST", "api/tools/call", json!({"name": "locked__whoami"}));
    assert_eq!(v["content"], "token AT1", "{v}");

    // the token file is readable by the user only
    let dir = home.0.join("oauth");
    let file = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // the server stops taking the token: one refresh, the call is repeated
    agent.get(&format!("{base}/expire")).call().unwrap();
    let (_, v) = call("POST", "api/tools/call", json!({"name": "locked__whoami"}));
    assert_eq!(v["content"], "token AT2", "{v}");

    // sign out: the token goes, the server is asked to revoke it, a sign-in is needed again
    let (c, v) = call("POST", "api/mcp/signout", json!({"server": "locked"}));
    assert_eq!(c, 200);
    assert_eq!(v["had_token"], true);
    let revoked: Value = serde_json::from_str(
        &agent
            .get(&format!("{base}/revoked"))
            .call()
            .unwrap()
            .into_string()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(revoked, json!(["refresh_token", "access_token"]));
    let sv = wait_status("auth_required");
    assert_eq!(sv["auth"]["signed_in"], false, "{sv}");
    let (c, _) = call("POST", "api/mcp/signin", json!({"server": "nope"}));
    assert_eq!(c, 404);
    s.stop();
}
