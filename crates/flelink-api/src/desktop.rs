//! Computer use: the agent sees this computer's screen and works its mouse and keys, but only while the person at the
//! Agent page allows it, and only for the minutes they chose. Off at every start of FLE Link, and no setting keeps it on.
//! The agent calls /desktop/… on the loopback with a token of its own; it has no way to turn this on (server.rs).
use base64::Engine;
use serde_json::{json, Map, Value};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MAX_MINUTES: u64 = 240;
const MAX_TEXT: usize = 500;
const LOCKED: &str = "Computer use is off. Only the person at this computer can allow it, for a limited time, on the Agent page of FLE Link.";
const NO_OWN_DESKTOP: &str =
    "This computer has no separate desktop for you. Use ziel mensch, once computer use is allowed.";

const MAC_WIDTH: &str = "ObjC.import('AppKit');$.NSScreen.mainScreen.frame.size.width";
const MAC_CLICK: &str = "ObjC.import('CoreGraphics');function run(a){const x=+a[1],y=+a[2],b=+a[3],n=+a[4],p=$.CGPointMake(x,y);const t=b==0?[$.kCGEventLeftMouseDown,$.kCGEventLeftMouseUp]:b==1?[$.kCGEventRightMouseDown,$.kCGEventRightMouseUp]:[$.kCGEventOtherMouseDown,$.kCGEventOtherMouseUp];for(let i=1;i<=n;i++){for(const k of t){const e=$.CGEventCreateMouseEvent(null,k,p,b);$.CGEventSetIntegerValueField(e,$.kCGMouseEventClickState,i);$.CGEventPost($.kCGHIDEventTap,e);}delay(0.03);}}";
const MAC_MOVE: &str = "ObjC.import('CoreGraphics');function run(a){$.CGEventPost($.kCGHIDEventTap,$.CGEventCreateMouseEvent(null,$.kCGEventMouseMoved,$.CGPointMake(+a[1],+a[2]),0));}";
const MAC_SCROLL: &str = "ObjC.import('CoreGraphics');function run(a){$.CGEventPost($.kCGHIDEventTap,$.CGEventCreateScrollWheelEvent(null,1,2,-a[1],-a[2]));}";
const MAC_TYPE: &str = "function run(a){Application('System Events').keystroke(a[1]);}";
const MAC_KEY: &str = "function run(a){const s=Application('System Events'),k=a[1],m=a.slice(2).map(x=>x+' down'),o=m.length?{using:m}:{},c={Return:36,Tab:48,space:49,BackSpace:51,Escape:53,Left:123,Right:124,Down:125,Up:126,Home:115,End:119,Prior:116,Next:121,Delete:117,F1:122,F2:120,F3:99,F4:118,F5:96,F6:97,F7:98,F8:100,F9:101,F10:109,F11:103,F12:111}[k];if(c!==undefined)s.keyCode(c,o);else s.keystroke(k,o);}";

#[cfg(windows)]
mod win; // desktop/win.rs: user32 and gdi32, no helper programs
#[cfg(not(windows))]
mod win {
    use super::Ev;
    const ONLY: &str = "Only on Windows.";
    pub fn send(_: &[Ev]) -> Result<(), String> {
        Err(ONLY.into())
    }
    pub fn move_to(_: i64, _: i64) -> Result<(), String> {
        Err(ONLY.into())
    }
    pub fn screenshot(_: f64) -> Result<Vec<u8>, String> {
        Err(ONLY.into())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Sys {
    Mac,
    X11,
    Wayland,
    Windows,
}

fn sys() -> Result<Sys, String> {
    if cfg!(target_os = "macos") {
        return Ok(Sys::Mac);
    }
    if cfg!(windows) {
        return Ok(Sys::Windows);
    }
    let set = |n: &str| std::env::var(n).is_ok_and(|v| !v.is_empty());
    if set("WAYLAND_DISPLAY") {
        Ok(Sys::Wayland)
    } else if set("DISPLAY") {
        Ok(Sys::X11)
    } else {
        Err("FLE Link does not run inside a graphical session here.".into())
    }
}

fn needs(s: Sys) -> &'static [&'static str] {
    match s {
        Sys::Mac => &["screencapture", "sips", "osascript"],
        Sys::X11 => &["xdotool", "import"],
        Sys::Wayland => &["grim", "wtype", "ydotool"],
        Sys::Windows => &[],
    }
}

/// One input event for Windows' SendInput, worked out here so it is tested on every system (desktop/win.rs sends it).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ev {
    /// a virtual key; `ext` for the keys of the extra block (arrows, Home, Delete …)
    Key {
        vk: u16,
        up: bool,
        ext: bool,
    },
    /// one UTF-16 unit of text, whatever the keyboard layout
    Unicode {
        unit: u16,
        up: bool,
    },
    Mouse {
        flags: u32,
        data: i32,
    },
}

