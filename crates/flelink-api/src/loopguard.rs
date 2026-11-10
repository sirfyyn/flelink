//! Ends an answer that repeats itself, for programs on `/v1` (coding agents). The rules are those of
//! `web/studio/js/loop-guard.js`, which does this for the chat page; both are tested on `tests/fixtures/loop-tails.json`.
//! Only the text and the thinking are read, never tool calls: a tool may well write something repetitive on purpose.
use serde_json::{json, Value};

const MIN_UNIT: usize = 12;
const MAX_UNIT: usize = 480;
const MIN_REPEATS: usize = 5;
const MIN_SPAN: usize = 400;
const WINDOW: usize = 4800;

const SOFT_WINDOW: usize = 4000;
const SOFT_MIN_PARTS: usize = 24;
const SOFT_MIN_SEG: usize = 25;
const SOFT_MIN_COUNT: usize = 14;
const SOFT_MIN_SHARE: f64 = 0.4;

/// The text is checked after this many new characters.
const CHECK_EVERY: usize = 128;

fn sentences(t: &[char]) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut i = 0;
    while i < t.len() {
        let after_stop = i > 0 && matches!(t[i - 1], '.' | '!' | '?' | ';');
        if t[i].is_whitespace() && after_stop {
            while i < t.len() && t[i].is_whitespace() {
                i += 1;
            }
            out.push(std::mem::take(&mut cur));
        } else if t[i] == '\n' {
            while i < t.len() && t[i] == '\n' {
                i += 1;
            }
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(t[i]);
            i += 1;
        }
    }
    out.push(cur);
    out
}

