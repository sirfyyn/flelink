//! FLE Link as a service of this user: the Studio starts at login and comes back when it stops, so the model and the agent
//! are always there. Nothing here needs administrator rights.
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "run.flelink.studio";
const SYSTEMD_NAME: &str = "flelink.service";
const WINDOWS_TASK: &str = "FLE Link";

/// The `flelink` program to run: this one, or the one on the PATH when this is the desktop app.
pub fn program() -> Result<PathBuf> {
    let me = std::env::current_exe().context("cannot tell which program this is")?;
    let stem = me
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if stem == "flelink" || stem == "fle" {
        return Ok(me);
    }
    crate::launch::which("flelink")
        .context("the flelink program was not found on the PATH; install it first (flelink.run)")
}

fn args(home: Option<&Path>) -> Vec<String> {
    let mut a = vec!["studio".to_string(), "--no-browser".to_string()];
    if let Some(h) = home {
        a.push("--home".to_string());
        a.push(h.to_string_lossy().to_string());
    }
    a
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn launchd_plist(program: &Path, home: Option<&Path>, log: &Path) -> String {
    let mut words = vec![program.to_string_lossy().to_string()];
    words.extend(args(home));
    let list: String = words
        .iter()
        .map(|w| format!("    <string>{}</string>\n", xml(w)))
        .collect();
    let log = xml(&log.to_string_lossy());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{LABEL}</string>\n  <key>ProgramArguments</key>\n  <array>\n{list}  </array>\n  <key>RunAtLoad</key>\n  <true/>\n  <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n  <key>StandardOutPath</key>\n  <string>{log}</string>\n  <key>StandardErrorPath</key>\n  <string>{log}</string>\n</dict>\n</plist>\n"
    )
}

fn systemd_unit(program: &Path, home: Option<&Path>) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut words = vec![quote(&program.to_string_lossy())];
    words.extend(args(home).iter().map(|a| quote(a)));
    format!(
        "[Unit]\nDescription=FLE Link\n\n[Service]\nExecStart={}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        words.join(" ")
    )
}

/// A unit somebody wrote by hand under our name is theirs: FLE Link neither replaces nor removes it.
fn ours(unit: &str) -> bool {
    unit.lines().any(|l| l == "Description=FLE Link")
}

fn ensure_ours(file: &Path) -> Result<()> {
    match std::fs::read_to_string(file) {
        Ok(t) if !ours(&t) => bail!(
            "{} was not written by FLE Link and stays as it is",
            file.display()
        ),
        _ => Ok(()),
    }
}

fn launchd_file() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home folder")?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn systemd_file() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("no config folder")?
        .join("systemd/user")
        .join(SYSTEMD_NAME))
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not run {program}"))?;
    if !out.status.success() {
        bail!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn launchd_domain() -> String {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no arguments and cannot fail
        format!("gui/{}", unsafe { libc::getuid() })
    }
    #[cfg(not(unix))]
    {
        String::new()
    }
}

/// Start the Studio at every login from now on; `now`: also at once (not while a Studio runs already, a second one would start).
pub fn install(program: &Path, home: Option<&Path>, log: &Path, now: bool) -> Result<()> {
    if cfg!(target_os = "macos") {
        let file = launchd_file()?;
        std::fs::create_dir_all(file.parent().context("no folder")?)?;
        std::fs::write(&file, launchd_plist(program, home, log))?;
        if !now {
            return Ok(());
        }
        let domain = launchd_domain();
        let f = file.to_string_lossy().to_string();
        let _ = run("launchctl", &["bootout", &domain, &f]);
        run("launchctl", &["bootstrap", &domain, &f])
    } else if cfg!(windows) {
        let mut line = format!("\"{}\"", program.display());
        for a in args(home) {
            line.push_str(&format!(" \"{a}\""));
        }
        run(
            "schtasks",
            &[
                "/Create",
                "/F",
                "/SC",
                "ONLOGON",
                "/TN",
                WINDOWS_TASK,
                "/TR",
                &line,
            ],
        )
    } else {
        let file = systemd_file()?;
        ensure_ours(&file)?;
        std::fs::create_dir_all(file.parent().context("no folder")?)?;
        std::fs::write(&file, systemd_unit(program, home))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        if now {
            run("systemctl", &["--user", "enable", "--now", SYSTEMD_NAME])
        } else {
            run("systemctl", &["--user", "enable", SYSTEMD_NAME])
        }
    }
}

/// Stop starting at login, and stop the service.
pub fn remove() -> Result<()> {
    if cfg!(target_os = "macos") {
        let file = launchd_file()?;
        let _ = run(
            "launchctl",
            &["bootout", &launchd_domain(), &file.to_string_lossy()],
        );
        if file.exists() {
            std::fs::remove_file(file)?;
        }
        Ok(())
    } else if cfg!(windows) {
        run("schtasks", &["/Delete", "/F", "/TN", WINDOWS_TASK])
    } else {
        let file = systemd_file()?;
        ensure_ours(&file)?;
        let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_NAME]);
        if file.exists() {
            std::fs::remove_file(file)?;
        }
        run("systemctl", &["--user", "daemon-reload"])
    }
}

pub fn installed() -> bool {
    if cfg!(target_os = "macos") {
        launchd_file().is_ok_and(|f| f.exists())
    } else if cfg!(windows) {
        run("schtasks", &["/Query", "/TN", WINDOWS_TASK]).is_ok()
    } else {
        systemd_file()
            .ok()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .is_some_and(|t| ours(&t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launchd_file_starts_the_studio_and_keeps_it_alive() {
        let t = launchd_plist(Path::new("/a b/flelink"), None, Path::new("/l/s&.log"));
        assert!(t.contains("<string>/a b/flelink</string>"), "{t}");
        assert!(t.contains("<string>studio</string>"), "{t}");
        assert!(t.contains("<string>--no-browser</string>"), "{t}");
        // a clean end ("one runs already") is not restarted, a crash is
        assert!(t.contains("<key>SuccessfulExit</key>\n    <false/>"), "{t}");
        assert!(t.contains("/l/s&amp;.log"), "{t}");
    }

    #[test]
    fn the_systemd_unit_quotes_paths_and_names_the_data_folder() {
        let t = systemd_unit(Path::new("/a b/flelink"), Some(Path::new("/d/x")));
        assert!(
            t.contains(
                "ExecStart=\"/a b/flelink\" \"studio\" \"--no-browser\" \"--home\" \"/d/x\""
            ),
            "{t}"
        );
        assert!(t.contains("WantedBy=default.target"), "{t}");
        assert!(ours(&t), "{t}");
    }

    #[test]
    fn a_unit_written_by_hand_stays() {
        let dir = std::env::temp_dir().join(format!("flelink-service-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("flelink.service");
        std::fs::write(&f, "[Unit]\nDescription=FLE Link (next to fle-studio)\n").unwrap();
        assert!(ensure_ours(&f).is_err());
        std::fs::write(&f, systemd_unit(Path::new("/x/flelink"), None)).unwrap();
        assert!(ensure_ours(&f).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
