//! Practical tests (`flelink bench --task …`): a real task instead of a short paragraph, at temperature 0 and without a
//! thinking phase, measured through any OpenAI-compatible `/v1` (FLE Link, llama.cpp, another server). Each run records
//! decode and prompt speed from the server's own timings, the answer's length, and whether the answer is complete (for
//! the pagoda: the HTML ends with `</html>` before the token limit). The result carries the machine and the server's
//! build, so two people can compare their files. Built for the Strata-vs-FLE comparison of 3 Nov 2026, where the answers
//! ran 3100 to 3600 tokens and a limit of 3000 cut every engine off mid-HTML.

use crate::FleError;
use anyhow::Result;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub struct Task {
    pub id: &'static str,
    pub title: &'static str,
    pub prompt: &'static str,
    pub max_tokens: u32,
    /// the answer counts as complete when it contains this (case-insensitive), and did not stop at the token limit
    pub done_marker: &'static str,
}

pub const TASKS: &[Task] = &[Task {
    id: "pagoda",
    title: "Voxel pagoda on a floating island in three.js, one HTML file",
    // word for word the prompt of the published DeepSeek and Strata numbers (3 Nov 2026), so runs compare with them
    prompt: "Code eine Pagoda im Voxelstil auf einer schwebenden Insel. Die Szene soll frei erkundbar sein und sonst um \
             die Pagoda kreisen, in three.js. Gib eine einzelne vollstaendige HTML-Datei aus.",
    max_tokens: 8000,
    done_marker: "</html>",
}];

/// A short request before the measured ones, so the first measured run does not pay for a cold cache.
const WARMUP_PROMPT: &str = "Zaehle von 1 bis 200.";
const WARMUP_TOKENS: u32 = 400;

pub fn task(id: &str) -> Result<&'static Task> {
    TASKS.iter().find(|t| t.id == id).ok_or_else(|| {
        FleError::new(
            format!("There is no practical test called {id:?}."),
            format!(
                "Known: {}",
                TASKS.iter().map(|t| t.id).collect::<Vec<_>>().join(", ")
            ),
        )
        .into()
    })
}

pub struct Params {
    pub url: String,
    pub model: String,
    pub runs: u32,
    pub warmup: bool,
    pub timeout: f64,
    /// where to write each run's answer (`<dir>/<task>-<n>.html`); none: answers are not kept
    pub answers: Option<std::path::PathBuf>,
}

/// The Studio's own `/v1` asks for its token: when the URL is the running Studio's (`studio.json`), it goes along.
fn studio_token(base: &str) -> Option<String> {
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(crate::data_dir().join("studio.json")).ok()?)
            .ok()?;
    let url = v["url"].as_str()?.trim_end_matches('/');
    let same = |u: &str| u.replace("localhost", "127.0.0.1");
    (same(url) == same(base))
        .then(|| v["token"].as_str().map(str::to_string))
        .flatten()
}

fn client(timeout: f64, base: &str) -> Result<reqwest::blocking::Client> {
    let mut h = reqwest::header::HeaderMap::new();
    if let Some(t) = studio_token(base) {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&t) {
            h.insert("X-FLE-Token", v);
        }
    }
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs_f64(timeout.max(1.0)))
        .default_headers(h)
        .danger_accept_invalid_certs(base.starts_with("https://127.0.0.1"))
        .build()?)
}

fn get(c: &reqwest::blocking::Client, url: &str) -> Option<Value> {
    c.get(url).send().ok()?.json().ok()
}

