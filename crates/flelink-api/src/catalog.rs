//! The built-in model catalog (the FLE files on Hugging Face) and the fit estimate for this machine. Port of
//! `fle_studio/catalog.py`. The model list is data (`data/catalog.json`, written from the Python catalog); the fit
//! rule is code.
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const GIB_I: i64 = 1024 * 1024 * 1024;
/// what the OS, the browser and FLE Studio itself keep
pub const OS_RESERVE: i64 = 8 * GIB_I;
/// KV cache at 32k context, compute buffers, prompt cache floor
pub const RUNTIME: i64 = 4 * GIB_I;

fn data() -> &'static Value {
    static D: OnceLock<Value> = OnceLock::new();
    D.get_or_init(|| {
        serde_json::from_str(include_str!("../data/catalog.json")).expect("catalog.json")
    })
}

pub fn reference_machine() -> &'static str {
    data()["reference_machine"].as_str().unwrap()
}
pub fn report_url() -> &'static str {
    data()["report_url"].as_str().unwrap()
}
pub fn models() -> &'static [Value] {
    data()["models"].as_array().unwrap()
}
pub fn by_id(id: &str) -> Option<&'static Value> {
    models().iter().find(|m| m["id"] == id)
}
/// The catalog model that owns a file name (any of its files).
pub fn by_filename(name: &str) -> Option<&'static Value> {
    models().iter().find(|m| {
        m["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == name)
    })
}
pub fn mtp_head() -> &'static Value {
    &data()["mtp_head"]
}
pub fn mtp_vocab() -> &'static Value {
    &data()["mtp_vocab"]
}

/// Will a model of `size` bytes run on this machine? A verdict and the numbers behind it.
/// What the OS keeps: 8 GiB, on a small machine at most a quarter of the RAM (DEVIATIONS.md D13: with a flat 8 GiB an
/// 8 GB Mac could fit nothing that misses the Metal limit; `flelink models` said a 1 GB model fits). The comparison tests
/// with the Python Studio set `FLE_FIT_PYTHON_RULE` and get its rule.
fn os_reserve(ram: i64) -> i64 {
    if std::env::var_os("FLE_FIT_PYTHON_RULE").is_some() {
        OS_RESERVE
    } else {
        OS_RESERVE.min(ram / 4)
    }
}

pub fn fit(size: i64, sys: &Value) -> Value {
    let need = size + RUNTIME;
    let ram = sys["memory"]["total"].as_i64().unwrap_or(0);
    let reserve = os_reserve(ram);
    let gpus = sys["gpus"].as_array().cloned().unwrap_or_default();
    if let Some(apple) = gpus.iter().find(|g| g["unified"].as_bool() == Some(true)) {
        let limit = apple["metal_limit"].as_i64().unwrap_or(0);
        let verdict = if need as f64 <= limit as f64 * 0.95 {
            "good"
        } else if need <= ram - reserve {
            "tight"
        } else {
            "too_big"
        };
        let advice = if verdict == "tight" {
            format!("Above macOS's default GPU memory limit ({:.0} GB). Raising it with `sudo sysctl iogpu.wired_limit_mb={}` lets the whole model run on the GPU (resets at reboot).", limit as f64 / GIB, (ram - reserve) / (1 << 20))
        } else {
            String::new()
        };
        return json!({"verdict": verdict, "kind": "unified", "detail": {"need": need, "gpu_limit": limit, "ram": ram}, "advice": advice});
    }
    let vram = gpus
        .iter()
        .filter_map(|g| g["memory_total"].as_i64())
        .max()
        .unwrap_or(0);
    let usable = (ram - reserve).max(0) + (vram - 2 * GIB_I).max(0);
    let verdict = if need as f64 <= usable as f64 * 0.9 {
        "good"
    } else if need <= usable {
        "tight"
    } else {
        "too_big"
    };
    let advice = match verdict {
        "tight" => "Close other large programs and keep the context at 32k or less.".to_string(),
        "too_big" => format!(
            "Needs about {:.0} GB of RAM and GPU memory together; this machine has {:.0} GB.",
            need as f64 / GIB,
            usable as f64 / GIB
        ),
        _ => String::new(),
    };
    json!({"verdict": verdict, "kind": "split", "detail": {"need": need, "ram": ram, "vram": vram, "usable": usable}, "advice": advice})
}

/// Every catalog entry with its fit on this machine (`catalog.catalog`).
pub fn catalog(sys: &Value) -> Vec<Map<String, Value>> {
    models()
        .iter()
        .map(|m| {
            let mut d = m.as_object().unwrap().clone();
            d.insert("fit".into(), fit(m["size"].as_i64().unwrap_or(0), sys));
            d
        })
        .collect()
}

/// The MTP state of a catalog model for a badge: `ready` (both sidecars there), `missing` or `none`.
pub fn mtp_status(supported: bool, head: bool, vocab: bool) -> Value {
    if !supported {
        return json!({"state": "none", "label": "no MTP", "head": false, "vocab": false});
    }
    let ready = head && vocab;
    json!({"state": if ready { "ready" } else { "missing" }, "label": if ready { "MTP ready" } else { "MTP files missing" }, "head": head, "vocab": vocab})
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    #[test]
    fn a_small_model_is_not_too_big_for_an_8_gb_mac() {
        if std::env::var_os("FLE_FIT_PYTHON_RULE").is_some() {
            return;
        }
        let mac = json!({"memory": {"total": 8 * GIB_I}, "gpus": [{"unified": true, "metal_limit": 5_755_256_176i64}]});
        assert_ne!(fit(1_320_000_000, &mac)["verdict"], "too_big", "Llama 3.2 1B Q8");
        assert_eq!(fit(6 * GIB_I, &mac)["verdict"], "too_big");
        let titan = json!({"memory": {"total": 121 * GIB_I}, "gpus": [{"memory_total": 32 * GIB_I}]});
        assert_eq!(fit(89 * GIB_I, &titan)["verdict"], "good");
    }
}
