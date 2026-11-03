//! Demo mode: drive the Studio page from outside, to record it (3 Nov 2026). Off unless the Studio starts with `--demo`
//! (or `FLE_DEMO=1`, for the desktop app). Then the page asks `/api/demo/next` twice a second for the next step and does
//! it the way a person would (types the message, sends it, opens a page, presses Preview), and reports whether an answer
//! is still being written. `flelink demo …` puts the steps in. Nothing here runs code: a step is one of a few fixed
//! actions with plain values, and the queue lives in memory only.

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// What a step may do, and the values it may carry.
const AKTIONEN: &[&str] = &["chat", "seite", "vorschau", "scroll", "neu"];
const QUEUE_MAX: usize = 32;

pub struct Demo {
    on: AtomicBool,
    queue: Mutex<VecDeque<Value>>,
    busy: AtomicBool,
    /// when the page last asked (to tell whether a page is open at all)
    seen: Mutex<Option<Instant>>,
}

impl Demo {
    pub fn new(on: bool) -> Self {
        Demo {
            on: AtomicBool::new(on || std::env::var("FLE_DEMO").map(|v| v == "1").unwrap_or(false)),
            queue: Mutex::new(VecDeque::new()),
            busy: AtomicBool::new(false),
            seen: Mutex::new(None),
        }
    }

    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> Value {
        let seen = self.seen.lock().unwrap().map(|t| t.elapsed().as_secs_f64());
        json!({"on": self.on(), "busy": self.busy.load(Ordering::Relaxed),
               "queue": self.queue.lock().unwrap().len(), "page_seen_s": seen})
    }

    /// A step from outside. Refused when the demo mode is off, the action is unknown or the queue is full.
    pub fn push(&self, step: &Value) -> Result<Value, String> {
        if !self.on() {
            return Err("the demo mode is off (start the Studio with --demo or FLE_DEMO=1)".into());
        }
        let a = step.get("aktion").and_then(Value::as_str).unwrap_or("");
        if !AKTIONEN.contains(&a) {
            return Err(format!(
                "unknown action {a:?} (chat, seite, vorschau, scroll, neu)"
            ));
        }
        let mut s = json!({"aktion": a});
        match a {
            "chat" => {
                let text = step
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if text.is_empty() || text.len() > 20_000 {
                    return Err("chat needs a text (at most 20,000 characters)".into());
                }
                s["text"] = json!(text);
                s["tippen_ms"] = json!(step
                    .get("tippen_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(12)
                    .min(200));
                if let Some(m) = step.get("max_tokens").and_then(Value::as_u64) {
                    s["max_tokens"] = json!(m.min(131_072));
                }
                if let Some(t) = step.get("temperature").and_then(Value::as_f64) {
                    s["temperature"] = json!(t.clamp(0.0, 2.0));
                }
            }
            "seite" => {
                let p = step.get("pfad").and_then(Value::as_str).unwrap_or("/");
                if !p.starts_with('/') || p.contains("//") || p.contains("..") || p.len() > 80 {
                    return Err("seite needs a path of this page like /engine".into());
                }
                s["pfad"] = json!(p);
            }
            "scroll" => {
                s["dy"] = json!(step
                    .get("dy")
                    .and_then(Value::as_i64)
                    .unwrap_or(600)
                    .clamp(-20_000, 20_000));
            }
            _ => {}
        }
        let mut q = self.queue.lock().unwrap();
        if q.len() >= QUEUE_MAX {
            return Err("the queue is full".into());
        }
        q.push_back(s);
        Ok(json!({"ok": true, "queue": q.len()}))
    }

    /// The page asks for its next step and says whether an answer is being written.
    pub fn next(&self, busy: Option<bool>) -> Value {
        *self.seen.lock().unwrap() = Some(Instant::now());
        if let Some(b) = busy {
            self.busy.store(b, Ordering::Relaxed);
        }
        if !self.on() {
            return json!({"on": false});
        }
        match self.queue.lock().unwrap().pop_front() {
            Some(s) => json!({"on": true, "schritt": s}),
            None => json!({"on": true}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_refuses_and_on_keeps_only_known_actions_with_plain_values() {
        let aus = Demo::new(false);
        if std::env::var("FLE_DEMO").is_err() {
            assert!(aus.push(&json!({"aktion": "chat", "text": "hi"})).is_err());
        }
        let d = Demo::new(true);
        assert!(d.push(&json!({"aktion": "eval", "code": "x"})).is_err());
        assert!(d
            .push(&json!({"aktion": "seite", "pfad": "https://evil"}))
            .is_err());
        assert!(d.push(&json!({"aktion": "chat", "text": "  "})).is_err());
        d.push(&json!({"aktion": "chat", "text": "Hallo", "max_tokens": 3000, "temperature": 0, "extra": 1})).unwrap();
        d.push(&json!({"aktion": "seite", "pfad": "/engine"}))
            .unwrap();
        let s = d.next(Some(true));
        assert_eq!(s["schritt"]["text"], "Hallo");
        assert_eq!(s["schritt"]["max_tokens"], 3000);
        assert!(s["schritt"].get("extra").is_none());
        assert_eq!(d.status()["busy"], true);
        assert_eq!(d.next(None)["schritt"]["pfad"], "/engine");
        assert!(d.next(None).get("schritt").is_none());
    }
}
