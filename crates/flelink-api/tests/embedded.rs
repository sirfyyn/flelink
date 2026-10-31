//! The server inside another program (the desktop app): it answers on a free loopback port and the port is closed after `stop`.
use flelink_api::run::start_embedded;
use std::time::Duration;

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_embedded_server_answers_and_lets_go_of_its_port() {
    let home = Tmp(std::env::temp_dir().join(format!("flelink-embedded-{}", std::process::id())));
    let mut s = start_embedded(Some(home.0.clone()), None).expect("start");
    assert!(s.url.starts_with("http://127.0.0.1:"));
    assert_ne!(s.port, 0);
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    let body = agent
        .get(&format!("{}api/ping", s.url))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(body.contains("\"app\":\"fle-studio\""), "{body}");
    let page = agent.get(&s.url).call().unwrap();
    assert_eq!(page.status(), 200);

    let url = s.url.clone();
    s.stop();
    assert!(
        agent.get(&format!("{url}api/ping")).call().is_err(),
        "the port still answers after stop"
    );
}
