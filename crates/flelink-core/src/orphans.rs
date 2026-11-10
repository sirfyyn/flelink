//! Engines whose program died without stopping them (`kill -9`, a crash, a closed laptop lid at the wrong moment).
//! Each running engine has a file `<data>/run/engine-<engine pid>` that holds the pid of the program that started it.
//! Before an engine starts, files whose program is gone are cleared up and their engine, if it still runs, is stopped:
//! without that a second start loads the model a second time next to the first. (PR_SET_PDEATHSIG does not fit here: it
//! fires when the starting *thread* ends, and the TUI starts the engine from a worker thread. On Windows the app's job
//! object ends the engine with the app.)
use std::path::{Path, PathBuf};

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

fn engine_running(home: &Path) -> bool {
    std::fs::read_dir(dir(home)).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_prefix("engine-")
                .and_then(|p| p.parse::<u32>().ok())
                .is_some_and(alive)
        })
    })
}

/// A start right after a stop finds the port still held by the engine on its way out. Wait for it, but only while an
/// engine of this data folder runs: a port some other program holds is not going to come free.
pub fn wait_for_port(home: &Path, port: u16, max: std::time::Duration) {
    reap(home);
    let start = std::time::Instant::now();
    while start.elapsed() < max
        && engine_running(home)
        && std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
    {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
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
    use std::time::{Duration, Instant};
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

#[cfg(all(test, unix))]
mod port_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn it_waits_only_for_an_engine_of_this_data_folder() {
        let home = std::env::temp_dir().join(format!("flelink-port-{}", std::process::id()));
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();

        let t = Instant::now();
        wait_for_port(&home, port, Duration::from_secs(5));
        assert!(t.elapsed() < Duration::from_secs(2), "no engine, no wait");

        let me = std::process::id();
        record(&home, me);
        let t = Instant::now();
        wait_for_port(&home, port, Duration::from_millis(400));
        assert!(t.elapsed() >= Duration::from_millis(400), "waited for it");

        drop(held);
        let t = Instant::now();
        wait_for_port(&home, port, Duration::from_secs(5));
        assert!(t.elapsed() < Duration::from_secs(2), "free port, no wait");
        let _ = std::fs::remove_dir_all(home);
    }
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
