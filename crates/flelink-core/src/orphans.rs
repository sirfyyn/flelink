//! Engines whose program died without stopping them (`kill -9`, a crash, a closed laptop lid at the wrong moment).
//! Each running engine has a file `<data>/run/engine-<engine pid>` that holds the pid of the program that started it.
//! Before an engine starts, files whose program is gone are cleared up and their engine, if it still runs, is stopped:
//! without that a second start loads the model a second time next to the first. (PR_SET_PDEATHSIG does not fit here: it
//! fires when the starting *thread* ends, and the TUI starts the engine from a worker thread. On Windows the app's job
//! object ends the engine with the app.)
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn dir(home: &Path) -> PathBuf {
    home.join("run")
}

/// Note that `engine_pid` belongs to this process.
pub fn record(home: &Path, engine_pid: u32) {
    let d = dir(home);
    if std::fs::create_dir_all(&d).is_ok() {
        let _ = std::fs::write(
            d.join(format!("engine-{engine_pid}")),
            std::process::id().to_string(),
        );
    }
}

/// The engine was stopped on purpose.
pub fn forget(home: &Path, engine_pid: u32) {
    let _ = std::fs::remove_file(dir(home).join(format!("engine-{engine_pid}")));
}

/// Stop the engines whose program no longer runs. Returns their pids.
pub fn reap(home: &Path) -> Vec<u32> {
    let mut stopped = vec![];
    let Ok(rd) = std::fs::read_dir(dir(home)) else {
        return stopped;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(pid) = name
            .strip_prefix("engine-")
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        let owner = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok());
        if !alive(pid) {
            let _ = std::fs::remove_file(e.path()); // the engine has ended; only its note was left
            continue;
        }
        if owner.is_some_and(alive) {
            continue; // its program still runs (another Studio or `flelink run` on the same data folder)
        }
        if is_engine(pid) {
            terminate(pid);
            stopped.push(pid);
        }
        let _ = std::fs::remove_file(e.path());
    }
    stopped
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
fn is_engine(pid: u32) -> bool {
    // a pid can be reused by an unrelated program; only a llama-server is stopped
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("llama-server"))
}

#[cfg(unix)]
fn terminate(pid: u32) {
    let send = |sig: &str| {
        let _ = std::process::Command::new("kill")
            .args([sig, &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status();
    };
    send("-TERM");
    let start = Instant::now();
    while alive(pid) && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(100));
    }
    if alive(pid) {
        send("-KILL");
    }
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    false // Windows: the app's job object ends the engine with the app; the files are only cleared up
}

#[cfg(not(unix))]
fn is_engine(_pid: u32) -> bool {
    false
}

#[cfg(not(unix))]
fn terminate(_pid: u32) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn notes_of_ended_engines_go_and_a_live_one_stays() {
        let home = std::env::temp_dir().join(format!("fle-orphans-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(dir(&home)).unwrap();
        // pid 999999998 does not run: nothing to stop, the note goes
        std::fs::write(dir(&home).join("engine-999999998"), "999999999").unwrap();
        // this test process stands in for a running engine whose program (also this process) is alive: kept
        let me = std::process::id();
        record(&home, me);
        assert!(reap(&home).is_empty());
        assert!(!dir(&home).join("engine-999999998").exists());
        assert!(dir(&home).join(format!("engine-{me}")).exists());
        forget(&home, me);
        assert!(!dir(&home).join(format!("engine-{me}")).exists());
        let _ = std::fs::remove_dir_all(&home);
    }
}
