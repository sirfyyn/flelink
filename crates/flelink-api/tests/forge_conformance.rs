//! The Forge against the Python Studio's Forge: the same synthetic models (written by tests/forge_ref.py with the Studio's
//! forge_synth) go through both, with the Studio's fake llama.cpp tools. Inspect, the layer calculator, the quantisation
//! estimate, the bundled texts, the checks and two whole jobs (a hybrid with pruning and KL check, a quantisation with
//! importance matrix) must agree: the built files byte for byte (SHA-256), the result cards and the recipe files.
#![cfg(unix)]
mod harness;
use flelink_api::run::start_embedded;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn python() -> String {
    std::env::var("FLE_STUDIO_PYTHON").unwrap_or_else(|_| {
        // the Studio needs a newer Python than the one macOS ships
        let brew = "/opt/homebrew/bin/python3";
        if Path::new(brew).exists() {
            brew.into()
        } else {
            "python3".into()
        }
    })
}

/// Drop what differs by design: where a server keeps its files.
fn strip(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("path");
    }
    v
}

fn same(label: &str, rust: &Value, py: &Value) {
    assert!(
        rust == py,
        "[{label}] differs\nrust:   {}\npython: {}",
        serde_json::to_string(rust).unwrap(),
        serde_json::to_string(py).unwrap()
    );
}

