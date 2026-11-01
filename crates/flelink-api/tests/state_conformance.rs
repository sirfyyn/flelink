//! `/api/state` (everything the page loads at once) against the Python Studio, and the page served from the copy inside
//! the binary against web/studio (D10).
#![cfg(unix)]
mod common;
mod harness;
use common::{kv, tensors, write};
use harness::*;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;

fn seed(home: &Path) {
    let m = home.join("models");
    std::fs::create_dir_all(&m).unwrap();
    write(
        &m.join("moe.gguf"),
        &kv("qwen3moe", "Own MoE", true),
        &tensors(true),
    );
    write(
        &m.join("dense.gguf"),
        &kv("qwen3", "Own Dense", false),
        &tensors(false),
    );
    std::fs::write(
        home.join("profiles.json"),
        json!([{"id": "mine", "name": "Mine", "args": ["-c", "1"]}]).to_string(),
    )
    .unwrap();
    let h = home.to_string_lossy();
    std::fs::write(home.join("downloads.json"), json!([{"id": "aaaaaaaaaaaa", "title": "done one", "kind": "model", "dedupe": "catalog:fle-iq3_m", "meta": {}, "state": "failed", "error": "boom", "created": 1.5, "finished": null,
        "files": [{"url": "https://example.invalid/a", "dest": format!("{h}/models/a"), "size": 100, "sha256": null, "done": 30}], "speed": 0.0}]).to_string()).unwrap();
    std::fs::write(
        home.join("settings.json"),
        json!({"theme": "dark", "hf_token": "secret", "launch": {"context": 8192}}).to_string(),
    )
    .unwrap();
}

fn scrub(mut v: Value) -> Value {
    v["system"]["python"] = json!("");
    v["system"]["memory"]["available"] = json!(0);
    v["system"]["disk_free"] = json!(0);
    if let Some(gs) = v["system"]["gpus"].as_array_mut() {
        gs.iter_mut().for_each(|g| {
            for k in ["utilization", "power_w", "temperature_c", "memory_used"] {
                if g.get(k).is_some() {
                    g[k] = json!(0);
                }
            }
        });
    }
    let t = v.to_string().replace("-rs", "-X").replace("-py", "-X");
    canon(serde_json::from_str(&t).unwrap())
}

#[test]
fn state_and_the_embedded_page_match_the_python_studio() {
    let Some(src) = studio_src() else {
        eprintln!("FLE Studio sources not found (set FLE_STUDIO_SRC): skipped");
        return;
    };
    let (hp, hr) = (tmp("state-py"), tmp("state-rs"));
    seed(&hp);
    seed(&hr);
    let py = start_python(&src, &hp);
    let rs: SocketAddr = start_rust_full(&src, &hr, flelink_api::engines::GITHUB_API, false); // the page comes from the binary
    let (ia, ib) = (
        http(rs, "GET", "/", &[], None),
        http(py.addr, "GET", "/", &[], None),
    );
    let (tr, _tp) = (token_of(&ia), token_of(&ib));
    // D10: the page is FLE Link's own since 0.1.0 (name, navigation, fixes), so it is no longer compared with the Python
    // page; the binary must serve exactly the repository's web/studio, with the token and the version filled in
    let own = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/studio");
    let fill = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .replace(&tr, "__FLE_TOKEN__")
            .replace(env!("CARGO_PKG_VERSION"), "__FLELINK_VERSION__")
            .into_bytes()
    };
    assert_eq!(ia.status, 200);
    assert_eq!(
        fill(&ia.body),
        std::fs::read(own.join("index.html")).unwrap(),
        "index from the binary"
    );
    let (a, b) = (
        http(rs, "GET", "/api/state", &[], None),
        http(py.addr, "GET", "/api/state", &[], None),
    );
    assert_eq!((a.status, b.status), (200, 200));
    let (x, y): (Value, Value) = (
        serde_json::from_slice(&a.body).unwrap(),
        serde_json::from_slice(&b.body).unwrap(),
    );
    assert_eq!(
        x.as_object().unwrap().keys().collect::<Vec<_>>(),
        y.as_object().unwrap().keys().collect::<Vec<_>>(),
        "state keys and their order"
    );
    let (x, y) = (scrub(x), scrub(y));
    for k in x.as_object().unwrap().keys() {
        assert_eq!(x[k], y[k], "state section {k}");
    }
    // the same page, file by file
    fn files(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                files(&p, base, out);
            } else {
                out.push(format!(
                    "/{}",
                    p.strip_prefix(base).unwrap().to_string_lossy()
                ));
            }
        }
    }
    let mut list = vec![];
    files(&own, &own, &mut list);
    list.sort();
    for f in &list {
        let a = http(rs, "GET", f, &[], None);
        let want = std::fs::read(own.join(f.trim_start_matches('/'))).unwrap();
        let got = if f.ends_with(".html") {
            fill(&a.body)
        } else {
            a.body.clone()
        };
        assert_eq!(a.status, 200, "page file {f}");
        assert!(got == want, "page file {f} differs from web/studio");
    }
    eprintln!(
        "compared with the Python Studio: /api/state ({} sections); {} page files served as in web/studio",
        x.as_object().unwrap().len(),
        list.len()
    );
}
