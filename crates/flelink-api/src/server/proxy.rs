//! The `/v1` proxy to the engine and the forwarding to another host, with what the Engine page records about each answer.
use super::*;

pub(super) async fn forward(
    st: &Ctx,
    parts: axum::http::request::Parts,
    body: Body,
    head: bool,
) -> Response<Body> {
    let path = parts.uri.path().to_string();
    let Some(rest) = path.strip_prefix("/h/") else {
        return error_json(404, "Not found", head);
    };
    let (id, rest) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let rest = rest.to_string();
    let ok_id = id.len() == 12
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !ok_id {
        return error_json(404, "Not found", head);
    }
    let h = match st.hosts.get(id) {
        Ok(h) => h,
        Err(e) => return error_json(e.0, &e.1, head),
    };
    let app_page = rest.starts_with("/app-frame/") && parts.method == Method::GET;
    if !(rest.starts_with("/api/") || rest.starts_with("/v1/") || app_page)
        || crate::remote::never_forwarded(&rest)
    {
        return error_json(403, "This path is not passed on to a host.", head);
    }
    if h["token"].as_str().is_none_or(|t| t.is_empty()) {
        return send_json(
            424,
            &json!({"error": {"message": "This host is not paired.", "code": "unpaired"}}),
            head,
        );
    }
    let bytes = match to_bytes(body, MAX_CHAT).await {
        Ok(b) => b.to_vec(),
        Err(_) => return error_json(413, "Request too large", head),
    };
    let target = match parts.uri.query() {
        Some(q) => format!("{rest}?{q}"),
        None => rest.clone(),
    };
    let method = parts.method.as_str().to_string();
    let pass: Vec<(String, String)> = ["content-type", "accept", "x-fle-confirm"]
        .iter()
        .filter_map(|n| {
            parts
                .headers
                .get(*n)
                .and_then(|v| v.to_str().ok())
                .map(|v| (n.to_string(), v.to_string()))
        })
        .collect();
    let stream_like = rest.starts_with("/v1/");
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(16);
    let (meta_tx, meta_rx) = tokio::sync::oneshot::channel::<
        Result<(u16, Vec<(String, String)>, Option<Vec<u8>>), Value>,
    >();
    let st2 = st.clone();
    let rest2 = rest.clone();
    std::thread::spawn(move || {
        let timeout = Duration::from_secs(if stream_like { 900 } else { 60 });
        let pinned = h["fingerprint"].as_str().unwrap_or("").to_string();
        let mirror_dir = st2.hosts.mirror_dir(h["id"].as_str().unwrap_or(""));
        let fail = |e: String, seen: String| -> Value {
            let (code, message, extra) = crate::remote::describe_failure(&e, &seen, &pinned);
            if code == "unreachable" && h["mirror"] == true && method == "GET" {
                if let Some(v) = crate::remote::mirror_read(&mirror_dir, &rest2) {
                    return json!({"mirror": v});
                }
            }
            let mut err = json!({"message": message, "code": code});
            for (k, v) in extra.as_object().into_iter().flatten() {
                err[k] = v.clone();
            }
            json!({"error": err})
        };
        let (base, agent, seen) = match st2.hosts.connection(&h, timeout) {
            Ok(c) => c,
            Err(e) => {
                let _ = meta_tx.send(Err(fail(e, String::new())));
                return;
            }
        };
        let mut req = agent.request(&method, &format!("{base}{target}")).set(
            "Authorization",
            &format!("Bearer {}", h["token"].as_str().unwrap_or("")),
        );
        for (k, v) in &pass {
            req = req.set(k, v);
        }
        let result = if bytes.is_empty() && (method == "GET" || method == "HEAD") {
            req.call()
        } else {
            req.send_bytes(&bytes)
        };
        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => {
                let s = seen.lock().unwrap().clone();
                let _ = meta_tx.send(Err(fail(e.to_string(), s)));
                return;
            }
        };
        let status = resp.status();
        let mut headers: Vec<(String, String)> = vec![];
        for n in [
            "Content-Type",
            "Content-Disposition",
            "Content-Range",
            "Accept-Ranges",
            "Content-Security-Policy",
            "Permissions-Policy",
        ] {
            if let Some(v) = resp.header(n) {
                headers.push((n.to_string(), v.to_string()));
            }
        }
        let sse = resp
            .header("Content-Type")
            .is_some_and(|c| c.contains("text/event-stream"));
        let size: Option<usize> = resp.header("Content-Length").and_then(|v| v.parse().ok());
        let mut reader = resp.into_reader();
        if !sse && size.is_some_and(|n| n <= 8 * 1024 * 1024) {
            let mut data = vec![];
            let _ = std::io::Read::read_to_end(&mut reader, &mut data);
            if status == 200 && h["mirror"] == true {
                crate::remote::mirror_save(&mirror_dir, &rest2, &data);
            }
            let _ = meta_tx.send(Ok((status, headers, Some(data))));
            return;
        }
        let _ = meta_tx.send(Ok((status, headers, None)));
        let mut buf = vec![0u8; 65536];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx
                        .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                        .is_err()
                    {
                        break; // the page stopped reading; closing the host side ends the generation there too
                    }
                }
            }
        }
    });
    match meta_rx.await {
        Err(_) => error_json(502, "The host did not answer", head),
        Ok(Err(v)) if v.get("mirror").is_some() => send_json(200, &v["mirror"], head),
        Ok(Err(v)) => send_json(502, &v, head),
        Ok(Ok((401, _, _))) => send_json(
            424,
            &json!({"error": {"message": "The host does not accept this client any more. It may have been revoked: pair again.", "code": "unpaired"}}),
            head,
        ),
        Ok(Ok((status, headers, Some(data)))) => {
            let ctype = headers
                .iter()
                .find(|(k, _)| k == "Content-Type")
                .map_or("application/octet-stream".to_string(), |(_, v)| v.clone());
            let extra: Vec<(&str, &str)> = headers
                .iter()
                .filter(|(k, _)| k != "Content-Type")
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            send(status, data, &ctype, &extra, head)
        }
        Ok(Ok((status, headers, None))) => {
            let mut r = Response::new(Body::from_stream(
                tokio_stream::wrappers::ReceiverStream::new(rx),
            ));
            *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            let hm = r.headers_mut();
            for (k, v) in &headers {
                if let (Ok(n), Ok(v)) = (
                    axum::http::HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(v),
                ) {
                    hm.insert(n, v);
                }
            }
            hm.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            hm.insert("x-accel-buffering", HeaderValue::from_static("no"));
            hm.insert(header::CONNECTION, HeaderValue::from_static("close"));
            r
        }
    }
}