#[test]
fn forge_matches_the_python_studio() {
    let Some(src) = harness::studio_src() else {
        eprintln!("skipped: no FLE Studio checkout");
        return;
    };
    let fakes = src.parent().unwrap().join("tests/fake_tools");
    if !src.parent().unwrap().join("tests/forge_synth.py").exists() || !fakes.is_dir() {
        eprintln!("skipped: the Studio checkout has no Forge test helpers");
        return;
    }
    let py = python();
    // the fake tools are scripts started as `python3 tool.py`; they need the same Python as the reference
    if let Some(dir) = Path::new(&py)
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
    {
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", dir.display()));
    }
    let tmp = harness::tmp("forge");
    let work = tmp.join("w");
    let out = Command::new(&py)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/forge_ref.py"))
        .arg(&src)
        .arg(&work)
        .arg(tmp.join("py-out"))
        .output()
        .expect("python");
    assert!(
        out.status.success(),
        "reference failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r: Value = serde_json::from_slice(&out.stdout).expect("reference JSON");
    let inp = work.join("in");
    let f = |n: &str| inp.join(n).to_string_lossy().to_string();

    let home = tmp.join("home");
    let s = start_embedded(Some(home), None).expect("start");
    let base = s.url.trim_end_matches('/').to_string();
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build();
    let page = agent.get(&s.url).call().unwrap().into_string().unwrap();
    let token = page
        .split("name=\"fle-token\" content=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    let call = |m: &str, p: &str, b: Value| -> (u16, Value) {
        let req = agent
            .request(m, &format!("{base}{p}"))
            .set("X-FLE-Token", &token)
            .set("Content-Type", "application/json");
        let res = if m == "POST" {
            req.send_string(&b.to_string())
        } else {
            req.call()
        };
        let (c, t) = match res {
            Ok(r) => (r.status(), r.into_string().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap()),
            Err(e) => panic!("{e}"),
        };
        (c, serde_json::from_str(&t).unwrap_or(json!({"raw": t})))
    };

    // the page finds the Forge in the plugin list
    let (_, plugins) = call("GET", "/api/plugins", json!(null));
    let forge = plugins["plugins"]
        .as_array()
        .unwrap_or_else(|| panic!("{plugins}"))
        .iter()
        .find(|p| p["id"] == "forge")
        .expect("forge plugin")
        .clone();
    assert_eq!(forge["page"]["path"], "/forge");
    let js = agent
        .get(&format!("{base}/plugins/forge/forge.js"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(js.contains("/api/forge"));

    let (c, ov) = call(
        "POST",
        "/api/forge/tools",
        json!({"tools_dir": fakes.to_string_lossy()}),
    );
    assert_eq!(c, 200, "{ov}");
    let found: Vec<&str> = ov["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["ok"] == true)
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(found.len(), 4, "{ov}");
    assert_eq!(ov["defaults"]["mode"], "hybrid");

    let (c, v) = call("POST", "/api/forge/inspect", json!({"path": f("low.gguf")}));
    assert_eq!(c, 200, "{v}");
    same("inspect", &v, &r["inspect"]);
    let (c, v) = call(
        "POST",
        "/api/forge/calculate",
        json!({"mode": "hybrid",
        "sources": {"A": f("full.gguf"), "B": f("low.gguf"), "C": f("mid.gguf")}, "base": "A", "budget_gb": 0.00016,
        "ram_gb": 16, "vram_gb": 8, "context": 8192, "reap_keep": 6}),
    );
    assert_eq!(c, 200, "{v}");
    same("calculate", &v, &r["calculate"]);
    let (_, v) = call(
        "POST",
        "/api/forge/calculate",
        json!({"mode": "quantize", "source_path": f("full.gguf"), "quantize": {"type": "q4_0"}}),
    );
    same("quantize estimate", &v, &r["quant_estimate"]);
    let (c, v) = call(
        "POST",
        "/api/forge/calculate",
        json!({"mode": "hybrid", "sources": {"A": f("full.gguf")}, "base": "A"}),
    );
    assert_eq!(
        (c, v["error"]["message"].as_str()),
        (400, Some("Enter a size budget in GB"))
    );

    for (key, body) in [
        ("corpus", json!({"bundled": true})),
        (
            "corpus_eval",
            json!({"bundled": true, "part": "evaluation"}),
        ),
    ] {
        let (c, mut v) = call("POST", "/api/forge/corpus", body);
        assert_eq!(c, 200, "{v}");
        v["text"] = json!(std::fs::read_to_string(v["path"].as_str().unwrap()).unwrap());
        same(key, &strip(v), &strip(r[key].clone()));
    }

    let recipes = json!({
        "hybrid": {"name": "htest", "mode": "hybrid",
            "sources": {"B": {"path": f("full.gguf"), "role": "base"}, "L": {"path": f("low.gguf"), "role": "donor"}},
            "hybrid": {"base": "B", "layers": {"1": "L", "3": "L"}},
            "imatrix": {"corpus": f("corpus.txt"), "context": 128},
            "reap": {"enabled": true, "keep": 6, "chunks": 2},
            "verify": {"enabled": true, "reference": "B", "text": f("eval.txt"), "chunks": 3, "context": 128}},
        "quantize": {"name": "qtest", "mode": "quantize",
            "sources": {"S": {"path": f("full.gguf"), "role": "base"}},
            "quantize": {"source": "S", "type": "q4_0", "rules": [{"pattern": "ffn_down_exps", "type": "q8_0"}]},
            "imatrix": {"corpus": f("corpus.txt"), "chunks": 3, "context": 128},
            "verify": {"enabled": true, "reference": "S", "text": f("eval.txt"), "chunks": 3, "context": 128}}
    });
    for key in ["hybrid", "quantize"] {
        let recipe = &recipes[key];
        let (c, v) = call("POST", "/api/forge/check", json!({"recipe": recipe}));
        assert_eq!(c, 200, "{v}");
        same(&format!("check {key}"), &v, &r[format!("check_{key}")]);
        let (c, job) = call("POST", "/api/forge/jobs", json!({"recipe": recipe}));
        assert_eq!(c, 200, "{job}");
        let id = job["id"].as_str().unwrap().to_string();
        let t = Instant::now();
        let j = loop {
            let (_, j) = call("GET", &format!("/api/forge/jobs/{id}"), json!(null));
            if ["done", "failed", "cancelled"].contains(&j["state"].as_str().unwrap_or(""))
                && j["running"] == false
            {
                break j;
            }
            assert!(
                t.elapsed() < Duration::from_secs(120),
                "job {key} hangs: {j}"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        let res = &j["result"];
        let steps: Vec<Value> = j["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| json!([s["id"], s["state"]]))
            .collect();
        let recipe_file = res["recipe_file"]
            .as_str()
            .map(|p| std::fs::read_to_string(p).unwrap());
        let (_, portable) = call("GET", &format!("/api/forge/jobs/{id}/recipe"), json!(null));
        let mine = json!({"state": j["state"], "error": j["error"], "steps": steps, "sha256": res["sha256"], "size": res["size"],
            "card": res["card"], "recipe_file": recipe_file, "portable": portable});
        let (_, log) = call(
            "GET",
            &format!("/api/forge/jobs/{id}/log?since=0"),
            json!(null),
        );
        assert!(log["lines"].as_array().is_some_and(|l| !l.is_empty()));
        let mut theirs = r["jobs"][key].clone();
        if theirs["card"]["tool"].is_string() {
            // the Rust build has its own version line for the fake tool: same text
            assert_eq!(mine["card"]["tool"], theirs["card"]["tool"]);
        }
        theirs["card"]["tool"] = mine["card"]["tool"].clone();
        // the recipe names the program that wrote it: FLE Link's version, not the Studio's
        let (pv, rv) = (
            theirs["portable"]["studio"].clone(),
            mine["portable"]["studio"].clone(),
        );
        theirs["portable"]["studio"] = rv.clone();
        if let Some(t) = theirs["recipe_file"].as_str() {
            theirs["recipe_file"] =
                json!(t.replace(&format!("\"studio\": {pv}"), &format!("\"studio\": {rv}")));
        }
        same(&format!("job {key}"), &mine, &theirs);
    }

    // cancel in the middle of a slow tool, resume: the finished steps stay, the result is the same file
    std::env::set_var("FAKE_DELAY", "0.3");
    let mut slow = recipes["quantize"].clone();
    slow["name"] = json!("qslow");
    let (c, job) = call("POST", "/api/forge/jobs", json!({"recipe": slow}));
    assert_eq!(c, 200, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let wait = |states: &[&str]| -> Value {
        let t = Instant::now();
        loop {
            let (_, j) = call("GET", &format!("/api/forge/jobs/{id}"), json!(null));
            if states.contains(&j["state"].as_str().unwrap_or("")) && j["running"] == false {
                return j;
            }
            assert!(t.elapsed() < Duration::from_secs(120), "job hangs: {j}");
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let t = Instant::now();
    loop {
        let (_, j) = call("GET", &format!("/api/forge/jobs/{id}"), json!(null));
        if j["steps"][1]["state"] == "running" {
            break; // the importance matrix is done, the quantiser runs
        }
        assert!(t.elapsed() < Duration::from_secs(60), "{j}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let (c, _) = call("POST", &format!("/api/forge/jobs/{id}/cancel"), json!({}));
    assert_eq!(c, 200);
    let j = wait(&["cancelled"]);
    assert_eq!(
        (&j["steps"][0]["state"], &j["steps"][1]["state"]),
        (&json!("done"), &json!("cancelled"))
    );
    std::env::remove_var("FAKE_DELAY");
    let (c, v) = call("POST", &format!("/api/forge/jobs/{id}/resume"), json!({}));
    assert_eq!(c, 200, "{v}");
    let j = wait(&["done", "failed"]);
    assert_eq!(j["state"], "done", "{j}");
    assert_eq!(j["result"]["sha256"], r["jobs"]["quantize"]["sha256"]);
    let (_, log) = call(
        "GET",
        &format!("/api/forge/jobs/{id}/log?since=0"),
        json!(null),
    );
    let imatrix_runs = log["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["text"].as_str().unwrap_or("").contains("llama-imatrix"))
        .count();
    assert_eq!(imatrix_runs, 1, "the finished step ran again");

    // a finished job is published: the model in the library, a start profile for it
    let (_, list) = call("GET", "/api/forge", json!(null));
    let done = list["jobs"].as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (c, pubd) = call(
        "POST",
        &format!("/api/forge/jobs/{done}/publish"),
        json!({}),
    );
    assert_eq!(c, 200, "{pubd}");
    assert!(pubd["profile"]["name"]
        .as_str()
        .unwrap()
        .starts_with("Forge: "));
    let (c, _) = call(
        "POST",
        &format!("/api/forge/jobs/{done}/delete"),
        json!({"delete_model": false}),
    );
    assert_eq!(c, 200);
    let (c, _) = call("GET", &format!("/api/forge/jobs/{done}"), json!(null));
    assert_eq!(c, 404);
}
