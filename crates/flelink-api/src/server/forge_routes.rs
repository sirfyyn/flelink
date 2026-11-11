//! Forge in the Studio's API: the tools it drives, the overview, publishing a build, and its routes (`/api/forge/…`).
use super::*;

impl Studio {
    /// The llama-server files whose folders are searched for the Forge tools.
    pub(super) fn forge_engine_paths(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .engines
            .installed()
            .iter()
            .map(|e| ("engine".to_string(), py_str(&e["path"])))
            .collect();
        let custom = self
            .config
            .lock()
            .unwrap()
            .get("engine.custom_path")
            .map(py_str)
            .unwrap_or_default();
        if !custom.is_empty() {
            out.push(("custom engine".into(), custom));
        }
        out
    }

    pub(super) fn forge_tools(
        &self,
        refresh: bool,
    ) -> std::collections::HashMap<String, crate::forge::tools::Tool> {
        let dir = self
            .config
            .lock()
            .unwrap()
            .get("forge.tools_dir")
            .map(py_str)
            .unwrap_or_default();
        self.forge.tools(refresh, &dir, &self.forge_engine_paths())
    }

    pub(super) fn forge_overview(&self) -> Value {
        let found = self.forge_tools(true);
        let dir = self
            .config
            .lock()
            .unwrap()
            .get("forge.tools_dir")
            .map(py_str)
            .unwrap_or_default();
        self.forge.overview(&found, &dir, &self.models_dir())
    }

    /// Add a finished Forge model to the library and save a start profile for it.
    pub(super) fn forge_publish(&self, id: &str) -> Result<Value, ApiError> {
        let info = self.sysinfo();
        let gpus = info["gpus"].as_array().cloned().unwrap_or_default();
        let unified = gpus.iter().any(|g| g["unified"] == true);
        let (final_, name, args, notes) = self
            .forge
            .publish_plan(id, !gpus.is_empty(), unified)
            .map_err(|e| ApiError(e.0, e.1))?;
        let fle = self.engines.installed().iter().any(|e| e["kind"] == "fle");
        let model = self
            .library
            .lock()
            .unwrap()
            .add(&self.models_dir(), &final_)
            .map_err(|e| ApiError(400, e))?;
        let Value::Object(data) = json!({"name": format!("Forge: {name}"), "engine": if fle { "fle" } else { "upstream" },
            "binary": "", "args": args, "env": {}, "notes": notes})
        else {
            unreachable!()
        };
        let prof = self
            .profiles
            .lock()
            .unwrap()
            .save(&data)
            .map_err(|e| ApiError(400, e.0))?;
        Ok(json!({"model": model, "profile": prof.public()}))
    }

