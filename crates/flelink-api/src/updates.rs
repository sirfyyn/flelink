//! Update check and one-click update of FLE Link itself. The idea of `fle_studio_updater/updates.py`, for a single file.
//!
//! The check asks GitHub (no login, nothing but the usual request headers) for the latest release of FLE Link and, when
//! an FLE engine is installed, for the latest FLE engine. It runs at most once a day, can be turned off in Settings, keeps
//! its answer in `<data>/flelink-update-check.json` and stays quiet without network. (Not `update-check.json`: the data
//! folder can still be FLE Studio's, and that file holds the Python Studio's own update answer, from another repository.)
//!
//! The one-click update works for the `flelink` file the installer put into a folder the user can write: it downloads the
//! release archive for this machine, checks it against the SHA-256 file of the release, puts the new `flelink` (and
//! `fle`) next to the old one and starts again with the same arguments. The desktop app, a build from source and a
//! program in a system folder get the command or the page that fits, to do it themselves. Nothing from the network is
//! rendered as HTML: the notes are plain text, the release link is built from a tag that passed a strict pattern.
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const REPO: &str = "sirfyyn/flelink";
const ENGINE_REPO: &str = "sirfyyn/FastLocalExperts";
/// seconds between two automatic checks
const CHECK_EVERY: f64 = 24.0 * 3600.0;
/// when the last try found no network at all
const RETRY_OFFLINE: f64 = 3600.0;
const MANUAL_GAP: f64 = 5.0;
const NOTES_MAX: usize = 4000;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn safe_tag(t: &str) -> bool {
    regex::Regex::new(r"^v?\d+\.\d+\.\d+(?:[-.][A-Za-z0-9.]{1,20})?$")
        .unwrap()
        .is_match(t)
}

/// (major, minor, patch, no pre-release part, pre-release text): a release without a pre-release part sorts above one with it.
type VersionKey = (u64, u64, u64, bool, String);

fn version_key(t: &str) -> Option<VersionKey> {
    let t = t.trim_start_matches('v');
    let (core, pre) = match t.split_once(['-', '+']) {
        Some((c, p)) => (c, p.to_string()),
        None => (t, String::new()),
    };
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let (a, b, c) = (it.next()??, it.next()??, it.next()??);
    Some((a, b, c, pre.is_empty(), pre))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (version_key(candidate), version_key(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn clean_notes(body: &Value) -> String {
    let text = body
        .as_str()
        .unwrap_or("")
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let text: String = text
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || (*c as u32) >= 32)
        .collect::<String>()
        .trim()
        .to_string();
    if text.chars().count() <= NOTES_MAX {
        return text;
    }
    let cut: String = text.chars().take(NOTES_MAX).collect();
    let cut = match cut.rfind("\n\n").filter(|i| *i > cut.len() / 2) {
        Some(i) => cut[..i].to_string(),
        None => cut,
    };
    format!("{}\n…", cut.trim_end())
}

/// The newest usable release of one release object or a list: drafts skipped, pre-releases only on request.
pub fn pick_release(releases: &Value, include_pre: bool) -> Option<Value> {
    let items = match releases {
        Value::Array(a) => a.clone(),
        v => vec![v.clone()],
    };
    let mut best: Option<(VersionKey, Value)> = None;
    for rel in items {
        if rel["draft"] == true {
            continue;
        }
        let tag = rel["tag_name"].as_str().unwrap_or("");
        let Some(key) = version_key(tag).filter(|_| safe_tag(tag)) else {
            continue;
        };
        let pre = rel["prerelease"] == true || !key.3;
        if pre && !include_pre {
            continue;
        }
        if best.as_ref().is_none_or(|(k, _)| key > *k) {
            let version = tag.trim_start_matches('v').to_string();
            best = Some((
                key,
                json!({"version": version, "tag": tag, "url": format!("https://github.com/{REPO}/releases/tag/{tag}"),
                    "notes": clean_notes(&rel["body"]),
                    "published": rel["published_at"].as_str().unwrap_or("").chars().take(25).collect::<String>(),
                    "prerelease": pre}),
            ));
        }
    }
    best.map(|(_, v)| v)
}

// ------------------------------------------------------------------------------------------------ this install

/// The name of the release archive for this machine.
pub fn asset_name() -> Option<String> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    };
    Some(if cfg!(windows) {
        format!("flelink-{target}.zip")
    } else {
        format!("flelink-{target}.tar.gz")
    })
}

/// Where a desktop app lives that can replace itself: the bundle on macOS, the AppImage file on Linux. An app from a
/// package (deb, rpm, the Windows setup) belongs to the system's installer and is not one of these.
#[derive(Debug, Clone, PartialEq)]
pub enum AppPlace {
    MacBundle(PathBuf),
    AppImage(PathBuf),
}

