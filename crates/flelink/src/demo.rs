//! `flelink demo …`: steps for a Studio that runs with `--demo` (server demo.rs), and a screen recording that FLE Link
//! starts and stops itself, for videos of the real Studio (3 Nov 2026).
//!
//!   flelink demo chat "text" [--max-tokens N] [--temperature T] [--wait]   type it into the chat and send it
//!   flelink demo page /engine | preview | new | scroll N | wait | status
//!   flelink demo record start FILE [--window] | record stop
//!
//! The recording uses the system's own recorder: wf-recorder on Wayland (with --window only the focused window, via
//! hyprctl on Hyprland), ffmpeg x11grab on X11, screencapture on macOS. Nothing is uploaded; the file stays where you say.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn studio() -> Result<(String, String)> {
    let p = flelink_core::data_dir().join("studio.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).with_context(|| {
        format!(
            "no Studio is running here ({} is missing); start one with `flelink studio --demo`",
            p.display()
        )
    })?)?;
    Ok((
        v["url"]
            .as_str()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string(),
        v["token"].as_str().unwrap_or("").to_string(),
    ))
}

fn call(method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let (base, token) = studio()?;
    let c = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .danger_accept_invalid_certs(base.starts_with("https://127.0.0.1"))
        .build()?;
    let mut r = c
        .request(method.parse()?, format!("{base}{path}"))
        .header("X-FLE-Token", token);
    if let Some(b) = body {
        r = r.json(&b);
    }
    let r = r.send()?;
    let code = r.status();
    let v: Value = r.json().unwrap_or(Value::Null);
    if !code.is_success() {
        bail!(
            "{}",
            v["error"]["message"].as_str().unwrap_or(code.as_str())
        );
    }
    Ok(v)
}

pub fn step(step: Value) -> Result<()> {
    let st = call("GET", "/api/demo", None)?;
    if st["on"] != true {
        bail!("the Studio runs without demo mode: start it with `flelink studio --demo` (the app: FLE_DEMO=1)");
    }
    call("POST", "/api/demo", Some(step))?;
    Ok(())
}

/// Until the page has taken every step and no answer is being written any more (at most `max`).
pub fn wait(max: Duration) -> Result<()> {
    let t = Instant::now();
    let mut ruhig = 0;
    std::thread::sleep(Duration::from_secs(2));
    while t.elapsed() < max {
        let s = call("GET", "/api/demo", None)?;
        if s["page_seen_s"].as_f64().map_or(true, |x| x > 10.0) {
            bail!("no Studio page has asked for steps in the last 10 s (is the app or the page open?)");
        }
        if s["busy"] == false && s["queue"] == 0 {
            ruhig += 1;
            if ruhig >= 3 {
                return Ok(());
            }
        } else {
            ruhig = 0;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!("still busy after {} s", max.as_secs())
}

pub fn status() -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&call("GET", "/api/demo", None)?)?
    );
    Ok(())
}

// ── recording ───────────────────────────────────────────────────────────────────────────────────────────────────────

fn pid_file() -> PathBuf {
    flelink_core::data_dir().join("demo-recording.json")
}

fn focused_window() -> Option<String> {
    // Hyprland: the focused window as wf-recorder's geometry "x,y wxh"
    let out = std::process::Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
        .ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let (at, size) = (v["at"].as_array()?, v["size"].as_array()?);
    Some(format!("{},{} {}x{}", at[0], at[1], size[0], size[1]))
}

pub fn record_start(file: &Path, window: bool) -> Result<()> {
    if pid_file().exists() {
        bail!("a recording is already running (flelink demo record stop)");
    }
    let file = std::path::absolute(file)?;
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("screencapture");
        c.args(["-v", "-C"]).arg(&file);
        c
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let mut c = std::process::Command::new("wf-recorder");
        if window {
            if let Some(g) = focused_window() {
                c.args(["-g", &g]);
            }
        }
        c.args(["-y", "-f"]).arg(&file);
        c
    } else if let Some(d) = std::env::var_os("DISPLAY") {
        let mut c = std::process::Command::new("ffmpeg");
        c.args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "x11grab",
            "-framerate",
            "30",
            "-i",
        ])
        .arg(d);
        c.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20"])
            .arg(&file);
        c
    } else {
        bail!("no screen to record (neither Wayland nor X11 nor macOS)");
    };
    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context(
            "the screen recorder did not start (wf-recorder, ffmpeg or screencapture installed?)",
        )?;
    std::fs::write(
        pid_file(),
        json!({"pid": child.id(), "file": file}).to_string(),
    )?;
    println!(
        "Recording to {} (stop: flelink demo record stop)",
        file.display()
    );
    Ok(())
}

pub fn record_stop() -> Result<()> {
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(pid_file()).context("no recording is running")?,
    )?;
    let pid = v["pid"].as_u64().context("pid")? as i32;
    #[cfg(unix)]
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(15) && Path::new(&format!("/proc/{pid}")).exists() {
        std::thread::sleep(Duration::from_millis(200));
    }
    std::fs::remove_file(pid_file()).ok();
    println!("Recording saved: {}", v["file"].as_str().unwrap_or(""));
    Ok(())
}
