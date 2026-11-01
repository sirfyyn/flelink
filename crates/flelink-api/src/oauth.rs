//! OAuth for remote MCP servers, following the MCP authorization specification (revision 2025-11-25). Port of
//! `fle_studio/oauth.py`.
//!
//! 1. The server answers with `401` and `WWW-Authenticate: Bearer resource_metadata="…", scope="…"`.
//! 2. Protected Resource Metadata (RFC 9728) names the authorization server: the URL from the header, then
//!    `/.well-known/oauth-protected-resource/<path>`, then `/.well-known/oauth-protected-resource`.
//! 3. Authorization Server Metadata: RFC 8414 with path insertion, then OpenID Connect Discovery. A server without
//!    `S256` in `code_challenge_methods_supported` is refused.
//! 4. A client id: one the user configured, else Dynamic Client Registration (RFC 7591), else the user is asked for one.
//! 5. Authorization code with PKCE (S256) in the user's browser, redirected to FLE Link's own loopback port, with the
//!    `resource` parameter (RFC 8707); `state` is checked.
//! 6. Tokens are kept in `<data>/oauth/` (files with mode 0600), refreshed shortly before they expire, removed by signing
//!    out. They never go into settings, logs or exports.
//!
//! Every address of the authorization server must be `https` (loopback `http` is accepted, for local servers and tests).
use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// how long a sign-in may take
const PENDING_SECONDS: f64 = 600.0;
/// refresh a token that has less than this many seconds left
const EXPIRY_MARGIN: f64 = 60.0;
const LOOPBACK: [&str; 4] = ["127.0.0.1", "localhost", "::1", "[::1]"];
const CLIENT_NAME: &str = "FLE Link";

/// Signing in failed; the message says why. `needs_sign_in`: there is no usable token.
#[derive(Debug, Clone)]
pub struct OAuthError {
    pub message: String,
    pub needs_sign_in: bool,
}

fn err<T>(m: impl Into<String>) -> Result<T, OAuthError> {
    Err(OAuthError {
        message: m.into(),
        needs_sign_in: false,
    })
}

fn needs<T>(m: impl Into<String>) -> Result<T, OAuthError> {
    Err(OAuthError {
        message: m.into(),
        needs_sign_in: true,
    })
}

type R<T> = Result<T, OAuthError>;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ------------------------------------------------------------------------------------------------ small helpers

struct Url {
    scheme: String,
    host: String,
    port: Option<u16>,
    path: String,
    query: String,
}

fn parse_url(url: &str) -> Url {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let rest = rest.split('#').next().unwrap_or("");
    let (netloc, path_q) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => match rest.find('?') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        },
    };
    let netloc = netloc.rsplit('@').next().unwrap_or("");
    let (host, port) = if let Some(h) = netloc.strip_prefix('[') {
        let (h, p) = h.split_once(']').unwrap_or((h, ""));
        (
            h.to_string(),
            p.strip_prefix(':').and_then(|p| p.parse().ok()),
        )
    } else {
        match netloc.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
                (h.to_string(), p.parse().ok())
            }
            _ => (netloc.to_string(), None),
        }
    };
    let (path, query) = path_q.split_once('?').unwrap_or((path_q, ""));
    Url {
        scheme: scheme.to_lowercase(),
        host: host.to_lowercase(),
        port,
        path: path.to_string(),
        query: query.to_string(),
    }
}

fn netloc(u: &Url) -> String {
    let host = if u.host.contains(':') {
        format!("[{}]", u.host)
    } else {
        u.host.clone()
    };
    let default = match u.scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    match u.port {
        Some(p) if Some(p) != default => format!("{host}:{p}"),
        _ => host,
    }
}

/// The canonical URI of an MCP server (RFC 8707): lower-case scheme and host, no fragment, no trailing slash.
pub fn canonical_resource(url: &str) -> String {
    let u = parse_url(url);
    let path = if u.path == "/" {
        String::new()
    } else {
        u.path.trim_end_matches('/').to_string()
    };
    let q = if u.query.is_empty() {
        String::new()
    } else {
        format!("?{}", u.query)
    };
    format!("{}://{}{path}{q}", u.scheme, netloc(&u))
}

fn require_secure(url: &str, what: &str) -> R<()> {
    let u = parse_url(url);
    if (u.scheme == "https" && !u.host.is_empty())
        || (u.scheme == "http" && LOOPBACK.contains(&u.host.as_str()))
    {
        return Ok(());
    }
    err(format!(
        "The {what} ({url}) is not an https address; FLE Link only signs in over https."
    ))
}

