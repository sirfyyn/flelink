//! `flelink connect`: how coding agents and chat apps reach the model FLE Link serves. The engine (llama-server, also the
//! FLE build) answers three API shapes on the same address: OpenAI Chat Completions (`/v1/chat/completions`), OpenAI
//! Responses (`/v1/responses`) and Anthropic Messages (`/v1/messages`), so each tool gets the shape it speaks natively.
//! No key is checked on this machine; the tools that insist on one get a placeholder.
use std::time::Duration;

/// One tool: where its setting goes and the text to put there.
pub struct Snippet {
    pub id: &'static str,
    pub title: &'static str,
    /// a file, or "shell" for lines to run before starting the tool
    pub place: &'static str,
    pub text: String,
}

/// The tools FLE Link writes a setting for; `base` without a trailing slash and without `/v1`, `model` as the API names it.
pub fn snippets(base: &str, model: &str) -> Vec<Snippet> {
    let base = base.trim_end_matches('/').trim_end_matches("/v1");
    // the Studio names a model by its title ("Llama 3.2 1B Instruct"): quoted for the shell, escaped for JSON and TOML
    let sh = if model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-:/".contains(c))
    {
        model.to_string()
    } else {
        format!("'{}'", model.replace('\'', "'\\''"))
    };
    let js = serde_json::to_string(model).unwrap_or_default();
    let js_inner = &js[1..js.len() - 1];
    let model = js_inner;
    vec![
        Snippet {
            id: "claude",
            title: "Claude Code",
            place: "shell",
            text: format!(
                "export ANTHROPIC_BASE_URL={base}\nexport ANTHROPIC_AUTH_TOKEN=flelink\nexport ANTHROPIC_MODEL={sh}\n\
                 export ANTHROPIC_DEFAULT_HAIKU_MODEL={sh}\nclaude"
            ),
        },
        Snippet {
            id: "codex",
            title: "Codex CLI",
            place: "~/.codex/config.toml",
            text: format!(
                "model = \"{model}\"\nmodel_provider = \"flelink\"\n\n[model_providers.flelink]\nname = \"FLE Link\"\n\
                 base_url = \"{base}/v1\"\nwire_api = \"responses\""
            ),
        },
        Snippet {
            id: "opencode",
            title: "OpenCode",
            place: "~/.config/opencode/opencode.json",
            text: format!(
                "{{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"model\": \"flelink/{model}\",\n  \"provider\": {{\n    \
                 \"flelink\": {{\n      \"npm\": \"@ai-sdk/openai-compatible\",\n      \"name\": \"FLE Link\",\n      \
                 \"options\": {{ \"baseURL\": \"{base}/v1\" }},\n      \"models\": {{ \"{model}\": {{ \"name\": \"{model}\" }} }}\n    \
                 }}\n  }}\n}}"
            ),
        },
        Snippet {
            id: "pi",
            title: "pi",
            place: "~/.pi/agent/models.json",
            text: format!(
                "{{\n  \"providers\": {{\n    \"flelink\": {{\n      \"baseUrl\": \"{base}/v1\",\n      \"api\": \"openai-completions\",\n      \
                 \"apiKey\": \"flelink\",\n      \"models\": [{{ \"id\": \"{model}\" }}]\n    }}\n  }}\n}}"
            ),
        },
        Snippet {
            id: "api",
            title: "Any other app",
            place: "settings of the app",
            text: format!(
                "OpenAI-compatible:    {base}/v1   (Chat Completions and Responses)\nAnthropic-compatible: {base}   (Messages)\n\
                 Model: {model}\nAPI key: none needed (any text where an app insists)"
            ),
        },
    ]
}

/// What is serving on this machine: the Studio (port 8642 and the next ones) with a loaded model, else an engine that
/// `flelink run` started (port 8080 and the next ones). The base address and the model id the API reports.
pub fn find_running() -> Option<(String, String)> {
    for port in (8642..8646).chain(8080..8090) {
        let base = format!("http://127.0.0.1:{port}");
        if let Some(m) = model_at(&base) {
            return Some((base, m));
        }
    }
    None
}

/// The model id an OpenAI-compatible server at `base` reports first (`GET /v1/models`).
pub fn model_at(base: &str) -> Option<String> {
    let c = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(700))
        .build()
        .ok()?;
    let base = base.trim_end_matches('/').trim_end_matches("/v1");
    let v: serde_json::Value = c
        .get(format!("{base}/v1/models"))
        .send()
        .ok()?
        .json()
        .ok()?;
    v["data"][0]["id"].as_str().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_gets_the_address_in_the_shape_it_expects() {
        let s = snippets("http://127.0.0.1:8080/v1/", "qwen");
        let get = |id: &str| s.iter().find(|x| x.id == id).unwrap().text.clone();
        // Anthropic clients add /v1/messages themselves: the base has no /v1
        assert!(get("claude").contains("ANTHROPIC_BASE_URL=http://127.0.0.1:8080\n"));
        assert!(get("codex").contains("base_url = \"http://127.0.0.1:8080/v1\""));
        assert!(get("codex").contains("wire_api = \"responses\""));
        let oc: serde_json::Value = serde_json::from_str(&get("opencode")).unwrap();
        assert_eq!(
            oc["provider"]["flelink"]["options"]["baseURL"],
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(oc["model"], "flelink/qwen");
        let pi: serde_json::Value = serde_json::from_str(&get("pi")).unwrap();
        assert_eq!(pi["providers"]["flelink"]["models"][0]["id"], "qwen");
        // a title with spaces stays one word for the shell, a quote does not break the JSON
        let s = snippets("http://127.0.0.1:8642", "Llama 3.2 1B \"Instruct\"");
        assert!(s[0]
            .text
            .contains("ANTHROPIC_MODEL='Llama 3.2 1B \"Instruct\"'"));
        let pi: serde_json::Value = serde_json::from_str(&s[3].text).unwrap();
        assert_eq!(
            pi["providers"]["flelink"]["models"][0]["id"],
            "Llama 3.2 1B \"Instruct\""
        );
    }
}