fn chat(
    c: &reqwest::blocking::Client,
    base: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> Result<Value> {
    // temperature 0 and no thinking phase: the answer is the measured text. Servers read different switches
    // (llama.cpp templates `enable_thinking`, others `thinking`; Strata only `enable_thinking`), so all are sent.
    let body = json!({
        "model": model, "messages": [{"role": "user", "content": prompt}], "max_tokens": max_tokens,
        "temperature": 0, "stream": false,
        "chat_template_kwargs": {"enable_thinking": false, "thinking": false},
    });
    let r = c
        .post(format!("{base}/v1/chat/completions"))
        .json(&body)
        .send()
        .map_err(|e| {
            FleError::new(
                format!("Cannot reach {base}: {e}"),
                "Start the server first, or pass --url.",
            )
        })?;
    let code = r.status();
    let text = r.text().unwrap_or_default();
    if !code.is_success() {
        return Err(FleError::new(
            format!(
                "{base} returned HTTP {}: {}",
                code.as_u16(),
                text.chars().take(300).collect::<String>()
            ),
            "Check the server log.",
        )
        .into());
    }
    Ok(serde_json::from_str(&text)?)
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// One measured answer: speeds from the server's timings (llama.cpp's `timings`), else from the wall clock.
pub fn measure(body: &Value, elapsed_s: f64, t: &Task) -> Value {
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    let thinking = body["choices"][0]["message"]["reasoning_content"]
        .as_str()
        .is_some_and(|r| !r.trim().is_empty());
    let finish = body["choices"][0]["finish_reason"].as_str().unwrap_or("");
    let tm = &body["timings"];
    let tokens = tm["predicted_n"]
        .as_u64()
        .or_else(|| body["usage"]["completion_tokens"].as_u64())
        .unwrap_or(0);
    let decode = tm["predicted_per_second"]
        .as_f64()
        .unwrap_or(if elapsed_s > 0.0 {
            tokens as f64 / elapsed_s
        } else {
            0.0
        });
    let complete = finish != "length"
        && content
            .to_lowercase()
            .contains(&t.done_marker.to_lowercase());
    json!({
        "tokens": tokens, "decode_tok_s": decode,
        "prompt_tokens": tm["prompt_n"].as_u64().or_else(|| body["usage"]["prompt_tokens"].as_u64()).unwrap_or(0),
        "prompt_tok_s": tm["prompt_per_second"].as_f64(), "prompt_ms": tm["prompt_ms"].as_f64(),
        "seconds": elapsed_s, "finish_reason": finish, "complete": complete,
        // a stray thinking phase makes the numbers incomparable (longer, other text); it is named, not hidden
        "thinking": thinking || content.trim_start().starts_with("<think>"),
        "accept": match (tm["draft_n"].as_u64(), tm["draft_n_accepted"].as_u64()) {
            (Some(n), Some(a)) if n > 0 => json!(a as f64 / n as f64),
            _ => Value::Null,
        },
    })
}

/// The machine and the server, so a result file says what it was measured on.
fn setup(c: &reqwest::blocking::Client, base: &str) -> Value {
    let s = crate::system::detect(&crate::data_dir());
    let props = get(c, &format!("{base}/props")).unwrap_or(Value::Null);
    let model_file = props["model_path"]
        .as_str()
        .and_then(|p| p.rsplit(['/', '\\']).next())
        .unwrap_or("")
        .to_string();
    json!({
        "flelink": env!("CARGO_PKG_VERSION"), "os": s.os, "arch": s.arch, "cpu": s.cpu, "cores": s.cores,
        "ram_gib": (s.ram_bytes as f64 / (1u64 << 30) as f64).round(),
        "gpus": s.gpus.iter().map(|g| json!({"name": g.name, "vram_gib": g.vram_bytes.map(|b| (b as f64 / (1u64 << 30) as f64).round())})).collect::<Vec<_>>(),
        "server_build": props["build_info"], "model_file": model_file,
    })
}

pub fn run(p: &Params, t: &Task) -> Result<Value> {
    let mut base = p.url.trim_end_matches('/').to_string();
    if !base.starts_with("http://") && !base.starts_with("https://") {
        base = format!("http://{base}");
    }
    let c = client(p.timeout, &base)?;
    let model = if p.model.is_empty() {
        get(&c, &format!("{base}/v1/models"))
            .and_then(|m| m["data"][0]["id"].as_str().map(str::to_string))
            .ok_or_else(|| {
                FleError::new(
                    format!("The server at {base} named no model."),
                    "Pass --model NAME.",
                )
            })?
    } else {
        p.model.clone()
    };
    if p.warmup {
        chat(&c, &base, &model, WARMUP_PROMPT, WARMUP_TOKENS)?;
    }
    if let Some(d) = &p.answers {
        std::fs::create_dir_all(d)?;
    }
    let mut runs = vec![];
    for i in 0..p.runs.max(1) {
        let t0 = Instant::now();
        let body = chat(&c, &base, &model, t.prompt, t.max_tokens)?;
        let mut m = measure(&body, t0.elapsed().as_secs_f64(), t);
        if let Some(d) = &p.answers {
            let f = d.join(format!("{}-{}.html", t.id, i + 1));
            std::fs::write(
                &f,
                body["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or(""),
            )?;
            m["answer_file"] = json!(f.display().to_string());
        }
        runs.push(m);
    }
    let col = |k: &str| -> Vec<f64> { runs.iter().filter_map(|r| r[k].as_f64()).collect() };
    let complete = runs.iter().filter(|r| r["complete"] == true).count();
    Ok(json!({
        "task": t.id, "title": t.title, "prompt": t.prompt, "max_tokens": t.max_tokens, "temperature": 0,
        "url": base, "model": model, "warmup": p.warmup, "runs": runs,
        "median_decode_tok_s": median(col("decode_tok_s")), "median_prompt_tok_s": median(col("prompt_tok_s")),
        "complete": complete, "thinking_runs": runs.iter().filter(|r| r["thinking"] == true).count(),
        "setup": setup(&c, &base),
    }))
}

pub fn result_text(r: &Value) -> String {
    let f = |v: &Value| v.as_f64().unwrap_or(0.0);
    let mut s = format!(
        "{} ({})\nmodel: {}  url: {}\n{:>3}  {:>7}  {:>9}  {:>13}  {:>8}\n",
        r["task"].as_str().unwrap_or(""),
        r["title"].as_str().unwrap_or(""),
        r["model"].as_str().unwrap_or(""),
        r["url"].as_str().unwrap_or(""),
        "run",
        "tokens",
        "decode",
        "prompt",
        "complete"
    );
    for (i, x) in r["runs"].as_array().into_iter().flatten().enumerate() {
        let prompt = match (x["prompt_tok_s"].as_f64(), x["prompt_ms"].as_f64()) {
            (Some(ts), Some(ms)) => format!("{:.1} s ({ts:.0}/s)", ms / 1000.0),
            _ => "-".into(),
        };
        s += &format!(
            "{:>3}  {:>7}  {:>9}  {:>13}  {:>8}\n",
            i + 1,
            x["tokens"].as_u64().unwrap_or(0),
            format!("{:.1}/s", f(&x["decode_tok_s"])),
            prompt,
            if x["complete"] == true { "yes" } else { "no" }
        );
    }
    let n = r["runs"].as_array().map_or(0, |a| a.len());
    s += &format!(
        "median: {:.1} tok/s decode, {:.1} tok/s prompt; complete {}/{n}\n",
        f(&r["median_decode_tok_s"]),
        f(&r["median_prompt_tok_s"]),
        r["complete"]
    );
    if r["thinking_runs"].as_u64().unwrap_or(0) > 0 {
        s += "warning: the server answered with a thinking phase; these numbers do not compare with answers without one.\n";
    }
    if r["complete"].as_u64().unwrap_or(0) < n as u64 {
        s += "note: an incomplete answer stopped at the token limit or left out the end of the file.\n";
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_counts_only_when_it_ends_and_names_its_speeds() {
        let t = task("pagoda").unwrap();
        let body = json!({"choices": [{"message": {"content": "<!DOCTYPE html>…</HTML>"}, "finish_reason": "stop"}],
            "timings": {"predicted_n": 3106, "predicted_per_second": 31.64, "prompt_n": 59, "prompt_ms": 2530.2, "prompt_per_second": 23.3}});
        let m = measure(&body, 100.7, t);
        assert_eq!(m["complete"], true);
        assert_eq!(m["tokens"], 3106);
        assert_eq!(m["decode_tok_s"], 31.64);
        assert_eq!(m["thinking"], false);
        let cut = json!({"choices": [{"message": {"content": "<html>… </html>"}, "finish_reason": "length"}], "usage": {"completion_tokens": 8000}});
        let m = measure(&cut, 100.0, t);
        assert_eq!(m["complete"], false, "the token limit is never complete");
        assert_eq!(
            m["decode_tok_s"], 80.0,
            "without timings: tokens over the wall clock"
        );
        let think = json!({"choices": [{"message": {"content": "<html></html>", "reasoning_content": "Let me plan"}, "finish_reason": "stop"}]});
        assert_eq!(measure(&think, 1.0, t)["thinking"], true);
        assert!(task("nope").is_err());
    }
}