/// Refuse a chat request that asks for tools and for a forced answer format together (`screen_proxy_body`).
pub(super) fn screen_proxy_body(path: &str, body: &[u8]) -> Result<(), String> {
    let p = path.split('?').next().unwrap_or("").trim_end_matches('/');
    if body.is_empty()
        || !p.ends_with("/chat/completions")
        || !body.windows(7).any(|w| w == b"\"tools\"")
    {
        return Ok(());
    }
    let Ok(Value::Object(doc)) = serde_json::from_slice::<Value>(body) else {
        return Ok(());
    }; // the engine will say what is wrong
    if !doc.get("tools").is_some_and(crate::settings::py_truthy)
        || doc.get("tool_choice") == Some(&json!("none"))
    {
        return Ok(());
    }
    let mut clash = vec![];
    for key in ["response_format", "json_schema", "grammar"] {
        let v = doc.get(key).cloned().unwrap_or(Value::Null);
        if key == "response_format" {
            if let Value::Object(o) = &v {
                if matches!(o.get("type"), None | Some(Value::Null))
                    || o.get("type") == Some(&json!("text"))
                {
                    continue;
                }
            }
        }
        if !(v.is_null() || v == json!("") || v == json!({}) || v == json!([])) {
            clash.push(key);
        }
    }
    if clash.is_empty() {
        return Ok(());
    }
    Err(format!("A chat request must not carry both tools and {}: the model would not call the tools and would answer with substitute JSON. Send them in separate requests.", clash.join(", ")))
}

enum Upstream {
    Failed(String),
    Answer {
        status: u16,
        ctype: String,
        stream: bool,
    },
}

