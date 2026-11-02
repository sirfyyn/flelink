//! The HTTP side of FLE Studio: the guard (host name, origin, session token), the JSON conventions and the static page.
//! Port of the plumbing in `fle_studio/server.py`; routes live next to the code they call.
use crate::chats::{ChatError, Chats, MAX_BYTES as MAX_CHAT};
use crate::downloads::Downloads;
use crate::engine::{Engine, EngineErr};
use crate::engines::{EngineError, Engines};
use crate::launch::Profiles;
use crate::library::{LibError, Library};
use crate::live::Live;
use crate::settings::{py_str, py_truthy, Config};
use axum::body::{to_bytes, Body};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use axum::Router;
use base64::Engine as _;
use serde_json::{json, Map, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MAX_JSON: usize = 4 * 1024 * 1024;
pub const MAX_PROXY: usize = 64 * 1024 * 1024;
const LOOPBACK_NAMES: [&str; 3] = ["127.0.0.1", "::1", "localhost"];

pub struct Options {
    pub home: PathBuf,
    /// The built web page (index.html, js/, css). Later it is embedded in the binary.
    pub web_dir: Option<PathBuf>,
    pub version: String,
    /// Where GitHub's releases are asked (tests point it at a server of their own).
    pub github_api: String,
}

pub struct Studio {
    pub opts: Options,
    pub token: String,
    pub config: Arc<Mutex<Config>>,
    pub chats: Chats,
    pub chat_lock: Mutex<()>,
    pub library: Mutex<Library>,
    pub downloads: Downloads,
    pub engines: Engines,
    pub profiles: Mutex<Profiles>,
    pub engine: Engine,
    pub bench: crate::bench::Bench,
    pub tuner: crate::tuner::Tuner,
    pub quality: crate::quality::Quality,
    pub forge: crate::forge::Forge,
    pub updates: crate::updates::Updates,
    pub clients: crate::remote::Clients,
    pub hosts: crate::remote::Hosts,
    /// the TLS certificate's fingerprint (64 hex digits) when this server speaks https; what clients pin
    pub tls_fingerprint: Mutex<String>,
    /// where this server listens: {host, port, tls}
    pub listen: Mutex<Value>,
    pub cache: Arc<crate::cachewatch::CacheWatch>,
    pub toolbox: crate::mcp::ToolBox,
    /// the port this server listens on (for the sign-in's loopback redirect); set when it starts serving
    pub port: std::sync::atomic::AtomicU16,
    pub live: Live,
    /// the server listens beyond this machine (host mode); set by the starter
    pub lan: std::sync::atomic::AtomicBool,
}

/// An error the page shows as `{"error": {"message": ...}}`.
pub struct ApiError(pub u16, pub String);

pub type Ctx = Arc<Studio>;

fn make_token() -> String {
    let mut b = [0u8; 24];
    getrandom::getrandom(&mut b).expect("random bytes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub fn new_studio(opts: Options) -> Ctx {
    new_studio_lan(opts, false)
}

/// `lan`: the server listens beyond this machine (host mode); other machines need the access key.
pub fn new_studio_lan(opts: Options, lan: bool) -> Ctx {
    let github_api = opts.github_api.clone();
    let config = Config::load(&opts.home.join("settings.json"));
    let home = opts.home.clone();
    let config = Arc::new(Mutex::new(config));
    let tok = config.clone();
    let downloads = Downloads::new(
        &home,
        Box::new(move || {
            tok.lock()
                .unwrap()
                .get("hf_token")
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default()
        }),
    );
    let engines = Engines::new(&home, downloads.clone(), config.clone(), &opts.github_api);
    let engine = Engine::new(&home);
    let cache = Arc::new(crate::cachewatch::CacheWatch::default());
    let feed = cache.clone();
    engine.add_listener(Box::new(move |line| {
        feed.feed(line);
    }));
    engine.set_restart_on_crash(
        config
            .lock()
            .unwrap()
            .get("launch.restart_on_crash")
            .map(crate::settings::py_truthy)
            .unwrap_or(false),
    );
    let chats = Chats::new(&home);
    let studio = Arc::new(Studio {
        opts,
        token: make_token(),
        config,
        chats,
        chat_lock: Mutex::new(()),
        library: Mutex::new(Library::new(&home)),
        engines,
        lan: std::sync::atomic::AtomicBool::new(lan),
        live: Live::new(engine.clone(), 1.0),
        bench: crate::bench::Bench::new(engine.clone(), &home),
        tuner: crate::tuner::Tuner::new(&home),
        quality: crate::quality::Quality::new(&home),
        forge: crate::forge::Forge::new(&home),
        updates: crate::updates::Updates::new(&home, &github_api),
        clients: crate::remote::Clients::new(&home),
        hosts: crate::remote::Hosts::new(&home),
        tls_fingerprint: Mutex::new(String::new()),
        listen: Mutex::new(json!({})),
        cache,
        toolbox: crate::mcp::ToolBox::new(&home),
        port: std::sync::atomic::AtomicU16::new(0),
        engine,
        profiles: Mutex::new(Profiles::new(&home)),
        downloads,
    });
    if std::env::var_os("FLE_NO_UPDATE").is_none_or(|v| v.is_empty()) {
        let weak = Arc::downgrade(&studio);
        let _ = std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(8));
                loop {
                    let Some(st) = weak.upgrade() else { return };
                    let config = Value::Object(st.config.lock().unwrap().data.clone());
                    if st.updates.due(&config) {
                        let _ = st.updates.check(&config, &st.engines.installed(), false);
                    }
                    drop(st);
                    std::thread::sleep(std::time::Duration::from_secs(1800));
                }
            });
    }
    let weak = Arc::downgrade(&studio);
    studio
        .engine
        .set_on_load_failure(Box::new(move |plan, rc, error| {
            weak.upgrade()
                .and_then(|s| s.load_failure_fallback(plan, rc, error))
        }));
    studio
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn base_headers(r: &mut Response<Body>, ctype: &str, extra: &[(&str, &str)]) {
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(ctype).unwrap());
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    for (k, v) in extra {
        // like Python's send_header: a header named twice is sent twice (Cache-Control on pages)
        h.append(
            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
}

pub fn send(
    status: u16,
    body: Vec<u8>,
    ctype: &str,
    extra: &[(&str, &str)],
    head: bool,
) -> Response<Body> {
    let len = body.len();
    let mut r = Response::new(if head {
        Body::empty()
    } else {
        Body::from(body)
    });
    *r.status_mut() = StatusCode::from_u16(status).unwrap();
    base_headers(&mut r, ctype, extra);
    r.headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    r
}

pub fn send_json(status: u16, data: &Value, head: bool) -> Response<Body> {
    send(
        status,
        serde_json::to_vec(data).unwrap(),
        "application/json; charset=utf-8",
        &[],
        head,
    )
}

fn error_json(status: u16, msg: &str, head: bool) -> Response<Body> {
    send_json(status, &json!({"error": {"message": msg}}), head)
}

fn host_ok(st: &Studio, headers: &HeaderMap) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string();
    let name = if let Some(r) = host.strip_prefix('[') {
        r.split(']').next().unwrap_or("").to_string()
    } else {
        host.rsplit_once(':')
            .map(|x| x.0)
            .unwrap_or(&host)
            .to_string()
    };
    let name = name.to_lowercase();
    if LOOPBACK_NAMES.contains(&name.as_str()) {
        return true;
    }
    if !st.is_lan() {
        return false;
    }
    if name.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let own = machine_name().to_lowercase();
    let configured = st
        .config
        .lock()
        .unwrap()
        .get("server.allowed_hosts")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let mut allowed: Vec<String> = vec![own.split('.').next().unwrap_or("").to_string(), own];
    allowed.extend(
        configured
            .split(',')
            .map(|h| h.trim().to_lowercase())
            .filter(|h| !h.is_empty()),
    );
    allowed
        .iter()
        .any(|a| !a.is_empty() && (name == *a || name.starts_with(&format!("{a}."))))
}

/// This machine's name, as `socket.gethostname()` says it.
pub(crate) fn machine_name() -> String {
    #[cfg(unix)]
    {
        let mut b = [0u8; 256];
        // SAFETY: the buffer is 256 bytes long and gethostname writes at most that many
        let rc = unsafe { libc::gethostname(b.as_mut_ptr() as *mut libc::c_char, b.len()) };
        if rc == 0 {
            let n = b.iter().position(|c| *c == 0).unwrap_or(b.len());
            return String::from_utf8_lossy(&b[..n]).to_string();
        }
        String::new()
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

/// The cookie that proves a sign-in: derived from the access key, so the key itself never sits in a cookie.
fn session_cookie(st: &Studio) -> String {
    use hmac::{Hmac, Mac};
    let key = access_key(st);
    if key.is_empty() {
        return String::new();
    }
    let mut m = <Hmac<sha2::Sha256> as Mac>::new_from_slice(key.as_bytes()).expect("hmac key");
    m.update(b"fle-studio-session");
    hex::encode(m.finalize().into_bytes())
}

fn access_key(st: &Studio) -> String {
    st.config
        .lock()
        .unwrap()
        .get("access_key")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// This machine itself, or another one that has the access key (a cookie after signing in, or a Bearer header).
fn authed(st: &Studio, headers: &HeaderMap, peer: &SocketAddr) -> bool {
    if peer.ip().is_loopback() {
        return true;
    }
    let expected = session_cookie(st);
    if expected.is_empty() {
        return false;
    }
    let key = access_key(st);
    if let Some(b) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        if ct_eq(b.trim(), &key) {
            return true;
        }
    }
    let cookies = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(";");
    cookies.split(';').any(|part| {
        part.trim()
            .split_once('=')
            .is_some_and(|(k, v)| k == "fle_session" && ct_eq(v, &expected))
    })
}

/// The Origin must be this page's own host (a request from another site is refused), and a mutating call carries the
/// session token that the served page was given.
fn guard(
    st: &Studio,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    peer: &SocketAddr,
    head: bool,
) -> Option<Response<Body>> {
    if !host_ok(st, headers) {
        return Some(send(
            403,
            b"FLE Studio: this host name is not allowed.".to_vec(),
            "text/plain; charset=utf-8",
            &[],
            head,
        ));
    }
    if path == "/login" || path == "/api/remote/pair" {
        return None;
    }
    if let Some(tok) = bearer(headers).filter(|t| t.starts_with(crate::remote::TOKEN_PREFIX)) {
        let Some(client) = st.clients.find(&tok, &peer.ip().to_string()) else {
            return Some(send_json(
                401,
                &json!({"error": {"message": "This client is not paired with this host (any more).", "code": "unpaired"}}),
                head,
            ));
        };
        let confirmed = headers.get("x-fle-confirm").and_then(|v| v.to_str().ok()) == Some("1");
        return crate::remote::check_rights(&client, method.as_str(), path, confirmed).map(
            |(status, message, code)| {
                let mut err = json!({"message": message, "code": code});
                if status == 428 {
                    err["confirm"] = json!(true);
                }
                send_json(status, &json!({"error": err}), head)
            },
        );
    }
    if !authed(st, headers, peer) {
        if path.starts_with("/api/") || path.starts_with("/v1/") {
            return Some(error_json_auth(head));
        }
        return Some(send(
            303,
            vec![],
            "text/plain",
            &[("Location", "/login")],
            head,
        ));
    }
    if matches!(
        *method,
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    ) {
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        if let Some(o) = origin.filter(|o| *o != "null" && !o.is_empty()) {
            let netloc = o
                .split_once("://")
                .map(|x| x.1)
                .unwrap_or(o)
                .split('/')
                .next()
                .unwrap_or("");
            if Some(netloc) != headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
                return Some(error_json(403, "Cross-origin request refused", head));
            }
        }
        if origin == Some("null") {
            return Some(error_json(
                403,
                "Request from an opaque origin refused",
                head,
            ));
        }
        if (path.starts_with("/api/") || path.starts_with("/h/"))
            && !ct_eq(
                headers
                    .get("x-fle-token")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(""),
                &st.token,
            )
        {
            return Some(send(
                403,
                serde_json::to_vec(
                    &json!({"error": {"message": "Session expired; reload the page"}}),
                )
                .unwrap(),
                "application/json; charset=utf-8",
                &[("X-FLE-Session", "expired")],
                head,
            ));
        }
    }
    None
}

/// The token of an `Authorization: Bearer …` header.
fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string())
}

fn error_json_auth(head: bool) -> Response<Body> {
    send_json(
        401,
        &json!({"error": {"message": "Sign in with the access key", "type": "auth"}}),
        head,
    )
}

fn query_of(q: Option<&str>) -> Map<String, Value> {
    let mut m = Map::new();
    for (k, v) in form_urlencoded::parse(q.unwrap_or("").as_bytes()) {
        if !v.is_empty() {
            m.insert(k.to_string(), Value::String(v.to_string()));
        }
    }
    m
}

pub struct Req {
    /// the address the request came from
    pub peer: String,
    /// the paired client a `Bearer fcl_…` token belongs to (null for everything else)
    pub client: Value,
    pub method: Method,
    pub path: String,
    pub query: Map<String, Value>,
    pub body: Vec<u8>,
    /// the body was larger than any route accepts
    pub too_large: bool,
}

impl Req {
    /// The JSON object in the body (an empty body is `{}`); more than `limit` bytes is a 413, like `read_json(limit)`.
    pub fn json(&self, limit: usize) -> Result<Map<String, Value>, ApiError> {
        if self.too_large || self.body.len() > limit {
            return Err(ApiError(413, "Request too large".into()));
        }
        if self.body.is_empty() {
            return Ok(Map::new());
        }
        match serde_json::from_slice::<Value>(&self.body) {
            Ok(Value::Object(m)) => Ok(m),
            Ok(_) => Err(ApiError(400, "Expected a JSON object".into())),
            Err(_) => Err(ApiError(400, "Invalid JSON".into())),
        }
    }
}

fn chat_err(e: ChatError) -> ApiError {
    match e {
        ChatError::NotFound => ApiError(404, "Not found".into()),
        ChatError::Invalid(m) => ApiError(400, m),
    }
}

