//! Host and client mode: one FLE Link computes for another. Port of `fle_studio_remote/remote.py`.
//!
//! Host side (the machine with the GPU, `flelink studio --host 0.0.0.0`):
//! * TLS with a certificate of its own (tls.rs); its SHA-256 fingerprint is pinned by every client when it pairs.
//! * Pairing: the host shows a short-lived code (8 characters, 10 minutes, one use). A client proves it knows the code with
//!   an HMAC bound to the fingerprint it saw, so a man in the middle neither learns the code nor gets a token. The reward is
//!   a client token; the host keeps only its hash, lists the clients and can revoke each one.
//! * Rights per client: by default it may chat, look and measure; "admin" may also change settings, install engines and
//!   load or delete models. Installs and updates additionally ask for a confirmation on the client.
//!
//! Client side (the machine with the browser):
//! * Hosts are added by address and code, or by an SSH target, for which the system `ssh -N -L` runs, watched and started
//!   again when it dies (keys or the agent only, never a password).
//! * The page talks only to its own FLE Link; everything under `/h/<host id>/` is passed on to the host with the client
//!   token, streams included. Chats live on the host; a host can be mirrored for reading them offline.
use hmac::Mac;
use serde_json::{json, Map, Value};
use sha2::Digest;
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no 0/O, 1/I
const CODE_LENGTH: usize = 8;
const CODE_TTL: f64 = 600.0;
const FAILS_PER_ADDRESS: usize = 5;
const FAILS_TOTAL: usize = 20;
const FAIL_WINDOW: f64 = 600.0;
/// wrong tries (all addresses together) after which the code is thrown away
const CODE_BURNED_AFTER: i64 = 10;
pub const TOKEN_PREFIX: &str = "fcl_";
pub const DEFAULT_PORT: u16 = 8642;
const COLORS: [&str; 8] = [
    "#22d3ee", "#f59e0b", "#34d399", "#f472b6", "#a78bfa", "#fb7185", "#60a5fa", "#a3e635",
];

/// An answer for the page: status, message, and a code it can act on.
#[derive(Debug)]
pub struct RemoteError(pub u16, pub String, pub &'static str);

fn rerr<T>(status: u16, m: impl Into<String>, code: &'static str) -> Result<T, RemoteError> {
    Err(RemoteError(status, m.into(), code))
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn read(path: &Path, default: Value) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(default)
}

/// Files that hold tokens are readable by the user only.
fn write(path: &Path, data: &Value) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("tmp");
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    if let Ok(mut f) = o.open(&tmp) {
        use std::io::Write;
        let _ = f.write_all(
            serde_json::to_string_pretty(data)
                .unwrap_or_default()
                .as_bytes(),
        );
        drop(f);
        let _ = std::fs::rename(&tmp, path);
    }
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("random bytes");
    hex::encode(b)
}

pub fn normalize_code(code: &str) -> String {
    code.to_uppercase()
        .bytes()
        .filter(|b| CODE_ALPHABET.contains(b))
        .map(char::from)
        .collect()
}

/// 64 lower-case hex digits, whatever the spelling (`AA:BB:…` or plain).
pub fn normalize_fp(fp: &str) -> String {
    fp.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect()
}

/// `AA:BB:CC` style, how people compare fingerprints.
pub fn pretty_fp(fp: &str) -> String {
    let n = normalize_fp(fp);
    n.as_bytes()
        .chunks(2)
        .map(|c| String::from_utf8_lossy(c).to_uppercase())
        .collect::<Vec<_>>()
        .join(":")
}