    pub(super) fn forge_route(
        &self,
        method: &str,
        p: &str,
        body: Option<Value>,
        query: &Map<String, Value>,
    ) -> Option<Result<Value, ApiError>> {
        let fe = |e: crate::forge::ForgeError| ApiError(e.0, e.1);
        let body = body.unwrap_or(json!({}));
        let jobs = &self.forge.jobs;
        let job = |suffix: &str| -> Option<String> {
            let id = p.strip_prefix("/api/forge/jobs/")?.strip_suffix(suffix)?;
            (id.len() == 12
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
            .then(|| id.to_string())
        };
        let not_found = || ApiError(404, "Not found".into());
        Some(match (method, p) {
            ("GET", "/api/forge") => Ok(self.forge_overview()),
            ("POST", "/api/forge/tools") => {
                let dir = py_str(&body["tools_dir"]);
                let mut patch = Map::new();
                patch.insert("forge".into(), json!({"tools_dir": if body["tools_dir"].is_null() { String::new() } else { dir }}));
                match self.config.lock().unwrap().update(&patch) {
                    Ok(_) => {}
                    Err(e) => return Some(Err(ApiError(400, e))),
                }
                Ok(self.forge_overview())
            }
            ("POST", "/api/forge/inspect") => {
                self.forge.inspect(&py_str(&body["path"])).map_err(fe)
            }
            ("POST", "/api/forge/calculate") => self.forge.calculate(&body).map_err(fe),
            ("POST", "/api/forge/corpus") => self.forge.make_corpus(&body).map_err(fe),
            ("GET", "/api/forge/usage/profiles") => {
                Ok(self.forge.usage_profiles(&self.models_dir()))
            }
            ("POST", "/api/forge/usage") => self.forge.usage_plan(&body).map_err(fe),
            ("POST", "/api/forge/recipe/import") => {
                let r = if body["recipe"].is_null() || body["recipe"] == false {
                    body.clone()
                } else {
                    body["recipe"].clone()
                };
                self.forge.import_recipe(&r.to_string()).map_err(fe)
            }
            ("POST", "/api/forge/recipe/share") => {
                let r = if body["recipe"].is_object() {
                    body["recipe"].clone()
                } else {
                    json!({})
                };
                crate::forge::recipe::portable(&r)
                    .map(|r| json!({"recipe": r}))
                    .map_err(|e| ApiError(400, e.0))
            }
            // a post for the Community quants discussions: the portable recipe (file names, no folders), the machine and
            // the check results the page has; the person opens the link and posts it themself
            ("POST", "/api/forge/recipe/github") => {
                let r = if body["recipe"].is_object() {
                    body["recipe"].clone()
                } else {
                    json!({})
                };
                let r = match crate::forge::recipe::portable(&r) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(ApiError(400, e.0))),
                };
                let sys = self.sysinfo();
                let gpus: Vec<String> = sys["gpus"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|g| g["name"].as_str().unwrap_or("?").to_string())
                    .collect();
                let mut text = format!(
                    "**{}** built with the FLE Link Forge ({}).\n\n**Machine** {}, {:.0} GiB RAM, {}\n\n",
                    r["name"].as_str().unwrap_or("my-quant"),
                    r["mode"].as_str().unwrap_or(""),
                    sys["cpu"]["name"].as_str().or(sys["cpu"].as_str()).unwrap_or("?"),
                    sys["memory"]["total"].as_f64().unwrap_or(0.0) / f64::from(1u32 << 30),
                    if gpus.is_empty() { "no GPU".into() } else { gpus.join(", ") }
                );
                if let Some(note) = r["notes"].as_str().filter(|n| !n.is_empty()) {
                    text += &format!("{note}\n\n");
                }
                if let Some(res) = body.get("results").filter(|v| !v.is_null()) {
                    text += &format!(
                        "**Check**\n```json\n{}\n```\n\n",
                        serde_json::to_string_pretty(res).unwrap_or_default()
                    );
                }
                text += &format!(
                    "**Recipe** (load it in the Forge and point each source at your own copy)\n```json\n{}\n```\n",
                    crate::forge::recipe::dumps(&r).trim_end()
                );
                let title = format!(
                    "{} ({})",
                    r["name"].as_str().unwrap_or("my-quant"),
                    r["mode"].as_str().unwrap_or("quant")
                );
                let sh =
                    flelink_core::share::discussion(flelink_core::share::QUANTS, &title, &text);
                Ok(json!({"url": sh.url, "paste": sh.paste}))
            }
            ("POST", "/api/forge/check") => {
                let r = if body["recipe"].is_object() {
                    body["recipe"].clone()
                } else {
                    json!({})
                };
                jobs.check(&r, &self.forge_tools(false))
                    .map_err(|e| ApiError(400, e))
            }
            ("POST", "/api/forge/jobs") => {
                let r = if body["recipe"].is_object() {
                    body["recipe"].clone()
                } else {
                    json!({})
                };
                jobs.create(&r, self.forge_tools(true), &self.models_dir().join("forge"))
                    .map_err(|(c, m)| ApiError(c, m))
            }
            ("GET", _) if job("").is_some() => {
                let id = job("").unwrap();
                jobs.get_job(&id)
                    .map(|j| jobs.public(&j))
                    .ok_or_else(not_found)
            }
            ("GET", _) if job("/log").is_some() => {
                let since = query
                    .get("since")
                    .map(py_str)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                jobs.log_since(&job("/log").unwrap(), since)
                    .ok_or_else(not_found)
            }
            ("GET", _) if job("/recipe").is_some() => {
                self.forge.recipe_of(&job("/recipe").unwrap()).map_err(fe)
            }
            ("POST", _) if job("/cancel").is_some() => {
                jobs.cancel(&job("/cancel").unwrap()).ok_or_else(not_found)
            }
            ("POST", _) if job("/resume").is_some() => {
                let id = job("/resume").unwrap();
                if jobs.get_job(&id).is_none() {
                    Err(not_found())
                } else {
                    jobs.resume(&id, self.forge_tools(true))
                        .map_err(|e| ApiError(409, e))
                }
            }
            ("POST", _) if job("/delete").is_some() => {
                let id = job("/delete").unwrap();
                if jobs.get_job(&id).is_none() {
                    Err(not_found())
                } else {
                    jobs.delete(&id, !py_truthy(&body["delete_model"]))
                        .map(|_| json!({"ok": true}))
                        .map_err(|e| ApiError(409, e))
                }
            }
            ("POST", _) if job("/publish").is_some() => {
                self.forge_publish(&job("/publish").unwrap())
            }
            _ => return None,
        })
    }
}
