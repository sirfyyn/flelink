//! The GitHub sign-in of FLE Link (Max 02.11.2026: "man soll seine anmeldung in der fle link app hinterlegen können, damit
//! das modell was man nutzt direkt einen gpat oder so hat fürs arbeiten in repos").
//!
//! A token (a fine-grained personal access token, or a classic one) is checked against GitHub (`GET /user`) and kept in
//! `<data>/secrets/github.json`, readable only by this user. The page never gets it back: only the account, the last four
//! characters and when it was saved. With a token saved:
//! - the agent (FLE Durable) gets it as `GH_TOKEN` / `GITHUB_TOKEN` and as git's credential for github.com, so `git` and
//!   `gh` in its working folder work with the user's repos;
//! - requests to the MCP hub carry it as `X-GitHub-Token`, for the hub's GitHub plugin.
//!
//! The MCP hub (Max 02.11.: "am besten für das mcp zeug unseren hub verwenden … so können wir viel mcp zeug einbauen ohne
//! den agent zu slowen"): one address (FYOS `fyos-hub`, local or on another machine) whose few meta tools (search a tool,
//! call it) stand for all of its plugins, so many tools do not fill the model's context. Kept in `<data>/secrets/hub.json`
//! with an optional access token; the chat's tools include it as the MCP server `hub`.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where a token is made, with the permissions FLE Link suggests (the page links here).
pub const NEW_TOKEN_URL: &str = "https://github.com/settings/personal-access-tokens/new";

pub struct GitHub {
    file: PathBuf,
}

impl GitHub {
    pub fn new(home: &Path) -> Self {
        Self {
            file: home.join("secrets").join("github.json"),
        }
    }

    fn read(&self) -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(&self.file).ok()?).ok()
    }

    pub fn token(&self) -> Option<String> {
        self.read()?["token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(String::from)
    }

    /// What the page may see: never the token itself.
    pub fn status(&self) -> Value {
        match self.read() {
            Some(v) if v["token"].as_str().is_some_and(|t| !t.is_empty()) => {
                let t = v["token"].as_str().unwrap_or("");
                json!({"connected": true, "login": v["login"], "name": v["name"],
                    "hint": format!("…{}", &t[t.len().saturating_sub(4)..]), "saved": v["saved"], "new_token_url": NEW_TOKEN_URL})
            }
            _ => json!({"connected": false, "new_token_url": NEW_TOKEN_URL}),
        }
    }

    /// Check the token with GitHub and keep it; the answer is `status()`.
    pub fn save(&self, token: &str, api: &str) -> Result<Value, (u16, String)> {
        let token = token.trim();
        if token.is_empty()
            || token.len() > 512
            || token.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err((400, "That does not look like a GitHub token".into()));
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();
        let user: Value = match agent
            .get(&format!("{}/user", api.trim_end_matches('/')))
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/vnd.github+json")
            .set("User-Agent", concat!("flelink/", env!("CARGO_PKG_VERSION")))
            .call()
        {
            Ok(r) => serde_json::from_str(&r.into_string().map_err(|e| (502, e.to_string()))?)
                .map_err(|e| (502, e.to_string()))?,
            Err(ureq::Error::Status(401, _)) => {
                return Err((
                    422, // not 401: the page reads a 401 as "signed out of FLE Link"
                    "GitHub does not accept this token (expired or mistyped?)".into(),
                ));
            }
            Err(ureq::Error::Status(c, _)) => {
                return Err((502, format!("GitHub answered HTTP {c}")))
            }
            Err(e) => return Err((502, format!("GitHub is not reachable: {e}"))),
        };
        let saved = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let rec =
            json!({"token": token, "login": user["login"], "name": user["name"], "saved": saved});
        write_private(&self.file, &rec.to_string()).map_err(|e| (500, e.to_string()))?;
        Ok(self.status())
    }

    pub fn remove(&self) -> Value {
        let _ = std::fs::remove_file(&self.file);
        self.status()
    }

    /// The environment for the agent: the token for `gh` and as git's credential for github.com (git reads
    /// GIT_CONFIG_COUNT/KEY/VALUE; the helper answers with the token from the environment, nothing is written to disk).
    pub fn env(&self) -> Vec<(String, String)> {
        let Some(t) = self.token() else {
            return vec![];
        };
        vec![
            ("GH_TOKEN".into(), t.clone()),
            ("GITHUB_TOKEN".into(), t),
            ("GIT_CONFIG_COUNT".into(), "1".into()),
            (
                "GIT_CONFIG_KEY_0".into(),
                "credential.https://github.com.helper".into(),
            ),
            (
                "GIT_CONFIG_VALUE_0".into(),
                "!f() { echo username=x-access-token; echo \"password=$GH_TOKEN\"; }; f".into(),
            ),
        ]
    }
}

/// The MCP hub FLE Link uses for its tools.
pub struct Hub {
    file: PathBuf,
}

impl Hub {
    pub fn new(home: &Path) -> Self {
        Self {
            file: home.join("secrets").join("hub.json"),
        }
    }

