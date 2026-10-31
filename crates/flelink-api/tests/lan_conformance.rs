//! Host mode against the Python Studio: another machine needs the access key (cookie or Bearer), the page redirects to the
//! sign-in form, names that are not this machine's are refused, and this machine itself is let in as before.
//! "Another machine" is this one reached by its LAN address, so the test needs a non-loopback interface (else it skips).
#![cfg(unix)]
mod harness;
use harness::*;

const KEY: &str = "test-access-key-1234";

#[test]
fn host_mode_matches_the_python_studio() {
    let Some(src) = studio_src() else {
        eprintln!("FLE Studio sources not found (set FLE_STUDIO_SRC): skipped");
        return;
    };
    let Some(ip) = lan_ip() else {
        eprintln!("no LAN interface: skipped");
        return;
    };
    let (hp, hr) = (tmp("lan-py"), tmp("lan-rs"));
    for h in [&hp, &hr] {
        std::fs::write(
            h.join("settings.json"),
            format!(r#"{{"access_key": "{KEY}"}}"#),
        )
        .unwrap();
    }
    let py = start_python_lan(&src, &hp);
    let rs_port = start_rust_lan(&src, &hr);
    let (a_rs, a_py) = (
        std::net::SocketAddr::new(ip, rs_port),
        std::net::SocketAddr::new(ip, py.addr.port()),
    );
    let both = |label: &str,
                method: &str,
                path: &str,
                headers: &[(&str, &str)],
                body: Option<&[u8]>|
     -> (Resp, Resp) {
        let hr_ = |port: u16| -> Vec<(String, String)> {
            let mut v: Vec<(String, String)> = headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            if !v.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
                v.push(("Host".into(), format!("{ip}:{port}")));
            }
            v
        };
        let go = |addr: std::net::SocketAddr| {
            let hs = hr_(addr.port());
            let hs: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            http(addr, method, path, &hs, body)
        };
        let (a, b) = (go(a_rs), go(a_py));
        compare(label, &a, &b, "", "");
        (a, b)
    };

    // not signed in
    let (a, _) = both("api without key", "GET", "/api/ping", &[], None);
    assert_eq!(a.status, 401);
    let (a, _) = both("proxy without key", "GET", "/v1/models", &[], None);
    assert_eq!(a.status, 401);
    let (a, b) = both("page without key", "GET", "/", &[], None);
    assert_eq!((a.status, b.status), (303, 303));
    assert_eq!(a.h("location"), vec!["/login"]);
    let (a, _) = both("sign-in page", "GET", "/login", &[], None);
    assert_eq!(a.status, 200);

    // a wrong key, then the right one
    let (a, _) = both(
        "wrong key",
        "POST",
        "/login",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        Some(b"key=nope"),
    );
    assert_eq!(a.status, 401);
    let (a, b) = both(
        "right key",
        "POST",
        "/login",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        Some(format!("key={KEY}").as_bytes()),
    );
    assert_eq!((a.status, b.status), (303, 303));
    assert_eq!(a.h("location"), vec!["/"]);
    let cookie_of = |r: &Resp| {
        r.h("set-cookie")
            .first()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        cookie_of(&a),
        cookie_of(&b),
        "the session cookie is derived from the key the same way"
    );
    assert_eq!(a.h("set-cookie"), b.h("set-cookie"));
    let cookie = cookie_of(&a);

    // signed in: the cookie, or the key as a Bearer header
    let (a, _) = both(
        "ping with cookie",
        "GET",
        "/api/ping",
        &[("Cookie", &cookie)],
        None,
    );
    assert_eq!(a.status, 200);
    let bearer = format!("Bearer {KEY}");
    let (a, _) = both(
        "ping with bearer",
        "GET",
        "/api/ping",
        &[("Authorization", &bearer)],
        None,
    );
    assert_eq!(a.status, 200);
    let (a, _) = both(
        "wrong bearer",
        "GET",
        "/api/ping",
        &[("Authorization", "Bearer nope")],
        None,
    );
    assert_eq!(a.status, 401);
    let (a, _) = both(
        "wrong cookie",
        "GET",
        "/api/ping",
        &[("Cookie", "fle_session=00")],
        None,
    );
    assert_eq!(a.status, 401);
    // the page carries a token that each server makes up for itself
    let page = |addr: std::net::SocketAddr| {
        let host = format!("{ip}:{}", addr.port());
        http(
            addr,
            "GET",
            "/",
            &[("Cookie", &cookie), ("Host", &host)],
            None,
        )
    };
    let (a, b) = (page(a_rs), page(a_py));
    compare("page with cookie", &a, &b, &token_of(&a), &token_of(&b));
    assert_eq!(a.status, 200);

    // a signed-in machine still needs the page's token for changes, and may not come from another site
    let (a, _) = both(
        "change without token",
        "POST",
        "/api/settings",
        &[("Cookie", &cookie), ("Content-Type", "application/json")],
        Some(b"{}"),
    );
    assert_eq!(a.status, 403);
    let origin = "http://evil.example";
    let (a, _) = both(
        "cross-origin change",
        "POST",
        "/api/settings",
        &[
            ("Cookie", &cookie),
            ("Origin", origin),
            ("Content-Type", "application/json"),
        ],
        Some(b"{}"),
    );
    assert_eq!(a.status, 403);

    // names: an IP address is fine, a stranger's name is not
    let (a, _) = both(
        "foreign host name",
        "GET",
        "/api/ping",
        &[("Cookie", &cookie), ("Host", "evil.example")],
        None,
    );
    assert_eq!(a.status, 403);

    // this machine itself needs no key
    let go = |port: u16| {
        http(
            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            "GET",
            "/api/ping",
            &[],
            None,
        )
    };
    let (a, b) = (go(rs_port), go(py.addr.port()));
    compare("loopback", &a, &b, "", "");
    assert_eq!(a.status, 200);
}