const WHEEL_STEP: i32 = 120;
const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;

/// The names the tool takes (as on X11), on Windows' virtual keys.
fn win_key(key: &str) -> Option<(u16, bool)> {
    let named = match key {
        "Return" => (0x0D, false),
        "Tab" => (0x09, false),
        "space" => (0x20, false),
        "BackSpace" => (0x08, false),
        "Escape" => (0x1B, false),
        "Left" => (0x25, true),
        "Up" => (0x26, true),
        "Right" => (0x27, true),
        "Down" => (0x28, true),
        "Home" => (0x24, true),
        "End" => (0x23, true),
        "Prior" => (0x21, true),
        "Next" => (0x22, true),
        "Insert" => (0x2D, true),
        "Delete" => (0x2E, true),
        _ => {
            if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
                return (1..=12).contains(&n).then_some((0x6F + n, false));
            }
            let mut c = key.chars();
            return match (c.next(), c.next()) {
                (Some(ch), None) if ch.is_ascii_alphanumeric() => {
                    Some((ch.to_ascii_uppercase() as u16, false))
                }
                _ => None,
            };
        }
    };
    Some(named)
}

fn win_key_events(mods: &[&str], key: &str) -> Result<Vec<Ev>, String> {
    let (vk, ext) = win_key(key).ok_or_else(|| format!("{key}: not a key FLE Link knows"))?;
    let mods: Vec<u16> = mods
        .iter()
        .map(|m| if *m == "ctrl" { VK_CONTROL } else { VK_SHIFT })
        .collect();
    let mut e: Vec<Ev> = mods
        .iter()
        .map(|&vk| Ev::Key {
            vk,
            up: false,
            ext: false,
        })
        .collect();
    e.push(Ev::Key { vk, up: false, ext });
    e.push(Ev::Key { vk, up: true, ext });
    e.extend(mods.iter().rev().map(|&vk| Ev::Key {
        vk,
        up: true,
        ext: false,
    }));
    Ok(e)
}

/// Text as typed characters; a line break is the Enter key and a tab the Tab key, which programs expect rather than the
/// characters themselves.
fn win_text_events(text: &str) -> Vec<Ev> {
    let mut e = vec![];
    for ch in text.chars() {
        let key = match ch {
            '\r' => continue,
            '\n' => Some(0x0D),
            '\t' => Some(0x09),
            _ => None,
        };
        if let Some(vk) = key {
            e.push(Ev::Key {
                vk,
                up: false,
                ext: false,
            });
            e.push(Ev::Key {
                vk,
                up: true,
                ext: false,
            });
            continue;
        }
        let mut buf = [0u16; 2];
        for &unit in ch.encode_utf16(&mut buf).iter() {
            e.push(Ev::Unicode { unit, up: false });
            e.push(Ev::Unicode { unit, up: true });
        }
    }
    e
}

fn win_click_events(b: usize, double: bool) -> Vec<Ev> {
    // MOUSEEVENTF_LEFTDOWN/UP, RIGHTDOWN/UP, MIDDLEDOWN/UP
    let (down, up) = [(0x0002, 0x0004), (0x0008, 0x0010), (0x0020, 0x0040)][b];
    let one = [
        Ev::Mouse {
            flags: down,
            data: 0,
        },
        Ev::Mouse { flags: up, data: 0 },
    ];
    one.iter()
        .cycle()
        .take(if double { 4 } else { 2 })
        .copied()
        .collect()
}