/// Pass /v1/* to the engine; streams (SSE) are forwarded as they arrive.
pub(super) async fn proxy(
    st: &Ctx,
    parts: axum::http::request::Parts,
    body: Body,
    head: bool,
) -> Response<Body> {
    let (state, base, key) = st.engine.target();
    if !(state == "ready" || state == "attached") || base.is_empty() {
        return send_json(
            503,
            &json!({"error": {"message": "No model is loaded. Load one in FLE Link first.", "type": "unavailable_error", "code": 503}}),
            head,
        );
    }
    let declared: usize = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if declared > MAX_PROXY {
        return error_json(413, "Request too large", head);
    }
    let Ok(bytes) = to_bytes(body, MAX_PROXY).await else {
        return error_json(413, "Request too large", head);
    };
    let path_q = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());
    if let Err(m) = screen_proxy_body(&path_q, &bytes) {
        return error_json(400, &m, head);
    }
    let method = parts.method.to_string();
    let ctype = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let accept = parts
        .headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*")
        .to_string();
    let live = st.live.clone();
    live.touch_api();
    // for the list of requests: which API, which program, how long
    let api = ["chat/completions", "messages", "responses", "completions"]
        .into_iter()
        .find(|k| parts.uri.path().ends_with(&format!("/v1/{k}")))
        .map(|k| match k {
            "messages" => "anthropic",
            "responses" => "responses",
            _ => "openai",
        });
    let client = client_name(
        parts
            .headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    );
    // an answer in a loop is ended for programs on /v1; the chat page (it sends an Origin) has its own guard
    let loop_format = if parts.headers.contains_key(header::ORIGIN)
        || std::env::var_os("FLE_NO_LOOP_GUARD").is_some_and(|x| !x.is_empty())
    {
        None
    } else if parts.uri.path().ends_with("/v1/chat/completions") {
        Some(crate::loopguard::Format::OpenAi)
    } else if parts.uri.path().ends_with("/v1/messages") {
        Some(crate::loopguard::Format::Anthropic)
    } else {
        None
    };
    let t0 = std::time::Instant::now();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(16);
    let (meta_tx, meta_rx) = tokio::sync::oneshot::channel::<Upstream>();
    std::thread::spawn(move || {
        let url = format!("{}{}", base.trim_end_matches('/'), path_q);
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(Duration::from_secs(900))
            .timeout_read(Duration::from_secs(900))
            .build();
        let mut req = agent
            .request(&method, &url)
            .set("Content-Type", &ctype)
            .set("Accept", &accept);
        if !key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let result = if bytes.is_empty() && method == "GET" {
            req.call()
        } else {
            req.send_bytes(&bytes)
        };
        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => {
                use std::error::Error;
                let mut cur: &dyn Error = &e;
                while let Some(s) = cur.source() {
                    cur = s;
                }
                let _ = meta_tx.send(Upstream::Failed(cur.to_string()));
                return;
            }
        };
        let rtype = resp
            .header("Content-Type")
            .unwrap_or("application/json")
            .to_string();
        let stream = rtype.contains("text/event-stream");
        let status = resp.status();
        let _ = meta_tx.send(Upstream::Answer {
            status: resp.status(),
            ctype: rtype,
            stream,
        });
        let mut reader = resp.into_reader();
        let mut all: Vec<u8> = vec![];
        let mut buf = vec![0u8; 65536];
        let mut watch = loop_format
            .filter(|_| stream)
            .map(crate::loopguard::Watch::new);
        let mut stopped_loop = false;
        loop {
            let n = match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if stream {
                let stop = watch.as_mut().and_then(|w| w.feed(&buf[..n]));
                let keep = stop.as_ref().map_or(n, |s| s.keep);
                if tx
                    .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..keep])))
                    .is_err()
                {
                    break; // the browser stopped reading (Stop button); closing our side cancels the generation
                }
                all.extend_from_slice(&buf[..keep]);
                if all.len() > 262144 {
                    all.drain(..all.len() - 262144);
                }
                if let Some(s) = stop {
                    let _ = tx.blocking_send(Ok(bytes::Bytes::from(s.close)));
                    stopped_loop = true;
                    break; // closing our side cancels the generation
                }
            } else {
                all.extend_from_slice(&buf[..n]);
            }
        }
        drop(reader);
        let extra = |status: u16| {
            let mut m = Map::new();
            m.insert("api".into(), json!(api));
            m.insert("client".into(), json!(client));
            m.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
            m.insert("status".into(), json!(status));
            if stopped_loop {
                m.insert("stopped_loop".into(), json!(true));
            }
            m
        };
        if api.is_some() && method == "POST" {
            if stream {
                record_stream(&live, &all, extra(status));
            } else {
                record_data(&live, &all, extra(status));
            }
        }
        if !stream {
            let _ = tx.blocking_send(Ok(bytes::Bytes::from(all)));
        }
    });
    let mut rx = rx;
    match meta_rx.await {
        Err(_) => error_json(502, "The engine did not answer", head),
        Ok(Upstream::Failed(why)) => send_json(
            502,
            &json!({"error": {"message": format!("The engine did not answer: {why}"), "type": "bad_gateway"}}),
            head,
        ),
        Ok(Upstream::Answer {
            status,
            ctype,
            stream: false,
        }) => {
            let mut data = vec![];
            while let Some(Ok(c)) = rx.recv().await {
                data.extend_from_slice(&c);
            }
            send(status, data, &ctype, &[], head)
        }
        Ok(Upstream::Answer {
            status,
            ctype,
            stream: true,
        }) => {
            let mut r = Response::new(Body::from_stream(
                tokio_stream::wrappers::ReceiverStream::new(rx),
            ));
            *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&ctype)
                    .unwrap_or(HeaderValue::from_static("text/event-stream")),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            h.insert("x-accel-buffering", HeaderValue::from_static("no"));
            h.insert(header::CONNECTION, HeaderValue::from_static("close"));
            r
        }
    }
}