/// The parameters of the first Bearer challenge of a `WWW-Authenticate` header (empty when there is none).
pub fn bearer_challenge(header: &str) -> HashMap<String, String> {
    let re = regex::Regex::new(
        r#"^\s*(?:(?P<param>[A-Za-z][\w-]*)\s*=\s*(?:"(?P<q>(?:[^"\\]|\\.)*)"|(?P<t>[^\s,]+))|(?P<scheme>[A-Za-z][\w-]*))\s*,?"#,
    )
    .unwrap();
    let unescape = regex::Regex::new(r"\\(.)").unwrap();
    let mut out: Vec<(String, HashMap<String, String>)> = vec![];
    let mut pos = 0;
    while pos < header.len() {
        let Some(m) = re.captures(&header[pos..]) else {
            break;
        };
        let end = m.get(0).unwrap().end();
        if end == 0 {
            break;
        }
        pos += end;
        if let Some(s) = m.name("scheme") {
            out.push((s.as_str().to_string(), HashMap::new()));
        } else if let Some(last) = out.last_mut() {
            let v = m
                .name("q")
                .or_else(|| m.name("t"))
                .map_or("", |x| x.as_str());
            last.1.insert(
                m["param"].to_lowercase(),
                unescape.replace_all(v, "$1").to_string(),
            );
        }
    }
    out.into_iter()
        .find(|(s, _)| s.eq_ignore_ascii_case("bearer"))
        .map(|(_, p)| p)
        .unwrap_or_default()
}

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("random bytes");
    b
}

pub fn challenge_s256(verifier: &str) -> String {
    b64url(&sha2::Sha256::digest(verifier.as_bytes()))
}

fn form_encode(form: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish()
}

/// (status, parsed JSON or null). An HTTP error status is a status, not an error; an unreachable server is one.
pub type Http =
    dyn Fn(&str, &str, Option<&[(&str, &str)]>, Option<&Value>) -> R<(u16, Value)> + Send + Sync;

pub fn http_request(
    method: &str,
    url: &str,
    form: Option<&[(&str, &str)]>,
    body: Option<&Value>,
) -> R<(u16, Value)> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build();
    let req = agent
        .request(method, url)
        .set(
            "User-Agent",
            &format!("flelink/{}", env!("CARGO_PKG_VERSION")),
        )
        .set("Accept", "application/json");
    let r = if let Some(f) = form {
        req.set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&form_encode(f))
    } else if let Some(b) = body {
        req.set("Content-Type", "application/json")
            .send_string(&b.to_string())
    } else {
        req.call()
    };
    let (status, text) = match r {
        Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
        Err(ureq::Error::Transport(t)) => {
            let why = t
                .message()
                .map(String::from)
                .unwrap_or_else(|| t.kind().to_string());
            return err(format!("Could not reach {}: {why}", parse_url(url).host));
        }
    };
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

// ------------------------------------------------------------------------------------------------ discovery

pub struct Discovery {
    pub resource: String,
    pub issuer: String,
    pub metadata: Value,
    pub scope: String,
}

fn protected_resource_urls(mcp_url: &str, challenge: &HashMap<String, String>) -> Vec<String> {
    if let Some(u) = challenge.get("resource_metadata").filter(|u| !u.is_empty()) {
        return vec![u.clone()];
    }
    let u = parse_url(mcp_url);
    let root = format!(
        "{}://{}/.well-known/oauth-protected-resource",
        u.scheme,
        netloc(&u)
    );
    let path = u.path.trim_end_matches('/');
    let mut out = vec![];
    if !path.is_empty() {
        out.push(format!("{root}{path}"));
    }
    out.push(root);
    out
}

fn authorization_server_urls(issuer: &str) -> Vec<String> {
    let u = parse_url(issuer);
    let base = format!("{}://{}", u.scheme, netloc(&u));
    let path = u.path.trim_end_matches('/');
    if path.is_empty() {
        return vec![
            format!("{base}/.well-known/oauth-authorization-server"),
            format!("{base}/.well-known/openid-configuration"),
        ];
    }
    vec![
        format!("{base}/.well-known/oauth-authorization-server{path}"),
        format!("{base}/.well-known/openid-configuration{path}"),
        format!("{base}{path}/.well-known/openid-configuration"),
    ]
}