/// `dy` > 0 scrolls down and `dx` > 0 to the right, as on the other systems; Windows counts the wheel the other way round.
fn win_scroll_events(dx: i64, dy: i64) -> Vec<Ev> {
    let mut e = vec![];
    if dy != 0 {
        e.push(Ev::Mouse {
            flags: 0x0800,
            data: -(dy as i32) * WHEEL_STEP,
        });
    }
    if dx != 0 {
        e.push(Ev::Mouse {
            flags: 0x1000,
            data: dx as i32 * WHEEL_STEP,
        });
    }
    e
}

/// A PNG of 8-bit RGB from 4 bytes per pixel (`bgra`: in the order Windows hands them out, else RGBA). No crate for it: the
/// format is a header, the zlib stream of the rows and an end, and flate2 is there anyway.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn png_rgb(w: u32, h: u32, pixels: &[u8], bgra: bool) -> Vec<u8> {
    use std::io::Write;
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        out.extend(kind);
        out.extend(data);
        out.extend(crc.sum().to_be_bytes());
    }
    let mut raw = Vec::with_capacity((w as usize * 3 + 1) * h as usize);
    for row in pixels.chunks_exact(w as usize * 4).take(h as usize) {
        raw.push(0); // no filter
        for p in row.as_chunks::<4>().0 {
            if bgra {
                raw.extend([p[2], p[1], p[0]]);
            } else {
                raw.extend([p[0], p[1], p[2]]);
            }
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = z.write_all(&raw);
    let idat = z.finish().unwrap_or_default();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = vec![];
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]); // 8 bits, RGB, deflate, no filter method, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

type Argv = Vec<String>;

fn argv(words: &[&str]) -> Argv {
    words.iter().map(|w| w.to_string()).collect()
}