impl AppPlace {
    fn path(&self) -> &Path {
        match self {
            AppPlace::MacBundle(p) | AppPlace::AppImage(p) => p,
        }
    }
}

/// `exe` is the running program, `appimage` what the AppImage runtime says in `APPIMAGE` (the file the user started).
pub fn app_place_of(os: &str, exe: &Path, appimage: Option<&str>) -> Option<AppPlace> {
    match os {
        "macos" => {
            // …/FLE Link.app/Contents/MacOS/flelink-app
            let macos = exe.parent()?;
            let contents = macos.parent()?;
            let bundle = contents.parent()?;
            (macos.file_name()? == "MacOS"
                && contents.file_name()? == "Contents"
                && bundle.extension()? == "app")
                .then(|| AppPlace::MacBundle(bundle.to_path_buf()))
        }
        "linux" => {
            let f = PathBuf::from(appimage.filter(|a| !a.is_empty())?);
            f.is_absolute().then_some(AppPlace::AppImage(f))
        }
        _ => None,
    }
}

fn app_place(exe: &Path) -> Option<AppPlace> {
    app_place_of(
        std::env::consts::OS,
        exe,
        std::env::var("APPIMAGE").ok().as_deref(),
    )
}

/// The release file of the desktop app for this machine (the names `install.sh` takes too).
pub fn app_asset(os: &str, arch: &str, tag: &str) -> Option<String> {
    match (os, arch) {
        ("macos", "aarch64") => Some("FLE.Link_aarch64.app.tar.gz".into()),
        ("macos", "x86_64") => Some("FLE.Link_x64.app.tar.gz".into()),
        ("linux", "x86_64") => Some(format!(
            "FLE.Link_{}_amd64.AppImage",
            tag.trim_start_matches('v')
        )),
        _ => None,
    }
}

/// `FLE_UPDATE_FORCE=1` offers the newest release again although it is the running version: the way to try the whole
/// update (download, signature, replacement, restart) without waiting for the next release.
fn worth_installing(candidate: &str) -> bool {
    let current = env!("CARGO_PKG_VERSION");
    is_newer(candidate, current)
        || (std::env::var("FLE_UPDATE_FORCE").as_deref() == Ok("1")
            && version_key(candidate).is_some()
            && version_key(candidate) == version_key(current))
}

/// One file of a release, at most 200 MB.
fn fetch(base: &str, name: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut buf = vec![];
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(600))
        .build()
        .get(&format!("{base}/{name}"))
        .call()
        .map_err(|e| format!("The download of {name} failed: {e}."))?
        .into_reader()
        .take(200_000_000)
        .read_to_end(&mut buf)
        .map_err(|e| format!("The download of {name} failed: {e}."))?;
    Ok(buf)
}

/// A release file that matches the signed checksum list, or the reason it does not.
fn fetch_checked(base: &str, asset: &str) -> Result<Vec<u8>, String> {
    use sha2::Digest;
    let (sums, signature) = (
        fetch(base, "SHA256SUMS")?,
        fetch(base, "SHA256SUMS.minisig")?,
    );
    let want = flelink_core::signature::signed_sha(&sums, &signature, asset).map_err(|e| {
        format!("This update is not signed correctly ({e:#}); nothing was changed.")
    })?;
    let data = fetch(base, asset)?;
    let got = hex::encode(sha2::Sha256::digest(&data));
    if want.len() != 64 || want != got {
        return Err(format!(
            "The download does not match its checksum (wanted {want}, got {got}); nothing was changed."
        ));
    }
    Ok(data)
}

/// Put the app of `archive` (the release's .app.tar.gz) where `bundle` is. The new one is unpacked beside it first; the
/// old one comes back if the last step fails.
pub fn swap_bundle(bundle: &Path, archive: &[u8]) -> Result<(), String> {
    let parent = bundle.parent().ok_or("The app has no folder.")?;
    let name = bundle
        .file_name()
        .ok_or("The app has no name.")?
        .to_string_lossy()
        .to_string();
    let stage = parent.join(format!(".flelink-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let clean = |r: Result<(), String>| {
        let _ = std::fs::remove_dir_all(&stage);
        r
    };
    let mut t = tar::Archive::new(flate2::read::GzDecoder::new(std::io::Cursor::new(archive)));
    t.set_preserve_permissions(true);
    if let Err(e) = t.unpack(&stage) {
        return clean(Err(format!("The app archive could not be unpacked: {e}.")));
    }
    let fresh = std::fs::read_dir(&stage)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "app") && p.join("Contents/MacOS").is_dir());
    let Some(fresh) = fresh else {
        return clean(Err("The archive has no app in it.".into()));
    };
    let old = parent.join(format!(".{name}.old-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&old);
    if let Err(e) = std::fs::rename(bundle, &old) {
        return clean(Err(format!(
            "The running app could not be moved aside: {e}."
        )));
    }
    if let Err(e) = std::fs::rename(&fresh, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return clean(Err(format!("The new app could not be put in place: {e}.")));
    }
    let _ = std::fs::remove_dir_all(&old);
    clean(Ok(()))
}

/// Replace one program file (the AppImage): written beside it, then renamed over it.
pub fn swap_file(file: &Path, data: &[u8]) -> Result<(), String> {
    let dir = file.parent().ok_or("The program has no folder.")?;
    let name = file
        .file_name()
        .ok_or("The program has no name.")?
        .to_string_lossy()
        .to_string();
    let new = dir.join(format!(".{name}.new"));
    std::fs::write(&new, data).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&new, file).map_err(|e| {
        let _ = std::fs::remove_file(&new);
        e.to_string()
    })
}