pub(super) fn record_data(live: &Live, data: &[u8], extra: Map<String, Value>) {
    if let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(data) {
        let t = o
            .get("timings")
            .filter(|t| crate::settings::py_truthy(t))
            .cloned()
            .unwrap_or(json!({}));
        // the Responses format keeps its usage inside `response` when it is an event; as a plain answer at the top
        live.record_request(&t, o.get("usage"), extra);
    }
}

/// A streamed answer: the engine timings of the OpenAI format, or the token counts the Anthropic format
/// (`message_start` input, `message_delta` output) and the Responses format (`response.completed`) send at the end.
pub(super) fn record_stream(live: &Live, tail: &[u8], extra: Map<String, Value>) {
    let text = String::from_utf8_lossy(tail).to_string();
    for line in text.lines().rev() {
        if line.starts_with("data:") && line.contains("\"timings\"") {
            let Ok(obj) = serde_json::from_str::<Value>(line[5..].trim()) else {
                continue;
            };
            live.record_request(
                obj.get("timings").unwrap_or(&json!({})),
                obj.get("usage"),
                extra,
            );
            return;
        }
    }
    let mut usage = Map::new();
    for line in text.lines() {
        let Some(d) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(obj) = serde_json::from_str::<Value>(d.trim()) else {
            continue;
        };
        for u in [
            &obj["usage"],
            &obj["message"]["usage"],
            &obj["response"]["usage"],
        ] {
            if let Some(m) = u.as_object() {
                for (k, v) in m {
                    if v.as_f64().is_some_and(|x| x > 0.0) || !usage.contains_key(k) {
                        usage.insert(k.clone(), v.clone());
                    }
                }
            }
        }
    }
    if !usage.is_empty() {
        live.record_request(&json!({}), Some(&Value::Object(usage)), extra);
    }
}

/// A short name for the program behind a User-Agent: `claude-cli/2.1 (external, cli)` is `claude-cli`, a browser is
/// `browser` (the Studio page itself, or a web app).
pub(super) fn client_name(ua: &str) -> String {
    if ua.is_empty() {
        return String::new();
    }
    if ua.starts_with("Mozilla/") {
        return "browser".into();
    }
    ua.split(['/', ' ', ';', '('])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c))
        .take(32)
        .collect()
}
