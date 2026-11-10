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

#[derive(Clone, Copy, PartialEq, Debug)]
enum Sys {
    Mac,
    X11,
    Wayland,
}

fn sys() -> Result<Sys, String> {
    if cfg!(target_os = "macos") {
        return Ok(Sys::Mac);
    }
    if cfg!(windows) {
        return Err("Computer use is not available on Windows yet.".into());
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
    }
}

type Argv = Vec<String>;

fn argv(words: &[&str]) -> Argv {
    words.iter().map(|w| w.to_string()).collect()
}

fn run(a: &Argv) -> Result<Vec<u8>, String> {
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
    }
}

fn type_command(s: Sys, text: &str) -> Argv {
    match s {
        Sys::Mac => mac(MAC_TYPE, &[text.to_string()]),
        Sys::X11 => argv(&["xdotool", "type", "--clearmodifiers", "--", text]),
        Sys::Wayland => argv(&["wtype", "--", text]),
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
    }
}

fn move_command(s: Sys, x: i64, y: i64) -> Argv {
    let (xs, ys) = (x.to_string(), y.to_string());
    match s {
        Sys::Mac => mac(MAC_MOVE, &[xs, ys]),
        Sys::X11 => argv(&["xdotool", "mousemove", "--sync", &xs, &ys]),
        Sys::Wayland => argv(&["ydotool", "mousemove", "--absolute", "-x", &xs, "-y", &ys]),
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
    }
}

fn screenshot(s: Sys, scale: f64) -> Result<Vec<u8>, String> {
    match s {
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
                run_all(click_commands(s, x, y, button(name), double)).map_err(failed)?;
                Ok(json!({}))
            }
            ("POST", "/desktop/tippen") => {
                let text = b.get("text").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() || text.chars().count() > MAX_TEXT {
                    return Err(bad("text: 1 to 500 characters"));
                }
                run(&type_command(s, text)).map_err(failed)?;
                Ok(json!({"zeichen": text.chars().count()}))
            }
            ("POST", "/desktop/taste") => {
                let kombi = b.get("kombi").and_then(Value::as_str).unwrap_or("");
                let (mods, key) = parse_keys(kombi).map_err(|e| (400, e))?;
                run(&key_command(s, &mods, key)).map_err(failed)?;
                Ok(json!({}))
            }
            ("POST", "/desktop/scrollen") => {
                let step = |k: &str| num(b, k).unwrap_or(0.0).clamp(-20.0, 20.0) as i64;
                if let Some((x, y)) = point(b) {
                    run(&move_command(s, x, y)).map_err(failed)?;
                }
                run_all(scroll_commands(s, step("dx"), step("dy"))).map_err(failed)?;
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