/// Started by a service manager. systemd's variables alone do not say so: a shell in a desktop terminal inherits them
/// from the terminal's own unit, so a Studio started by hand there counted as a service and lost its update button.
fn in_service() -> bool {
    use std::io::IsTerminal;
    std::env::var_os("FLE_STUDIO_SERVICE").is_some()
        || ((std::env::var_os("INVOCATION_ID").is_some()
            || std::env::var_os("JOURNAL_STREAM").is_some())
            && !std::io::stdout().is_terminal())
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".flelink-write-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// How this FLE Link was installed: {kind: binary | app | cargo | source, service, exe}.
pub fn detect_install() -> Value {
    let exe = std::env::current_exe().unwrap_or_default();
    let name = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let path = exe.to_string_lossy().replace('\\', "/");
    let kind = if path.contains("/target/debug/") || path.contains("/target/release/") {
        "source"
    } else if name != "flelink" && name != "fle" {
        "app" // the desktop app runs the server inside its own program
    } else if path.contains("/.cargo/bin/") {
        "cargo"
    } else {
        "binary"
    };
    json!({"kind": kind, "source": kind, "service": in_service(), "exe": exe.to_string_lossy()})
}

/// Whether the one-click update is possible and, when it is not, what to do instead.
pub fn advice(install: &Value, tag: &str) -> Value {
    let kind = install["kind"].as_str().unwrap_or("");
    let exe = PathBuf::from(install["exe"].as_str().unwrap_or(""));
    let shell = if cfg!(windows) {
        "irm https://flelink.run/install.ps1 | iex"
    } else {
        "curl -fsSL https://flelink.run/install.sh | sh"
    };
    let (reason, command): (String, String) = match kind {
        // the bundle (macOS) or the AppImage replaces itself; an app from a package or the Windows setup does not
        "app" => {
            let by_hand = format!("The desktop app updates by installing the new version from https://github.com/{REPO}/releases/tag/{tag}.");
            match app_place(&exe) {
                Some(place)
                    if app_asset(std::env::consts::OS, std::env::consts::ARCH, tag).is_some() =>
                {
                    if place.path().parent().is_some_and(writable) {
                        (String::new(), String::new())
                    } else {
                        (
                            format!(
                                "FLE Link cannot write to {}, so the app cannot replace itself. Install the new version from https://github.com/{REPO}/releases/tag/{tag}.",
                                place.path().parent().map_or(String::new(), |p| p.display().to_string())
                            ),
                            String::new(),
                        )
                    }
                }
                _ => (by_hand, String::new()),
            }
        }
        "source" => (
            "This copy was built from source. Update the checkout with git and build again.".into(),
            "git pull && cargo build --release -p flelink".into(),
        ),
        "cargo" => (
            "This copy was installed with cargo, so cargo updates it.".into(),
            format!("cargo install --git https://github.com/{REPO} --tag {tag} flelink"),
        ),
        // Unix replaces the process in place (same pid), which a service manager does not notice; Windows starts a new
        // process, which the task that started the old one does not own
        _ if install["service"] == true && cfg!(windows) => (
            "FLE Link runs as a service here. Update it with the installer, then restart the service.".into(),
            shell.into(),
        ),
        _ if asset_name().is_none() => ("There is no release build for this machine.".into(), String::new()),
        _ if !exe.parent().is_some_and(writable) => (
            format!(
                "FLE Link cannot write to {}, so it cannot replace itself. Update it with the installer.",
                exe.parent().map_or(String::new(), |p| p.display().to_string())
            ),
            shell.into(),
        ),
        _ => (String::new(), String::new()),
    };
    json!({"can_auto": reason.is_empty(), "reason": reason, "command": command})
}

// ------------------------------------------------------------------------------------------------ the service

pub struct Updates {
    file: PathBuf,
    result_file: PathBuf,
    api: String,
    state: Mutex<Value>,
    checking: Mutex<bool>,
    last_manual: Mutex<f64>,
    pub run: Arc<Mutex<Value>>,
}

fn idle() -> Value {
    json!({"state": "idle", "tag": "", "message": "", "tail": []})
}

impl Updates {
    pub fn new(data_dir: &Path, github_api: &str) -> Updates {
        let file = data_dir.join("flelink-update-check.json");
        let state = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(|v| v["v"] == 1)
            .unwrap_or(json!({}));
        Updates {
            file,
            result_file: data_dir.join("flelink-update-result.json"),
            api: github_api.trim_end_matches('/').to_string(),
            state: Mutex::new(state),
            checking: Mutex::new(false),
            last_manual: Mutex::new(0.0),
            run: Arc::new(Mutex::new(idle())),
        }
    }

    fn get(&self, path: &str) -> Result<Value, (bool, String)> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();
        match agent
            .get(&format!("{}/{path}", self.api))
            .set("Accept", "application/vnd.github+json")
            .set(
                "User-Agent",
                &format!("flelink/{}", env!("CARGO_PKG_VERSION")),
            )
            .call()
        {
            Ok(r) => r
                .into_string()
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .ok_or((false, "GitHub's answer could not be read".into())),
            Err(ureq::Error::Status(c, _)) => Err((false, format!("GitHub answered {c}"))),
            Err(e) => Err((true, format!("No connection ({e})"))),
        }
    }

    fn enabled(config: &Value) -> bool {
        config["updates"]["check"] != false
    }

    pub fn due(&self, config: &Value) -> bool {
        if !Self::enabled(config) {
            return false;
        }
        let s = self.state.lock().unwrap();
        let last = s["attempt"].as_f64().unwrap_or(0.0);
        let wait = if s["offline"] == true {
            RETRY_OFFLINE
        } else {
            CHECK_EVERY
        };
        now() - last >= wait
    }

    /// Ask GitHub unless the answer is fresh (or `force`). Err only when checking is off.
    pub fn check(&self, config: &Value, installed: &[Value], force: bool) -> Result<(), String> {
        if !Self::enabled(config) {
            return Err("Update checks are turned off in Settings.".into());
        }
        {
            let mut c = self.checking.lock().unwrap();
            if *c {
                return Ok(());
            }
            if force {
                let mut last = self.last_manual.lock().unwrap();
                if now() - *last < MANUAL_GAP {
                    return Ok(());
                }
                *last = now();
            } else if !self.due(config) {
                return Ok(());
            }
            *c = true;
        }
        self.ask(config, installed);
        *self.checking.lock().unwrap() = false;
        Ok(())
    }

    fn ask(&self, config: &Value, installed: &[Value]) {
        let pre = config["updates"]["prereleases"] == true;
        let old = self.state.lock().unwrap().clone();
        let mut state = json!({"v": 1, "attempt": now(), "pre": pre, "offline": false, "error": "",
            "checked": old["checked"], "app": old["app"], "engines": old.get("engines").cloned().unwrap_or(json!([]))});
        let path = if pre {
            format!("{REPO}/releases?per_page=15")
        } else {
            format!("{REPO}/releases/latest")
        };
        match self.get(&path) {
            Ok(v) => {
                state["app"] = pick_release(&v, pre).unwrap_or(Value::Null);
                state["checked"] = json!(now());
            }
            Err((offline, e)) => {
                state["offline"] = json!(offline);
                state["error"] = json!(e);
            }
        }
        let fle: Vec<&Value> = installed.iter().filter(|e| e["kind"] == "fle").collect();
        if state["error"] == "" && !fle.is_empty() {
            state["engines"] = match self.get(&format!("{ENGINE_REPO}/releases/latest")) {
                Ok(rel) => json!(engine_updates(&fle, &rel)),
                Err(_) => json!([]), // the engine notice is a bonus; a failure is not shown
            };
        } else if fle.is_empty() {
            state["engines"] = json!([]);
        }
        let _ = std::fs::write(
            &self.file,
            serde_json::to_string_pretty(&state).unwrap_or_default(),
        );
        *self.state.lock().unwrap() = state;
    }

    fn result(&self) -> Value {
        std::fs::read_to_string(&self.result_file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null)
    }

    pub fn view(&self, config: &Value) -> Value {
        let s = self.state.lock().unwrap().clone();
        let current = env!("CARGO_PKG_VERSION");
        let install = detect_install();
        let dismissed = config["updates"]["dismissed"].as_str().unwrap_or("");
        let hidden: Vec<&str> = config["updates"]["dismissed_engines"]
            .as_str()
            .unwrap_or("")
            .split(',')
            .filter(|x| !x.is_empty())
            .collect();
        let engines: Vec<Value> = s["engines"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| {
                let mut e = e.clone();
                e["dismissed"] = json!(hidden.contains(&e["id"].as_str().unwrap_or("")));
                e
            })
            .collect();
        let mut app = Value::Null;
        if let Some(a) = s["app"]
            .as_object()
            .filter(|a| worth_installing(a["version"].as_str().unwrap_or("")))
        {
            let mut a = Value::Object(a.clone());
            a["dismissed"] = json!(a["version"] == dismissed);
            let tag = a["tag"].as_str().unwrap_or("").to_string();
            if let Some(adv) = advice(&install, &tag).as_object() {
                for (k, v) in adv {
                    a[k] = v.clone();
                }
            }
            app = a;
        }
        json!({"enabled": Self::enabled(config), "prereleases": config["updates"]["prereleases"] == true,
            "current": current, "checked": s["checked"], "checking": *self.checking.lock().unwrap(),
            "error": s["error"].as_str().unwrap_or(""), "app": app, "engines": engines,
            "install": {"kind": install["kind"], "source": install["source"], "service": install["service"]},
            "run": self.run.lock().unwrap().clone(), "result": self.result()})
    }

    pub fn dismiss_patch(&self, version: &str, engines: bool) -> Map<String, Value> {
        let mut patch = Map::new();
        if !version.is_empty() {
            patch.insert("dismissed".into(), json!(version));
        }
        if engines {
            let ids: Vec<String> = self.state.lock().unwrap()["engines"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e["id"].as_str().map(String::from))
                .collect();
            patch.insert("dismissed_engines".into(), json!(ids.join(",")));
        }
        patch
    }

    pub fn ack(&self) {
        let _ = std::fs::remove_file(&self.result_file);
        let mut run = self.run.lock().unwrap();
        if run["state"] == "failed" || run["state"] == "done" {
            *run = idle();
        }
    }

    /// Check that `tag` is the version the last check found and this install can replace itself; the archive URL base.
    pub fn prepare(&self, config: &Value, tag: &str) -> Result<String, String> {
        let app = self.state.lock().unwrap()["app"].clone();
        if !Self::enabled(config)
            || app["tag"] != tag
            || !worth_installing(app["version"].as_str().unwrap_or(""))
        {
            return Err(
                "That version is not the one the last check found. Check again first.".into(),
            );
        }
        if matches!(
            self.run.lock().unwrap()["state"].as_str(),
            Some("running" | "restarting")
        ) {
            return Err("An update is already running.".into());
        }
        let adv = advice(&detect_install(), tag);
        if adv["can_auto"] != true {
            return Err(adv["reason"]
                .as_str()
                .unwrap_or("This install cannot update itself.")
                .to_string());
        }
        *self.run.lock().unwrap() = json!({"state": "running", "tag": tag, "message": "Downloading the new version …", "tail": []});
        // FLE_RELEASE_BASE points the download at another server (tests)
        Ok(
            match std::env::var("FLE_RELEASE_BASE")
                .ok()
                .filter(|v| !v.is_empty())
            {
                Some(b) => format!("{}/{tag}", b.trim_end_matches('/')),
                None => format!("https://github.com/{REPO}/releases/download/{tag}"),
            },
        )
    }

    fn fail(&self, message: &str) {
        *self.run.lock().unwrap() =
            json!({"state": "failed", "tag": "", "message": message, "tail": []});
    }

    /// Download, check and put the new program in place; Ok means the caller restarts.
    pub fn install(&self, base: &str, tag: &str) -> Result<(), String> {
        let r = self.install_inner(base, tag);
        if let Err(e) = &r {
            self.fail(&format!("{e} The old version keeps running."));
        }
        r
    }

    /// The desktop app replaces its bundle (macOS) or its AppImage file (Linux) with the release's, checked like the program.
    fn install_app(&self, base: &str, tag: &str, exe: &Path) -> Result<(), String> {
        let place = app_place(exe).ok_or("This copy of the app cannot replace itself.")?;
        let asset = app_asset(std::env::consts::OS, std::env::consts::ARCH, tag)
            .ok_or("There is no app build for this machine.")?;
        let data = fetch_checked(base, &asset)?;
        *self.run.lock().unwrap() = json!({"state": "running", "tag": tag, "message": "Installing the new version …", "tail": []});
        match &place {
            AppPlace::MacBundle(bundle) => swap_bundle(bundle, &data)?,
            AppPlace::AppImage(file) => swap_file(file, &data)?,
        }
        let _ = std::fs::write(
            &self.result_file,
            json!({"status": "done", "tag": tag, "from": env!("CARGO_PKG_VERSION"), "at": now()})
                .to_string(),
        );
        *self.run.lock().unwrap() = json!({"state": "restarting", "tag": tag, "message": "FLE Link starts again with the new version …", "tail": []});
        Ok(())
    }

    fn install_inner(&self, base: &str, tag: &str) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if detect_install()["kind"] == "app" {
            return self.install_app(base, tag, &exe);
        }
        let asset = asset_name().ok_or("There is no release build for this machine.")?;
        let dir = exe
            .parent()
            .ok_or("The program has no folder.")?
            .to_path_buf();
        let archive = fetch_checked(base, &asset)?;
        *self.run.lock().unwrap() = json!({"state": "running", "tag": tag, "message": "Installing the new version …", "tail": []});
        let names: Vec<String> = ["flelink", "fle"]
            .iter()
            .map(|n| {
                if cfg!(windows) {
                    format!("{n}.exe")
                } else {
                    n.to_string()
                }
            })
            .collect();
        let mut files: Vec<(String, Vec<u8>)> = vec![];
        if asset.ends_with(".zip") {
            let mut z =
                zip::ZipArchive::new(std::io::Cursor::new(&archive)).map_err(|e| e.to_string())?;
            for n in &names {
                if let Ok(mut f) = z.by_name(n) {
                    let mut buf = vec![];
                    std::io::Read::read_to_end(&mut f, &mut buf).map_err(|e| e.to_string())?;
                    files.push((n.clone(), buf));
                }
            }
        } else {
            let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(&archive));
            let mut t = tar::Archive::new(gz);
            for entry in t.entries().map_err(|e| e.to_string())? {
                let mut entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path().map_err(|e| e.to_string())?.to_path_buf();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if names.contains(&name) {
                    let mut buf = vec![];
                    std::io::Read::read_to_end(&mut entry, &mut buf).map_err(|e| e.to_string())?;
                    files.push((name, buf));
                }
            }
        }
        let main = names[0].clone();
        if !files.iter().any(|(n, _)| *n == main) {
            return Err(format!("The archive has no {main}."));
        }
        // the running program keeps its file open: the new one is written beside it and renamed over it
        for (name, data) in &files {
            let target = if *name == main {
                exe.clone()
            } else {
                dir.join(name)
            };
            let new = dir.join(format!(".{name}.new"));
            std::fs::write(&new, data).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
            }
            if cfg!(windows) && target.exists() {
                // Windows cannot replace a running program, but it can rename it
                let old = dir.join(format!("{name}.old"));
                let _ = std::fs::remove_file(&old);
                std::fs::rename(&target, &old).map_err(|e| e.to_string())?;
            }
            std::fs::rename(&new, &target).map_err(|e| e.to_string())?;
        }
        let _ = std::fs::write(
            &self.result_file,
            json!({"status": "done", "tag": tag, "from": env!("CARGO_PKG_VERSION"), "at": now()})
                .to_string(),
        );
        *self.run.lock().unwrap() = json!({"state": "restarting", "tag": tag, "message": "FLE Link starts again with the new version …", "tail": []});
        Ok(())
    }
}

