//! Sharing a result on GitHub (Max 04.11.2026): a link to a new discussion in sirfyyn/flelink, title and text filled in.
//! FLE Link uploads nothing itself: the person opens the link in their browser, reads what it says, and posts it with
//! their own account. A text too long for a link goes to the clipboard instead, and the link opens an empty form.

pub const REPO: &str = "sirfyyn/flelink";
/// Discussion categories (slugs) in the repo
pub const QUANTS: &str = "community-quants";
pub const BENCHMARKS: &str = "benchmarks";
/// GitHub refuses very long URLs; this leaves room for the title and the encoding
const MAX_URL: usize = 7500;

pub struct Share {
    pub url: String,
    /// the text to paste when it did not fit into the link
    pub paste: Option<String>,
}

fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                o.push(b as char)
            }
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

pub fn discussion(category: &str, title: &str, body: &str) -> Share {
    let base = format!(
        "https://github.com/{REPO}/discussions/new?category={}&title={}",
        enc(category),
        enc(title)
    );
    let full = format!("{base}&body={}", enc(body));
    if full.len() <= MAX_URL {
        Share {
            url: full,
            paste: None,
        }
    } else {
        Share {
            url: base,
            paste: Some(body.to_string()),
        }
    }
}

/// A practical test result (`flelink bench --task`) as a post: the machine, the server, every run; no paths or addresses.
pub fn bench_post(r: &serde_json::Value) -> (String, String) {
    let s = &r["setup"];
    let gpus: Vec<String> = s["gpus"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| match g["vram_gib"].as_f64() {
            Some(v) => format!("{} ({v:.0} GiB)", g["name"].as_str().unwrap_or("?")),
            None => g["name"].as_str().unwrap_or("?").to_string(),
        })
        .collect();
    let f = |v: &serde_json::Value| v.as_f64().unwrap_or(0.0);
    let title = format!(
        "{}: {:.1} tok/s, {} on {}",
        r["task"].as_str().unwrap_or("bench"),
        f(&r["median_decode_tok_s"]),
        s["model_file"]
            .as_str()
            .filter(|m| !m.is_empty())
            .unwrap_or(r["model"].as_str().unwrap_or("?")),
        gpus.first()
            .cloned()
            .unwrap_or_else(|| s["cpu"].as_str().unwrap_or("CPU").to_string())
    );
    let mut b = format!(
        "**Task** `{}` (temperature 0, no thinking, up to {} tokens), FLE Link {}\n\n\
         **Machine** {} ({} cores), {} GiB RAM, {}, {} {}\n\n\
         **Server** {} · model file `{}`\n\n| run | tokens | decode tok/s | prompt | complete |\n|---|---|---|---|---|\n",
        r["task"].as_str().unwrap_or(""),
        r["max_tokens"],
        s["flelink"].as_str().unwrap_or(""),
        s["cpu"].as_str().unwrap_or("?"),
        s["cores"],
        s["ram_gib"],
        if gpus.is_empty() { "no GPU".to_string() } else { gpus.join(", ") },
        s["os"].as_str().unwrap_or(""),
        s["arch"].as_str().unwrap_or(""),
        s["server_build"].as_str().unwrap_or("?"),
        s["model_file"].as_str().unwrap_or("?"),
    );
    for (i, x) in r["runs"].as_array().into_iter().flatten().enumerate() {
        let prompt = match x["prompt_ms"].as_f64() {
            Some(ms) => format!("{:.1} s", ms / 1000.0),
            None => "-".into(),
        };
        b += &format!(
            "| {} | {} | {:.2} | {} | {} |\n",
            i + 1,
            x["tokens"],
            f(&x["decode_tok_s"]),
            prompt,
            if x["complete"] == true { "yes" } else { "no" }
        );
    }
    b += &format!(
        "\nMedian **{:.2} tok/s** decode, {:.1} tok/s prompt.{}\n",
        f(&r["median_decode_tok_s"]),
        f(&r["median_prompt_tok_s"]),
        if r["thinking_runs"].as_u64().unwrap_or(0) > 0 {
            " The server answered with a thinking phase."
        } else {
            ""
        }
    );
    (title, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_short_post_goes_into_the_link_and_a_long_one_to_the_clipboard() {
        let s = discussion(BENCHMARKS, "pagoda: 31.6 tok/s", "a | b\n");
        assert!(s.url.starts_with("https://github.com/sirfyyn/flelink/discussions/new?category=benchmarks&title=pagoda%3A%2031.6"));
        assert!(s.url.contains("&body=a%20%7C%20b%0A"));
        assert!(s.paste.is_none());
        let long = "x".repeat(10_000);
        let s = discussion(QUANTS, "q", &long);
        assert!(!s.url.contains("body="));
        assert_eq!(s.paste.as_deref(), Some(long.as_str()));
    }

    #[test]
    fn a_bench_post_names_the_machine_and_every_run_but_no_address() {
        let r = json!({"task": "pagoda", "max_tokens": 8000, "url": "http://127.0.0.1:8660", "median_decode_tok_s": 31.6,
            "median_prompt_tok_s": 23.3, "thinking_runs": 0,
            "runs": [{"tokens": 3106, "decode_tok_s": 31.64, "prompt_ms": 2530.0, "complete": true, "answer_file": "/home/me/a.html"}],
            "setup": {"flelink": "0.1.5", "os": "linux", "arch": "x64", "cpu": "Ryzen 9 9950X3D", "cores": 16, "ram_gib": 121.0,
                "gpus": [{"name": "RTX PRO 4500", "vram_gib": 32.0}], "server_build": "b1-fle", "model_file": "DS-IQ2_XXS-00001.gguf"}});
        let (t, b) = bench_post(&r);
        assert_eq!(
            t,
            "pagoda: 31.6 tok/s, DS-IQ2_XXS-00001.gguf on RTX PRO 4500 (32 GiB)"
        );
        assert!(b.contains("| 1 | 3106 | 31.64 | 2.5 s | yes |"));
        assert!(!b.contains("127.0.0.1") && !b.contains("/home/me"));
    }
}