/// What a client sends instead of the code: bound to the certificate it saw.
pub fn pair_proof(code: &str, fingerprint: &str) -> String {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(normalize_code(code).as_bytes())
        .expect("hmac key");
    mac.update(format!("fle-pair|{}", normalize_fp(fingerprint)).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn token_hash(token: &str) -> String {
    hex::encode(sha2::Sha256::digest(token.as_bytes()))
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

pub fn clean_name(text: &str, fallback: &str) -> String {
    let re = regex::Regex::new(r"[^\w .@()+'-]").unwrap();
    let out: String = re.replace_all(text, "").trim().chars().take(60).collect();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

// ------------------------------------------------------------------------------------------------ host: clients

/// The paired clients and the pairing code, in `clients.json` and `pairing.json` of the data folder.
pub struct Clients {
    file: PathBuf,
    code_file: PathBuf,
    lock: Mutex<()>,
    fails: Mutex<HashMap<String, Vec<f64>>>,
}

impl Clients {
    pub fn new(data_dir: &Path) -> Clients {
        Clients {
            file: data_dir.join("clients.json"),
            code_file: data_dir.join("pairing.json"),
            lock: Mutex::new(()),
            fails: Mutex::new(HashMap::new()),
        }
    }

    pub fn new_code(&self) -> Value {
        let mut b = [0u8; CODE_LENGTH];
        getrandom::getrandom(&mut b).expect("random bytes");
        // rejection sampling keeps every character equally likely (32 divides 256)
        let code: String = b
            .iter()
            .map(|x| char::from(CODE_ALPHABET[(*x as usize) % CODE_ALPHABET.len()]))
            .collect();
        let expires = now() + CODE_TTL;
        let _g = self.lock.lock().unwrap();
        write(
            &self.code_file,
            &json!({"code": code, "expires": expires, "fails": 0}),
        );
        json!({"code": code, "pretty": format!("{}-{}", &code[..4], &code[4..]), "expires": expires, "ttl": CODE_TTL as i64})
    }

    pub fn code_state(&self) -> Value {
        let d = read(&self.code_file, json!({}));
        let exp = d["expires"].as_f64().unwrap_or(0.0);
        if d["code"].as_str().is_some_and(|c| !c.is_empty()) && exp > now() {
            json!({"active": true, "expires": exp})
        } else {
            json!({"active": false, "expires": 0})
        }
    }

    fn fail(&self, address: &str) {
        let t = now();
        let mut f = self.fails.lock().unwrap();
        for key in [address, "*"] {
            let v = f.entry(key.to_string()).or_default();
            v.retain(|x| t - x < FAIL_WINDOW);
            v.push(t);
        }
    }

    fn limited(&self, address: &str) -> bool {
        let t = now();
        let f = self.fails.lock().unwrap();
        let n = |k: &str| {
            f.get(k)
                .map_or(0, |v| v.iter().filter(|x| t - **x < FAIL_WINDOW).count())
        };
        n(address) >= FAILS_PER_ADDRESS || n("*") >= FAILS_TOTAL
    }

    pub fn pair(
        &self,
        proof: &str,
        name: &str,
        address: &str,
        fingerprint: &str,
    ) -> Result<Value, RemoteError> {
        let _g = self.lock.lock().unwrap();
        if self.limited(address) {
            return rerr(
                429,
                "Too many wrong tries. Wait a few minutes.",
                "rate_limited",
            );
        }
        let mut data = read(&self.code_file, json!({}));
        let code = data["code"].as_str().unwrap_or("").to_string();
        if code.is_empty() || data["expires"].as_f64().unwrap_or(0.0) <= now() {
            self.fail(address);
            return rerr(
                400,
                "There is no pairing code, or it has expired. Create a new one on the host.",
                "no_code",
            );
        }
        if !ct_eq(proof, &pair_proof(&code, fingerprint)) {
            self.fail(address);
            let fails = data["fails"].as_i64().unwrap_or(0) + 1;
            data["fails"] = json!(fails);
            if fails >= CODE_BURNED_AFTER {
                let _ = std::fs::remove_file(&self.code_file); // somebody is guessing: the code is gone
            } else {
                write(&self.code_file, &data);
            }
            return rerr(
                400,
                "The code is not right (or the host's certificate changed on the way).",
                "bad_code",
            );
        }
        let _ = std::fs::remove_file(&self.code_file); // one use
        let mut tb = [0u8; 32];
        getrandom::getrandom(&mut tb).expect("random bytes");
        use base64::Engine as _;
        let token = format!(
            "{TOKEN_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tb)
        );
        let client = json!({"id": random_hex(6), "name": clean_name(name, "Client"), "hash": token_hash(&token),
            "admin": false, "created": now(), "last_seen": 0.0, "last_address": address});
        let mut clients = read(&self.file, json!([]));
        if let Some(a) = clients.as_array_mut() {
            a.push(client.clone());
        }
        write(&self.file, &clients);
        Ok(json!({"token": token, "client": Self::public(&client)}))
    }

    fn public(c: &Value) -> Value {
        let mut m = Map::new();
        for k in [
            "id",
            "name",
            "admin",
            "created",
            "last_seen",
            "last_address",
        ] {
            if let Some(v) = c.get(k) {
                m.insert(k.into(), v.clone());
            }
        }
        Value::Object(m)
    }

    pub fn list(&self) -> Vec<Value> {
        read(&self.file, json!([]))
            .as_array()
            .into_iter()
            .flatten()
            .map(Self::public)
            .collect()
    }

    /// The client of a token; "last seen" is written at most once a minute.
    pub fn find(&self, token: &str, address: &str) -> Option<Value> {
        let want = token_hash(token);
        let found = read(&self.file, json!([]))
            .as_array()
            .into_iter()
            .flatten()
            .rfind(|c| ct_eq(c["hash"].as_str().unwrap_or(""), &want))
            .cloned()?;
        if now() - found["last_seen"].as_f64().unwrap_or(0.0) > 60.0 {
            let _g = self.lock.lock().unwrap();
            let mut clients = read(&self.file, json!([]));
            for c in clients.as_array_mut().into_iter().flatten() {
                if c["id"] == found["id"] {
                    c["last_seen"] = json!(now());
                    c["last_address"] = json!(address);
                }
            }
            write(&self.file, &clients);
        }
        Some(found)
    }

    fn change(&self, id: &str, f: impl Fn(&mut Vec<Value>) -> bool) -> bool {
        let _g = self.lock.lock().unwrap();
        let mut clients: Vec<Value> = read(&self.file, json!([]))
            .as_array()
            .cloned()
            .unwrap_or_default();
        let _ = id;
        let hit = f(&mut clients);
        if hit {
            write(&self.file, &json!(clients));
        }
        hit
    }

    pub fn revoke(&self, id: &str) -> bool {
        self.change(id, |cs| {
            let n = cs.len();
            cs.retain(|c| c["id"] != id);
            cs.len() != n
        })
    }

    pub fn set_admin(&self, id: &str, admin: bool) -> bool {
        self.change(id, |cs| {
            let mut hit = false;
            for c in cs.iter_mut().filter(|c| c["id"] == id) {
                c["admin"] = json!(admin);
                hit = true;
            }
            hit
        })
    }
}

/// None when a paired client may do this, else (status, message, code).
pub fn check_rights(
    client: &Value,
    method: &str,
    path: &str,
    confirmed: bool,
) -> Option<(u16, &'static str, &'static str)> {
    let re = |p: &str| regex::Regex::new(p).unwrap();
    if path == "/api/remote/whoami" {
        return None;
    }
    // client tokens never manage clients or hosts, never chain to other hosts, never use the host's plugins
    if re(r"^/(?:api/(?:hosts|remote|plugins|p)(?:/|$)|h/|plugins/)").is_match(path) {
        return Some((
            403,
            "This can only be done on the host itself.",
            "owner_only",
        ));
    }
    if method == "GET" || method == "HEAD" {
        return None;
    }
    // what the default right allows beyond reading: chat, measure, and computing a command preview
    if re(r"^/(?:api/chats(?:/|$)|api/attachments$|api/bench(?:/|$)|api/engine/(?:preview|plan)$|api/flags/explain$|v1/)").is_match(path) {
        return None;
    }
    if client["admin"] != true {
        return Some((403, "This client may chat, look and measure. Changes need admin rights; the owner of the host can grant them.", "no_admin"));
    }
    if re(r"^/api/(?:engines/install|engines/[\w.-]+/remove|update/apply|models/[0-9a-f]{12}/remove)$").is_match(path) && !confirmed {
        return Some((428, "Confirm this on the client.", "confirm"));
    }
    None
}

// ------------------------------------------------------------------------------------------------ client: ssh tunnel

/// The ssh command: `FLE_SSH` (a path, or a JSON list; for tests), else the system's ssh.
fn ssh_program() -> Result<Vec<String>, RemoteError> {
    let o = std::env::var("FLE_SSH").unwrap_or_default();
    let o = o.trim();
    if o.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Vec<String>>(o) {
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    if !o.is_empty() {
        return Ok(vec![o.to_string()]);
    }
    flelink_core::launch::which("ssh")
        .map(|p| vec![p.to_string_lossy().to_string()])
        .ok_or(RemoteError(
            400,
            "The ssh program was not found. Install the OpenSSH client.".into(),
            "no_ssh",
        ))
}

fn check_ssh(
    target: &str,
    port: &Value,
    remote_port: &Value,
) -> Result<(String, u16, u16), RemoteError> {
    let target = target.trim();
    let ok = regex::Regex::new(r"^(?:[A-Za-z0-9._-]+@)?[A-Za-z0-9._:-]+$").unwrap();
    if target.is_empty() || target.starts_with('-') || !ok.is_match(target) {
        return rerr(
            400,
            "The SSH target must look like user@host (letters, digits, dots, dashes).",
            "",
        );
    }
    let num = |v: &Value, d: u16| -> Result<u16, RemoteError> {
        let n = match v {
            Value::Null => Some(d as i64),
            Value::String(s) if s.is_empty() => Some(d as i64),
            Value::String(s) => s.trim().parse().ok(),
            v => v.as_i64(),
        };
        match n {
            Some(n) if (1..=65535).contains(&n) => Ok(n as u16),
            Some(_) => rerr(400, "Ports must be between 1 and 65535.", ""),
            None => rerr(400, "Ports must be numbers.", ""),
        }
    };
    Ok((
        target.to_string(),
        num(port, 22)?,
        num(remote_port, DEFAULT_PORT)?,
    ))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(0)
}

fn listening(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(1))
        .is_ok()
}

struct TunnelState {
    state: String, // stopped | starting | up | down
    error: String,
    local_port: u16,
    child: Option<Child>,
}

/// `ssh -N -L` kept alive: started on first use, watched, started again with a growing pause when it dies.
pub struct Tunnel {
    target: String,
    port: u16,
    remote_port: u16,
    st: Arc<Mutex<TunnelState>>,
    stop: Arc<AtomicBool>,
    restart: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
}

impl Tunnel {
    fn new(target: &str, port: u16, remote_port: u16) -> Tunnel {
        Tunnel {
            target: target.into(),
            port,
            remote_port,
            st: Arc::new(Mutex::new(TunnelState {
                state: "stopped".into(),
                error: String::new(),
                local_port: 0,
                child: None,
            })),
            stop: Arc::new(AtomicBool::new(false)),
            restart: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn info(&self) -> Value {
        let s = self.st.lock().unwrap();
        json!({"state": s.state, "error": s.error, "local_port": if s.state == "up" { s.local_port } else { 0 }})
    }

    fn start(&self) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        self.stop.store(false, Ordering::SeqCst);
        self.st.lock().unwrap().state = "starting".into();
        let (st, stop, restart, running) = (
            self.st.clone(),
            self.stop.clone(),
            self.restart.clone(),
            self.running.clone(),
        );
        let (target, port, remote) = (self.target.clone(), self.port, self.remote_port);
        std::thread::spawn(move || {
            let mut pause = 1.0f64;
            while !stop.load(Ordering::SeqCst) {
                let program = match ssh_program() {
                    Ok(p) => p,
                    Err(e) => {
                        let mut s = st.lock().unwrap();
                        s.state = "down".into();
                        s.error = e.1;
                        break;
                    }
                };
                let local = free_port();
                let mut cmd = Command::new(&program[0]);
                cmd.args(&program[1..])
                    .args([
                        "-N",
                        "-T",
                        "-L",
                        &format!("127.0.0.1:{local}:127.0.0.1:{remote}"),
                        "-p",
                        &port.to_string(),
                        "-o",
                        "BatchMode=yes", // never ask for a password: keys or the agent
                        "-o",
                        "ExitOnForwardFailure=yes",
                        "-o",
                        "ServerAliveInterval=15",
                        "-o",
                        "ServerAliveCountMax=3",
                        "-o",
                        "StrictHostKeyChecking=accept-new",
                        "-o",
                        "ConnectTimeout=10",
                        &target,
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped());
                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(e) => {
                        {
                            let mut s = st.lock().unwrap();
                            s.state = "down".into();
                            s.error = format!("Could not start ssh: {e}");
                        }
                        std::thread::sleep(Duration::from_secs_f64(pause));
                        pause = (pause * 2.0).min(30.0);
                        continue;
                    }
                };
                let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
                if let Some(err) = child.stderr.take() {
                    let tail = tail.clone();
                    std::thread::spawn(move || {
                        let mut s = String::new();
                        let _ = std::io::BufReader::new(err).read_to_string(&mut s);
                        let mut t = tail.lock().unwrap();
                        for l in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
                            t.push_back(l.to_string());
                            if t.len() > 8 {
                                t.pop_front();
                            }
                        }
                    });
                }
                st.lock().unwrap().child = Some(child);
                let alive = |st: &Mutex<TunnelState>| {
                    st.lock()
                        .unwrap()
                        .child
                        .as_mut()
                        .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
                };
                let deadline = Instant::now() + Duration::from_secs(20);
                while !stop.load(Ordering::SeqCst) && alive(&st) && Instant::now() < deadline {
                    if listening(local) {
                        let mut s = st.lock().unwrap();
                        s.local_port = local;
                        s.state = "up".into();
                        s.error.clear();
                        pause = 1.0;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                while !stop.load(Ordering::SeqCst) && alive(&st) {
                    std::thread::sleep(Duration::from_millis(300));
                }
                if let Some(mut c) = st.lock().unwrap().child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if restart.swap(false, Ordering::SeqCst) {
                    continue; // asked for a new one: no pause
                }
                std::thread::sleep(Duration::from_millis(100)); // let the reader see the last lines
                {
                    let mut s = st.lock().unwrap();
                    s.state = "down".into();
                    let t: Vec<String> = tail.lock().unwrap().iter().cloned().collect();
                    s.error = if t.is_empty() {
                        "ssh ended".into()
                    } else {
                        t.join(" ")
                    };
                }
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_secs_f64(pause)
                    && !stop.load(Ordering::SeqCst)
                    && !restart.load(Ordering::SeqCst)
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
                restart.store(false, Ordering::SeqCst);
                pause = (pause * 2.0).min(30.0);
            }
            running.store(false, Ordering::SeqCst);
            if stop.load(Ordering::SeqCst) {
                st.lock().unwrap().state = "stopped".into();
            }
        });
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.st.lock().unwrap().child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.st.lock().unwrap().state = "stopped".into();
    }

    /// A new ssh process now, without the pause ("Reconnect").
    fn restart(&self) {
        self.restart.store(true, Ordering::SeqCst);
        self.st.lock().unwrap().state = "starting".into();
        if let Some(c) = self.st.lock().unwrap().child.as_mut() {
            let _ = c.kill(); // the supervisor starts a new one
        }
        self.start();
    }

    fn wait_ready(&self, timeout: Duration) -> Result<u16, String> {
        self.start();
        let t = Instant::now();
        while t.elapsed() < timeout {
            {
                let s = self.st.lock().unwrap();
                if s.state == "up" {
                    return Ok(s.local_port);
                }
                if s.state == "down" {
                    return Err(if s.error.is_empty() {
                        "The SSH tunnel is not up yet".into()
                    } else {
                        s.error.clone()
                    });
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let s = self.st.lock().unwrap();
        if s.state == "up" {
            Ok(s.local_port)
        } else {
            Err(if s.error.is_empty() {
                "The SSH tunnel is not up yet".into()
            } else {
                s.error.clone()
            })
        }
    }
}

// ------------------------------------------------------------------------------------------------ client: to a host

/// TLS that accepts exactly the pinned certificate (or, with nothing pinned, any, remembering what it saw).
#[derive(Debug)]
struct Pinned {
    pinned: String,
    seen: Arc<Mutex<String>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let fp = hex::encode(sha2::Sha256::digest(end_entity.as_ref()));
        *self.seen.lock().unwrap() = fp.clone();
        if !self.pinned.is_empty() && fp != self.pinned {
            return Err(rustls::Error::General("fingerprint_changed".into()));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// An HTTP agent for a host: pinned TLS for https; (agent, what it saw).
fn agent_for(pinned: &str, timeout: Duration) -> (ureq::Agent, Arc<Mutex<String>>) {
    let seen = Arc::new(Mutex::new(String::new()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            pinned: normalize_fp(pinned),
            seen: seen.clone(),
            provider,
        }))
        .with_no_client_auth();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(timeout)
        .tls_config(Arc::new(config))
        .redirects(0)
        .build();
    (agent, seen)
}

/// "gpu-box", "gpu-box:8642", "https://10.0.0.5:9000" or "http://…" to (scheme, host, port).
pub fn parse_address(text: &str) -> Result<(String, String, u16), RemoteError> {
    let raw = text.trim();
    if raw.is_empty() {
        return rerr(400, "Enter the host's address.", "");
    }
    let raw = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let (scheme, rest) = raw.split_once("://").unwrap();
    let netloc = rest.split(['/', '?', '#']).next().unwrap_or("");
    let (host, port) = if let Some(h) = netloc.strip_prefix('[') {
        let (h, p) = h.split_once(']').unwrap_or((h, ""));
        (h.to_string(), p.strip_prefix(':').unwrap_or(""))
    } else {
        match netloc.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p),
            None => (netloc.to_string(), ""),
        }
    };
    if !(scheme == "http" || scheme == "https") || host.is_empty() {
        return rerr(400, "The address must look like host or host:port.", "");
    }
    let port = if port.is_empty() {
        DEFAULT_PORT
    } else {
        port.parse()
            .map_err(|_| RemoteError(400, "The port is not a number.".into(), ""))?
    };
    Ok((scheme.to_string(), host.to_lowercase(), port))
}

fn resolve(name: &str) -> Result<String, String> {
    use std::net::ToSocketAddrs;
    (name, 0)
        .to_socket_addrs()
        .map_err(|e| format!("Cannot find {name}: {e}"))?
        .next()
        .map(|a| a.ip().to_string())
        .ok_or_else(|| format!("Cannot find {name}"))
}

/// The paired hosts of this machine (`hosts.json`, with their tokens, readable by the user only) and their tunnels.
pub struct Hosts {
    data_dir: PathBuf,
    file: PathBuf,
    lock: Mutex<()>,
    tunnels: Mutex<HashMap<String, Arc<Tunnel>>>,
}

/// What went wrong talking to a host: (code, message, extra).
pub fn describe_failure(e: &str, seen: &str, pinned: &str) -> (&'static str, String, Value) {
    if e.contains("fingerprint_changed") {
        return ("fingerprint_changed", "The host's certificate has changed since you paired. It may be a new installation of the host, or somebody in between. Nothing was sent.".into(),
            json!({"seen": pretty_fp(seen), "pinned": pretty_fp(pinned)}));
    }
    if e.to_lowercase().contains("timed out") {
        return (
            "unreachable",
            "The host did not answer in time.".into(),
            json!({}),
        );
    }
    ("unreachable", format!("Host not reachable: {e}"), json!({}))
}

impl Hosts {
    pub fn new(data_dir: &Path) -> Hosts {
        Hosts {
            data_dir: data_dir.to_path_buf(),
            file: data_dir.join("hosts.json"),
            lock: Mutex::new(()),
            tunnels: Mutex::new(HashMap::new()),
        }
    }

    pub fn all(&self) -> Vec<Value> {
        read(&self.file, json!([]))
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Result<Value, RemoteError> {
        self.all()
            .into_iter()
            .find(|h| h["id"] == id)
            .ok_or(RemoteError(404, "This host is not in the list.".into(), ""))
    }

    pub fn save(&self, record: &Value) {
        let _g = self.lock.lock().unwrap();
        let mut hosts: Vec<Value> = self
            .all()
            .into_iter()
            .filter(|h| h["id"] != record["id"])
            .collect();
        hosts.push(record.clone());
        hosts.sort_by(|a, b| {
            a["added"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&b["added"].as_f64().unwrap_or(0.0))
        });
        write(&self.file, &json!(hosts));
    }

    pub fn remove(&self, id: &str) {
        if let Some(t) = self.tunnels.lock().unwrap().remove(id) {
            t.stop();
        }
        let _g = self.lock.lock().unwrap();
        let hosts: Vec<Value> = self.all().into_iter().filter(|h| h["id"] != id).collect();
        write(&self.file, &json!(hosts));
        let _ = std::fs::remove_dir_all(self.data_dir.join("mirror").join(id));
    }

    pub fn public(&self, h: &Value) -> Value {
        let mut out = Map::new();
        for k in [
            "id",
            "name",
            "color",
            "kind",
            "scheme",
            "address",
            "fingerprint",
            "mirror",
            "ssh",
            "paired",
        ] {
            out.insert(k.into(), h.get(k).cloned().unwrap_or(Value::Null));
        }
        out.insert(
            "fingerprint_pretty".into(),
            json!(pretty_fp(h["fingerprint"].as_str().unwrap_or(""))),
        );
        let t = self
            .tunnels
            .lock()
            .unwrap()
            .get(h["id"].as_str().unwrap_or(""))
            .map(|t| t.info());
        out.insert("tunnel".into(), t.unwrap_or(Value::Null));
        Value::Object(out)
    }

    fn next_color(&self) -> String {
        let used: Vec<String> = self
            .all()
            .iter()
            .filter_map(|h| h["color"].as_str().map(String::from))
            .collect();
        COLORS
            .iter()
            .find(|c| !used.iter().any(|u| u == *c))
            .map_or_else(
                || COLORS[used.len() % COLORS.len()].to_string(),
                |c| c.to_string(),
            )
    }

    fn tunnel(&self, h: &Value) -> Arc<Tunnel> {
        let id = h["id"].as_str().unwrap_or("").to_string();
        self.tunnels
            .lock()
            .unwrap()
            .entry(id)
            .or_insert_with(|| {
                let ssh = &h["ssh"];
                Arc::new(Tunnel::new(
                    ssh["target"].as_str().unwrap_or(""),
                    ssh["port"].as_u64().unwrap_or(22) as u16,
                    ssh["remote_port"].as_u64().unwrap_or(DEFAULT_PORT as u64) as u16,
                ))
            })
            .clone()
    }

    pub fn shutdown(&self) {
        for t in self.tunnels.lock().unwrap().values() {
            t.stop();
        }
    }

    /// The base URL of a host and an agent for it (pinned TLS, or through the tunnel), and what TLS saw.
    pub fn connection(
        &self,
        h: &Value,
        timeout: Duration,
    ) -> Result<(String, ureq::Agent, Arc<Mutex<String>>), String> {
        if h["kind"] == "ssh" {
            let port = self.tunnel(h).wait_ready(Duration::from_secs(15))?;
            let (a, seen) = agent_for("", timeout);
            return Ok((format!("http://127.0.0.1:{port}"), a, seen));
        }
        let ip = resolve(h["address_host"].as_str().unwrap_or(""))?;
        let port = h["port"].as_u64().unwrap_or(DEFAULT_PORT as u64);
        let host = if ip.contains(':') {
            format!("[{ip}]:{port}")
        } else {
            format!("{ip}:{port}")
        };
        let scheme = if h["scheme"] == "http" {
            "http"
        } else {
            "https"
        };
        let (a, seen) = agent_for(h["fingerprint"].as_str().unwrap_or(""), timeout);
        Ok((format!("{scheme}://{host}"), a, seen))
    }

    /// One JSON request to a host, for the add and check dialogs: (status, body).
    pub fn request(
        &self,
        h: &Value,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
        token: bool,
    ) -> Result<(u16, Value), (String, String)> {
        let (base, agent, seen) = self
            .connection(h, timeout)
            .map_err(|e| (e, String::new()))?;
        let mut req = agent
            .request(method, &format!("{base}{path}"))
            .set("Accept", "application/json");
        if token {
            if let Some(t) = h["token"].as_str().filter(|t| !t.is_empty()) {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
        }
        let r = match body {
            Some(b) => req
                .set("Content-Type", "application/json")
                .send_string(&b.to_string()),
            None => req.call(),
        };
        let (status, text) = match r {
            Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
            Err(e) => return Err((e.to_string(), seen.lock().unwrap().clone())),
        };
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    /// Is the host there and does it know this client?
    pub fn probe(&self, h: &Value) -> Value {
        match self.request(
            h,
            "GET",
            "/api/remote/whoami",
            None,
            Duration::from_secs(6),
            true,
        ) {
            Err((e, seen)) => {
                let (code, message, extra) =
                    describe_failure(&e, &seen, h["fingerprint"].as_str().unwrap_or(""));
                let mut v = json!({"ok": false, "code": code, "message": message});
                if let Some(o) = extra.as_object() {
                    for (k, x) in o {
                        v[k] = x.clone();
                    }
                }
                v
            }
            Ok((401, _)) => {
                json!({"ok": false, "code": "unpaired", "message": "The host no longer knows this client. Pair again."})
            }
            Ok((200, d)) if d.is_object() => {
                json!({"ok": true, "host_name": d["host"].as_str().unwrap_or(""),
                "admin": d["admin"] == true, "name": d["name"].as_str().unwrap_or("")})
            }
            Ok((s, _)) => {
                json!({"ok": false, "code": "unreachable", "message": format!("The host answered {s}.")})
            }
        }
    }

    /// Pin the certificate the host shows now and trade the code for a token; the changed record.
    pub fn pair(
        &self,
        record: &Value,
        code: &str,
        client_name: &str,
    ) -> Result<Value, RemoteError> {
        let mut record = record.clone();
        if record["kind"] == "direct" && record["scheme"] == "https" {
            let mut probe = record.clone();
            probe["fingerprint"] = json!("");
            let seen = match self.connection(&probe, Duration::from_secs(10)) {
                Ok((base, agent, seen)) => {
                    let _ = agent.get(&format!("{base}/api/ping")).call();
                    let s = seen.lock().unwrap().clone();
                    if s.is_empty() {
                        return rerr(502, "Host not reachable: no TLS answer", "unreachable");
                    }
                    s
                }
                Err(e) => return rerr(502, describe_failure(&e, "", "").1, "unreachable"),
            };
            let expect = normalize_fp(record["expect"].as_str().unwrap_or(""));
            if !expect.is_empty() && expect != seen {
                return rerr(
                    400,
                    "The host shows a different certificate than the fingerprint you entered.",
                    "fingerprint_mismatch",
                );
            }
            record["fingerprint"] = json!(seen);
        } else {
            record["fingerprint"] = json!("");
        }
        let proof = pair_proof(code, record["fingerprint"].as_str().unwrap_or(""));
        let (status, data) = self
            .request(
                &record,
                "POST",
                "/api/remote/pair",
                Some(&json!({"proof": proof, "name": client_name})),
                Duration::from_secs(15),
                false,
            )
            .map_err(|(e, seen)| {
                RemoteError(502, describe_failure(&e, &seen, "").1, "unreachable")
            })?;
        let Some(token) = data["token"].as_str().filter(|_| status == 200) else {
            let err = &data["error"];
            let code: &'static str = match err["code"].as_str() {
                Some("rate_limited") => "rate_limited",
                Some("no_code") => "no_code",
                Some("bad_code") => "bad_code",
                _ => "",
            };
            return Err(RemoteError(
                if status == 400 || status == 429 {
                    status
                } else {
                    502
                },
                err["message"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| format!("The host answered {status}.")),
                code,
            ));
        };
        record["token"] = json!(token);
        record["paired"] = json!(now());
        if let Some(o) = record.as_object_mut() {
            o.remove("expect");
        }
        if record["name"].as_str().is_none_or(|n| n.is_empty())
            || record["name"] == record["address"]
        {
            let fallback = record["name"].as_str().unwrap_or("").to_string();
            record["name"] = json!(clean_name(data["host"].as_str().unwrap_or(""), &fallback));
        }
        Ok(record)
    }

    /// `POST /api/hosts`: add a host by address or SSH target, pair it, check it.
    pub fn add(&self, body: &Value, client_name: &str) -> Result<Value, RemoteError> {
        let kind = body["kind"]
            .as_str()
            .filter(|k| !k.is_empty())
            .unwrap_or("direct");
        let code = body["code"].as_str().unwrap_or("");
        if normalize_code(code).is_empty() {
            return rerr(400, "Enter the pairing code that the host shows.", "");
        }
        let mut record = json!({"id": random_hex(6), "kind": kind, "color": self.next_color(), "mirror": body["mirror"] == true,
            "added": now(), "token": "", "fingerprint": ""});
        match kind {
            "ssh" => {
                let (target, port, rport) = check_ssh(
                    body["target"].as_str().unwrap_or(""),
                    &body["port"],
                    &body["remote_port"],
                )?;
                let short = target.rsplit('@').next().unwrap_or(&target).to_string();
                record["ssh"] = json!({"target": target, "port": port, "remote_port": rport});
                record["scheme"] = json!("http");
                record["address"] = json!(target);
                record["address_host"] = json!("127.0.0.1");
                record["port"] = json!(rport);
                record["name"] = json!(clean_name(body["name"].as_str().unwrap_or(""), &short));
            }
            "direct" => {
                let (scheme, name, port) = parse_address(body["address"].as_str().unwrap_or(""))?;
                record["scheme"] = json!(scheme);
                record["address"] = json!(format!("{name}:{port}"));
                record["address_host"] = json!(name);
                record["port"] = json!(port);
                record["ssh"] = Value::Null;
                record["name"] = json!(clean_name(body["name"].as_str().unwrap_or(""), &name));
                record["expect"] = json!(body["fingerprint"].as_str().unwrap_or(""));
            }
            _ => return rerr(400, "Unknown kind of host.", ""),
        }
        if self
            .all()
            .iter()
            .any(|h| h["address"] == record["address"] && h["kind"] == kind)
        {
            return rerr(
                409,
                "This host is already in the list. Remove it first to pair again.",
                "",
            );
        }
        let paired = self.pair(&record, code, client_name)?;
        self.save(&paired);
        let probe = self.probe(&paired);
        Ok(json!({"host": self.public(&paired), "probe": probe}))
    }

    pub fn reconnect(&self, h: &Value) {
        if h["kind"] == "ssh" {
            self.tunnel(h).restart();
        }
    }

    pub fn mirror_dir(&self, id: &str) -> PathBuf {
        self.data_dir.join("mirror").join(id)
    }
}

/// The paths that stay on this machine: pairing and host management, and anything trying to reach them around the check.
pub fn never_forwarded(rest: &str) -> bool {
    let low = rest.to_lowercase();
    if low.contains("%2e") || low.contains("%2f") || rest.contains('\\') {
        return true;
    }
    if rest
        .split('?')
        .next()
        .unwrap_or("")
        .split('/')
        .any(|s| s == "." || s == "..")
    {
        return true;
    }
    rest == "/api/remote" || rest.starts_with("/api/remote/") || rest.starts_with("/api/hosts")
}

/// A chat list or a chat of a mirrored host, saved after a good answer and shown while the host is away.
pub fn mirror_file(dir: &Path, path: &str) -> Option<PathBuf> {
    if path == "/api/chats" {
        return Some(dir.join("chats.json"));
    }
    let id = path.strip_prefix("/api/chats/")?;
    (id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| dir.join(format!("chat-{id}.json")))
}

pub fn mirror_save(dir: &Path, path: &str, raw: &[u8]) {
    if let (Some(f), Ok(v)) = (mirror_file(dir, path), serde_json::from_slice::<Value>(raw)) {
        write(&f, &v);
    }
}

pub fn mirror_read(dir: &Path, path: &str) -> Option<Value> {
    let f = mirror_file(dir, path)?;
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?;
    if v.is_object() {
        v["mirror"] = json!(true);
    }
    Some(v)
}

pub fn ssh_available() -> bool {
    ssh_program().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proof_and_names_are_the_python_studios() {
        // fle_studio_remote.remote.pair_proof("abcd-efgh", "AA:BB") (Python Studio 055f4aa)
        assert_eq!(pair_proof("abcd-efgh", "AA:BB"), PY_PROOF);
        assert_eq!(normalize_code("ab1c-d0ef"), "ABCDEF");
        assert_eq!(pretty_fp("aabbcc"), "AA:BB:CC");
        assert_eq!(clean_name("My <Mac>!", "x"), "My Mac");
        assert_eq!(
            parse_address("gpu-box").unwrap(),
            ("https".into(), "gpu-box".into(), 8642)
        );
        assert_eq!(
            parse_address("http://10.0.0.5:9000/x").unwrap(),
            ("http".into(), "10.0.0.5".into(), 9000)
        );
        assert!(
            never_forwarded("/api/../api/remote/code")
                && never_forwarded("/api/hosts")
                && !never_forwarded("/api/chats")
        );
    }

    #[test]
    fn rights_of_a_client() {
        let c = json!({"admin": false});
        assert!(check_rights(&c, "GET", "/api/models", false).is_none());
        assert!(check_rights(&c, "POST", "/v1/chat/completions", false).is_none());
        assert_eq!(
            check_rights(&c, "POST", "/api/settings", false).unwrap().2,
            "no_admin"
        );
        assert_eq!(
            check_rights(&c, "GET", "/api/hosts", false).unwrap().2,
            "owner_only"
        );
        let a = json!({"admin": true});
        assert_eq!(
            check_rights(&a, "POST", "/api/engines/install", false)
                .unwrap()
                .0,
            428
        );
        assert!(check_rights(&a, "POST", "/api/engines/install", true).is_none());
    }

    const PY_PROOF: &str = "29c41bca41c2e0bfb94af97354a2ce026851958228cda2634b6da0bef1c6ac93";
}