/// `flelink update`: the Studio's check and its signed replacement, from the terminal. `check_only` only says what there
/// is. The text to print; Err when the check or the installation failed (nothing was changed then).
pub fn cli(
    data_dir: &Path,
    github_api: &str,
    check_only: bool,
    pre: bool,
) -> Result<String, String> {
    let u = Updates::new(data_dir, github_api);
    let path = if pre {
        format!("{REPO}/releases?per_page=15")
    } else {
        format!("{REPO}/releases/latest")
    };
    let found = u
        .get(&path)
        .map_err(|(_, e)| format!("The check failed: {e}."))?;
    let app = pick_release(&found, pre).ok_or("GitHub lists no release of FLE Link.")?;
    let (version, tag) = (
        app["version"].as_str().unwrap_or(""),
        app["tag"].as_str().unwrap_or(""),
    );
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(version, current) {
        return Ok(format!("FLE Link {current} is the newest version."));
    }
    if check_only {
        return Ok(format!(
            "FLE Link {version} is out (this is {current}): {}\nInstall it with: flelink update",
            app["url"].as_str().unwrap_or("")
        ));
    }
    // from the terminal the program replaces its own file; whether a service runs it does not matter here
    let mut install = detect_install();
    install["service"] = json!(false);
    let adv = advice(&install, tag);
    if adv["can_auto"] != true {
        let cmd = adv["command"].as_str().unwrap_or("");
        return Err(format!(
            "{}{}",
            adv["reason"]
                .as_str()
                .unwrap_or("This install cannot update itself."),
            if cmd.is_empty() {
                String::new()
            } else {
                format!("\n  {cmd}")
            }
        ));
    }
    let base = match std::env::var("FLE_RELEASE_BASE")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(b) => format!("{}/{tag}", b.trim_end_matches('/')),
        None => format!("https://github.com/{REPO}/releases/download/{tag}"),
    };
    u.install_inner(&base, tag)?;
    Ok(format!(
        "FLE Link {version} is installed (was {current}), checked against the signed list of the release.\n\
         A Studio, app or service that is running keeps the old version until it starts again."
    ))
}