/// A sentence with every number turned into `#` and every run of spaces into one.
fn shape(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        let digit_at = |k: usize| c.get(k).is_some_and(char::is_ascii_digit);
        if digit_at(i) || (c[i] == '-' && digit_at(i + 1)) {
            i += 1;
            while digit_at(i) {
                i += 1;
            }
            if c.get(i) == Some(&'.') && digit_at(i + 1) {
                i += 1;
                while digit_at(i) {
                    i += 1;
                }
            }
            out.push('#');
        } else if c[i].is_whitespace() {
            while i < c.len() && c[i].is_whitespace() {
                i += 1;
            }
            out.push(' ');
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

fn soft_repeats(text: &[char]) -> bool {
    let t = &text[text.len().saturating_sub(SOFT_WINDOW)..];
    let parts: Vec<String> = sentences(t)
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| s.chars().count() >= SOFT_MIN_SEG)
        .collect();
    if parts.len() < SOFT_MIN_PARTS {
        return false;
    }
    let mut count = std::collections::HashMap::new();
    let mut top = 0;
    for p in &parts {
        let n = count.entry(shape(p)).or_insert(0usize);
        *n += 1;
        top = top.max(*n);
    }
    top >= SOFT_MIN_COUNT && top as f64 / parts.len() as f64 >= SOFT_MIN_SHARE
}

fn repeats_itself(text: &[char]) -> bool {
    if soft_repeats(text) {
        return true;
    }
    let t = &text[text.len().saturating_sub(WINDOW)..];
    if t.len() < MIN_SPAN {
        return false;
    }
    for unit in MIN_UNIT..=MAX_UNIT {
        if unit * MIN_REPEATS > t.len() {
            break;
        }
        let piece = &t[t.len() - unit..];
        if piece.iter().all(|c| c.is_whitespace()) {
            continue;
        }
        let mut n = 1;
        while t.len() >= unit * (n + 1) && &t[t.len() - unit * (n + 1)..t.len() - unit * n] == piece
        {
            n += 1;
        }
        if n >= MIN_REPEATS && n * unit >= MIN_SPAN {
            return true;
        }
    }
    false
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Format {
    OpenAi,
    Anthropic,
}

/// Where to cut a stream: keep this many bytes of the chunk, then send `close` so the client sees a normal end.
#[derive(Debug)]
pub struct Stop {
    pub keep: usize,
    pub close: Vec<u8>,
}

/// Follows one streamed answer.
pub struct Watch {
    format: Format,
    line: Vec<u8>,
    tail: Vec<char>,
    unchecked: usize,
    seen: usize,
    last: Value,
    block: i64,
}

impl Watch {
    pub fn new(format: Format) -> Self {
        Watch {
            format,
            line: vec![],
            tail: vec![],
            unchecked: 0,
            seen: 0,
            last: json!({}),
            block: 0,
        }
    }

    /// A chunk of the stream, as it came. `Some` when the answer has fallen into a loop.
    pub fn feed(&mut self, chunk: &[u8]) -> Option<Stop> {
        let mut start = 0;
        for (i, b) in chunk.iter().enumerate() {
            if *b != b'\n' {
                continue;
            }
            self.line.extend_from_slice(&chunk[start..i]);
            start = i + 1;
            let line = std::mem::take(&mut self.line);
            if self.read_line(&line) {
                return Some(Stop {
                    keep: start,
                    close: self.close(),
                });
            }
        }
        self.line.extend_from_slice(&chunk[start..]);
        None
    }

    fn read_line(&mut self, line: &[u8]) -> bool {
        let Some(data) = line.strip_prefix(b"data:") else {
            return false;
        };
        let Ok(v) = serde_json::from_slice::<Value>(data) else {
            return false; // [DONE]
        };
        let text = match self.format {
            Format::OpenAi => {
                let d = &v["choices"][0]["delta"];
                let t = [d["content"].as_str(), d["reasoning_content"].as_str()];
                self.last = v.clone();
                t.into_iter().flatten().collect::<String>()
            }
            Format::Anthropic if v["type"] == "content_block_delta" => {
                self.block = v["index"].as_i64().unwrap_or(self.block);
                let d = &v["delta"];
                let t = [d["text"].as_str(), d["thinking"].as_str()];
                t.into_iter().flatten().collect::<String>()
            }
            Format::Anthropic => String::new(),
        };
        let before = self.tail.len();
        self.tail.extend(text.chars());
        let added = self.tail.len() - before;
        self.seen += added;
        self.unchecked += added;
        if self.tail.len() > WINDOW + CHECK_EVERY {
            self.tail.drain(..self.tail.len() - WINDOW);
        }
        if self.unchecked < CHECK_EVERY {
            return false;
        }
        self.unchecked = 0;
        repeats_itself(&self.tail)
    }

    /// The end of the stream, as the format has it, with the reason a client knows for "cut short". It starts with the
    /// blank line that ends the event before it.
    fn close(&self) -> Vec<u8> {
        match self.format {
            Format::OpenAi => {
                let mut end = self.last.clone();
                end["choices"] = json!([{"index": 0, "delta": {}, "finish_reason": "length"}]);
                if let Some(o) = end.as_object_mut() {
                    o.remove("timings");
                }
                format!("\ndata: {end}\n\ndata: [DONE]\n\n").into_bytes()
            }
            Format::Anthropic => {
                let stop = json!({"type": "content_block_stop", "index": self.block});
                let delta = json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "max_tokens", "stop_sequence": null},
                    "usage": {"output_tokens": self.seen / 4},
                });
                format!(
                    "\nevent: content_block_stop\ndata: {stop}\n\nevent: message_delta\ndata: {delta}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
                )
                .into_bytes()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn it_agrees_with_the_chat_page_on_real_answers() {
        let fx: Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/loop-tails.json")).unwrap();
        for (name, v) in fx.as_object().unwrap() {
            let tail = chars(v["tail"].as_str().unwrap());
            assert_eq!(
                repeats_itself(&tail),
                name.starts_with("loop_soft"),
                "{name}"
            );
        }
        assert!(repeats_itself(&chars(
            &"I should check the answer again. ".repeat(20)
        )));
        assert!(!repeats_itself(&chars(&"hello ".repeat(3))));
        assert!(!repeats_itself(&chars(&"\n".repeat(900))));
    }

    fn openai(text: &str) -> String {
        let chunk = json!({"id": "c1", "model": "m", "choices": [{"index": 0, "delta": {"content": text}}]});
        format!("data: {chunk}\n\n")
    }

    #[test]
    fn an_openai_stream_is_cut_with_a_normal_end() {
        let mut w = Watch::new(Format::OpenAi);
        let chunk = openai(&"I should check the answer again. ".repeat(20));
        let stop = w.feed(chunk.as_bytes()).expect("a loop");
        let close = String::from_utf8(stop.close).unwrap();
        assert!(close.starts_with('\n'), "{close}");
        assert!(close.contains("\"finish_reason\":\"length\""), "{close}");
        assert!(close.contains("\"id\":\"c1\""), "{close}");
        assert!(close.ends_with("data: [DONE]\n\n"), "{close}");
    }

    #[test]
    fn a_line_split_over_two_chunks_is_read_whole() {
        let mut w = Watch::new(Format::OpenAi);
        let chunk = openai(&"I should check the answer again. ".repeat(20));
        let (a, b) = chunk.as_bytes().split_at(chunk.len() / 2);
        assert!(w.feed(a).is_none());
        assert!(w.feed(b).is_some());
    }

    #[test]
    fn an_anthropic_stream_ends_with_its_own_events() {
        let mut w = Watch::new(Format::Anthropic);
        let event = json!({"type": "content_block_delta", "index": 2,
            "delta": {"type": "text_delta", "text": "I should check the answer again. ".repeat(20)}});
        let chunk = format!("event: content_block_delta\ndata: {event}\n\n");
        let close = String::from_utf8(w.feed(chunk.as_bytes()).expect("a loop").close).unwrap();
        assert!(close.contains("\"index\":2"), "{close}");
        assert!(close.contains("\"stop_reason\":\"max_tokens\""), "{close}");
        assert!(close.contains("message_stop"), "{close}");
    }

    #[test]
    fn tool_calls_and_varied_text_are_left_alone() {
        let mut w = Watch::new(Format::Anthropic);
        let event = json!({"type": "content_block_delta", "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "row, row, row. ".repeat(100)}});
        let chunk = format!("data: {event}\n\n");
        assert!(w.feed(chunk.as_bytes()).is_none());
        let mut w = Watch::new(Format::OpenAi);
        let varied: String = (0..200)
            .map(|i| {
                format!(
                    "Part {i} of the plan is called {}. ",
                    "abcdefgh".repeat(i % 5 + 1)
                )
            })
            .collect();
        assert!(w.feed(openai(&varied).as_bytes()).is_none());
    }
}