/// `/api/chats/<16 hex>` -> the id
fn chat_id(p: &str) -> Option<&str> {
    let id = p.strip_prefix("/api/chats/")?;
    (id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(id)
}

impl Studio {
    /// Does the server listen beyond this machine?
    pub fn is_lan(&self) -> bool {
        self.lan.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_lan(&self, on: bool) {
        self.lan.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn models_dir(&self) -> std::path::PathBuf {
        self.config.lock().unwrap().models_dir()
    }

    /// The engine a start should use (`engine_for`).
    pub fn engine_for(&self, want: &str, info: &Value) -> Result<Value, ApiError> {
        let (custom_path, choice) = {
            let c = self.config.lock().unwrap();
            let g = |k: &str| {
                c.get("engine")
                    .and_then(|e| e.get(k))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            (g("custom_path"), g("choice"))
        };
        let custom = if custom_path.is_empty() {
            None
        } else {
            self.engines.resolve("custom", info).ok()
        };
        if want == "custom" {
            return custom.ok_or_else(|| {
                ApiError(
                    400,
                    "Set a working llama-server path on the Engine page".into(),
                )
            });
        }
        if let Some(c) = &custom {
            if flelink_core::adapter::is_installable(want)
                && c["flavor"] == want
                && choice == "custom"
            {
                return Ok(c.clone());
            }
        }
        match self.engines.resolve(want, info) {
            Ok(e) => Ok(e),
            Err(EngineError::Runtime(m)) => match custom {
                Some(c) if want == "auto" || c["flavor"] == want => Ok(c),
                _ => Err(ApiError(409, m)),
            },
            Err(_) => Err(ApiError(409, "No engine".into())),
        }
    }

    /// The command a start would run (`make_plan`).
    pub fn make_plan(
        &self,
        model_ident: &str,
        profile_ident: Option<&Value>,
    ) -> Result<crate::launch::Plan, ApiError> {
        use crate::launch::{
            auto_plan, free_port, profile_plan, AutoInput, PlanError, ProfileInput,
        };
        enum Fail {
            Api(ApiError),
            Plan(PlanError),
        }
        let model = if model_ident.is_empty() {
            None
        } else {
            self.library
                .lock()
                .unwrap()
                .get(&self.models_dir(), model_ident)
        };
        let settings = Value::Object(self.config.lock().unwrap().data.clone());
        let given = profile_ident
            .filter(|v| crate::settings::py_truthy(v))
            .map(crate::settings::py_str);
        let prof_id = given
            .clone()
            .unwrap_or_else(|| match settings["launch"].get("profile") {
                Some(v) if crate::settings::py_truthy(v) => crate::settings::py_str(v),
                _ => "auto".into(),
            });
        let info = self.sysinfo();
        let want_port = settings["launch"]
            .get("port")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 65535) as u16;
        let port = i64::from(free_port(want_port));
        let data_dir = self.opts.home.clone();
        let attempt = || -> Result<crate::launch::Plan, Fail> {
            if prof_id == "auto" {
                let Some(m) = &model else {
                    return Err(Fail::Api(ApiError(404, "Choose a model first".into())));
                };
                if m["complete"] != json!(true) {
                    return Err(Fail::Api(ApiError(
                        409,
                        "This model is not completely downloaded yet".into(),
                    )));
                }
                let eng = self.engine_for("auto", &info).map_err(Fail::Api)?;
                return auto_plan(&AutoInput {
                    model: m,
                    engine: &eng,
                    settings: &settings,
                    sysinfo: &info,
                    port,
                    data_dir: &data_dir,
                })
                .map_err(Fail::Plan);
            }
            let prof =
                self.profiles.lock().unwrap().get(&prof_id).ok_or_else(|| {
                    Fail::Api(ApiError(404, format!("No launch profile {prof_id}")))
                })?;
            let pengine = crate::settings::py_str(&prof.engine);
            let eng: Option<Value> = if pengine == "custom" {
                let bin = crate::settings::py_str(&prof.binary);
                // Only to learn which flags its binary knows; the profile still starts that binary.
                if !bin.is_empty() && !bin.contains('{') {
                    Some(self.engines.detect(&bin))
                } else {
                    None
                }
            } else {
                Some(self.engine_for(&pengine, &info).map_err(Fail::Api)?)
            };
            profile_plan(&ProfileInput {
                profile: &prof,
                model: model.as_ref(),
                engine: eng.as_ref(),
                settings: &settings,
                sysinfo: &info,
                port,
                data_dir: &data_dir,
            })
            .map_err(Fail::Plan)
        };
        match attempt() {
            Ok(p) => Ok(p),
            Err(Fail::Api(e)) => Err(e),
            Err(Fail::Plan(exc)) => {
                // Plan-time upstream fallback: if the FLE plan cannot be built (no NVIDIA GPU for the FLE engine, say),
                // retry once with an installed upstream llama-server and speculation off. This does not rescue a model
                // that fails after it has started loading (the engine's own load fallback does that).
                if prof_id != "auto" {
                    // (the page sends "auto" itself, so that is not a reason to refuse)
                    return Err(ApiError(400, exc.0));
                }
                let Some(up) = self.engines.resolve("upstream", &info).ok() else {
                    return Err(ApiError(
                        400,
                        format!(
                            "{} No upstream llama.cpp is installed to fall back to.",
                            exc.0
                        ),
                    ));
                };
                let mut plan = match self.upstream_plan(model.as_ref(), &settings, &up, &info, port)
                {
                    Ok(p) => p,
                    Err(fb) => {
                        return Err(ApiError(
                            400,
                            format!("{} The upstream fallback failed too: {}", exc.0, fb.0),
                        ))
                    }
                };
                let why = exc.0.trim().to_string();
                plan.fallback = format!(
                    "The FLE plan could not be built: {why}{} Studio fell back to upstream llama.cpp; FLE features and speculation are off.",
                    if why.ends_with('.') { "" } else { "." }
                );
                plan.notes.insert(0, plan.fallback.clone());
                Ok(plan)
            }
        }
    }

    /// The automatic plan for an upstream llama-server with speculation off (the fallback of the FLE plan).
    fn upstream_plan(
        &self,
        model: Option<&Value>,
        settings: &Value,
        upstream: &Value,
        info: &Value,
        port: i64,
    ) -> Result<crate::launch::Plan, crate::launch::PlanError> {
        let mut fb = settings.clone();
        fb["launch"]["mtp"] = json!(false);
        fb["launch"]["spec"] = json!("none");
        let m = model.cloned().unwrap_or(json!({}));
        crate::launch::auto_plan(&crate::launch::AutoInput {
            model: &m,
            engine: upstream,
            settings: &fb,
            sysinfo: info,
            port,
            data_dir: &self.opts.home,
        })
    }

    /// The engine ended while loading. With the automatic profile, start upstream llama.cpp once instead. Only for a plan that
    /// Studio built itself (profile auto), not for one that already is the upstream engine, and not when
    /// `launch.fallback_on_failure` is off or no upstream llama-server is installed.
    pub fn load_failure_fallback(
        &self,
        failed: &crate::launch::Plan,
        rc: i32,
        error: &str,
    ) -> Option<crate::launch::Plan> {
        let settings = Value::Object(self.config.lock().unwrap().data.clone());
        if !settings["launch"]
            .get("fallback_on_failure")
            .map(crate::settings::py_truthy)
            .unwrap_or(true)
            || failed.profile != "auto"
        {
            return None;
        }
        let info = self.sysinfo();
        let upstream = self.engines.resolve("upstream", &info).ok()?;
        if upstream["path"].as_str() == Some(failed.binary.as_str()) {
            return None;
        }
        let want_port = settings["launch"]
            .get("port")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 65535) as u16;
        let port = i64::from(crate::launch::free_port(want_port));
        let mut plan = self
            .upstream_plan(Some(&failed.model), &settings, &upstream, &info, port)
            .ok()?;
        let what = format!("exit code {rc}")
            + &if error.is_empty() {
                String::new()
            } else {
                format!(": {error}")
            };
        plan.fallback = format!("The engine ended while loading ({what}). Starting upstream llama.cpp instead, once; FLE features and speculation are off. Switch this off under Engine > Advanced > Start upstream if loading fails.");
        plan.notes.insert(0, plan.fallback.clone());
        Some(plan)
    }

    /// What the user can do in one click so that the plan can use MTP, as buttons (`plan_fixes`).
    pub fn plan_fixes(&self, model_ident: &str, profile_ident: Option<&Value>) -> Vec<Value> {
        let launch_cfg = self
            .config
            .lock()
            .unwrap()
            .get("launch")
            .cloned()
            .unwrap_or(json!({}));
        let prof_id = match profile_ident.filter(|v| crate::settings::py_truthy(v)) {
            Some(v) => crate::settings::py_str(v),
            None => match launch_cfg.get("profile") {
                Some(v) if crate::settings::py_truthy(v) => crate::settings::py_str(v),
                _ => "auto".into(),
            },
        };
        let model = if model_ident.is_empty() {
            None
        } else {
            self.library
                .lock()
                .unwrap()
                .get(&self.models_dir(), model_ident)
        };
        let Some(model) = model.filter(|m| prof_id == "auto" && m["catalog_id"].is_string()) else {
            return vec![];
        };
        let mtp = if model["mtp"].is_object() {
            model["mtp"].clone()
        } else {
            json!({})
        };
        if mtp["supported"] != json!(true) || (mtp["head"].is_string() && mtp["vocab"].is_string())
        {
            return vec![];
        }
        let spec = match launch_cfg.get("spec") {
            Some(v) if crate::settings::py_truthy(v) => crate::settings::py_str(v).to_lowercase(),
            _ => "auto".into(),
        };
        if (spec != "auto" && spec != "mtp")
            || !launch_cfg
                .get("mtp")
                .map(crate::settings::py_truthy)
                .unwrap_or(true)
        {
            return vec![]; // MTP is off on purpose (or contradicted): a download would not change the plan
        }
        let Some(entry) = crate::catalog::by_id(model["catalog_id"].as_str().unwrap_or(""))
            .filter(|e| e["mtp"] == json!(true))
        else {
            return vec![];
        };
        let size = crate::catalog::mtp_head()["size"].as_i64().unwrap_or(0)
            + crate::catalog::mtp_vocab()["size"].as_i64().unwrap_or(0);
        vec![
            json!({"kind": "download_mtp", "model": entry["id"], "label": "Download MTP files", "size": size}),
        ]
    }

    /// Whether the Speculation setting changes the command: `{"applies", "reason"}` (`speculation_view`).
    pub fn speculation_view(
        &self,
        profile_ident: Option<&Value>,
        plan: Option<&crate::launch::Plan>,
    ) -> Value {
        let cfg_profile = self.config.lock().unwrap().get("launch.profile").cloned();
        let prof_id = match profile_ident.filter(|v| crate::settings::py_truthy(v)) {
            Some(v) => crate::settings::py_str(v),
            None => match cfg_profile {
                Some(v) if crate::settings::py_truthy(&v) => crate::settings::py_str(&v),
                _ => "auto".into(),
            },
        };
        if prof_id != "auto" {
            return json!({"applies": false, "reason": "A start profile's own command decides; speculation applies to the automatic start only."});
        }
        if let Some(p) = plan.filter(|p| !p.fallback.is_empty()) {
            return json!({"applies": false, "reason": p.fallback});
        }
        let flavor: Option<String> = match plan {
            Some(p) if p.profile == "auto" && flelink_core::adapter::is_installable(&p.engine) => {
                Some(p.engine.clone())
            }
            _ => self
                .engine_for("auto", &self.sysinfo())
                .ok()
                .map(|e| match e.get("flavor") {
                    Some(v) if crate::settings::py_truthy(v) => crate::settings::py_str(v),
                    _ => crate::settings::py_str(&e["kind"]),
                }),
        };
        if flavor.as_deref() == Some("upstream") {
            return json!({"applies": false, "reason": "Speculation needs the FLE engine; the selected engine is upstream llama.cpp. Pick an FLE engine to use it."});
        }
        json!({"applies": true, "reason": ""})
    }

    /// `make_plan` for the live preview: a plan that cannot be built is an answer, not a failure (`plan_view`).
    pub fn plan_view(&self, model_ident: &str, profile_ident: Option<&Value>) -> Value {
        let fixes = self.plan_fixes(model_ident, profile_ident);
        match self.make_plan(model_ident, profile_ident) {
            Err(e) => {
                json!({"ok": false, "error": e.1, "fixes": fixes, "speculation": self.speculation_view(profile_ident, None)})
            }
            Ok(plan) => {
                let public = plan.public();
                let launch_cfg = self
                    .config
                    .lock()
                    .unwrap()
                    .get("launch")
                    .cloned()
                    .unwrap_or(json!({}));
                let model = if model_ident.is_empty() {
                    None
                } else {
                    self.library
                        .lock()
                        .unwrap()
                        .get(&self.models_dir(), model_ident)
                };
                let spec = match launch_cfg.get("spec") {
                    Some(v) if crate::settings::py_truthy(v) => crate::settings::py_str(v),
                    _ => "auto".into(),
                };
                let chips = crate::planview::summary(
                    &public,
                    model.as_ref(),
                    &spec,
                    launch_cfg
                        .get("mtp")
                        .map(crate::settings::py_truthy)
                        .unwrap_or(true),
                );
                json!({"ok": true, "plan": public, "fixes": fixes, "speculation": self.speculation_view(profile_ident, Some(&plan)), "summary": chips})
            }
        }
    }

    /// Everything the page needs at once (`Studio.state`).
    pub fn state(&self) -> Value {
        let info = self.sysinfo();
        let (os, arch) = crate::engines::os_arch();
        let models = self.library.lock().unwrap().scan(&self.models_dir());
        json!({
            "app": "fle-studio", "version": self.opts.version, "platform": info["platform"],
            "systemd": cfg!(target_os = "linux") && flelink_core::launch::which("systemctl").is_some(),
            "data_dir": self.opts.home.to_string_lossy(), "lan": self.is_lan(),
            "settings": self.config.lock().unwrap().public(), "system": info.clone(), "engine": self.engine.status(), "models": models,
            "catalog": self.catalog_view(), "extras": self.extras_view(),
            "profiles": self.profiles.lock().unwrap().all().iter().map(|p| p.public()).collect::<Vec<_>>(),
            "engines": {"installed": self.engines.installed(), "variants": crate::engines::variants(&info, os, &arch)},
            "downloads": self.downloads.list(),
            "reference": {"machine": crate::catalog::reference_machine(), "report": crate::catalog::report_url()},
        })
    }

    /// What the loaded model can do, as far as the engine says (`capabilities`).
    pub fn capabilities(&self) -> Value {
        let props = self.engine.props();
        let modalities = props
            .get("modalities")
            .filter(|m| m.is_object())
            .cloned()
            .unwrap_or(json!({}));
        let caps = props
            .get("chat_template_caps")
            .filter(|m| m.is_object())
            .cloned()
            .unwrap_or(json!({}));
        let limit = match self.config.lock().unwrap().get("chat.max_upload_mb") {
            Some(v) => v.as_i64().unwrap_or(25).clamp(1, 512),
            None => 25,
        };
        json!({"vision": modalities.get("vision").is_some_and(crate::settings::py_truthy), "tools": caps.get("supports_tools").cloned().unwrap_or(Value::Null),
            "n_ctx": self.engine.status()["n_ctx"], "max_upload_mb": limit, "pdf": false})
    }

    pub fn tool_settings(&self) -> Value {
        self.config
            .lock()
            .unwrap()
            .get("tools")
            .cloned()
            .unwrap_or(json!({}))
    }

    /// One tool call (`ToolBox.call`): a built-in tool or one of an MCP server.
    pub fn tools_call(&self, body: &Map<String, Value>) -> Result<Value, ApiError> {
        let name = body
            .get("name")
            .filter(|v| py_truthy(v))
            .map(py_str)
            .unwrap_or_default();
        let args = body
            .get("arguments")
            .filter(|v| py_truthy(v))
            .cloned()
            .unwrap_or(json!({}));
        let approved = body.get("approved") == Some(&json!(true));
        self.toolbox
            .call(&self.tool_settings(), &name, &args, approved)
            .map_err(|e| match e {
                crate::mcp::CallRefused::Unknown => ApiError(
                    404,
                    "That tool is not available (turned off, or its server is not connected)."
                        .into(),
                ),
                crate::mcp::CallRefused::NeedsApproval => {
                    ApiError(409, "This tool needs your approval first.".into())
                }
            })
    }

    pub fn sysinfo(&self) -> Value {
        crate::system::info(&self.models_dir())
    }

    /// The llama-server files whose folders are searched for the Forge tools.
    fn forge_engine_paths(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .engines
            .installed()
            .iter()
            .map(|e| ("engine".to_string(), py_str(&e["path"])))
            .collect();
        let custom = self
            .config
            .lock()
            .unwrap()
            .get("engine.custom_path")
            .map(py_str)
            .unwrap_or_default();
        if !custom.is_empty() {
            out.push(("custom engine".into(), custom));
        }
        out
    }

    fn forge_tools(
        &self,
        refresh: bool,
    ) -> std::collections::HashMap<String, crate::forge::tools::Tool> {
        let dir = self
            .config
            .lock()
            .unwrap()
            .get("forge.tools_dir")
            .map(py_str)
            .unwrap_or_default();
        self.forge.tools(refresh, &dir, &self.forge_engine_paths())
    }

    fn forge_overview(&self) -> Value {
        let found = self.forge_tools(true);
        let dir = self
            .config
            .lock()
            .unwrap()
            .get("forge.tools_dir")
            .map(py_str)
            .unwrap_or_default();
        self.forge.overview(&found, &dir, &self.models_dir())
    }

    /// Add a finished Forge model to the library and save a start profile for it.
    fn forge_publish(&self, id: &str) -> Result<Value, ApiError> {
        let info = self.sysinfo();
        let gpus = info["gpus"].as_array().cloned().unwrap_or_default();
        let unified = gpus.iter().any(|g| g["unified"] == true);
        let (final_, name, args, notes) = self
            .forge
            .publish_plan(id, !gpus.is_empty(), unified)
            .map_err(|e| ApiError(e.0, e.1))?;
        let fle = self.engines.installed().iter().any(|e| e["kind"] == "fle");
        let model = self
            .library
            .lock()
            .unwrap()
            .add(&self.models_dir(), &final_)
            .map_err(|e| ApiError(400, e))?;
        let Value::Object(data) = json!({"name": format!("Forge: {name}"), "engine": if fle { "fle" } else { "upstream" },
            "binary": "", "args": args, "env": {}, "notes": notes})
        else {
            unreachable!()
        };
        let prof = self
            .profiles
            .lock()
            .unwrap()
            .save(&data)
            .map_err(|e| ApiError(400, e.0))?;
        Ok(json!({"model": model, "profile": prof.public()}))
    }

    fn forge_route(
        &self,
        method: &str,
        p: &str,
        body: Option<Value>,
        query: &Map<String, Value>,
    ) -> Option<Result<Value, ApiError>> {
        let fe = |e: crate::forge::ForgeError| ApiError(e.0, e.1);
        let body = body.unwrap_or(json!({}));
        let jobs = &self.forge.jobs;
        let job = |suffix: &str| -> Option<String> {
            let id = p.strip_prefix("/api/forge/jobs/")?.strip_suffix(suffix)?;
            (id.len() == 12
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
            .then(|| id.to_string())
        };
        let not_found = || ApiError(404, "Not found".into());
        Some(match (method, p) {
            ("GET", "/api/forge") => Ok(self.forge_overview()),
            ("POST", "/api/forge/tools") => {
                let dir = py_str(&body["tools_dir"]);
                let mut patch = Map::new();
                patch.insert("forge".into(), json!({"tools_dir": if body["tools_dir"].is_null() { String::new() } else { dir }}));
                match self.config.lock().unwrap().update(&patch) {
                    Ok(_) => {}
                    Err(e) => return Some(Err(ApiError(400, e))),
                }
                Ok(self.forge_overview())
            }
            ("POST", "/api/forge/inspect") => {
                self.forge.inspect(&py_str(&body["path"])).map_err(fe)
            }
            ("POST", "/api/forge/calculate") => self.forge.calculate(&body).map_err(fe),
            ("POST", "/api/forge/corpus") => self.forge.make_corpus(&body).map_err(fe),
            ("POST", "/api/forge/recipe/import") => {
                let r = if body["recipe"].is_null() || body["recipe"] == false {
                    body.clone()
                } else {
                    body["recipe"].clone()
                };
                self.forge.import_recipe(&r.to_string()).map_err(fe)
            }
            ("POST", "/api/forge/recipe/share") => {
                let r = if body["recipe"].is_object() {
                    body["recipe"].clone()
                } else {
                    json!({})
                };
                crate::forge::recipe::portable(&r)
                    .map(|r| json!({"recipe": r}))
                    .map_err(|e| ApiError(400, e.0))
            }
            ("POST", "/api/forge/check") => {
                let r = if body["recipe"].is_object() {
                    body["recipe"].clone()
                } else {
                    json!({})
                };
                jobs.check(&r, &self.forge_tools(false))
                    .map_err(|e| ApiError(400, e))
            }
            ("POST", "/api/forge/jobs") => {
                let r = if body["recipe"].is_object() {
                    body["recipe"].clone()
                } else {
                    json!({})
                };
                jobs.create(&r, self.forge_tools(true), &self.models_dir().join("forge"))
                    .map_err(|(c, m)| ApiError(c, m))
            }
            ("GET", _) if job("").is_some() => {
                let id = job("").unwrap();
                jobs.get_job(&id)
                    .map(|j| jobs.public(&j))
                    .ok_or_else(not_found)
            }
            ("GET", _) if job("/log").is_some() => {
                let since = query
                    .get("since")
                    .map(py_str)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                jobs.log_since(&job("/log").unwrap(), since)
                    .ok_or_else(not_found)
            }
            ("GET", _) if job("/recipe").is_some() => {
                self.forge.recipe_of(&job("/recipe").unwrap()).map_err(fe)
            }
            ("POST", _) if job("/cancel").is_some() => {
                jobs.cancel(&job("/cancel").unwrap()).ok_or_else(not_found)
            }
            ("POST", _) if job("/resume").is_some() => {
                let id = job("/resume").unwrap();
                if jobs.get_job(&id).is_none() {
                    Err(not_found())
                } else {
                    jobs.resume(&id, self.forge_tools(true))
                        .map_err(|e| ApiError(409, e))
                }
            }
            ("POST", _) if job("/delete").is_some() => {
                let id = job("/delete").unwrap();
                if jobs.get_job(&id).is_none() {
                    Err(not_found())
                } else {
                    jobs.delete(&id, !py_truthy(&body["delete_model"]))
                        .map(|_| json!({"ok": true}))
                        .map_err(|e| ApiError(409, e))
                }
            }
            ("POST", _) if job("/publish").is_some() => {
                self.forge_publish(&job("/publish").unwrap())
            }
            _ => return None,
        })
    }

    /// Which of the two MTP files are in the models folder (`extras_view`).
    pub fn extras_view(&self) -> Value {
        let root = self.models_dir();
        let one = |r: &Value| {
            let dest = root
                .join(r["repo"].as_str().unwrap_or(""))
                .join(r["path"].as_str().unwrap_or(""));
            json!({"name": r["name"], "size": r["size"], "present": dest.exists()})
        };
        json!({"mtp_head": one(crate::catalog::mtp_head()), "mtp_vocab": one(crate::catalog::mtp_vocab())})
    }

    /// The files of a catalog model (and its MTP head and vocabulary): name for the job, and `[{url, dest, size, sha256}]`.
    pub fn catalog_files(
        ident: &str,
        with_mtp: bool,
        root: &std::path::Path,
    ) -> Result<(String, Vec<Value>), ApiError> {
        let model = crate::catalog::by_id(ident)
            .ok_or_else(|| ApiError(404, "Unknown catalog model".into()))?;
        let mut refs: Vec<Value> = model["files"].as_array().cloned().unwrap_or_default();
        if with_mtp && model["mtp"].as_bool() == Some(true) {
            refs.push(crate::catalog::mtp_head().clone());
            refs.push(crate::catalog::mtp_vocab().clone());
        }
        let files = refs
            .iter()
            .map(|r| {
                let (repo, path) = (r["repo"].as_str().unwrap_or(""), r["path"].as_str().unwrap_or(""));
                json!({"url": crate::hf::resolve_url(repo, path, "main"), "dest": root.join(repo).join(path).to_string_lossy(), "size": r["size"], "sha256": r["sha256"]})
            })
            .collect();
        Ok((model["name"].as_str().unwrap_or("").to_string(), files))
    }

    /// Queue a catalog model for download (`download_catalog`).
    pub fn download_catalog(&self, ident: &str, with_mtp: bool) -> Result<Value, ApiError> {
        let (name, files) = Self::catalog_files(ident, with_mtp, &self.models_dir())?;
        Ok(self.downloads.add(
            &name,
            &files,
            "model",
            json!({"catalog_id": ident}),
            &format!("catalog:{ident}"),
        ))
    }

    /// Where a recipe's file goes in the Studio's models folder (the same layout as the terminal's).
    fn recipe_file(&self, r: &flelink_core::recipe::Recipe, file: &str) -> std::path::PathBuf {
        self.models_dir().join(r.local_path(file))
    }

    /// Every file of a recipe (the model's parts and a draft model) is on disk.
    fn recipe_on_disk(&self, r: &flelink_core::recipe::Recipe) -> bool {
        let models = self.models_dir();
        r.fetches().iter().all(|x| models.join(&x.local).exists())
    }

    /// `/api/recipes`: every recipe (built in, the recipes folder, plugins) with fit and whether it is on disk.
    pub fn recipes(&self) -> Value {
        let (all, problems) = flelink_core::recipe::load_all(&self.opts.home);
        let sys = self.sysinfo();
        let list: Vec<Value> = all
            .iter()
            .map(|r| {
                let have = self.recipe_on_disk(r);
                json!({"id": r.id, "title": r.title, "note": r.note, "repo": r.model.repo, "files": r.model.files,
                    "draft": r.draft.as_ref().map(|d| json!({"file": d.file, "size": d.size})),
                    "size": r.model.size, "source": r.source, "engine": r.engine.prefer, "strict": r.engine.strict,
                    "context": r.run.context, "chat": serde_json::to_value(&r.chat).unwrap_or(Value::Null),
                    "downloaded": have,
                    "fit": recipe_fit(r, &self.models_dir(), &sys)})
            })
            .collect();
        json!({"recipes": list, "problems": problems})
    }

    /// `POST /api/recipes/<id>/download`: one job for every file of the recipe that is not on disk yet.
    pub fn recipe_download(&self, id: &str) -> Result<Value, ApiError> {
        let r = flelink_core::recipe::find(&self.opts.home, id)
            .map_err(|e| ApiError(404, e.to_string()))?;
        // sizes and checksums from the repo where the recipe does not give them (the recipe's own values win)
        let mut trees: std::collections::HashMap<(String, String), Vec<Value>> = Default::default();
        let mut files = vec![];
        let models = self.models_dir();
        // a copy already on this machine is linked, not downloaded again; Hugging Face's checksums confirm it
        let mut wanted = r.fetches();
        for x in wanted
            .iter_mut()
            .filter(|x| !models.join(&x.local).exists())
        {
            let tree = trees
                .entry((x.repo.clone(), x.revision.clone()))
                .or_insert_with(|| {
                    crate::hf::tree(&x.repo, &x.revision, &self.hf_token()).unwrap_or_default()
                });
            if let Some(t) = tree
                .iter()
                .find(|t| t["path"].as_str() == Some(x.file.as_str()))
            {
                if x.sha256.is_none() {
                    x.sha256 = t["lfs"]["oid"].as_str().map(String::from);
                }
                if x.size == 0 {
                    x.size = t["size"].as_u64().unwrap_or(0);
                }
            }
        }
        let found =
            flelink_core::locate::adopt_fetches(wanted, &self.opts.home, &models, &mut |_| {});
        if self.recipe_on_disk(&r) {
            return Ok(
                json!({"id": null, "state": "done", "title": r.title, "found": found.iter()
                .map(|(f, p)| json!({"file": f, "path": p.to_string_lossy()})).collect::<Vec<_>>()}),
            );
        }
        for x in r
            .fetches()
            .into_iter()
            .filter(|x| !models.join(&x.local).exists())
        {
            let tree = trees
                .entry((x.repo.clone(), x.revision.clone()))
                .or_insert_with(|| {
                    crate::hf::tree(&x.repo, &x.revision, &self.hf_token()).unwrap_or_default()
                });
            let t = tree
                .iter()
                .find(|t| t["path"].as_str() == Some(x.file.as_str()));
            if !tree.is_empty() && t.is_none() {
                return Err(ApiError(404, format!("{} has no file {}", x.repo, x.file)));
            }
            let size = t.and_then(|t| t["size"].as_i64()).unwrap_or(x.size as i64);
            let sha = x
                .sha256
                .clone()
                .or_else(|| t.and_then(|t| t["lfs"]["oid"].as_str().map(String::from)));
            files.push(
                json!({"url": crate::hf::resolve_url(&x.repo, &x.file, &x.revision),
                "dest": models.join(&x.local).to_string_lossy(), "size": size,
                "sha256": sha.map(Value::from).unwrap_or(Value::Null)}),
            );
        }
        let mut job = self.downloads.add(
            &r.title,
            &files,
            "model",
            json!({"recipe": r.id}),
            &format!("recipe:{}", r.id),
        );
        job["found"] = json!(found
            .iter()
            .map(|(f, p)| json!({"file": f, "path": p.to_string_lossy()}))
            .collect::<Vec<_>>());
        Ok(job)
    }

    /// The start plan for a plugin engine: its server (installed from its release the first time, which this request
    /// waits for), then either its own start template or the automatic llama-server arguments for what that binary knows,
    /// plus the plugin's arguments, environment and readiness path.
    fn plugin_plan(
        &self,
        r: &flelink_core::recipe::Recipe,
        e: &flelink_core::plugin::PluginEngine,
        main: &str,
    ) -> Result<crate::launch::Plan, ApiError> {
        let sys = flelink_core::system::detect(&self.opts.home);
        let server =
            flelink_core::plugin::server(&self.opts.home, e, &sys.os, &sys.arch, &mut |_, _, _| {})
                .map_err(|x| ApiError(409, format!("{x:#}")))?;
        let model = self
            .library
            .lock()
            .unwrap()
            .get(&self.models_dir(), main)
            .unwrap_or_else(|| json!({"path": main}));
        let settings = Value::Object(self.config.lock().unwrap().data.clone());
        let want_port = settings["launch"]
            .get("port")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 65535) as u16;
        let port = crate::launch::free_port(want_port);
        let mut plan = if e.command.is_empty() {
            let d = self.engines.detect(&server.to_string_lossy());
            if d["ok"] != json!(true) {
                return Err(ApiError(
                    409,
                    format!(
                        "The {} engine does not run here: {}",
                        e.label,
                        crate::settings::py_str(&d["problem"])
                    ),
                ));
            }
            let eng = json!({"kind": "custom", "flavor": d["flavor"], "backend": d["backend"],
                "path": server.to_string_lossy(), "version": d["version"],
                "log_verbosity": d["log_verbosity"], "spec_type": d["spec_type"]});
            let info = self.sysinfo();
            let mut p = crate::launch::auto_plan(&crate::launch::AutoInput {
                model: &model,
                engine: &eng,
                settings: &settings,
                sysinfo: &info,
                port: i64::from(port),
                data_dir: &self.opts.home,
            })
            .map_err(|x| ApiError(400, x.0))?;
            p.args.extend(e.args.iter().cloned());
            p
        } else {
            let filled = flelink_core::plugin::fill_command(
                e,
                &server,
                std::path::Path::new(main),
                "127.0.0.1",
                port,
                r.run.context.unwrap_or(8192),
                &r.id,
            );
            let first = PathBuf::from(&filled[0]);
            let program = if first.is_absolute() {
                first
            } else {
                flelink_core::launch::which(&filled[0])
                    .ok_or_else(|| ApiError(409, format!("{} is not on the PATH", filled[0])))?
            };
            crate::launch::Plan {
                engine: "custom".into(),
                binary: program.to_string_lossy().to_string(),
                args: filled[1..].to_vec(),
                env: vec![],
                port: i64::from(port),
                model,
                profile: "auto".into(),
                notes: vec![],
                draft: None,
                fallback: String::new(),
                health: String::new(),
            }
        };
        for (k, v) in &e.env {
            match plan.env.iter_mut().find(|(a, _)| a == k) {
                Some(x) => x.1 = v.clone(),
                None => plan.env.push((k.clone(), v.clone())),
            }
        }
        plan.health = e.health.clone();
        plan.notes.push(format!("Engine {} from a plugin", e.label));
        Ok(plan)
    }

    /// The model a tuning run or comparison measures: the one named, else the last one started.
    fn tuning_model(&self, body: &Value) -> Result<String, ApiError> {
        let ident = if py_truthy(&body["model"]) {
            py_str(&body["model"])
        } else {
            self.config
                .lock()
                .unwrap()
                .get("last_model")
                .filter(|v| py_truthy(v))
                .map(py_str)
                .unwrap_or_default()
        };
        let m = self.library.lock().unwrap().get(&self.models_dir(), &ident);
        m.map(|m| py_str(&m["id"]))
            .ok_or_else(|| ApiError(400, "Choose a model first".into()))
    }

    fn clamp_int(v: &Value, default: i64, low: i64, high: i64) -> i64 {
        let n = v
            .as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()));
        n.map_or(default, |n| n.clamp(low, high))
    }

    /// A library model by id or path, or 400 "Choose a model first".
    fn library_model(&self, ident: &Value) -> Result<Value, ApiError> {
        let ident = if py_truthy(ident) {
            py_str(ident)
        } else {
            String::new()
        };
        self.library
            .lock()
            .unwrap()
            .get(&self.models_dir(), &ident)
            .ok_or_else(|| ApiError(400, "Choose a model first".into()))
    }

    /// The name this machine shows to the others (`remote.name` in the settings, else the machine's name).
    pub fn host_name(&self) -> String {
        let n = self
            .config
            .lock()
            .unwrap()
            .get("remote.name")
            .map(py_str)
            .unwrap_or_default();
        let n = n.trim();
        if n.is_empty() || n == "None" {
            machine_name().split('.').next().unwrap_or("").to_string()
        } else {
            n.to_string()
        }
    }

    /// `GET /api/remote`: what the host side shows: where it listens, its fingerprint, the code, the clients.
    pub fn remote_view(&self) -> Value {
        let listen = self.listen.lock().unwrap().clone();
        let fp = self.tls_fingerprint.lock().unwrap().clone();
        let port = listen["port"]
            .as_u64()
            .unwrap_or(crate::remote::DEFAULT_PORT as u64);
        let own = machine_name();
        let addresses: Vec<String> = [own]
            .into_iter()
            .chain(crate::run::lan_ip_string())
            .filter(|n| !n.is_empty())
            .map(|n| format!("{n}:{port}"))
            .collect();
        json!({"name": self.host_name(),
            "listening": {"host": listen["host"].as_str().unwrap_or(""), "port": listen["port"].as_u64().unwrap_or(0), "tls": listen["tls"] == true},
            "fingerprint": fp, "fingerprint_pretty": crate::remote::pretty_fp(&fp), "addresses": addresses,
            "code": self.clients.code_state(), "clients": self.clients.list()})
    }

    /// `POST /api/quality/kld`: KL divergence of a candidate against a reference, with the engine's llama-perplexity.
    pub fn quality_kld(&self, body: &Value) -> Result<Value, ApiError> {
        let reference = self.library_model(&body["reference"])?;
        let candidate = self.library_model(&body["candidate"])?;
        let engine = self.engine_for("auto", &self.sysinfo())?;
        self.quality
            .start_kld(
                &json!({"name": reference["name"], "path": reference["path"]}),
                &json!({"name": candidate["name"], "path": candidate["path"]}),
                engine["path"].as_str().unwrap_or(""),
                &if py_truthy(&body["text"]) {
                    py_str(&body["text"])
                } else {
                    String::new()
                },
                Self::clamp_int(&body["ctx"], crate::quality::KLD_CTX, 64, 8192),
                Self::clamp_int(&body["chunks"], 0, 0, 1000),
                Self::clamp_int(&body["threads"], 0, 0, 512),
            )
            .map_err(|(c, m)| ApiError(c, m))
    }

    /// `POST /api/quality/probe`: the knowledge questions to the running engine.
    pub fn quality_probe(&self, body: &Value) -> Result<Value, ApiError> {
        let (state, base, key) = self.engine.target();
        if !matches!(state.as_str(), "ready" | "attached") || base.is_empty() {
            return Err(ApiError(
                400,
                "Load a model first: the knowledge probe asks the running engine".into(),
            ));
        }
        let name = self.engine.status()["model"]["name"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let g = |k: &str| {
            if py_truthy(&body[k]) {
                py_str(&body[k])
            } else {
                String::new()
            }
        };
        let source = if g("source").is_empty() {
            crate::quality::BUILTIN.to_string()
        } else {
            g("source")
        };
        self.quality
            .start_probe(
                &base,
                &key,
                &name,
                &source,
                &g("lang"),
                Self::clamp_int(&body["limit"], 0, 0, 1000) as usize,
                &g("label"),
            )
            .map_err(|(c, m)| ApiError(c, m))
    }

    /// `GET /api/tuning`
    pub fn tuning_state(self: &Arc<Self>, model: &str) -> Value {
        use crate::tuner::*;
        let best = if model.is_empty() {
            Value::Null
        } else {
            Tuner::tuned_profile(self, model)
        };
        json!({"current": self.tuner.status(), "results": self.tuner.results(), "best": best,
            "params": PARAM_LABELS.iter().map(|(k, l)| json!({"key": k, "label": l})).collect::<Vec<_>>(),
            "defaults": {"params": DEFAULT_PARAMS, "runs": 3, "budget_min": 20}})
    }

    /// `POST /api/tuning/start`
    pub fn tuning_start(self: &Arc<Self>, body: &Value) -> Result<Value, ApiError> {
        use crate::tuner::*;
        let model = self.tuning_model(body)?;
        let params: Vec<String> = match body["params"].as_array().filter(|a| !a.is_empty()) {
            Some(a) => a.iter().map(py_str).collect(),
            None => DEFAULT_PARAMS.iter().map(|s| s.to_string()).collect(),
        };
        let state = self.engine.status()["state"].clone();
        if state != "stopped" && state != "crashed" {
            return Err(ApiError(409, "Stop the engine first: the tuner starts it itself, once for every setting it tries.".into()));
        }
        let profile = if py_truthy(&body["profile"]) {
            py_str(&body["profile"])
        } else {
            "auto".into()
        };
        let screen = body.get("screen").is_none_or(py_truthy);
        Tuner::start_tune(
            self,
            &model,
            &profile,
            Self::clamp_int(&body["budget_min"], 20, 1, 1440) * 60,
            Self::clamp_int(&body["runs"], 3, MIN_RUNS, MAX_RUNS),
            params,
            screen,
        )
    }

    /// `POST /api/tuning/aba`
    pub fn tuning_aba(self: &Arc<Self>, body: &Value) -> Result<Value, ApiError> {
        use crate::tuner::*;
        let model = self.tuning_model(body)?;
        let get = |k: &str| {
            if py_truthy(&body[k]) {
                py_str(&body[k])
            } else {
                String::new()
            }
        };
        let (a, b) = (get("a"), get("b"));
        if a.is_empty() || b.is_empty() {
            return Err(ApiError(
                400,
                "Pick the two settings to compare (launch profiles, or Automatic)".into(),
            ));
        }
        Tuner::start_aba(
            self,
            &model,
            &a,
            &b,
            Self::clamp_int(&body["runs"], 3, MIN_RUNS, MAX_RUNS),
            &get("label"),
        )
    }

    /// `POST /api/bench/start`: what is running (model, profile, engine, command, machine) goes with the result, so runs
    /// of different settings can be told apart. With `knowledge` the knowledge probe runs after the speed requests.
    pub fn bench_start(&self, body: &Value) -> Result<Value, ApiError> {
        let st = self.engine.status();
        let info = self.sysinfo();
        let gib = |v: &Value| v.as_f64().unwrap_or(0.0) / 1024f64.powi(3);
        let gpu = info["gpus"]
            .as_array()
            .map(|g| {
                g.iter()
                    .map(|g| {
                        format!(
                            "{} {:.0} GB",
                            crate::settings::py_str(&g["name"]),
                            gib(&g["memory_total"])
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let svc = st["service"].as_object().cloned().unwrap_or_default();
        let plan = &st["plan"];
        let truthy = crate::settings::py_truthy;
        let profile = if truthy(&plan["profile"]) {
            plan["profile"].clone()
        } else if !svc.is_empty() {
            json!(format!("service {}", crate::settings::py_str(&svc["unit"])))
        } else if truthy(&st["attached"]) {
            json!("attached")
        } else {
            json!("")
        };
        let command = if truthy(&plan["command"]) {
            plan["command"].clone()
        } else {
            svc.get("command").cloned().unwrap_or(Value::Null)
        };
        let machine = format!(
            "{}, {:.0} GB RAM{}",
            crate::settings::py_str(&info["cpu"]["name"]),
            gib(&info["memory"]["total"]),
            if gpu.is_empty() {
                String::new()
            } else {
                format!(", {gpu}")
            }
        );
        let context = json!({"model": st["model"].get("name").cloned().unwrap_or(Value::Null), "profile": profile,
            "engine": if truthy(&plan["engine"]) { plan["engine"].clone() } else { json!("") },
            "build": st.get("build").cloned().unwrap_or(Value::Null), "n_ctx": st.get("n_ctx").cloned().unwrap_or(Value::Null),
            "machine": machine, "command": command});
        let mut context = context;
        let probe = if truthy(&body["knowledge"]) {
            let source = if truthy(&body["knowledge_set"]) {
                py_str(&body["knowledge_set"])
            } else {
                crate::quality::BUILTIN.to_string()
            };
            // fail now, not after the speed requests
            crate::quality::load_questions(&source, &self.opts.home, "", 0)
                .map_err(|e| ApiError(400, e))?;
            context["knowledge_set"] = json!(source);
            Some((source, self.opts.home.clone()))
        } else {
            None
        };
        let runs = body["runs"].as_i64().filter(|r| *r != 0).unwrap_or(1);
        let label = if truthy(&body["label"]) {
            crate::settings::py_str(&body["label"])
        } else {
            String::new()
        };
        self.bench
            .start(runs, &label, context, probe)
            .map_err(|e| ApiError(409, e))
    }

    /// `POST /api/recipes/<id>/start`: start the engine with the recipe's model and start values, and use its chat
    /// defaults. A recipe that prefers a plugin engine starts that engine (as `flelink run <id>` does).
    pub fn recipe_start(&self, id: &str) -> Result<Value, ApiError> {
        let r = flelink_core::recipe::find(&self.opts.home, id)
            .map_err(|e| ApiError(404, e.to_string()))?;
        // copies already on this machine count as downloaded
        flelink_core::locate::adopt(&r, &self.opts.home, &self.models_dir(), &mut |_| {});
        if !self.recipe_on_disk(&r) {
            return Err(ApiError(409, format!("Download {} first", r.title)));
        }
        let main = self
            .recipe_file(&r, &r.model.files[0])
            .to_string_lossy()
            .to_string();
        // an engine a plugin brings, when the recipe prefers one and no built-in engine comes before it (as `flelink run`)
        let builtin = flelink_core::adapter::installable_ids();
        let builtin_first = r
            .engine
            .prefer
            .iter()
            .position(|w| builtin.contains(&w.as_str()));
        let plugin_first = r
            .engine
            .prefer
            .iter()
            .position(|w| flelink_core::plugin::engine(&self.opts.home, w).is_some());
        let plugin = plugin_first
            .filter(|pi| builtin_first.is_none_or(|bi| *pi < bi))
            .and_then(|pi| flelink_core::plugin::engine(&self.opts.home, &r.engine.prefer[pi]));
        if plugin.is_none()
            && builtin_first.is_none()
            && !r.engine.prefer.is_empty()
            && r.engine.strict
        {
            return Err(ApiError(
                409,
                format!(
                    "{} needs the {} engine from a plugin that is not installed (flelink plugins lists them)",
                    r.title,
                    r.engine.prefer.join(" or ")
                ),
            ));
        }
        let template = plugin.as_ref().is_some_and(|(_, e)| !e.command.is_empty());
        let mut plan = match &plugin {
            Some((_, e)) => self.plugin_plan(&r, e, &main)?,
            None => self.make_plan(&main, Some(&json!("auto")))?,
        };
        if plugin.is_none() && r.engine.strict && !r.engine.prefer.contains(&plan.engine) {
            return Err(ApiError(
                409,
                format!(
                    "{} needs the {} engine; install it on the Engine page",
                    r.title,
                    r.engine.prefer.join(" or ")
                ),
            ));
        }
        // a template already carries the context in {ctx}
        if let Some(c) = r.run.context.filter(|_| !template) {
            match plan
                .args
                .iter()
                .position(|a| a == "-c" || a == "--ctx-size")
            {
                Some(i) if i + 1 < plan.args.len() => plan.args[i + 1] = c.to_string(),
                _ => plan.args.extend(["-c".to_string(), c.to_string()]),
            }
        }
        plan.args.extend(r.run_args(&self.models_dir()));
        for (k, v) in r.run_env(&self.models_dir()) {
            match plan.env.iter_mut().find(|(a, _)| *a == k) {
                Some(e) => e.1 = v,
                None => plan.env.push((k, v)),
            }
        }
        plan.notes.push(format!("Recipe {}", r.id));
        self.engine.start(plan).map_err(engine_err)?;
        // the recipe's chat defaults become the chat settings; what it does not say stays as it was
        let mut chat = serde_json::Map::new();
        if let Some(t) = r.chat.thinking {
            chat.insert("thinking".into(), json!(t));
        }
        if !r.chat.system.is_empty() {
            chat.insert("system_prompt".into(), json!(r.chat.system));
        }
        if r.chat.temperature.is_some() || r.chat.top_p.is_some() {
            chat.insert("sampling".into(), json!("custom"));
            if let Some(t) = r.chat.temperature {
                chat.insert("temperature".into(), json!(t));
            }
            if let Some(p) = r.chat.top_p {
                chat.insert("top_p".into(), json!(p));
            }
        }
        let mut patch = json!({"last_model": main, "launch": {"profile": "auto"}});
        if !chat.is_empty() {
            patch["chat"] = Value::Object(chat);
        }
        if let Some(t) = r.chat.tools {
            patch["tools"] = json!({"enabled": t});
        }
        self.config
            .lock()
            .unwrap()
            .update(patch.as_object().unwrap())
            .map_err(|e| ApiError(400, e))?;
        Ok(self.engine.status())
    }

    fn hf_token(&self) -> String {
        self.config
            .lock()
            .unwrap()
            .get("hf_token")
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default()
    }

    /// `/api/hf/search?q=`: GGUF repos on Hugging Face, most downloaded first.
    pub fn hf_search(&self, q: &str) -> Result<Value, ApiError> {
        let q = q.trim();
        if q.chars().count() < 2 {
            return Ok(json!({"results": []}));
        }
        crate::hf::search(q, 20, &self.hf_token())
            .map(|r| json!({"results": r}))
            .map_err(|e| ApiError(502, format!("Could not reach Hugging Face: {e}")))
    }

    /// `/api/hf/repo?repo=`: the GGUF units of a repo, each with whether it fits this machine.
    pub fn hf_repo(&self, text: &str) -> Result<Value, ApiError> {
        let (repo, revision, hint) = crate::hf::parse_repo(text).map_err(|e| ApiError(400, e))?;
        let token = self.hf_token();
        let files = crate::hf::tree(&repo, &revision, &token)
            .and_then(|f| crate::hf::repo_info(&repo, &token).map(|i| (f, i)));
        let (files, info) =
            files.map_err(|e| ApiError(404, format!("Could not open {repo}: {e}")))?;
        let sys = self.sysinfo();
        let mut groups = crate::hf::gguf_groups(&files, &hint);
        for g in groups.iter_mut() {
            g["fit"] = if g["kind"] == "model" {
                crate::catalog::fit(g["size"].as_i64().unwrap_or(0), &sys)
            } else {
                Value::Null
            };
        }
        Ok(json!({"repo": repo, "revision": revision, "info": info, "groups": groups}))
    }

    /// `POST /api/hf/download {repo, revision, files}`: one download job for the chosen files of a repo.
    pub fn hf_download(&self, body: &Map<String, Value>) -> Result<Value, ApiError> {
        let text = body.get("repo").and_then(Value::as_str).unwrap_or("");
        let (repo, rev0, _) = crate::hf::parse_repo(text).map_err(|e| ApiError(400, e))?;
        let revision = body
            .get("revision")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
            .map(String::from)
            .unwrap_or(rev0);
        let wanted: Vec<String> = body
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let tree = crate::hf::tree(&repo, &revision, &self.hf_token())
            .map_err(|e| ApiError(404, format!("Could not open {repo}: {e}")))?;
        let missing = wanted
            .iter()
            .find(|p| !tree.iter().any(|f| f["path"].as_str() == Some(p.as_str())));
        // a path from the repo stays inside the models folder
        let unsafe_path = wanted.iter().find(|p| {
            std::path::Path::new(p.as_str())
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        });
        if let Some(p) = unsafe_path {
            return Err(ApiError(400, format!("Refusing the path {p}")));
        }
        if wanted.is_empty() || missing.is_some() {
            let tail = missing
                .map(|m| format!(": {m} does not"))
                .unwrap_or_default();
            return Err(ApiError(
                400,
                format!("Pick files that exist in the repo{tail}"),
            ));
        }
        let root = self.models_dir();
        let files: Vec<Value> = wanted
            .iter()
            .map(|p| {
                let f = tree
                    .iter()
                    .find(|f| f["path"].as_str() == Some(p.as_str()))
                    .unwrap();
                json!({"url": crate::hf::resolve_url(&repo, p, &revision),
                    "dest": root.join(&repo).join(p).to_string_lossy(),
                    "size": f["size"].as_i64().unwrap_or(0),
                    "sha256": f["lfs"].get("oid").cloned().unwrap_or(Value::Null)})
            })
            .collect();
        let first = wanted[0].rsplit('/').next().unwrap_or(&wanted[0]);
        let title = format!(
            "{} · {}",
            repo.rsplit('/').next().unwrap_or(&repo),
            crate::hf::quant_label(first)
        );
        let mut sorted = wanted.clone();
        sorted.sort();
        Ok(self.downloads.add(
            &title,
            &files,
            "model",
            json!({}),
            &format!("hf:{repo}:{}", sorted.join(",")),
        ))
    }

    /// The catalog with what is already on disk and what is being downloaded (`catalog_view`).
    pub fn catalog_view(&self) -> Value {
        let info = crate::system::info(&self.models_dir());
        let models = self.library.lock().unwrap().scan(&self.models_dir());
        let jobs = self.downloads.list();
        let extras = self.extras_view();
        let mut out = vec![];
        for mut entry in crate::catalog::catalog(&info) {
            let id = entry["id"].clone();
            entry.insert(
                "mtp_status".into(),
                crate::catalog::mtp_status(
                    entry.get("mtp").is_some_and(crate::settings::py_truthy),
                    extras["mtp_head"]["present"] == json!(true),
                    extras["mtp_vocab"]["present"] == json!(true),
                ),
            );
            let local = models
                .iter()
                .rev()
                .find(|m| !m["catalog_id"].is_null() && m["catalog_id"] == id)
                .cloned();
            let key = format!("catalog:{}", id.as_str().unwrap_or(""));
            let job = jobs
                .iter()
                .rev()
                .find(|j| {
                    j["dedupe"] == key.as_str()
                        && !matches!(j["state"].as_str(), Some("done" | "cancelled"))
                })
                .cloned();
            entry.insert("local".into(), local.unwrap_or(Value::Null));
            entry.insert("job".into(), job.unwrap_or(Value::Null));
            out.push(Value::Object(entry));
        }
        Value::Array(out)
    }
}

/// `/api/downloads/<hex>/<pause|resume|cancel|remove>` -> (id, action)
fn download_action(p: &str) -> Option<(&str, &str)> {
    let rest = p.strip_prefix("/api/downloads/")?;
    let (id, what) = rest.split_once('/')?;
    (!id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && matches!(what, "pause" | "resume" | "cancel" | "remove"))
    .then_some((id, what))
}

/// `/api/hosts/<12 hex>/<check|connect|repair|update|remove>` -> (id, action)
fn host_action(p: &str) -> Option<(&str, &str)> {
    let (id, action) = p.strip_prefix("/api/hosts/")?.split_once('/')?;
    let ok = id.len() == 12
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    (ok && matches!(action, "check" | "connect" | "repair" | "update" | "remove"))
        .then_some((id, action))
}

fn remote_err(e: crate::remote::RemoteError) -> ApiError {
    // the page reads `code` from the message: the code travels after a marker the error helper understands
    if e.2.is_empty() {
        ApiError(e.0, e.1)
    } else {
        ApiError(e.0, format!("{}\u{1f}{}", e.1, e.2))
    }
}

/// `<prefix><id><suffix>` -> the id when it is `[0-9a-f]+`
fn hex_id<'a>(p: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let id = p.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(id)
}

/// `/api/bench/<id>/delete` -> the result id (`[0-9a-f]+`)
fn bench_delete_id(p: &str) -> Option<&str> {
    let id = p.strip_prefix("/api/bench/")?.strip_suffix("/delete")?;
    (!id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(id)
}

/// `/api/recipes/<id>/download` -> the recipe id (`[\w.-]+`)
fn recipe_download_id(p: &str) -> Option<&str> {
    let id = p.strip_prefix("/api/recipes/")?.strip_suffix("/download")?;
    (!id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
    .then_some(id)
}

/// `/api/recipes/<id>/start` -> the recipe id (`[\w.-]+`)
fn recipe_start_id(p: &str) -> Option<&str> {
    let id = p.strip_prefix("/api/recipes/")?.strip_suffix("/start")?;
    (!id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
    .then_some(id)
}

/// `/api/catalog/<id>/download` -> the catalog id (`[\w.-]+`)
fn catalog_download_id(p: &str) -> Option<&str> {
    let id = p.strip_prefix("/api/catalog/")?.strip_suffix("/download")?;
    (!id.is_empty()
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-')))
    .then_some(id)
}

/// `/api/engines/<id>/remove` -> the engine id (`[\w.-]+`)
fn engine_remove_id(p: &str) -> Option<&str> {
    let id = p.strip_prefix("/api/engines/")?.strip_suffix("/remove")?;
    (!id.is_empty()
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-')))
    .then_some(id)
}

/// `/api/profiles/<id>/delete` -> the profile id (`[\w.-]+`)
fn profile_delete_id(p: &str) -> Option<&str> {
    let id = p.strip_prefix("/api/profiles/")?.strip_suffix("/delete")?;
    (!id.is_empty()
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-')))
    .then_some(id)
}

fn engine_err(e: EngineErr) -> ApiError {
    match e {
        EngineErr::Busy(m) => ApiError(409, m),
        EngineErr::Invalid(m) => ApiError(400, m),
    }
}

/// `/api/engine/service/<start|stop|restart>` -> the action
fn engine_service_action(p: &str) -> Option<&str> {
    p.strip_prefix("/api/engine/service/")
        .filter(|a| matches!(*a, "start" | "stop" | "restart"))
}

/// A call under `/api/p/<id>/`: no plugin is installed in this program, so none is on.
fn plugin_off(p: &str) -> ApiError {
    let rest = &p["/api/p/".len()..];
    let id = rest.split('/').next().unwrap_or("");
    let ok = !id.is_empty()
        && id.len() <= 32
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        ApiError(404, format!("The plugin {id} is not on"))
    } else {
        ApiError(404, "Not found".into())
    }
}

/// `/api/plugins/<id>/<enable|disable>` -> the plugin id
fn plugin_toggle(p: &str) -> Option<&str> {
    let rest = p.strip_prefix("/api/plugins/")?;
    let (id, act) = rest.split_once('/')?;
    let ok = !id.is_empty()
        && id.len() <= 32
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    (ok && matches!(act, "enable" | "disable")).then_some(id)
}

/// `/api/models/<12 hex>/remove` -> the id
fn model_remove_id(p: &str) -> Option<&str> {
    let id = p.strip_prefix("/api/models/")?.strip_suffix("/remove")?;
    (id.len() == 12
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(id)
}

fn route(st: &Ctx, r: &Req) -> Option<Result<Value, ApiError>> {
    let (m, p) = (r.method.as_str(), r.path.as_str());
    Some(match (m, p) {
        ("GET", "/api/ping") => Ok(json!({"app": "fle-studio", "version": st.opts.version})),
        ("GET", "/api/system") => Ok(crate::system::info(&st.config.lock().unwrap().models_dir())),
        ("GET", "/api/chat/capabilities") => Ok(st.capabilities()),
        ("GET", "/api/tools") => Ok(st.toolbox.status(&st.tool_settings())),
        ("POST", "/api/tools/reload") => Ok(st.toolbox.reload(&st.tool_settings())),
        ("POST", "/api/tools/call") => r.json(MAX_JSON).and_then(|body| st.tools_call(&body)),
        ("GET", "/api/state") => Ok(st.state()),
        ("GET", "/api/settings") => Ok(st.config.lock().unwrap().public()),
        ("POST", "/api/settings") => r.json(MAX_JSON).and_then(|body| {
            st.config
                .lock()
                .unwrap()
                .update(&body)
                .map_err(|e| ApiError(400, e))
        }),
        ("GET", "/api/models") => {
            let models = st.library.lock().unwrap().scan(&st.models_dir());
            Ok(json!({"models": models, "extras": st.extras_view()}))
        }
        ("POST", "/api/models/add") => r.json(MAX_JSON).and_then(|body| {
            let path = body
                .get("path")
                .filter(|v| crate::settings::py_truthy(v))
                .map(crate::settings::py_str)
                .unwrap_or_default();
            st.library
                .lock()
                .unwrap()
                .add(&st.models_dir(), &path)
                .map_err(|e| ApiError(400, e))
        }),
        ("POST", _) if model_remove_id(p).is_some() => r.json(MAX_JSON).and_then(|body| {
            let delete = body.get("delete").is_some_and(crate::settings::py_truthy);
            match st.library.lock().unwrap().remove(
                &st.models_dir(),
                model_remove_id(p).unwrap(),
                delete,
            ) {
                Ok(()) => Ok(json!({"ok": true})),
                Err(LibError::NotFound) => Err(ApiError(404, "Not found".into())),
                Err(LibError::Invalid(m)) => Err(ApiError(400, m)),
            }
        }),
        ("POST", _) if catalog_download_id(p).is_some() => r.json(MAX_JSON).and_then(|body| {
            let mtp = body
                .get("mtp")
                .map(crate::settings::py_truthy)
                .unwrap_or(true);
            st.download_catalog(catalog_download_id(p).unwrap(), mtp)
        }),
        ("GET", "/api/engines") => {
            let info = st.sysinfo();
            let (os, arch) = crate::engines::os_arch();
            Ok(
                json!({"installed": st.engines.installed(), "variants": crate::engines::variants(&info, os, &arch)}),
            )
        }
        ("POST", "/api/engines/install") => r.json(MAX_JSON).and_then(|body| {
            let variant = body
                .get("variant")
                .filter(|v| crate::settings::py_truthy(v))
                .map(crate::settings::py_str)
                .unwrap_or_default();
            let info = st.sysinfo();
            let (os, arch) = crate::engines::os_arch();
            if !crate::engines::variants(&info, os, &arch)
                .iter()
                .any(|v| v["id"] == variant.as_str())
            {
                return Err(ApiError(
                    400,
                    "That engine does not fit this machine".into(),
                ));
            }
            st.engines.install(&variant, &info).map_err(|e| match e {
                EngineError::Runtime(m) => ApiError(409, m),
                EngineError::Network(m) => ApiError(502, format!("Could not reach GitHub: {m}")),
                EngineError::NotFound => ApiError(404, "Not found".into()),
            })
        }),
        ("POST", "/api/engines/check") => r.json(MAX_JSON).map(|body| {
            let path = body
                .get("path")
                .filter(|v| crate::settings::py_truthy(v))
                .map(crate::settings::py_str)
                .unwrap_or_default();
            st.engines.detect(&path)
        }),
        ("POST", _) if engine_remove_id(p).is_some() => {
            match st.engines.remove(engine_remove_id(p).unwrap()) {
                Ok(()) => Ok(json!({"ok": true})),
                Err(_) => Err(ApiError(404, "Not found".into())),
            }
        }
        ("GET", "/api/profiles") => Ok(
            json!({"profiles": st.profiles.lock().unwrap().all().iter().map(|p| p.public()).collect::<Vec<_>>()}),
        ),
        ("POST", "/api/profiles") => r.json(MAX_JSON).and_then(|body| {
            st.profiles
                .lock()
                .unwrap()
                .save(&body)
                .map(|p| p.public())
                .map_err(|e| ApiError(400, e.0))
        }),
        ("POST", _) if profile_delete_id(p).is_some() => {
            let id = profile_delete_id(p).unwrap();
            st.profiles.lock().unwrap().delete(id);
            let mut cfg = st.config.lock().unwrap();
            if cfg.get("launch.profile").and_then(Value::as_str) == Some(id) {
                let patch = json!({"launch": {"profile": "auto"}});
                let _ = cfg.update(patch.as_object().unwrap());
            }
            Ok(json!({"ok": true}))
        }
        ("GET", "/api/live") => {
            let since = r
                .query
                .get("since")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty());
            match since.map(|v| v.parse::<f64>()) {
                None => Ok(st.live.snapshot(0.0)),
                Some(Ok(f)) => Ok(st.live.snapshot(f)),
                Some(Err(_)) => Err(ApiError(
                    400,
                    format!(
                        "could not convert string to float: {}",
                        crate::settings::py_repr(&json!(since.unwrap_or("")))
                    ),
                )),
            }
        }
        ("GET", "/api/engine") => Ok(st.engine.status()),
        ("GET", "/api/engine/log") => Ok(st.engine.log_since(
            r.query
                .get("since")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0),
            2000,
        )),
        ("POST", "/api/engine/plan") => r.json(MAX_JSON).map(|body| {
            let model = body
                .get("model")
                .filter(|v| crate::settings::py_truthy(v))
                .map(crate::settings::py_str)
                .unwrap_or_default();
            st.plan_view(&model, body.get("profile"))
        }),
        (_, _) if p.starts_with("/api/p/") => Err(plugin_off(p)),
        ("GET", "/api/plugins") => Ok(json!({"api_version": 1, "plugins": builtin_plugins(), "error": ""})),
        ("POST", _) if plugin_toggle(p).is_some() => {
            let id = plugin_toggle(p).unwrap();
            if BUILTIN_PLUGINS.iter().any(|(i, _, _)| *i == id) {
                Err(ApiError(400, format!("{id} is part of FLE Link and always on")))
            } else {
                Err(ApiError(404, format!("No plugin named {id} is installed")))
            }
        }
        ("GET", "/api/remote") => Ok(st.remote_view()),
        ("POST", "/api/remote/code") => Ok(st.clients.new_code()),
        ("POST", "/api/remote/pair") => r.json(MAX_JSON).and_then(|body| {
            let g = |k: &str| body.get(k).map(py_str).unwrap_or_default();
            let fp = st.tls_fingerprint.lock().unwrap().clone();
            match st.clients.pair(&g("proof"), &g("name"), &r.peer, &fp) {
                Ok(mut v) => {
                    v["host"] = json!(st.host_name());
                    v["fingerprint"] = json!(fp);
                    Ok(v)
                }
                Err(e) => Err(remote_err(e)),
            }
        }),
        ("GET", "/api/remote/whoami") => Ok(json!({"host": st.host_name(),
            "name": r.client["name"].as_str().unwrap_or(""),
            "admin": if r.client.is_null() { true } else { r.client["admin"] == true },
            "paired": !r.client.is_null()})),
        ("POST", _) if hex_id(p, "/api/remote/clients/", "/revoke").is_some() => {
            if st.clients.revoke(hex_id(p, "/api/remote/clients/", "/revoke").unwrap()) {
                Ok(json!({"ok": true, "clients": st.clients.list()}))
            } else {
                Err(ApiError(404, "No such client".into()))
            }
        }
        ("POST", _) if hex_id(p, "/api/remote/clients/", "/admin").is_some() => r.json(MAX_JSON).and_then(|body| {
            let admin = body.get("admin").is_some_and(py_truthy);
            if st.clients.set_admin(hex_id(p, "/api/remote/clients/", "/admin").unwrap(), admin) {
                Ok(json!({"ok": true, "clients": st.clients.list()}))
            } else {
                Err(ApiError(404, "No such client".into()))
            }
        }),
        ("GET", "/api/hosts") => Ok(json!({"hosts": st.hosts.all().iter().map(|h| st.hosts.public(h)).collect::<Vec<_>>(),
            "ssh": crate::remote::ssh_available(), "client_name": st.host_name()})),
        ("POST", "/api/hosts") => r
            .json(MAX_JSON)
            .and_then(|body| st.hosts.add(&Value::Object(body), &st.host_name()).map_err(remote_err)),
        ("POST", _) if host_action(p).is_some() => r.json(MAX_JSON).and_then(|body| {
            let (id, action) = host_action(p).unwrap();
            let body = Value::Object(body);
            let h = st.hosts.get(id).map_err(remote_err)?;
            match action {
                "check" => {
                    let mut v = st.hosts.probe(&h);
                    v["host"] = st.hosts.public(&h);
                    Ok(v)
                }
                "connect" => {
                    st.hosts.reconnect(&h);
                    let mut v = st.hosts.probe(&h);
                    v["host"] = st.hosts.public(&h);
                    Ok(v)
                }
                "repair" => {
                    let mut rec = h.clone();
                    rec["expect"] = json!(body["fingerprint"].as_str().unwrap_or(""));
                    let paired = st
                        .hosts
                        .pair(&rec, body["code"].as_str().unwrap_or(""), &st.host_name())
                        .map_err(remote_err)?;
                    st.hosts.save(&paired);
                    Ok(json!({"host": st.hosts.public(&paired), "probe": st.hosts.probe(&paired)}))
                }
                "update" => {
                    let mut h = h;
                    if let Some(n) = body.get("name") {
                        let old = h["name"].as_str().unwrap_or("").to_string();
                        h["name"] = json!(crate::remote::clean_name(&py_str(n), &old));
                    }
                    if let Some(m) = body.get("mirror") {
                        h["mirror"] = json!(py_truthy(m));
                    }
                    if let Some(c) = body["color"].as_str().filter(|c| {
                        c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
                    }) {
                        h["color"] = json!(c);
                    }
                    st.hosts.save(&h);
                    Ok(json!({"host": st.hosts.public(&h)}))
                }
                _ => {
                    st.hosts.remove(id);
                    Ok(json!({"ok": true, "hosts": st.hosts.all().iter().map(|h| st.hosts.public(h)).collect::<Vec<_>>()}))
                }
            }
        }),
        ("GET", "/api/update") => Ok(st.updates.view(&Value::Object(st.config.lock().unwrap().data.clone()))),
        ("POST", "/api/update/check") => {
            let config = Value::Object(st.config.lock().unwrap().data.clone());
            st.updates
                .check(&config, &st.engines.installed(), true)
                .map(|_| st.updates.view(&config))
                .map_err(|e| ApiError(409, e))
        }
        ("POST", "/api/update/dismiss") => r.json(MAX_JSON).and_then(|body| {
            let version = body.get("version").filter(|v| py_truthy(v)).map(py_str).unwrap_or_default();
            let patch = st.updates.dismiss_patch(&version, body.get("engines") == Some(&json!(true)));
            if !patch.is_empty() {
                st.config
                    .lock()
                    .unwrap()
                    .update(json!({"updates": patch}).as_object().unwrap())
                    .map_err(|e| ApiError(400, e))?;
            }
            Ok(st.updates.view(&Value::Object(st.config.lock().unwrap().data.clone())))
        }),
        ("POST", "/api/update/apply") => r.json(MAX_JSON).and_then(|body| {
            let tag = body.get("tag").map(py_str).unwrap_or_default();
            let config = Value::Object(st.config.lock().unwrap().data.clone());
            let base = st.updates.prepare(&config, &tag).map_err(|e| ApiError(409, e))?;
            let st2 = st.clone();
            std::thread::spawn(move || {
                if st2.updates.install(&base, &tag).is_ok() {
                    // the page sees "restarting" first, then the engine and the MCP servers stop and the new version starts
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    st2.engine.shutdown();
                    st2.toolbox.mcp.close_all();
                    crate::updates::restart();
                }
            });
            Ok(st.updates.view(&config))
        }),
        ("POST", "/api/update/ack") => {
            st.updates.ack();
            Ok(st.updates.view(&Value::Object(st.config.lock().unwrap().data.clone())))
        }
        ("POST", "/api/engine/preview") => r.json(MAX_JSON).and_then(|body| {
            let model = body
                .get("model")
                .filter(|v| crate::settings::py_truthy(v))
                .map(crate::settings::py_str)
                .unwrap_or_default();
            st.make_plan(&model, body.get("profile"))
                .map(|p| p.public())
        }),
        ("POST", "/api/engine/start") => r.json(MAX_JSON).and_then(|body| {
            let last = st
                .config
                .lock()
                .unwrap()
                .get("last_model")
                .map(crate::settings::py_str)
                .unwrap_or_default();
            let model = body
                .get("model")
                .filter(|v| crate::settings::py_truthy(v))
                .map(crate::settings::py_str)
                .unwrap_or(last);
            let cfg_profile = st.config.lock().unwrap().get("launch.profile").cloned();
            let profile = body
                .get("profile")
                .filter(|v| crate::settings::py_truthy(v))
                .cloned()
                .or(cfg_profile);
            let plan = st.make_plan(&model, profile.as_ref())?;
            let roc = st
                .config
                .lock()
                .unwrap()
                .get("launch.restart_on_crash")
                .map(crate::settings::py_truthy)
                .unwrap_or(false);
            st.engine.set_restart_on_crash(roc);
            st.engine.start(plan).map_err(engine_err)?;
            let shown = profile
                .filter(crate::settings::py_truthy)
                .unwrap_or(json!("auto"));
            let patch = json!({"last_model": model, "launch": {"profile": shown}});
            st.config
                .lock()
                .unwrap()
                .update(patch.as_object().unwrap())
                .map_err(|e| ApiError(400, e))?;
            Ok(st.engine.status())
        }),
        ("POST", "/api/engine/stop") => {
            if !st.engine.bound().is_empty() {
                let _ = st
                    .config
                    .lock()
                    .unwrap()
                    .update(json!({"attach_auto": false}).as_object().unwrap());
            }
            st.engine.stop();
            Ok(st.engine.status())
        }
        ("POST", "/api/engine/attach") => r.json(MAX_JSON).and_then(|body| {
            let get = |k: &str| {
                body.get(k)
                    .filter(|v| crate::settings::py_truthy(v))
                    .map(crate::settings::py_str)
                    .unwrap_or_default()
            };
            let (url, unit) = (get("url"), get("service").trim().to_string());
            let mut patch = json!({"attach_url": url, "attach_service": unit});
            if let Some(k) = body.get("key").filter(|k| !k.is_null()) {
                patch["attach_key"] = json!(crate::settings::py_str(k));
            }
            st.config
                .lock()
                .unwrap()
                .update(patch.as_object().unwrap())
                .map_err(|e| ApiError(400, e))?;
            let key = st
                .config
                .lock()
                .unwrap()
                .get("attach_key")
                .map(crate::settings::py_str)
                .unwrap_or_default();
            let status = st.engine.attach(&url, &key, &unit).map_err(engine_err)?;
            let _ = st
                .config
                .lock()
                .unwrap()
                .update(json!({"attach_auto": true}).as_object().unwrap());
            Ok(status)
        }),
        ("POST", _) if engine_service_action(p).is_some() => Err(ApiError(
            400,
            "FLE Studio is not connected to a service.".into(),
        )),
        ("GET", "/api/catalog") => {
            Ok(json!({"catalog": st.catalog_view(), "extras": st.extras_view()}))
        }
        ("POST", _) if download_action(p).is_some() => {
            let (id, what) = download_action(p).unwrap();
            st.downloads
                .action(id, what)
                .ok_or(ApiError(404, "Not found".into()))
        }
        ("GET", "/api/bench") => Ok(
            json!({"current": st.bench.status(), "results": st.bench.results(),
            "tests": crate::bench::Bench::test_names()}),
        ),
        ("POST", "/api/bench/start") => r
            .json(MAX_JSON)
            .and_then(|body| st.bench_start(&Value::Object(body))),
        ("POST", "/api/bench/cancel") => {
            st.bench.cancel();
            Ok(json!({"ok": true}))
        }
        ("POST", _) if bench_delete_id(p).is_some() => {
            st.bench.delete(bench_delete_id(p).unwrap());
            Ok(json!({"ok": true}))
        }
        ("GET", "/api/quality") => {
            let builtin = crate::quality::load_questions(crate::quality::BUILTIN, &st.opts.home, "", 0)
                .map(|(m, _)| m)
                .unwrap_or(Value::Null);
            Ok(json!({"current": st.quality.status(), "results": st.quality.results(),
                "kld_by_model": st.quality.latest_kld_by_model(), "builtin": builtin,
                "mkqa_license": crate::quality::MKQA_LICENSE, "kld_ctx": crate::quality::KLD_CTX}))
        }
        ("POST", "/api/quality/kld") => r.json(MAX_JSON).and_then(|body| st.quality_kld(&Value::Object(body))),
        ("POST", "/api/quality/probe") => r.json(MAX_JSON).and_then(|body| st.quality_probe(&Value::Object(body))),
        ("POST", "/api/quality/mkqa") => r.json(MAX_JSON).and_then(|body| {
            let body = Value::Object(body);
            let lang = if py_truthy(&body["lang"]) { py_str(&body["lang"]) } else { "en".into() };
            crate::quality::import_mkqa(
                &st.opts.home,
                &lang,
                Studio::clamp_int(&body["count"], 48, 1, 500) as usize,
                Studio::clamp_int(&body["offset"], 0, 0, 100_000) as usize,
            )
            .map_err(|e| ApiError(400, e))
        }),
        ("POST", "/api/quality/cancel") => {
            st.quality.cancel();
            Ok(json!({"ok": true}))
        }
        ("POST", _) if hex_id(p, "/api/quality/", "/delete").is_some() => {
            st.quality.delete(hex_id(p, "/api/quality/", "/delete").unwrap());
            Ok(json!({"ok": true}))
        }
        ("GET", "/api/tuning") => {
            Ok(st.tuning_state(r.query.get("model").and_then(|v| v.as_str()).unwrap_or("")))
        }
        ("GET", "/api/tuning/best") => Ok(json!({"profile": crate::tuner::Tuner::tuned_profile(
            st,
            r.query.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        )})),
        ("POST", "/api/tuning/start") => r
            .json(MAX_JSON)
            .and_then(|body| st.tuning_start(&Value::Object(body))),
        ("POST", "/api/tuning/aba") => r
            .json(MAX_JSON)
            .and_then(|body| st.tuning_aba(&Value::Object(body))),
        ("POST", "/api/tuning/cancel") => {
            st.tuner.cancel();
            Ok(json!({"ok": true}))
        }
        ("POST", _) if hex_id(p, "/api/tuning/", "/delete").is_some() => {
            st.tuner
                .delete(hex_id(p, "/api/tuning/", "/delete").unwrap());
            Ok(json!({"ok": true}))
        }
        ("GET", "/api/cache") => {
            let layer = r
                .query
                .get("layer")
                .and_then(|v| v.as_str())
                .filter(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|l| l.parse().ok());
            Ok(st.cache.snapshot(layer))
        }
        ("POST", "/api/apps/prepare") => r.json(MAX_JSON).and_then(|body| {
            let name = body.get("name").map(py_str).unwrap_or_default();
            st.toolbox
                .prepare_app(&st.tool_settings(), &name)
                .map_err(|(c, m)| ApiError(c, m))
        }),
        ("POST", "/api/apps/call") => r.json(MAX_JSON).and_then(|body| {
            let frame = body.get("frame").map(py_str).unwrap_or_default();
            let rec = st.toolbox.frames.get(&frame).ok_or_else(|| {
                ApiError(404, "This app is no longer available. Run the tool again.".into())
            })?;
            let name = body.get("name").map(py_str).unwrap_or_default();
            let args = body.get("arguments").cloned().unwrap_or(json!({}));
            let approved = body.get("approved") == Some(&json!(true));
            match st.toolbox.app_call(
                &st.tool_settings(),
                rec["server"].as_str().unwrap_or(""),
                &name,
                &args,
                approved,
            ) {
                Ok(result) => Ok(json!({"result": result})),
                Err(crate::mcp::CallRefused::NeedsApproval) => {
                    Err(ApiError(409, "This tool needs your approval first.".into()))
                }
                Err(crate::mcp::CallRefused::Unknown) => Err(ApiError(
                    404,
                    "That tool is not available to this app (the server it came from may have gone).".into(),
                )),
            }
        }),
        ("POST", "/api/mcp/signin") => r.json(MAX_JSON).and_then(|body| {
            let name = body.get("server").map(py_str).unwrap_or_default();
            let port = st.port.load(std::sync::atomic::Ordering::SeqCst);
            let redirect = format!("http://127.0.0.1:{port}/oauth/callback");
            match st.toolbox.begin_sign_in(&name, &redirect) {
                None => Err(ApiError(404, "Not found".into())),
                Some(Err(e)) => Err(ApiError(400, e)),
                Some(Ok(v)) => Ok(v),
            }
        }),
        ("POST", "/api/mcp/signout") => r.json(MAX_JSON).and_then(|body| {
            let name = body.get("server").map(py_str).unwrap_or_default();
            match st.toolbox.sign_out(&name) {
                None => Err(ApiError(404, "Not found".into())),
                Some(had) => Ok(json!({"signed_out": true, "had_token": had})),
            }
        }),
        (m, _) if p == "/api/forge" || p.starts_with("/api/forge/") => {
            let body = if m == "POST" {
                match r.json(MAX_JSON) {
                    Ok(b) => Some(Value::Object(b)),
                    Err(e) => return Some(Err(e)),
                }
            } else {
                None
            };
            let q: Map<String, Value> = r.query.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            return st.forge_route(m, p, body, &q);
        }
        ("GET", "/api/flags") => Ok(crate::flaghelp::table()),
        ("POST", "/api/flags/explain") => r.json(MAX_JSON).map(|body| {
            let args: Vec<String> = body
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(py_str).collect())
                .unwrap_or_default();
            let env: Vec<(String, String)> = body
                .get("env")
                .and_then(Value::as_object)
                .map(|o| o.iter().map(|(k, v)| (k.clone(), py_str(v))).collect())
                .unwrap_or_default();
            json!({"items": crate::flaghelp::explain(&args, &env)})
        }),
        ("GET", "/api/recipes") => Ok(st.recipes()),
        ("POST", _) if recipe_download_id(p).is_some() => {
            st.recipe_download(recipe_download_id(p).unwrap())
        }
        ("POST", _) if recipe_start_id(p).is_some() => st.recipe_start(recipe_start_id(p).unwrap()),
        ("GET", "/api/hf/search") => {
            st.hf_search(r.query.get("q").and_then(|v| v.as_str()).unwrap_or(""))
        }
        ("GET", "/api/hf/repo") => {
            st.hf_repo(r.query.get("repo").and_then(|v| v.as_str()).unwrap_or(""))
        }
        ("POST", "/api/hf/download") => r.json(MAX_JSON).and_then(|body| st.hf_download(&body)),
        ("GET", "/api/downloads") => Ok(json!({"downloads": st.downloads.list()})),
        ("GET", "/api/chats") => Ok(json!({"chats": st.chats.list()})),
        ("POST", "/api/chats") => r.json(MAX_CHAT).and_then(|body| {
            let _g = st.chat_lock.lock().unwrap();
            st.chats.save(None, &body).map_err(chat_err)
        }),
        ("GET", _) if chat_id(p).is_some() => st.chats.get(chat_id(p).unwrap()).map_err(chat_err),
        ("PUT", _) if chat_id(p).is_some() => r.json(MAX_CHAT).and_then(|body| {
            let _g = st.chat_lock.lock().unwrap();
            st.chats.save(chat_id(p), &body).map_err(chat_err)
        }),
        ("DELETE", _) if chat_id(p).is_some() => st
            .chats
            .delete(chat_id(p).unwrap())
            .map(|_| json!({"ok": true}))
            .map_err(chat_err),
        _ => return None,
    })
}

/// The fit of a recipe for the Studio: what it was measured with (`[requires]`) and what is still to download decide when it
/// says so (flelink_core::recipe::Recipe::fit), the Studio's size rule otherwise.
fn recipe_fit(r: &flelink_core::recipe::Recipe, models: &std::path::Path, sys: &Value) -> Value {
    if r.requires.ram_gib.is_some() || r.requires.vram_gib.is_some() {
        use flelink_core::catalog::Fit;
        let verdict = match r.fit(models, &flelink_core::system::detect(models)) {
            Fit::Fits => "good",
            Fit::Tight => "tight",
            Fit::TooBig => "too_big",
        };
        return json!({"verdict": verdict, "kind": "recipe", "detail": {"ram_gib": r.requires.ram_gib,
            "vram_gib": r.requires.vram_gib, "missing": r.missing_bytes(models)}, "advice": ""});
    }
    if r.model.size > 0 {
        crate::catalog::fit(r.model.size as i64, sys)
    } else {
        Value::Null
    }
}

const STATIC_TYPES: [(&str, &str); 12] = [
    (".html", "text/html"),
    (".js", "text/javascript"),
    (".mjs", "text/javascript"),
    (".css", "text/css"),
    (".svg", "image/svg+xml"),
    (".json", "application/json"),
    (".png", "image/png"),
    (".ico", "image/x-icon"),
    (".woff2", "font/woff2"),
    (".woff", "font/woff"),
    (".txt", "text/plain"),
    (".map", "application/json"),
];

/// The built web page: a folder when one is given (tests, development), else the copy inside the binary.
/// The plugins that are part of FLE Link (their pages live in web/plugins/<id>/): (id, title, summary).
const BUILTIN_PLUGINS: [(&str, &str, &str); 3] = [
    (
        "forge",
        "Forge",
        "Build your own FLE quant: hybrid builds, expert pruning, KL checks.",
    ),
    (
        "remote",
        "Remote access",
        "One FLE Link computes for another: pairing with a code, rights per client, hosts by address or SSH.",
    ),
    (
        "updater",
        "Updates",
        "Tells you when a new FLE Link or FLE engine is out, and updates in one click.",
    ),
];

static PLUGIN_WEB: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../web/plugins");

/// `/api/plugins`: the built-in plugins, in the form the page knows from FLE Studio's plugin list.
fn builtin_plugins() -> Vec<Value> {
    BUILTIN_PLUGINS
        .iter()
        .map(|(id, title, summary)| {
            // the Forge is a page of its own and has nothing to start with the page; the others are boot scripts
            let (page, boot) = if *id == "forge" {
                (json!({"title": "Forge", "icon": "forge", "script": "forge.js", "path": "/forge", "nav": true}), Value::Null)
            } else {
                (Value::Null, json!("boot.js"))
            };
            json!({"id": id, "package": "flelink", "version": env!("CARGO_PKG_VERSION"), "bundled": true, "enabled": true,
                "loaded": true, "error": "", "page": page, "boot": boot, "boot_early": false, "title": title,
                "summary": summary})
        })
        .collect()
}

static WEB: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../web/studio");

fn read_web(st: &Studio, name: &str) -> Option<Vec<u8>> {
    match &st.opts.web_dir {
        Some(dir) => std::fs::read(dir.join(name)).ok(),
        None => WEB.get_file(name).map(|f| f.contents().to_vec()),
    }
}

fn static_file(st: &Studio, path: &str, head: bool) -> Response<Body> {
    let mut name = if path == "/" || path == "/index.html" {
        "index.html".to_string()
    } else {
        path.trim_start_matches('/').to_string()
    };
    let ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
        && !name.contains("..");
    let not_found = || error_json(404, "Not found", head);
    if !ok {
        return not_found();
    }
    if let Some(rest) = name.strip_prefix("plugins/") {
        // the files of a plugin that is part of FLE Link
        if let Some(f) = PLUGIN_WEB.get_file(rest) {
            let ctype = match rest.rsplit('.').next() {
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("svg") => "image/svg+xml",
                _ => "application/octet-stream",
            };
            return send(
                200,
                f.contents().to_vec(),
                ctype,
                &[("Cache-Control", "no-cache")],
                head,
            );
        }
        let id = rest.split('/').next().unwrap_or("");
        let valid = !id.is_empty()
            && id.len() <= 32
            && id.as_bytes()[0].is_ascii_lowercase()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && rest.contains('/');
        return error_json(
            404,
            &if valid {
                format!("The plugin {id} is not on")
            } else {
                "Not found".to_string()
            },
            head,
        );
    }
    let mut data: Option<Vec<u8>> = read_web(st, &name);
    if data.is_none() {
        if !name.rsplit('/').next().unwrap_or("").contains('.') {
            name = "index.html".into(); // client-side routes fall back to the app
            data = read_web(st, &name);
        } else {
            return not_found();
        }
    }
    let Some(mut data) = data else {
        return not_found();
    };
    if name == "index.html" {
        let t = String::from_utf8_lossy(&data)
            .replace("__FLE_TOKEN__", &st.token)
            .replace("__FLELINK_VERSION__", env!("CARGO_PKG_VERSION"));
        data = t.into_bytes();
    }
    let suffix = name
        .rfind('.')
        .map(|i| name[i..].to_lowercase())
        .unwrap_or_default();
    let mut ctype = STATIC_TYPES
        .iter()
        .find(|(s, _)| *s == suffix)
        .map(|(_, c)| c.to_string())
        .unwrap_or_else(|| "application/octet-stream".into());
    if ctype.starts_with("text/") || ctype == "application/json" || ctype == "image/svg+xml" {
        ctype += "; charset=utf-8";
    }
    let csp = "default-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'; font-src 'self'; frame-ancestors 'none'; base-uri 'none'";
    send(
        200,
        data,
        &ctype,
        &[
            ("Content-Security-Policy", csp),
            ("X-Frame-Options", "DENY"),
            ("X-Content-Type-Options", "nosniff"),
            ("Cache-Control", "no-cache"),
        ],
        head,
    )
}

async fn handler(
    State(st): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request<Body>,
) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let head = parts.method == Method::HEAD;
    let path = parts.uri.path().to_string();
    if let Some(r) = guard(&st, &parts.method, &path, &parts.headers, &peer, head) {
        return r;
    }
    if path == "/login" {
        return login(&st, &parts.method, body, head).await;
    }
    if let Some(frame) = path.strip_prefix("/app-frame/") {
        if matches!(parts.method, Method::GET | Method::HEAD) {
            return app_frame(&st, frame, head);
        }
    }
    if path == "/oauth/callback" && matches!(parts.method, Method::GET | Method::HEAD) {
        let q = query_of(parts.uri.query());
        let st2 = st.clone();
        return tokio::task::spawn_blocking(move || oauth_callback(&st2, &q, head))
            .await
            .unwrap_or_else(|_| error_json(500, "The sign-in failed", head));
    }
    if path.starts_with("/v1/") {
        return proxy(&st, parts, body, head).await;
    }
    if path.starts_with("/h/") {
        return forward(&st, parts, body, head).await;
    }
    let declared: usize = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let (body, too_large) = if declared > MAX_CHAT {
        (vec![], true)
    } else {
        match to_bytes(body, MAX_CHAT).await {
            Ok(b) => (b.to_vec(), false),
            Err(_) => (vec![], true),
        }
    };
    let client = bearer(&parts.headers)
        .filter(|t| t.starts_with(crate::remote::TOKEN_PREFIX))
        .and_then(|t| st.clients.find(&t, &peer.ip().to_string()))
        .unwrap_or(Value::Null);
    let r = Req {
        peer: peer.ip().to_string(),
        client,
        method: parts.method.clone(),
        path: path.clone(),
        query: query_of(parts.uri.query()),
        body,
        too_large,
    };
    let st2 = st.clone();
    let routed = tokio::task::spawn_blocking(move || route(&st2, &r))
        .await
        .unwrap_or(None);
    match routed {
        Some(Ok(v)) => send_json(200, &v, head),
        Some(Err(ApiError(s, m))) => match m.split_once('\u{1f}') {
            // an error with a code the page acts on (remote_err)
            Some((msg, code)) => {
                send_json(s, &json!({"error": {"message": msg, "code": code}}), head)
            }
            None => error_json(s, &m, head),
        },
        None if matches!(parts.method, Method::GET | Method::HEAD)
            && !path.starts_with("/api/") =>
        {
            static_file(&st, &path, head)
        }
        None => match not_yet(&path, &parts.method) {
            Some(what) => error_json(
                501,
                &format!("{what} is not in this version of FLE yet."),
                head,
            ),
            None => error_json(404, "Not found", head),
        },
    }
}

/// `/app-frame/<id>`: an MCP App's document, under the policy its resource asked for.
fn app_frame(st: &Ctx, frame: &str, head: bool) -> Response<Body> {
    let ok = (20..=64).contains(&frame.len())
        && frame
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let Some(rec) = st.toolbox.frames.get(frame).filter(|_| ok) else {
        return error_json(
            404,
            "This app is no longer available. Run the tool again.",
            head,
        );
    };
    send(
        200,
        rec["html"].as_str().unwrap_or("").as_bytes().to_vec(),
        "text/html; charset=utf-8",
        &[
            (
                "Content-Security-Policy",
                rec["csp"].as_str().unwrap_or("default-src 'none'"),
            ),
            (
                "Permissions-Policy",
                "camera=(), microphone=(), geolocation=(), payment=(), usb=(), clipboard-write=()",
            ),
            ("Cross-Origin-Resource-Policy", "same-origin"),
        ],
        head,
    )
}

/// `/h/<host id>/api/…` and `/h/<host id>/v1/…` to a paired host, with the client token. Small answers are read whole (and
/// mirrored); streams, downloads and other large answers are passed on as they arrive.
async fn forward(
    st: &Ctx,
    parts: axum::http::request::Parts,
    body: Body,
    head: bool,
) -> Response<Body> {
    let path = parts.uri.path().to_string();
    let Some(rest) = path.strip_prefix("/h/") else {
        return error_json(404, "Not found", head);
    };
    let (id, rest) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let rest = rest.to_string();
    let ok_id = id.len() == 12
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !ok_id {
        return error_json(404, "Not found", head);
    }
    let h = match st.hosts.get(id) {
        Ok(h) => h,
        Err(e) => return error_json(e.0, &e.1, head),
    };
    let app_page = rest.starts_with("/app-frame/") && parts.method == Method::GET;
    if !(rest.starts_with("/api/") || rest.starts_with("/v1/") || app_page)
        || crate::remote::never_forwarded(&rest)
    {
        return error_json(403, "This path is not passed on to a host.", head);
    }
    if h["token"].as_str().is_none_or(|t| t.is_empty()) {
        return send_json(
            424,
            &json!({"error": {"message": "This host is not paired.", "code": "unpaired"}}),
            head,
        );
    }
    let bytes = match to_bytes(body, MAX_CHAT).await {
        Ok(b) => b.to_vec(),
        Err(_) => return error_json(413, "Request too large", head),
    };
    let target = match parts.uri.query() {
        Some(q) => format!("{rest}?{q}"),
        None => rest.clone(),
    };
    let method = parts.method.as_str().to_string();
    let pass: Vec<(String, String)> = ["content-type", "accept", "x-fle-confirm"]
        .iter()
        .filter_map(|n| {
            parts
                .headers
                .get(*n)
                .and_then(|v| v.to_str().ok())
                .map(|v| (n.to_string(), v.to_string()))
        })
        .collect();
    let stream_like = rest.starts_with("/v1/");
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(16);
    let (meta_tx, meta_rx) = tokio::sync::oneshot::channel::<
        Result<(u16, Vec<(String, String)>, Option<Vec<u8>>), Value>,
    >();
    let st2 = st.clone();
    let rest2 = rest.clone();
    std::thread::spawn(move || {
        let timeout = Duration::from_secs(if stream_like { 900 } else { 60 });
        let pinned = h["fingerprint"].as_str().unwrap_or("").to_string();
        let mirror_dir = st2.hosts.mirror_dir(h["id"].as_str().unwrap_or(""));
        let fail = |e: String, seen: String| -> Value {
            let (code, message, extra) = crate::remote::describe_failure(&e, &seen, &pinned);
            if code == "unreachable" && h["mirror"] == true && method == "GET" {
                if let Some(v) = crate::remote::mirror_read(&mirror_dir, &rest2) {
                    return json!({"mirror": v});
                }
            }
            let mut err = json!({"message": message, "code": code});
            for (k, v) in extra.as_object().into_iter().flatten() {
                err[k] = v.clone();
            }
            json!({"error": err})
        };
        let (base, agent, seen) = match st2.hosts.connection(&h, timeout) {
            Ok(c) => c,
            Err(e) => {
                let _ = meta_tx.send(Err(fail(e, String::new())));
                return;
            }
        };
        let mut req = agent.request(&method, &format!("{base}{target}")).set(
            "Authorization",
            &format!("Bearer {}", h["token"].as_str().unwrap_or("")),
        );
        for (k, v) in &pass {
            req = req.set(k, v);
        }
        let result = if bytes.is_empty() && (method == "GET" || method == "HEAD") {
            req.call()
        } else {
            req.send_bytes(&bytes)
        };
        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => {
                let s = seen.lock().unwrap().clone();
                let _ = meta_tx.send(Err(fail(e.to_string(), s)));
                return;
            }
        };
        let status = resp.status();
        let mut headers: Vec<(String, String)> = vec![];
        for n in [
            "Content-Type",
            "Content-Disposition",
            "Content-Range",
            "Accept-Ranges",
            "Content-Security-Policy",
            "Permissions-Policy",
        ] {
            if let Some(v) = resp.header(n) {
                headers.push((n.to_string(), v.to_string()));
            }
        }
        let sse = resp
            .header("Content-Type")
            .is_some_and(|c| c.contains("text/event-stream"));
        let size: Option<usize> = resp.header("Content-Length").and_then(|v| v.parse().ok());
        let mut reader = resp.into_reader();
        if !sse && size.is_some_and(|n| n <= 8 * 1024 * 1024) {
            let mut data = vec![];
            let _ = std::io::Read::read_to_end(&mut reader, &mut data);
            if status == 200 && h["mirror"] == true {
                crate::remote::mirror_save(&mirror_dir, &rest2, &data);
            }
            let _ = meta_tx.send(Ok((status, headers, Some(data))));
            return;
        }
        let _ = meta_tx.send(Ok((status, headers, None)));
        let mut buf = vec![0u8; 65536];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx
                        .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                        .is_err()
                    {
                        break; // the page stopped reading; closing the host side ends the generation there too
                    }
                }
            }
        }
    });
    match meta_rx.await {
        Err(_) => error_json(502, "The host did not answer", head),
        Ok(Err(v)) if v.get("mirror").is_some() => send_json(200, &v["mirror"], head),
        Ok(Err(v)) => send_json(502, &v, head),
        Ok(Ok((401, _, _))) => send_json(
            424,
            &json!({"error": {"message": "The host does not accept this client any more. It may have been revoked: pair again.", "code": "unpaired"}}),
            head,
        ),
        Ok(Ok((status, headers, Some(data)))) => {
            let ctype = headers
                .iter()
                .find(|(k, _)| k == "Content-Type")
                .map_or("application/octet-stream".to_string(), |(_, v)| v.clone());
            let extra: Vec<(&str, &str)> = headers
                .iter()
                .filter(|(k, _)| k != "Content-Type")
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            send(status, data, &ctype, &extra, head)
        }
        Ok(Ok((status, headers, None))) => {
            let mut r = Response::new(Body::from_stream(
                tokio_stream::wrappers::ReceiverStream::new(rx),
            ));
            *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            let hm = r.headers_mut();
            for (k, v) in &headers {
                if let (Ok(n), Ok(v)) = (
                    axum::http::HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(v),
                ) {
                    hm.insert(n, v);
                }
            }
            hm.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            hm.insert("x-accel-buffering", HeaderValue::from_static("no"));
            hm.insert(header::CONNECTION, HeaderValue::from_static("close"));
            r
        }
    }
}

const CALLBACK_PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>FLE Link</title>
<style>body{font:16px system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 1rem;color:#222}
@media (prefers-color-scheme:dark){body{background:#16181c;color:#e6e6e6}}h1{font-size:1.2rem}</style>
<h1>__TITLE__</h1><p>__TEXT__</p>"#;

fn html_escape(t: &str) -> String {
    t.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// The browser comes back from a sign-in: exchange the code, connect the server again, say how it went.
fn oauth_callback(st: &Ctx, q: &Map<String, Value>, head: bool) -> Response<Body> {
    let g = |k: &str| q.get(k).and_then(Value::as_str).unwrap_or("");
    let (title, text, status) =
        match st
            .toolbox
            .finish_sign_in(g("state"), g("code"), g("error"), g("error_description"))
        {
            Ok(name) => (
                "Signed in".to_string(),
                format!("{name} is connected. You can close this tab and go back to FLE Link."),
                200,
            ),
            Err(e) => ("Sign-in did not work".to_string(), e, 400),
        };
    let page = CALLBACK_PAGE
        .replace("__TITLE__", &html_escape(&title))
        .replace("__TEXT__", &html_escape(&text));
    send(
        status,
        page.into_bytes(),
        "text/html; charset=utf-8",
        &[
            (
                "Content-Security-Policy",
                "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
            ),
            ("X-Frame-Options", "DENY"),
        ],
        head,
    )
}

const LOGIN_PAGE: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>FLE Studio · sign in</title>
<style>body{margin:0;min-height:100vh;display:grid;place-items:center;background:#0b0e14;color:#e9edf3;
font:15px/1.5 system-ui,sans-serif}form{background:#10141c;border:1px solid #1f2633;border-radius:14px;padding:28px;
width:min(360px,90vw);display:grid;gap:12px}input{background:#151a24;border:1px solid #1f2633;border-radius:8px;
color:inherit;padding:10px 12px;font:inherit}button{background:#7aa7ff;color:#0b0e14;border:0;border-radius:8px;
padding:10px;font-weight:600;cursor:pointer}p{margin:0;color:#94a0b2;font-size:13px}.err{color:#ff8a8a}</style></head>
<body><form method="post" action="/login"><strong>FLE Studio</strong><p>This FLE Studio is open to the local network.
Enter its access key (Settings → Network on the machine that runs it).</p>__ERR__
<input type="password" name="key" autocomplete="current-password" autofocus placeholder="Access key">
<button type="submit">Sign in</button></form></body></html>"#;

/// The sign-in page and the form that turns the access key into a cookie.
async fn login(st: &Ctx, method: &Method, body: Body, head: bool) -> Response<Body> {
    if *method != Method::POST {
        return send(
            200,
            LOGIN_PAGE.replace("__ERR__", "").into_bytes(),
            "text/html; charset=utf-8",
            &[],
            head,
        );
    }
    let raw = to_bytes(body, 4096).await.unwrap_or_default();
    let key = form_urlencoded::parse(&raw)
        .filter(|(k, _)| k == "key")
        .map(|(_, v)| v.to_string())
        .last()
        .unwrap_or_default();
    let real = access_key(st);
    if !real.is_empty() && ct_eq(&key, &real) {
        let cookie = format!(
            "fle_session={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000",
            session_cookie(st)
        );
        return send(
            303,
            vec![],
            "text/plain",
            &[("Location", "/"), ("Set-Cookie", &cookie)],
            head,
        );
    }
    tokio::time::sleep(Duration::from_secs(1)).await; // slow down guessing
    send(
        401,
        LOGIN_PAGE
            .replace("__ERR__", "<p class=\"err\">That key is not right.</p>")
            .into_bytes(),
        "text/html; charset=utf-8",
        &[],
        head,
    )
}

/// Features of the Python Studio that this server does not have yet: a clear 501 instead of a 404 that looks like a bug.
fn not_yet(path: &str, _method: &Method) -> Option<&'static str> {
    let is = |p: &str| path == p || path.starts_with(&format!("{p}/"));
    let _ = is;
    None
}

pub fn router(st: Ctx) -> Router {
    Router::new().fallback(handler).with_state(st)
}

/// Serve on an open socket until `shutdown` finishes.
pub async fn serve_listener(
    st: Ctx,
    l: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    if let Ok(a) = l.local_addr() {
        st.port.store(a.port(), std::sync::atomic::Ordering::SeqCst);
    }
    axum::serve(
        l,
        router(st).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// Serve until the process ends. Returns the bound address through `ready` once the socket is open.
pub async fn serve(
    st: Ctx,
    addr: SocketAddr,
    ready: Option<std::sync::mpsc::Sender<SocketAddr>>,
) -> std::io::Result<()> {
    let l = tokio::net::TcpListener::bind(addr).await?;
    if let Some(r) = ready {
        let _ = r.send(l.local_addr()?);
    }
    serve_listener(st, l, std::future::pending()).await
}

/// Refuse a chat request that asks for tools and for a forced answer format together (`screen_proxy_body`).
fn screen_proxy_body(path: &str, body: &[u8]) -> Result<(), String> {
    let p = path.split('?').next().unwrap_or("").trim_end_matches('/');
    if body.is_empty()
        || !p.ends_with("/chat/completions")
        || !body.windows(7).any(|w| w == b"\"tools\"")
    {
        return Ok(());
    }
    let Ok(Value::Object(doc)) = serde_json::from_slice::<Value>(body) else {
        return Ok(());
    }; // the engine will say what is wrong
    if !doc.get("tools").is_some_and(crate::settings::py_truthy)
        || doc.get("tool_choice") == Some(&json!("none"))
    {
        return Ok(());
    }
    let mut clash = vec![];
    for key in ["response_format", "json_schema", "grammar"] {
        let v = doc.get(key).cloned().unwrap_or(Value::Null);
        if key == "response_format" {
            if let Value::Object(o) = &v {
                if matches!(o.get("type"), None | Some(Value::Null))
                    || o.get("type") == Some(&json!("text"))
                {
                    continue;
                }
            }
        }
        if !(v.is_null() || v == json!("") || v == json!({}) || v == json!([])) {
            clash.push(key);
        }
    }
    if clash.is_empty() {
        return Ok(());
    }
    Err(format!("A chat request must not carry both tools and {}: the model would not call the tools and would answer with substitute JSON. Send them in separate requests.", clash.join(", ")))
}

enum Upstream {
    Failed(String),
    Answer {
        status: u16,
        ctype: String,
        stream: bool,
    },
}

/// Pass /v1/* to the engine; streams (SSE) are forwarded as they arrive.
async fn proxy(
    st: &Ctx,
    parts: axum::http::request::Parts,
    body: Body,
    head: bool,
) -> Response<Body> {
    let (state, base, key) = st.engine.target();
    if !(state == "ready" || state == "attached") || base.is_empty() {
        return send_json(
            503,
            &json!({"error": {"message": "No model is loaded. Load one in FLE Studio first.", "type": "unavailable_error", "code": 503}}),
            head,
        );
    }
    let declared: usize = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if declared > MAX_PROXY {
        return error_json(413, "Request too large", head);
    }
    let Ok(bytes) = to_bytes(body, MAX_PROXY).await else {
        return error_json(413, "Request too large", head);
    };
    let path_q = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());
    if let Err(m) = screen_proxy_body(&path_q, &bytes) {
        return error_json(400, &m, head);
    }
    let method = parts.method.to_string();
    let ctype = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let accept = parts
        .headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*")
        .to_string();
    let live = st.live.clone();
    live.touch();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(16);
    let (meta_tx, meta_rx) = tokio::sync::oneshot::channel::<Upstream>();
    std::thread::spawn(move || {
        let url = format!("{}{}", base.trim_end_matches('/'), path_q);
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(Duration::from_secs(900))
            .timeout_read(Duration::from_secs(900))
            .build();
        let mut req = agent
            .request(&method, &url)
            .set("Content-Type", &ctype)
            .set("Accept", &accept);
        if !key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let result = if bytes.is_empty() && method == "GET" {
            req.call()
        } else {
            req.send_bytes(&bytes)
        };
        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => {
                use std::error::Error;
                let mut cur: &dyn Error = &e;
                while let Some(s) = cur.source() {
                    cur = s;
                }
                let _ = meta_tx.send(Upstream::Failed(cur.to_string()));
                return;
            }
        };
        let rtype = resp
            .header("Content-Type")
            .unwrap_or("application/json")
            .to_string();
        let stream = rtype.contains("text/event-stream");
        let _ = meta_tx.send(Upstream::Answer {
            status: resp.status(),
            ctype: rtype,
            stream,
        });
        let mut reader = resp.into_reader();
        let mut all: Vec<u8> = vec![];
        let mut buf = vec![0u8; 65536];
        loop {
            let n = match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if stream {
                if tx
                    .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                    .is_err()
                {
                    break; // the browser stopped reading (Stop button); closing our side cancels the generation
                }
                all.extend_from_slice(&buf[..n]);
                if all.len() > 262144 {
                    all.drain(..all.len() - 262144);
                }
            } else {
                all.extend_from_slice(&buf[..n]);
            }
        }
        if stream {
            record_stream(&live, &all);
        } else {
            record_data(&live, &all);
            let _ = tx.blocking_send(Ok(bytes::Bytes::from(all)));
        }
    });
    let mut rx = rx;
    match meta_rx.await {
        Err(_) => error_json(502, "The engine did not answer", head),
        Ok(Upstream::Failed(why)) => send_json(
            502,
            &json!({"error": {"message": format!("The engine did not answer: {why}"), "type": "bad_gateway"}}),
            head,
        ),
        Ok(Upstream::Answer {
            status,
            ctype,
            stream: false,
        }) => {
            let mut data = vec![];
            while let Some(Ok(c)) = rx.recv().await {
                data.extend_from_slice(&c);
            }
            send(status, data, &ctype, &[], head)
        }
        Ok(Upstream::Answer {
            status,
            ctype,
            stream: true,
        }) => {
            let mut r = Response::new(Body::from_stream(
                tokio_stream::wrappers::ReceiverStream::new(rx),
            ));
            *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&ctype)
                    .unwrap_or(HeaderValue::from_static("text/event-stream")),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            h.insert("x-accel-buffering", HeaderValue::from_static("no"));
            h.insert(header::CONNECTION, HeaderValue::from_static("close"));
            r
        }
    }
}

fn record_data(live: &Live, data: &[u8]) {
    if let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(data) {
        if let Some(t) = o.get("timings").filter(|t| crate::settings::py_truthy(t)) {
            live.record_response(t, o.get("usage"));
        }
    }
}

fn record_stream(live: &Live, tail: &[u8]) {
    let text = String::from_utf8_lossy(tail).to_string();
    for line in text.lines().rev() {
        if line.starts_with("data:") && line.contains("\"timings\"") {
            let Ok(obj) = serde_json::from_str::<Value>(line[5..].trim()) else {
                continue;
            };
            live.record_response(obj.get("timings").unwrap_or(&json!({})), obj.get("usage"));
            return;
        }
    }
}