/// FLE engines that have a newer build in `release`, one entry per installed variant.
fn engine_updates(installed: &[&Value], release: &Value) -> Vec<Value> {
    let latest = release["tag_name"].as_str().unwrap_or("");
    let ok = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,60}$").unwrap();
    if !ok.is_match(latest) {
        return vec![];
    }
    let mut variants: Vec<String> = installed
        .iter()
        .filter_map(|e| e["variant"].as_str().map(String::from))
        .collect();
    variants.sort();
    variants.dedup();
    let mut out = vec![];
    for v in variants {
        let have: Vec<&&Value> = installed
            .iter()
            .filter(|e| e["variant"] == v.as_str())
            .collect();
        if have.iter().any(|e| e["tag"] == latest) {
            continue;
        }
        let newest = have
            .iter()
            .max_by(|a, b| {
                a["installed"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&b["installed"].as_f64().unwrap_or(0.0))
            })
            .unwrap();
        let backend = newest["backend"].as_str().unwrap_or(&v).to_string();
        out.push(json!({"id": format!("{v}@{latest}"), "variant": v, "label": format!("FLE engine ({backend})"),
            "installed": newest["tag"].as_str().unwrap_or(""), "latest": latest,
            "url": format!("https://github.com/{ENGINE_REPO}/releases/tag/{latest}")}));
    }
    out
}

