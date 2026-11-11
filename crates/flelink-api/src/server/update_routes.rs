//! Updates in the Studio's API: applying one, and when FLE Link may install one by itself.
use super::*;

impl Studio {
    /// Install the release `tag` (the one the last check found) and start again with it.
    pub fn update_apply(self: &Arc<Self>, tag: &str) -> Result<(), String> {
        let config = Value::Object(self.config.lock().unwrap().data.clone());
        let base = self.updates.prepare(&config, tag)?;
        let (st, tag) = (self.clone(), tag.to_string());
        std::thread::spawn(move || {
            if st.updates.install(&base, &tag).is_ok() {
                // the page sees "restarting" first, then the engine and the MCP servers stop and the new version starts
                std::thread::sleep(std::time::Duration::from_millis(1500));
                st.engine.shutdown();
                st.toolbox.mcp.close_all();
                // the agent too: the new version starts its own, and two agents on one queue would be one too many
                st.agent.stop();
                crate::updates::restart();
            }
        });
        Ok(())
    }

    /// "Install new versions by themselves": install a found version without a click, but only while nobody is served. No engine runs, or it
    /// has been quiet for a quarter of an hour and "Load this model when FLE Link starts" brings the model back; an
    /// engine that would stay stopped after the restart is left alone until it is stopped by hand.
    pub fn update_by_itself(self: &Arc<Self>) {
        let config = Value::Object(self.config.lock().unwrap().data.clone());
        if !self.updates.auto() {
            return;
        }
        let view = self.updates.view(&config);
        let (Some(tag), true) = (view["app"]["tag"].as_str(), view["app"]["can_auto"] == true)
        else {
            return;
        };
        let state = self.engine.status()["state"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let (quiet, busy) = self.live.quiet();
        let comes_back = config["launch"]["autostart"] == true;
        if !quiet_enough(&state, quiet, busy, comes_back) {
            return;
        }
        eprintln!("[update] installing {tag} by itself");
        if let Err(e) = self.update_apply(tag) {
            eprintln!("[update] not installed: {e}");
        }
    }
}

/// Whether an update may restart FLE Link by itself now: `state` of the engine, seconds since the last answer, whether it
/// answers right now, and whether the model is loaded again at the next start.
pub fn quiet_enough(state: &str, quiet: Option<f64>, busy: bool, comes_back: bool) -> bool {
    match state {
        "ready" | "attached" => !busy && comes_back && quiet.is_none_or(|s| s >= 900.0),
        "starting" | "stopping" => false,
        _ => true, // no engine runs: nothing is interrupted
    }
}

#[cfg(test)]
mod tests {
    use super::quiet_enough;

    #[test]
    fn an_update_by_itself_never_interrupts_an_answer_or_leaves_the_model_unloaded() {
        // no engine: nothing to interrupt
        assert!(quiet_enough("stopped", None, false, false));
        assert!(quiet_enough("crashed", Some(3.0), false, false));
        // a loaded model: only when quiet for 15 minutes and the next start loads it again
        assert!(quiet_enough("ready", Some(901.0), false, true));
        assert!(quiet_enough("ready", None, false, true));
        assert!(!quiet_enough("ready", Some(120.0), false, true));
        assert!(!quiet_enough("ready", Some(5000.0), true, true));
        assert!(!quiet_enough("ready", Some(5000.0), false, false));
        assert!(!quiet_enough("attached", Some(5000.0), false, false));
        // while a model loads or stops
        assert!(!quiet_enough("starting", None, false, true));
        assert!(!quiet_enough("stopping", None, false, true));
    }
}
