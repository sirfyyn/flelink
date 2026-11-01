//! Checking a result: KL divergence and top-1 agreement against the full-precision model, from `llama-perplexity`. Port of
//! `fle_studio_forge/verify.py`. Compare only runs with the same text, context, chunks, tool build and hardware.
use regex::Regex;
use serde_json::{json, Map, Value};

const PM: &str = r"(?:±|\+/-|Â±|�+)";
const NUM: &str = r"(-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?|nan|-?inf)";

fn f(v: &str) -> Value {
    match v.parse::<f64>() {
        Ok(x) if x.is_finite() => json!(x),
        _ => Value::Null,
    }
}

/// Statistics of one comparison run; `complete` says whether the KLD is there.
pub fn parse(text: &str) -> Value {
    let mut out = Map::new();
    let mut pair = |key: &str, label: &str| {
        let re = Regex::new(&format!(r"(?m)^\s*{label}\s*:\s*{NUM}\s*(?:{PM}\s*{NUM})?")).unwrap();
        if let Some(m) = re.captures(text) {
            out.insert(key.into(), f(&m[1]));
            if let Some(se) = m.get(2) {
                out.insert(format!("{key}_se"), f(se.as_str()));
            }
        }
    };
    pair("kld_mean", r"Mean\s+KLD");
    pair("kld_max", r"Maximum\s+KLD");
    pair("kld_999", r"99\.9%\s+KLD");
    pair("kld_99", r"99\.0%\s+KLD");
    pair("kld_95", r"95\.0%\s+KLD");
    pair("kld_median", r"Median\s+KLD");
    pair("ppl_q", r"Mean\s+PPL\(Q\)");
    pair("ppl_base", r"Mean\s+PPL\(base\)");
    pair("ppl_ratio", r"Mean\s+PPL\(Q\)/PPL\(base\)");
    pair("ln_ppl_ratio", r"Mean\s+ln\(PPL\(Q\)/PPL\(base\)\)");
    pair("delta_p_mean", r"Mean\s+Δp");
    pair("delta_p_rms", r"RMS\s+Δp");
    pair("top1", r"Same\s+top\s+p");
    if let Some(m) = Regex::new(&format!(
        r"(?m)^\s*Cor\(ln\(PPL\(Q\)\),\s*ln\(PPL\(base\)\)\)\s*:\s*{NUM}\s*%"
    ))
    .unwrap()
    .captures(text)
    {
        out.insert("ppl_correlation".into(), f(&m[1]));
    }
    if let Some(m) = Regex::new(r"tokens\s*[:=]\s*(\d+)|(\d+)\s+tokens")
        .unwrap()
        .captures(text)
    {
        let n = m.get(1).or(m.get(2)).map(|x| x.as_str()).unwrap_or("0");
        out.insert("tokens".into(), json!(n.parse::<u64>().unwrap_or(0)));
    }
    let complete = out.get("kld_mean").is_some_and(|v| !v.is_null());
    out.insert("complete".into(), json!(complete));
    Value::Object(out)
}

/// The published FLE figures for orientation (measured on the FLE project's own texts, not on yours).
pub fn reference_cards() -> Value {
    json!([
        {"name": "FLE-IQ2_M", "kld": [0.16, 0.163, 0.339], "size": 71617465824u64, "texts": "E / W / K", "source": "sirfyyn/Qwen3.8-Flash-Next-FLE-GGUF"},
        {"name": "FLE-IQ3_XXS", "kld": [0.09, 0.093, 0.227], "size": 79483800128u64, "texts": "E / W / K", "source": "sirfyyn/Qwen3.8-Flash-Next-FLE-GGUF"},
        {"name": "FLE-IQ3_M-REAP448", "kld": [0.08, 0.086, 0.185], "size": 82002102816u64, "texts": "E / W / K", "source": "sirfyyn/Qwen3.8-Flash-Next-FLE-REAP448-GGUF"},
        {"name": "FLE-IQ3_M", "kld": [0.051, 0.05, 0.129], "size": 88872241728u64, "texts": "E / W / K", "source": "sirfyyn/Qwen3.8-Flash-Next-FLE-GGUF"}
    ])
}

/// The result card: what was measured, against what, and how big the file is.
#[allow(clippy::too_many_arguments)]
pub fn card(
    stats: &Value,
    name: &str,
    size: u64,
    params: Option<u64>,
    reference: &str,
    text: &str,
    context: i64,
    chunks: i64,
    tool_version: &str,
) -> Value {
    let bpw = params
        .filter(|p| *p > 0)
        .map(|p| crate::stats::round_to(size as f64 * 8.0 / p as f64, 3));
    let pct = |k: &str| {
        stats[k]
            .as_f64()
            .map(|v| json!(v / 100.0))
            .unwrap_or(Value::Null)
    };
    let g = |k: &str| stats.get(k).cloned().unwrap_or(Value::Null);
    json!({"name": name, "complete": stats["complete"] == true, "size": size, "params": params, "bpw": bpw,
        "kld": g("kld_mean"), "kld_se": g("kld_mean_se"), "kld_99": g("kld_99"), "kld_max": g("kld_max"),
        "top1": pct("top1"), "top1_se": pct("top1_se"), "ppl_ratio": g("ppl_ratio"), "ppl_q": g("ppl_q"), "ppl_base": g("ppl_base"),
        "reference": reference, "text": text, "context": context, "chunks": chunks, "tool": tool_version,
        "note": "Compare only runs with the same text, context, chunks, tool build and hardware."})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_is_parsed() {
        let s = parse("Mean    KLD:   0.012345 ±   0.000456\nMaximum KLD:   1.5\n99.0%   KLD:   0.3\nSame top p: 95.4 ± 0.5 %\nMean PPL(Q)/PPL(base): 1.01 ± 0.002\n");
        assert_eq!(s["kld_mean"], 0.012345);
        assert_eq!(s["kld_mean_se"], 0.000456);
        assert_eq!(s["top1"], 95.4);
        assert_eq!(s["ppl_ratio"], 1.01);
        assert_eq!(s["complete"], true);
        assert_eq!(parse("nothing")["complete"], false);
    }
}