/// Start this program again with the same arguments (no second browser tab). Unix replaces the process; Windows starts a
/// new one a moment after this one has ended.
pub fn restart() -> ! {
    let exe = std::env::current_exe().unwrap_or_default();
    #[cfg(unix)]
    if detect_install()["kind"] == "app" {
        if let Some(place) = app_place(&exe) {
            relaunch_app(&place);
        }
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "studio") && !args.iter().any(|a| a == "--no-browser") {
        args.push("--no-browser".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = std::process::Command::new(&exe).args(&args).exec();
        eprintln!("flelink: could not start the new version: {e}");
        std::process::exit(1);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let line = std::iter::once(format!("\"{}\"", exe.display()))
            .chain(args.iter().map(|a| format!("\"{a}\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = std::process::Command::new("cmd")
            .args([
                "/C",
                &format!("timeout /t 2 /nobreak >nul & start \"\" {line}"),
            ])
            .creation_flags(0x0800_0000 | 0x0000_0008)
            .spawn();
        std::process::exit(0);
    }
}

/// Start the replaced app and end this one. The app allows one instance, so the new one starts a moment after this one
/// is gone; the shell that waits is in its own process group and outlives it.
#[cfg(unix)]
fn relaunch_app(place: &AppPlace) -> ! {
    use std::os::unix::process::CommandExt;
    let script = match place {
        AppPlace::MacBundle(_) => "sleep 2; /usr/bin/open \"$0\"",
        AppPlace::AppImage(_) => "sleep 2; \"$0\" >/dev/null 2>&1 &",
    };
    let started = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg(place.path())
        .env_remove("FLE_UPDATE_FORCE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
    if let Err(e) = started {
        eprintln!("flelink: could not start the new app: {e}; open it again by hand");
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_knows_where_it_lives_and_which_file_replaces_it() {
        let exe = Path::new("/Applications/FLE Link.app/Contents/MacOS/flelink-app");
        assert_eq!(
            app_place_of("macos", exe, None),
            Some(AppPlace::MacBundle("/Applications/FLE Link.app".into()))
        );
        // a build run from the source tree is not a bundle, and a Linux app from a package has no AppImage file
        assert_eq!(
            app_place_of("macos", Path::new("/x/target/release/flelink-app"), None),
            None
        );
        assert_eq!(
            app_place_of("linux", Path::new("/usr/bin/flelink-app"), None),
            None
        );
        assert_eq!(
            app_place_of(
                "linux",
                Path::new("/tmp/.mount_x/usr/bin/flelink-app"),
                Some("/home/u/.local/bin/flelink-app")
            ),
            Some(AppPlace::AppImage("/home/u/.local/bin/flelink-app".into()))
        );
        assert_eq!(app_place_of("windows", exe, None), None);
        assert_eq!(
            app_asset("macos", "aarch64", "v0.1.11").as_deref(),
            Some("FLE.Link_aarch64.app.tar.gz")
        );
        assert_eq!(
            app_asset("linux", "x86_64", "v0.1.11").as_deref(),
            Some("FLE.Link_0.1.11_amd64.AppImage")
        );
        assert_eq!(app_asset("windows", "x86_64", "v0.1.11"), None);
    }

    /// A .app.tar.gz like the release's, with one program file that says `text`.
    #[cfg(unix)]
    fn app_archive(text: &str) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            vec![],
            flate2::Compression::fast(),
        ));
        let mut put = |path: &str, data: &[u8], mode: u32| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(mode);
            h.set_cksum();
            b.append_data(&mut h, path, data).unwrap();
        };
        put(
            "FLE Link.app/Contents/MacOS/flelink-app",
            text.as_bytes(),
            0o755,
        );
        put("FLE Link.app/Contents/Info.plist", b"<plist/>", 0o644);
        b.into_inner().unwrap().finish().unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_bundle_is_swapped_whole_and_a_bad_archive_changes_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("flelink-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let bundle = d.join("FLE Link.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/MacOS/flelink-app"), "old").unwrap();
        std::fs::write(bundle.join("Contents/left-over"), "from the old version").unwrap();
        // not an archive, and an archive without an app: the old app stays as it is
        assert!(swap_bundle(&bundle, b"not a tar").is_err());
        let mut empty = tar::Builder::new(flate2::write::GzEncoder::new(
            vec![],
            flate2::Compression::fast(),
        ));
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_cksum();
        empty.append_data(&mut h, "readme.txt", &b"x"[..]).unwrap();
        let empty = empty.into_inner().unwrap().finish().unwrap();
        assert!(swap_bundle(&bundle, &empty).is_err());
        assert_eq!(
            std::fs::read_to_string(bundle.join("Contents/MacOS/flelink-app")).unwrap(),
            "old"
        );
        // the real thing: the new program, executable, and nothing of the old bundle left in it or beside it
        swap_bundle(&bundle, &app_archive("new")).unwrap();
        let exe = bundle.join("Contents/MacOS/flelink-app");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!bundle.join("Contents/left-over").exists());
        let beside: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(beside, vec!["FLE Link.app"], "{beside:?}");
        // one file (the AppImage)
        let file = d.join("flelink-app");
        std::fs::write(&file, "old").unwrap();
        swap_file(&file, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn releases_are_picked_and_compared() {
        let rels = json!([
            {"tag_name": "v0.2.0-rc.1", "prerelease": true, "body": "rc"},
            {"tag_name": "v0.1.1", "body": "fix\r\n\r\n- one"},
            {"tag_name": "v0.3.0", "draft": true},
            {"tag_name": "nightly"}
        ]);
        assert_eq!(pick_release(&rels, false).unwrap()["tag"], "v0.1.1");
        assert_eq!(pick_release(&rels, false).unwrap()["notes"], "fix\n\n- one");
        assert_eq!(pick_release(&rels, true).unwrap()["tag"], "v0.2.0-rc.1");
        assert!(is_newer("0.1.1", "0.1.0") && !is_newer("0.1.0", "0.1.0"));
        assert!(is_newer("0.2.0", "0.2.0-rc.1") && !is_newer("0.2.0-rc.1", "0.2.0"));
        assert!(pick_release(&json!({"tag_name": "v1.0.0\"><script>"}), false).is_none());
    }

    #[test]
    fn a_source_build_says_how_to_update() {
        let a = advice(
            &json!({"kind": "source", "service": false, "exe": "/x/target/debug/flelink"}),
            "v0.1.1",
        );
        assert_eq!(a["can_auto"], false);
        assert!(a["command"].as_str().unwrap().contains("cargo build"));
        let a = advice(
            &json!({"kind": "app", "service": false, "exe": "/Applications/FLE Link.app/x"}),
            "v0.1.1",
        );
        assert!(a["reason"]
            .as_str()
            .unwrap()
            .contains("releases/tag/v0.1.1"));
    }

    #[test]
    fn the_python_studios_update_answer_is_not_ours() {
        // a data folder FLE Link took over from FLE Studio: its update-check.json names FLE Studio 0.4.0
        let d = std::env::temp_dir().join(format!("fle-upd-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("update-check.json"),
            r#"{"v": 1, "checked": 1, "app": {"version": "0.4.0", "tag": "v0.4.0", "url": "https://github.com/sirfyyn/fle-studio/releases/tag/v0.4.0"}}"#,
        )
        .unwrap();
        let u = Updates::new(&d, "http://127.0.0.1:9");
        assert_eq!(*u.state.lock().unwrap(), json!({}));
        let _ = std::fs::remove_dir_all(&d);
    }
}
