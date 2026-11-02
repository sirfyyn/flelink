//! An attached server that goes away and comes back: `/api/engine` follows it without a page watching the live numbers
//! (before, the answer stayed at the last look, and an engine that came back read "not answering").
#![cfg(unix)]
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A health endpoint that answers 200 while `up` is set and drops connections otherwise.
fn health_server(port: u16, up: Arc<AtomicBool>) {
    let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            if !up.load(Ordering::SeqCst) {
                continue; // closed at once: a refused request
            }
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"status\":\"ok\"}");
        }
    });
}

fn engine(port: u16) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET /api/engine HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    let _ = s.read_to_string(&mut raw);
    raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
}

#[test]
fn an_attached_server_that_comes_back_is_seen_without_a_page_watching() {
    let (hp, sp) = (free_port(), free_port());
    let up = Arc::new(AtomicBool::new(true));
    health_server(hp, up.clone());
    let home = std::env::temp_dir().join(format!("fle-attach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut studio = Command::new(env!("CARGO_BIN_EXE_flelink"))
        .args(["studio", "--no-browser", "--home"])
        .arg(&home)
        .args([
            "--port",
            &sp.to_string(),
            "--attach",
            &format!("http://127.0.0.1:{hp}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t = Instant::now();
    while TcpStream::connect(("127.0.0.1", sp)).is_err() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "the Studio did not start"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(engine(sp).contains("\"reachable\":true"), "{}", engine(sp));
    up.store(false, Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(6));
    assert!(engine(sp).contains("\"reachable\":false"), "{}", engine(sp));
    up.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(6));
    assert!(engine(sp).contains("\"reachable\":true"), "{}", engine(sp));
    let _ = studio.kill();
    let _ = studio.wait();
    let _ = std::fs::remove_dir_all(&home);
}