    fn read(&self) -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(&self.file).ok()?).ok()
    }

    pub fn status(&self) -> Value {
        match self.read() {
            Some(v) if v["url"].as_str().is_some_and(|u| !u.is_empty()) => {
                let t = v["token"].as_str().unwrap_or("");
                json!({"connected": true, "url": v["url"], "token": !t.is_empty(),
                    "hint": if t.is_empty() { String::new() } else { format!("…{}", &t[t.len().saturating_sub(4)..]) }})
            }
            _ => json!({"connected": false}),
        }
    }

    pub fn save(&self, url: &str, token: &str) -> Result<Value, (u16, String)> {
        let url = url.trim();
        let token = token.trim();
        let ok = (url.starts_with("http://") || url.starts_with("https://"))
            && url.len() < 512
            && !url.contains(char::is_whitespace);
        if !ok {
            return Err((
                400,
                "The hub address must start with http:// or https://".into(),
            ));
        }
        // a token typed earlier stays when the field is left empty
        let keep = self
            .read()
            .and_then(|v| v["token"].as_str().map(String::from))
            .unwrap_or_default();
        let token = if token.is_empty() {
            keep
        } else {
            token.to_string()
        };
        write_private(&self.file, &json!({"url": url, "token": token}).to_string())
            .map_err(|e| (500, e.to_string()))?;
        Ok(self.status())
    }

    pub fn remove(&self) -> Value {
        let _ = std::fs::remove_file(&self.file);
        self.status()
    }
}

/// The hub for the agent (FLE Durable: FLE_DURABLE_HUB_URL and FLE_DURABLE_HUB_HEADERS, src/hub.js): the same address and
/// headers as the chat's `hub` server. Empty without a hub.
pub fn agent_hub_env(home: &Path) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<Value>(&extra_mcp(home)) else {
        return vec![];
    };
    let hub = &v["mcpServers"]["hub"];
    let Some(url) = hub["url"].as_str() else {
        return vec![];
    };
    vec![
        ("FLE_DURABLE_HUB_URL".into(), url.to_string()),
        ("FLE_DURABLE_HUB_HEADERS".into(), hub["headers"].to_string()),
    ]
}

/// The MCP servers FLE Link adds to the user's own (ToolBox.extra): the hub, with its access token and the GitHub token
/// for its GitHub plugin. Empty without a hub.
pub fn extra_mcp(home: &Path) -> String {
    let Some(h) = Hub::new(home).read() else {
        return String::new();
    };
    let Some(url) = h["url"].as_str().filter(|u| !u.is_empty()) else {
        return String::new();
    };
    let mut headers = serde_json::Map::new();
    if let Some(t) = h["token"].as_str().filter(|t| !t.is_empty()) {
        headers.insert("Authorization".into(), json!(format!("Bearer {t}")));
    }
    if let Some(g) = GitHub::new(home).token() {
        headers.insert("X-GitHub-Token".into(), json!(g));
    }
    json!({"mcpServers": {"hub": {"url": url, "headers": headers}}}).to_string()
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_never_sees_the_token_and_the_agent_gets_it_for_git_and_gh() {
        let home = std::env::temp_dir().join(format!("fle-gh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let g = GitHub::new(&home);
        assert_eq!(g.status()["connected"], false);
        assert!(g.env().is_empty() && extra_mcp(&home).is_empty());
        let rec =
            json!({"token": "github_pat_secret1234", "login": "octo", "name": "Octo", "saved": 1});
        write_private(&g.file, &rec.to_string()).unwrap();
        let st = g.status().to_string();
        assert!(!st.contains("secret"), "{st}");
        assert!(st.contains("…1234") && st.contains("octo"));
        // no hub: no MCP server; with a hub, its token as Bearer and the GitHub token for its plugin
        assert!(extra_mcp(&home).is_empty());
        Hub::new(&home)
            .save("http://127.0.0.1:8099/mcp", "hubtok")
            .unwrap();
        let x = extra_mcp(&home);
        let a: std::collections::HashMap<_, _> = agent_hub_env(&home).into_iter().collect();
        assert_eq!(a["FLE_DURABLE_HUB_URL"], "http://127.0.0.1:8099/mcp");
        assert!(a["FLE_DURABLE_HUB_HEADERS"].contains("X-GitHub-Token"));
        assert!(
            x.contains("Bearer hubtok") && x.contains("X-GitHub-Token") && x.contains("8099/mcp"),
            "{x}"
        );
        assert!(!Hub::new(&home).status().to_string().contains("hubtok"));
        assert!(Hub::new(&home).save("ftp://x", "").is_err());
        let env: std::collections::HashMap<_, _> = g.env().into_iter().collect();
        assert_eq!(env["GH_TOKEN"], "github_pat_secret1234");
        assert_eq!(
            env["GIT_CONFIG_KEY_0"],
            "credential.https://github.com.helper"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&g.file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(g.remove()["connected"], false);
        assert!(g.save("not a token", "http://127.0.0.1:9").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}