pub fn discover(mcp_url: &str, challenge: &HashMap<String, String>, http: &Http) -> R<Discovery> {
    let resource = canonical_resource(mcp_url);
    let mut rm: Option<Value> = None;
    for url in protected_resource_urls(mcp_url, challenge) {
        require_secure(&url, "resource metadata address")?;
        let (status, doc) = http("GET", &url, None, None)?;
        if status == 200 && doc.is_object() {
            rm = Some(doc);
            break;
        }
    }
    let rm = match rm {
        // servers of the 2025-03-26 revision publish no resource metadata and are their own authorization server
        None => {
            let u = parse_url(mcp_url);
            json!({"authorization_servers": [format!("{}://{}", u.scheme, netloc(&u))]})
        }
        Some(rm) => {
            if let Some(r) = rm["resource"].as_str().filter(|r| !r.is_empty()) {
                if canonical_resource(r) != resource {
                    return err(format!("The server describes itself as {r}, not {resource}; refusing to sign in (the metadata does not belong to this server)."));
                }
            }
            rm
        }
    };
    let Some(first) = rm["authorization_servers"][0].as_str() else {
        return err("The server's resource metadata names no authorization server.");
    };
    let issuer = if matches!(parse_url(first).path.as_str(), "" | "/") {
        first.trim_end_matches('/').to_string()
    } else {
        first.to_string()
    };
    require_secure(&issuer, "authorization server")?;
    let mut meta: Option<Value> = None;
    for url in authorization_server_urls(&issuer) {
        let (status, doc) = http("GET", &url, None, None)?;
        if status == 200
            && doc["authorization_endpoint"]
                .as_str()
                .is_some_and(|e| !e.is_empty())
        {
            meta = Some(doc);
            break;
        }
    }
    let Some(meta) = meta else {
        return err(format!(
            "The authorization server {issuer} has no metadata at the well-known addresses."
        ));
    };
    let named = meta["issuer"]
        .as_str()
        .filter(|i| !i.is_empty())
        .unwrap_or(&issuer);
    if named.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return err(format!(
            "The metadata of {issuer} names a different issuer ({named}); refusing to go on."
        ));
    }
    for key in ["authorization_endpoint", "token_endpoint"] {
        let Some(v) = meta[key].as_str() else {
            return err(format!("The authorization server metadata has no {key}."));
        };
        require_secure(v, &key.replace('_', " "))?;
    }
    if !meta["code_challenge_methods_supported"]
        .as_array()
        .is_some_and(|a| a.iter().any(|m| m == "S256"))
    {
        return err("The authorization server does not say that it supports PKCE with S256 (code_challenge_methods_supported), so FLE Link will not sign in.");
    }
    let scope = match challenge.get("scope").filter(|s| !s.is_empty()) {
        Some(s) => s.clone(),
        None => rm["scopes_supported"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(crate::settings::py_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default(),
    };
    Ok(Discovery {
        resource,
        issuer,
        metadata: meta,
        scope,
    })
}

fn refusal(what: &str, status: u16, doc: &Value) -> OAuthError {
    let why = doc["error_description"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| doc["error"].as_str())
        .unwrap_or("");
    OAuthError {
        message: format!(
            "{what} was refused ({status}{}).",
            if why.is_empty() {
                String::new()
            } else {
                format!(": {why}")
            }
        ),
        needs_sign_in: false,
    }
}

/// Dynamic Client Registration (RFC 7591) as a public client: (client_id, client_secret).
fn register_client(
    meta: &Value,
    redirect_uri: &str,
    scope: &str,
    http: &Http,
) -> R<(String, String)> {
    let Some(endpoint) = meta["registration_endpoint"].as_str() else {
        return err(format!("This server does not offer automatic client registration. Register FLE Link with it yourself and add \"oauth\": {{\"client_id\": \"…\"}} to its entry in the MCP servers text. FLE Link's redirect address is {redirect_uri}"));
    };
    require_secure(endpoint, "registration endpoint")?;
    let mut body = json!({"client_name": CLIENT_NAME, "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
        "token_endpoint_auth_method": "none"});
    if !scope.is_empty() {
        body["scope"] = json!(scope);
    }
    let (status, doc) = http("POST", endpoint, None, Some(&body))?;
    let id = doc["client_id"].as_str().filter(|i| !i.is_empty());
    match (status, id) {
        (200 | 201, Some(id)) => Ok((
            id.to_string(),
            doc["client_secret"].as_str().unwrap_or("").to_string(),
        )),
        _ => Err(refusal("Client registration", status, &doc)),
    }
}

// ------------------------------------------------------------------------------------------------ tokens on disk

/// One JSON file per server in `<data>/oauth/`, readable by the user only. The file name is a hash of the server's name and
/// address, so changing the address never sends the old token to the new one.
struct TokenStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl TokenStore {
    fn path(&self, name: &str, resource: &str) -> PathBuf {
        let digest = hex::encode(sha2::Sha256::digest(
            format!("{name}\n{}", canonical_resource(resource)).as_bytes(),
        ));
        self.dir.join(format!("{}.json", &digest[..20]))
    }

    fn load(&self, name: &str, resource: &str) -> Option<Value> {
        let t = std::fs::read_to_string(self.path(name, resource)).ok()?;
        serde_json::from_str::<Value>(&t)
            .ok()
            .filter(Value::is_object)
    }

    fn save(&self, name: &str, resource: &str, record: &Value) -> R<()> {
        let _g = self.lock.lock().unwrap();
        let fail = |e: std::io::Error| OAuthError {
            message: format!("The sign-in could not be saved: {e}"),
            needs_sign_in: false,
        };
        std::fs::create_dir_all(&self.dir).map_err(fail)?;
        let target = self.path(name, resource);
        let tmp = target.with_extension("tmp");
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            o.mode(0o600);
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
        use std::io::Write;
        let mut f = o.open(&tmp).map_err(fail)?;
        f.write_all(record.to_string().as_bytes()).map_err(fail)?;
        drop(f);
        std::fs::rename(&tmp, &target).map_err(fail)
    }

    fn delete(&self, name: &str, resource: &str) -> bool {
        let _g = self.lock.lock().unwrap();
        std::fs::remove_file(self.path(name, resource)).is_ok()
    }
}

fn token_record(doc: &Value, old: Option<&Value>) -> R<Map<String, Value>> {
    let Some(access) = doc["access_token"].as_str().filter(|a| !a.is_empty()) else {
        return err("The token response has no access token.");
    };
    let ttype = doc["token_type"].as_str().unwrap_or("Bearer");
    if !ttype.eq_ignore_ascii_case("bearer") {
        return err(format!(
            "The server issued a {ttype} token; only Bearer tokens are supported."
        ));
    }
    let old_str = |k: &str| old.and_then(|o| o[k].as_str()).unwrap_or("").to_string();
    let mut m = Map::new();
    m.insert("access_token".into(), json!(access));
    m.insert(
        "refresh_token".into(),
        json!(doc["refresh_token"]
            .as_str()
            .filter(|r| !r.is_empty())
            .map(String::from)
            .unwrap_or_else(|| old_str("refresh_token"))),
    );
    m.insert(
        "expires_at".into(),
        json!(doc["expires_in"].as_f64().map_or(0.0, |e| now() + e)),
    );
    m.insert(
        "scope".into(),
        json!(doc["scope"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| old_str("scope"))),
    );
    Ok(m)
}

// ------------------------------------------------------------------------------------------------ the flows

struct Pending {
    at: f64,
    name: String,
    url: String,
    verifier: String,
    redirect_uri: String,
    client_id: String,
    client_secret: String,
    issuer: String,
    token_endpoint: String,
    revocation_endpoint: String,
    resource: String,
}

/// Sign-ins in progress, tokens, refreshing. One per Studio.
pub struct OAuthManager {
    store: TokenStore,
    http: Box<Http>,
    pending: Mutex<HashMap<String, Pending>>,
    refresh_lock: Mutex<()>,
}

impl OAuthManager {
    pub fn new(data_dir: &Path) -> OAuthManager {
        OAuthManager {
            store: TokenStore {
                dir: data_dir.join("oauth"),
                lock: Mutex::new(()),
            },
            http: Box::new(http_request),
            pending: Mutex::new(HashMap::new()),
            refresh_lock: Mutex::new(()),
        }
    }

    /// Start a sign-in: {auth_url, state, issuer, scope}. `preset`: client_id, client_secret, scope from the settings.
    pub fn begin(
        &self,
        name: &str,
        url: &str,
        redirect_uri: &str,
        challenge: &HashMap<String, String>,
        preset: &Value,
    ) -> R<Value> {
        let disc = discover(url, challenge, &*self.http)?;
        let p = |k: &str| preset[k].as_str().unwrap_or("").to_string();
        let scope = if p("scope").is_empty() {
            disc.scope.clone()
        } else {
            p("scope")
        };
        let (client_id, client_secret) = if p("client_id").is_empty() {
            register_client(&disc.metadata, redirect_uri, &scope, &*self.http)?
        } else {
            (p("client_id"), p("client_secret"))
        };
        let state = b64url(&random(24));
        let verifier = b64url(&random(48));
        let endpoint = disc.metadata["authorization_endpoint"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let challenge_value = challenge_s256(&verifier);
        let mut params: Vec<(&str, &str)> = vec![
            ("response_type", "code"),
            ("client_id", &client_id),
            ("redirect_uri", redirect_uri),
            ("state", &state),
            ("code_challenge", &challenge_value),
            ("code_challenge_method", "S256"),
            ("resource", &disc.resource),
        ];
        if !scope.is_empty() {
            params.push(("scope", &scope));
        }
        let sep = if parse_url(&endpoint).query.is_empty() {
            "?"
        } else {
            "&"
        };
        let auth_url = format!("{endpoint}{sep}{}", form_encode(&params));
        let mut pending = self.pending.lock().unwrap();
        let t = now();
        pending.retain(|_, v| t - v.at < PENDING_SECONDS);
        pending.insert(
            state.clone(),
            Pending {
                at: t,
                name: name.into(),
                url: url.into(),
                verifier,
                redirect_uri: redirect_uri.into(),
                client_id,
                client_secret,
                issuer: disc.issuer.clone(),
                token_endpoint: disc.metadata["token_endpoint"]
                    .as_str()
                    .unwrap_or("")
                    .into(),
                revocation_endpoint: disc.metadata["revocation_endpoint"]
                    .as_str()
                    .unwrap_or("")
                    .into(),
                resource: disc.resource.clone(),
            },
        );
        Ok(json!({"auth_url": auth_url, "state": state, "issuer": disc.issuer, "scope": scope}))
    }

    /// The browser came back to the callback: exchange the code, store the token, return the server's name.
    pub fn complete(&self, state: &str, code: &str, error: &str, description: &str) -> R<String> {
        let pend = if state.is_empty() {
            None
        } else {
            self.pending.lock().unwrap().remove(state)
        };
        let Some(pend) = pend.filter(|p| now() - p.at <= PENDING_SECONDS) else {
            return err("This sign-in is not known here (it may have expired or been opened twice). Start it again.");
        };
        if !error.is_empty() {
            return err(format!(
                "The server refused the sign-in: {error}{}",
                if description.is_empty() {
                    String::new()
                } else {
                    format!(" - {description}")
                }
            ));
        }
        if code.is_empty() {
            return err("The server's answer has no authorization code.");
        }
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &pend.redirect_uri),
            ("client_id", &pend.client_id),
            ("code_verifier", &pend.verifier),
            ("resource", &pend.resource),
        ];
        if !pend.client_secret.is_empty() {
            form.push(("client_secret", &pend.client_secret));
        }
        let (status, doc) = (self.http)("POST", &pend.token_endpoint, Some(&form), None)?;
        if status != 200 || !doc.is_object() {
            return Err(refusal("The token request", status, &doc));
        }
        let mut rec = token_record(&doc, None)?;
        for (k, v) in [
            ("client_id", &pend.client_id),
            ("client_secret", &pend.client_secret),
            ("token_endpoint", &pend.token_endpoint),
            ("revocation_endpoint", &pend.revocation_endpoint),
            ("issuer", &pend.issuer),
            ("resource", &pend.resource),
        ] {
            rec.insert(k.into(), json!(v));
        }
        rec.insert("saved_at".into(), json!(now()));
        self.store
            .save(&pend.name, &pend.url, &Value::Object(rec))?;
        Ok(pend.name)
    }

    /// What the page may know: never the token, only whether there is one.
    pub fn status(&self, name: &str, url: &str) -> Value {
        let Some(rec) = self.store.load(name, url) else {
            return json!({"signed_in": false});
        };
        let exp = rec["expires_at"].as_f64().unwrap_or(0.0);
        json!({"signed_in": true, "scope": rec["scope"].as_str().unwrap_or(""), "issuer": rec["issuer"].as_str().unwrap_or(""),
            "expires_at": if exp != 0.0 { json!(exp) } else { Value::Null },
            "can_refresh": rec["refresh_token"].as_str().is_some_and(|r| !r.is_empty())})
    }

    /// A token for a request; refreshes one that is about to expire.
    pub fn access_token(&self, name: &str, url: &str) -> R<String> {
        let Some(mut rec) = self
            .store
            .load(name, url)
            .filter(|r| r["access_token"].as_str().is_some_and(|a| !a.is_empty()))
        else {
            return needs("Sign in to use this server.");
        };
        let exp = rec["expires_at"].as_f64().unwrap_or(0.0);
        if exp != 0.0 && exp - now() < EXPIRY_MARGIN {
            rec = self.refresh(name, url)?;
        }
        Ok(rec["access_token"].as_str().unwrap_or("").to_string())
    }

    /// Use the refresh token; forgets the token when the server refuses it.
    pub fn refresh(&self, name: &str, url: &str) -> R<Value> {
        let _g = self.refresh_lock.lock().unwrap();
        let Some(rec) = self.store.load(name, url) else {
            return needs("Sign in to use this server.");
        };
        let s = |k: &str| rec[k].as_str().unwrap_or("").to_string();
        if s("refresh_token").is_empty() {
            self.store.delete(name, url);
            return needs("The sign-in expired. Sign in again.");
        }
        let (rt, cid, res, sec) = (
            s("refresh_token"),
            s("client_id"),
            s("resource"),
            s("client_secret"),
        );
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", &rt),
            ("client_id", &cid),
            ("resource", &res),
        ];
        if !sec.is_empty() {
            form.push(("client_secret", &sec));
        }
        let (status, doc) = (self.http)("POST", &s("token_endpoint"), Some(&form), None)?;
        if status != 200 || !doc.is_object() {
            if (400..500).contains(&status) {
                self.store.delete(name, url);
                return needs("The sign-in expired. Sign in again.");
            }
            return err(format!(
                "The token server did not answer the refresh ({status}); try again in a moment."
            ));
        }
        let mut new = rec.as_object().cloned().unwrap_or_default();
        for (k, v) in token_record(&doc, Some(&rec))? {
            new.insert(k, v);
        }
        new.insert("saved_at".into(), json!(now()));
        let new = Value::Object(new);
        self.store.save(name, url, &new)?;
        Ok(new)
    }

    /// Remove the token; asks the server to revoke it when it can (best effort).
    pub fn sign_out(&self, name: &str, url: &str) -> bool {
        if let Some(rec) = self.store.load(name, url) {
            let s = |k: &str| rec[k].as_str().unwrap_or("").to_string();
            let endpoint = s("revocation_endpoint");
            if !endpoint.is_empty() && !s("access_token").is_empty() {
                for (token, hint) in [
                    (s("refresh_token"), "refresh_token"),
                    (s("access_token"), "access_token"),
                ] {
                    if token.is_empty() {
                        continue;
                    }
                    let (cid, sec) = (s("client_id"), s("client_secret"));
                    let mut form: Vec<(&str, &str)> = vec![
                        ("token", &token),
                        ("token_type_hint", hint),
                        ("client_id", &cid),
                    ];
                    if !sec.is_empty() {
                        form.push(("client_secret", &sec));
                    }
                    let _ = (self.http)("POST", &endpoint, Some(&form), None);
                }
            }
        }
        self.store.delete(name, url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_behave_like_the_python_studios() {
        assert_eq!(
            canonical_resource("HTTPS://Example.COM:443/mcp/"),
            "https://example.com/mcp"
        );
        assert_eq!(
            canonical_resource("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080"
        );
        let c = bearer_challenge(
            r#"Basic realm="x", Bearer resource_metadata="https://a.example/.well-known/oauth-protected-resource", scope="read write""#,
        );
        assert_eq!(c["scope"], "read write");
        assert!(c["resource_metadata"].starts_with("https://a.example/"));
        assert!(bearer_challenge("Basic realm=x").is_empty());
        // RFC 7636 appendix B
        assert_eq!(
            challenge_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(
            protected_resource_urls("https://x.example/a/mcp", &HashMap::new()),
            [
                "https://x.example/.well-known/oauth-protected-resource/a/mcp",
                "https://x.example/.well-known/oauth-protected-resource"
            ]
        );
        assert_eq!(
            authorization_server_urls("https://id.example/tenant").len(),
            3
        );
        assert!(require_secure("http://localhost:9/x", "x").is_ok());
        assert!(require_secure("http://evil.example/x", "x").is_err());
    }
}