fn run(a: &Argv) -> Result<Vec<u8>, String> {
    if a.is_empty() {
        return Err("No program for this here.".into());
    }
    let out = Command::new(&a[0])
        .args(&a[1..])
        .output()
        .map_err(|e| format!("{} does not start: {e}", a[0]))?;
    if !out.status.success() {
        return Err(format!(
            "{}: {}",
            a[0],
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn mac(script: &str, args: &[String]) -> Argv {
    // the first argument is a fixed word, so that text starting with a dash is never read as an option
    let mut a = argv(&["osascript", "-l", "JavaScript", "-e", script, "run"]);
    a.extend_from_slice(args);
    a
}

fn button(name: Option<&str>) -> usize {
    match name {
        Some("rechts") => 1,
        Some("mitte") => 2,
        _ => 0,
    }
}

fn click_commands(s: Sys, x: i64, y: i64, b: usize, double: bool) -> Vec<Argv> {
    let (xs, ys, n) = (x.to_string(), y.to_string(), if double { 2 } else { 1 });
    match s {
        Sys::Mac => vec![mac(MAC_CLICK, &[xs, ys, b.to_string(), n.to_string()])],
        Sys::X11 => {
            let code = ["1", "3", "2"][b];
            let mut a = argv(&["xdotool", "mousemove", "--sync", &xs, &ys, "click"]);
            if double {
                a.extend(argv(&["--repeat", "2"]));
            }
            a.push(code.into());
            vec![a]
        }
        Sys::Wayland => {
            let code = ["0xC0", "0xC1", "0xC2"][b];
            let mut a = argv(&["ydotool", "click"]);
            if double {
                a.extend(argv(&["--repeat", "2"]));
            }
            a.push(code.into());
            vec![
                argv(&["ydotool", "mousemove", "--absolute", "-x", &xs, "-y", &ys]),
                a,
            ]
        }
        Sys::Windows => vec![], // no program: Ev and desktop/win.rs
    }
}

fn type_command(s: Sys, text: &str) -> Argv {
    match s {
        Sys::Mac => mac(MAC_TYPE, &[text.to_string()]),
        Sys::X11 => argv(&["xdotool", "type", "--clearmodifiers", "--", text]),
        Sys::Wayland => argv(&["wtype", "--", text]),
        Sys::Windows => vec![],
    }
}

/// `ctrl+l`, `shift+Tab`, `Return`: only ctrl and shift, as the tool promises; no alt, no super.
fn parse_keys(kombi: &str) -> Result<(Vec<&str>, &str), String> {
    let mut parts: Vec<&str> = kombi.split('+').collect();
    let key = parts.pop().unwrap_or("");
    let plain = !key.is_empty()
        && key.len() <= 12
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !plain || parts.iter().any(|m| !matches!(*m, "ctrl" | "shift")) {
        return Err(format!(
            "{kombi}: only a key, with ctrl and shift if needed"
        ));
    }
    Ok((parts, key))
}

fn key_command(s: Sys, mods: &[&str], key: &str) -> Argv {
    match s {
        Sys::Mac => {
            let mut a = vec![key.to_string()];
            // what a program calls ctrl+l is command+l on a Mac
            a.extend(
                mods.iter()
                    .map(|m| if *m == "ctrl" { "command" } else { "shift" }.to_string()),
            );
            mac(MAC_KEY, &a)
        }
        Sys::X11 => {
            let combo = mods
                .iter()
                .copied()
                .chain([key])
                .collect::<Vec<_>>()
                .join("+");
            argv(&["xdotool", "key", "--clearmodifiers", &combo])
        }
        Sys::Wayland => {
            let mut a = argv(&["wtype"]);
            for m in mods {
                a.extend(argv(&["-M", m]));
            }
            a.extend(argv(&["-k", key]));
            for m in mods.iter().rev() {
                a.extend(argv(&["-m", m]));
            }
            a
        }
        Sys::Windows => vec![],
    }
}

fn move_command(s: Sys, x: i64, y: i64) -> Argv {
    let (xs, ys) = (x.to_string(), y.to_string());
    match s {
        Sys::Mac => mac(MAC_MOVE, &[xs, ys]),
        Sys::X11 => argv(&["xdotool", "mousemove", "--sync", &xs, &ys]),
        Sys::Wayland => argv(&["ydotool", "mousemove", "--absolute", "-x", &xs, "-y", &ys]),
        Sys::Windows => vec![],
    }
}

fn scroll_commands(s: Sys, dx: i64, dy: i64) -> Vec<Argv> {
    match s {
        Sys::Mac => vec![mac(MAC_SCROLL, &[dy.to_string(), dx.to_string()])],
        Sys::Wayland => vec![argv(&[
            "ydotool",
            "mousemove",
            "--wheel",
            "-x",
            &dx.to_string(),
            "-y",
            &(-dy).to_string(),
        ])],
        Sys::X11 => [(dy, "4", "5"), (dx, "6", "7")]
            .iter()
            .filter(|(d, _, _)| *d != 0)
            .map(|(d, back, forth)| {
                let code = if *d < 0 { back } else { forth };
                argv(&["xdotool", "click", "--repeat", &d.abs().to_string(), code])
            })
            .collect(),
        Sys::Windows => vec![],
    }
}

// What the agent asks for, on each system: a program per step, or Windows' own input.
fn do_click(s: Sys, x: i64, y: i64, b: usize, double: bool) -> Result<(), String> {
    if s == Sys::Windows {
        win::move_to(x, y)?;
        return win::send(&win_click_events(b, double));
    }
    run_all(click_commands(s, x, y, b, double))
}

fn do_type(s: Sys, text: &str) -> Result<(), String> {
    if s == Sys::Windows {
        return win::send(&win_text_events(text));
    }
    run(&type_command(s, text)).map(|_| ())
}

fn do_key(s: Sys, mods: &[&str], key: &str) -> Result<(), String> {
    if s == Sys::Windows {
        return win::send(&win_key_events(mods, key)?);
    }
    run(&key_command(s, mods, key)).map(|_| ())
}

fn do_move(s: Sys, x: i64, y: i64) -> Result<(), String> {
    if s == Sys::Windows {
        return win::move_to(x, y);
    }
    run(&move_command(s, x, y)).map(|_| ())
}

fn do_scroll(s: Sys, dx: i64, dy: i64) -> Result<(), String> {
    if s == Sys::Windows {
        return win::send(&win_scroll_events(dx, dy));
    }
    run_all(scroll_commands(s, dx, dy))
}

fn screenshot(s: Sys, scale: f64) -> Result<Vec<u8>, String> {
    match s {
        Sys::Windows => win::screenshot(scale),
        Sys::X11 => run(&argv(&[
            "import",
            "-window",
            "root",
            "-resize",
            &format!("{}%", (scale * 100.0).round()),
            "png:-",
        ])),
        Sys::Wayland => run(&argv(&["grim", "-s", &format!("{scale:.2}"), "-"])),
        Sys::Mac => {
            let f =
                std::env::temp_dir().join(format!("flelink-screen-{}.png", crate::agent::token()));
            let path = f.to_string_lossy().to_string();
            let result = (|| {
                // the picture has twice the pixels on a Retina screen, a click counts in points: the file is made to match
                let width = String::from_utf8_lossy(&run(&argv(&[
                    "osascript",
                    "-l",
                    "JavaScript",
                    "-e",
                    MAC_WIDTH,
                ]))?)
                .trim()
                .parse::<f64>()
                .map_err(|e| format!("screen width: {e}"))?;
                run(&argv(&["screencapture", "-x", "-t", "png", &path]))?;
                let w = (width * scale).round().to_string();
                run(&argv(&["sips", "--resampleWidth", &w, &path]))?;
                std::fs::read(&f).map_err(|e| e.to_string())
            })();
            let _ = std::fs::remove_file(&f);
            result
        }
    }
}

fn windows(s: Sys) -> Vec<Value> {
    if s != Sys::X11 || flelink_core::launch::which("wmctrl").is_none() {
        return vec![]; // a list of windows is a bonus: the picture shows them too
    }
    let Ok(out) = run(&argv(&["wmctrl", "-lx"])) else {
        return vec![];
    };
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (id, desk, class, _host) = (w.next()?, w.next()?, w.next()?, w.next()?);
            Some(json!({"adresse": id, "arbeitsflaeche": desk, "klasse": class, "titel": w.collect::<Vec<_>>().join(" ")}))
        })
        .collect()
}

fn num(b: &Map<String, Value>, key: &str) -> Option<f64> {
    b.get(key).and_then(Value::as_f64)
}

/// A point in the picture the agent got, on the real screen.
fn point(b: &Map<String, Value>) -> Option<(i64, i64)> {
    let scale = num(b, "skala").unwrap_or(1.0).clamp(0.1, 1.0);
    let at = |k: &str| num(b, k).map(|v| (v.clamp(0.0, 20000.0) / scale).round() as i64);
    Some((at("x")?, at("y")?))
}

fn run_all(c: Vec<Argv>) -> Result<(), String> {
    c.iter().try_for_each(|a| run(a).map(|_| ()))
}

/// `flelink desktop-probe`: the same calls the agent makes, from a terminal and without an agent, to try computer use on a
/// new system. A picture into `out`, the mouse to the middle of the screen, `text` typed where the focus is (the terminal).
/// Started by the person at the computer, so it is its own permission for these few calls.
pub fn probe(out: &std::path::Path, text: Option<&str>) -> Result<String, String> {
    let d = Desktop::new();
    d.allow(1);
    let ask = |method: &str, path: &str, q: Value, body: Value| {
        let q = q.as_object().cloned().unwrap_or_default();
        match d.answer(method, path, &q, body.to_string().as_bytes()) {
            (200, v) => Ok(v),
            (code, v) => Err(format!(
                "{path}: {code} {}",
                v["error"].as_str().unwrap_or("")
            )),
        }
    };
    let mut said = vec![format!("status: {}", d.status())];
    let pic = ask("GET", "/desktop/bild", json!({"skala": "1"}), json!({}))?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(pic["daten"].as_str().unwrap_or(""))
        .map_err(|e| e.to_string())?;
    // the PNG header holds width and height at bytes 16 to 24
    let size = |at: usize| {
        png.get(at..at + 4)
            .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (w, h) = (size(16), size(20));
    std::fs::write(out, &png).map_err(|e| format!("{}: {e}", out.display()))?;
    said.push(format!(
        "picture: {w} x {h}, {} bytes, in {}",
        png.len(),
        out.display()
    ));
    ask(
        "POST",
        "/desktop/scrollen",
        json!({}),
        json!({"x": w / 2, "y": h / 2, "dy": 0}),
    )?;
    said.push(format!("mouse: moved to {} {}", w / 2, h / 2));
    if let Some(t) = text.filter(|t| !t.is_empty()) {
        ask("POST", "/desktop/tippen", json!({}), json!({"text": t}))?;
        said.push(format!("typed: {} characters", t.chars().count()));
    }
    d.allow(0);
    Ok(said.join("\n"))
}

pub struct Desktop {
    token: String,
    until: Mutex<Option<Instant>>,
}

impl Default for Desktop {
    fn default() -> Self {
        Self::new()
    }
}

impl Desktop {
    pub fn new() -> Self {
        Self {
            token: crate::agent::token(),
            until: Mutex::new(None),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn seconds_left(&self) -> u64 {
        let until = *self.until.lock().unwrap();
        until.map_or(0, |u| u.saturating_duration_since(Instant::now()).as_secs())
    }

    /// Allow it for this many minutes; 0 ends it now.
    pub fn allow(&self, minutes: u64) {
        *self.until.lock().unwrap() = (minutes > 0)
            .then(|| Instant::now() + Duration::from_secs(minutes.min(MAX_MINUTES) * 60));
    }

    /// For the Agent page.
    pub fn status(&self) -> Value {
        let (problem, missing): (String, Vec<&str>) = match sys() {
            Err(p) => (p, vec![]),
            Ok(s) => (
                String::new(),
                needs(s)
                    .iter()
                    .copied()
                    .filter(|t| flelink_core::launch::which(t).is_none())
                    .collect(),
            ),
        };
        json!({
            "ready": problem.is_empty() && missing.is_empty(),
            "problem": problem, "missing": missing, "mac": cfg!(target_os = "macos"),
            "seconds_left": self.seconds_left(), "max_minutes": MAX_MINUTES,
        })
    }

    /// One call of the agent. Status and answer as the tools expect them: `{ok: true, …}`, 423 while it is not allowed.
    pub fn answer(
        &self,
        method: &str,
        path: &str,
        q: &Map<String, Value>,
        body: &[u8],
    ) -> (u16, Value) {
        let b: Map<String, Value> = serde_json::from_slice(body).unwrap_or_default();
        let fail = |code: u16, msg: &str| (code, json!({"ok": false, "error": msg}));
        let target = q
            .get("ziel")
            .or_else(|| b.get("ziel"))
            .and_then(Value::as_str);
        if target == Some("agent") {
            return fail(400, NO_OWN_DESKTOP);
        }
        let left = self.seconds_left();
        if (method, path) == ("GET", "/desktop/eingabe") {
            return (
                200,
                json!({"ok": true, "freigabe": left > 0, "rest_sekunden": left}),
            );
        }
        if left == 0 {
            return fail(423, LOCKED);
        }
        let s = match sys() {
            Ok(s) => s,
            Err(p) => return fail(503, &p),
        };
        match Self::act(s, method, path, q, &b) {
            Ok(mut v) => {
                v["ok"] = json!(true);
                (200, v)
            }
            Err((code, e)) => fail(code, &e),
        }
    }

    fn act(
        s: Sys,
        method: &str,
        path: &str,
        q: &Map<String, Value>,
        b: &Map<String, Value>,
    ) -> Result<Value, (u16, String)> {
        let failed = |e: String| (500u16, e);
        let bad = |e: &str| (400u16, e.to_string());
        match (method, path) {
            ("GET", "/desktop/bild") => {
                let scale = q
                    .get("skala")
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.6)
                    .clamp(0.1, 1.0);
                let png = screenshot(s, scale).map_err(failed)?;
                Ok(json!({
                    "typ": "image/png", "groesse": png.len(),
                    "daten": base64::engine::general_purpose::STANDARD.encode(png),
                }))
            }
            ("GET", "/desktop/fenster") => Ok(json!({"fenster": windows(s)})),
            ("GET", "/desktop/starter") => Ok(json!({"starter": []})),
            ("POST", "/desktop/klick") => {
                let (x, y) = point(b).ok_or_else(|| bad("x and y are needed"))?;
                let name = b.get("taste").and_then(Value::as_str);
                let double = b.get("doppelt").and_then(Value::as_bool).unwrap_or(false);
                do_click(s, x, y, button(name), double).map_err(failed)?;
                Ok(json!({}))
            }
            ("POST", "/desktop/tippen") => {
                let text = b.get("text").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() || text.chars().count() > MAX_TEXT {
                    return Err(bad("text: 1 to 500 characters"));
                }
                do_type(s, text).map_err(failed)?;
                Ok(json!({"zeichen": text.chars().count()}))
            }
            ("POST", "/desktop/taste") => {
                let kombi = b.get("kombi").and_then(Value::as_str).unwrap_or("");
                let (mods, key) = parse_keys(kombi).map_err(|e| (400, e))?;
                do_key(s, &mods, key).map_err(failed)?;
                Ok(json!({}))
            }
            ("POST", "/desktop/scrollen") => {
                let step = |k: &str| num(b, k).unwrap_or(0.0).clamp(-20.0, 20.0) as i64;
                if let Some((x, y)) = point(b) {
                    do_move(s, x, y).map_err(failed)?;
                }
                do_scroll(s, step("dx"), step("dy")).map_err(failed)?;
                Ok(json!({}))
            }
            ("POST", "/desktop/fokus") if s == Sys::X11 => {
                let adr = b.get("adresse").and_then(Value::as_str).unwrap_or("");
                if !adr.starts_with("0x") || !adr[2..].chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(bad("adresse: a window from desktop_fenster"));
                }
                run(&argv(&["wmctrl", "-ia", adr])).map_err(failed)?;
                Ok(json!({}))
            }
            ("POST", "/desktop/fokus") => Err(bad("Click the window to bring it to the front.")),
            ("POST", "/desktop/starten") => Err(bad("No programs are set up to start here.")),
            _ => Err((404, "No such call.".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn nothing_happens_until_a_person_allows_it() {
        let d = Desktop::new();
        for (m, p) in [
            ("GET", "/desktop/bild"),
            ("POST", "/desktop/klick"),
            ("POST", "/desktop/tippen"),
        ] {
            let (code, v) = d.answer(m, p, &Map::new(), br#"{"x":1,"y":1,"text":"a"}"#);
            assert_eq!(code, 423, "{p}: {v}");
        }
        let (code, v) = d.answer("GET", "/desktop/eingabe", &Map::new(), b"");
        assert_eq!((code, v["freigabe"].as_bool()), (200, Some(false)));
    }

    #[test]
    fn allowing_is_limited_and_can_be_ended() {
        let d = Desktop::new();
        d.allow(100_000);
        assert!(d.seconds_left() <= MAX_MINUTES * 60 && d.seconds_left() > 0);
        d.allow(0);
        assert_eq!(d.seconds_left(), 0);
    }

    #[test]
    fn the_agent_is_told_it_has_no_desktop_of_its_own() {
        let d = Desktop::new();
        d.allow(1);
        let q = map(json!({"ziel": "agent"}));
        let (code, v) = d.answer("GET", "/desktop/bild", &q, b"");
        assert_eq!(code, 400, "{v}");
    }

    #[test]
    fn a_point_in_the_picture_is_a_point_on_the_screen() {
        assert_eq!(
            point(&map(json!({"x": 300, "y": 150, "skala": 0.5}))),
            Some((600, 300))
        );
        assert_eq!(point(&map(json!({"dy": 3}))), None);
    }

    #[test]
    fn only_ctrl_and_shift_count_as_modifiers() {
        assert_eq!(parse_keys("ctrl+l").unwrap(), (vec!["ctrl"], "l"));
        assert_eq!(parse_keys("shift+Tab").unwrap(), (vec!["shift"], "Tab"));
        assert_eq!(parse_keys("Return").unwrap(), (vec![], "Return"));
        for bad in ["alt+F4", "super+l", "ctrl+", "ctrl+a b", ""] {
            assert!(parse_keys(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn text_is_never_read_as_an_option() {
        assert_eq!(
            type_command(Sys::X11, "--help")[3..],
            ["--".to_string(), "--help".to_string()]
        );
        assert_eq!(
            type_command(Sys::Wayland, "-x")[1..],
            ["--".to_string(), "-x".to_string()]
        );
        let m = type_command(Sys::Mac, "-e");
        assert_eq!(m[m.len() - 2..], ["run".to_string(), "-e".to_string()]);
    }

    #[test]
    fn windows_gets_its_own_input_events() {
        use Ev::*;
        let k = |vk, up, ext| Key { vk, up, ext };
        assert_eq!(
            win_key_events(&["ctrl", "shift"], "t").unwrap(),
            [
                k(VK_CONTROL, false, false),
                k(VK_SHIFT, false, false),
                k(0x54, false, false),
                k(0x54, true, false),
                k(VK_SHIFT, true, false),
                k(VK_CONTROL, true, false)
            ]
        );
        assert_eq!(
            win_key_events(&[], "Left").unwrap()[0],
            k(0x25, false, true)
        );
        assert_eq!(
            win_key_events(&[], "F12").unwrap()[0],
            k(0x7B, false, false)
        );
        assert_eq!(win_key_events(&[], "7").unwrap()[0], k(0x37, false, false));
        for bad in ["F13", "F0", "Hyper", "ab"] {
            assert!(win_key_events(&[], bad).is_err(), "{bad}");
        }
        // every key name the other systems take, Windows takes too
        for name in [
            "Return",
            "Tab",
            "space",
            "BackSpace",
            "Escape",
            "Left",
            "Right",
            "Down",
            "Up",
            "Home",
            "End",
            "Prior",
            "Next",
            "Delete",
            "F1",
            "F5",
        ] {
            assert!(win_key(name).is_some(), "{name}");
        }
        // text: a character outside the basic plane is two units, a line break the Enter key
        let t = win_text_events("a😀\r\n");
        assert_eq!(t.len(), 2 + 4 + 2);
        assert_eq!(
            t[0],
            Unicode {
                unit: 'a' as u16,
                up: false
            }
        );
        assert_eq!(
            t[2],
            Unicode {
                unit: 0xD83D,
                up: false
            }
        );
        assert_eq!(t[6], k(0x0D, false, false));
        assert_eq!(win_click_events(1, true).len(), 4);
        assert_eq!(
            win_click_events(1, false)[0],
            Mouse {
                flags: 0x0008,
                data: 0
            }
        );
        assert_eq!(
            win_scroll_events(2, 3),
            [
                Mouse {
                    flags: 0x0800,
                    data: -360
                },
                Mouse {
                    flags: 0x1000,
                    data: 240
                }
            ]
        );
        assert!(win_scroll_events(0, 0).is_empty());
        assert!(needs(Sys::Windows).is_empty());
    }

    #[test]
    fn the_picture_is_a_valid_png() {
        use std::io::Read;
        // 2 x 2, blue green red alpha as Windows gives them
        let px = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
        let png = png_rgb(2, 2, &px, true);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // the end chunk always has this checksum
        assert_eq!(&png[png.len() - 8..], b"IEND\xae\x42\x60\x82");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 2);
        let len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let mut raw = vec![];
        flate2::read::ZlibDecoder::new(&png[41..41 + len])
            .read_to_end(&mut raw)
            .unwrap();
        assert_eq!(raw, [0, 3, 2, 1, 6, 5, 4, 0, 9, 8, 7, 12, 11, 10]);
    }

    #[test]
    fn the_commands_for_each_system() {
        assert_eq!(
            click_commands(Sys::X11, 10, 20, 1, true)[0].join(" "),
            "xdotool mousemove --sync 10 20 click --repeat 2 3"
        );
        assert_eq!(
            click_commands(Sys::Wayland, 10, 20, 0, false)
                .iter()
                .map(|a| a.join(" "))
                .collect::<Vec<_>>(),
            [
                "ydotool mousemove --absolute -x 10 -y 20",
                "ydotool click 0xC0"
            ]
        );
        assert_eq!(
            key_command(Sys::Wayland, &["ctrl", "shift"], "t").join(" "),
            "wtype -M ctrl -M shift -k t -m shift -m ctrl"
        );
        assert_eq!(
            key_command(Sys::X11, &["ctrl"], "l").join(" "),
            "xdotool key --clearmodifiers ctrl+l"
        );
        assert_eq!(
            scroll_commands(Sys::X11, 0, -3)[0].join(" "),
            "xdotool click --repeat 3 4"
        );
    }
}
