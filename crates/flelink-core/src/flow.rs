//! The whole way from nothing to an answering model, as one function that reports events.
use crate::{catalog, engines, plan, server::Server, system, Event};
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::time::Duration;

/// Where a catalog model's main file lives; the same place a recipe puts it (subfolders of the repo are kept, so the
/// parts of a split model sit together).
pub fn model_path(home: &Path, m: &catalog::Model) -> std::path::PathBuf {
    home.join("models")
        .join(m.repo.replace('/', "__"))
        .join(m.file)
}

/// Why a recipe is refused: what this machine has and, when the recipe says so, what it asks for.
fn too_big_text(r: &crate::recipe::Recipe, sys: &system::SysInfo) -> String {
    let wanted: Vec<String> = [
        r.requires.ram_gib.map(|g| format!("{g} GB RAM")),
        r.requires.vram_gib.map(|g| format!("{g} GB GPU memory")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let asks = if wanted.is_empty() {
        String::new()
    } else {
        format!("; the recipe asks for {}", wanted.join(", "))
    };
    format!(
        "{} does not fit this machine ({} RAM, {} GPU memory, {} free on disk{asks}). `flelink recipes` shows what fits.",
        r.title,
        system::gib(sys.ram_bytes),
        system::gib(sys.gpu_usable_bytes),
        system::gib(sys.disk_free_bytes)
    )
}

/// Engine downloads and installs report to the same event.
fn progress(emit: &mut dyn FnMut(Event)) -> impl FnMut(&str, u64, u64) + '_ {
    move |label: &str, done, total| {
        emit(Event::Progress {
            label: label.into(),
            done,
            total,
        })
    }
}

/// Install the engine for this machine if needed, download the model if needed (resumable, checked), start the engine
/// with automatic settings and wait until it answers.
pub fn ensure_and_start_recipe(
    home: &Path,
    r: &crate::recipe::Recipe,
    emit: &mut dyn FnMut(Event),
) -> Result<Server> {
    let sys = system::detect(home);
    // copies already on this machine first: the fit check then counts only what is really missing
    crate::locate::adopt(r, home, &home.join("models"), &mut |e| emit(e));
    if r.fit(&home.join("models"), &sys) == catalog::Fit::TooBig {
        bail!("{}", too_big_text(r, &sys));
    }
    // an engine a plugin brings, when the recipe prefers one and no built-in engine comes before it
    let builtin_first = r
        .engine
        .prefer
        .iter()
        .position(|w| engines::variants(&sys).iter().any(|v| v.kind == w.as_str()));
    let plugin_first = r
        .engine
        .prefer
        .iter()
        .position(|w| crate::plugin::engine(home, w).is_some());
    if let Some(pi) = plugin_first.filter(|pi| builtin_first.is_none_or(|bi| *pi < bi)) {
        let (m, e) = crate::plugin::engine(home, &r.engine.prefer[pi]).unwrap();
        emit(Event::Step(format!(
            "Engine: {} (plugin {})",
            e.label, m.id
        )));
        let server_bin =
            crate::plugin::server(home, &e, &sys.os, &sys.arch, &mut progress(&mut *emit))?;
        return start_with(
            home,
            r,
            &sys,
            &server_bin,
            "custom",
            &e.label,
            Some(&e),
            emit,
        );
    }
    let variants = engines::variants(&sys);
    // the recipe's preference first, in its order; otherwise (and unless strict) the best for this machine
    let preferred = r
        .engine
        .prefer
        .iter()
        .find_map(|want| variants.iter().find(|v| v.kind == want.as_str()));
    let v = match preferred {
        Some(v) => v.clone(),
        None if r.engine.strict => bail!(
            "{} needs the {} engine, which does not fit this machine",
            r.title,
            r.engine.prefer.join(" or ")
        ),
        None => variants
            .first()
            .cloned()
            .context("no engine fits this machine")?,
    };
    emit(Event::Step(format!("Engine: {} - {}", v.label, v.why)));
    let existing = engines::newest_installed(home, v.id);
    let server_bin = match existing {
        // a recipe's numbers belong to an engine: a newer release is only taken when this recipe needs it (the old folder
        // stays, so a failed update keeps it)
        Some((name, p)) => match engines::update_available(home, &v, &name) {
            Some(tag)
                if !r
                    .engine
                    .min_version
                    .as_deref()
                    .is_some_and(|min| crate::recipe::engine_too_old(&name, min)) =>
            {
                emit(Event::Step(format!(
                    "{} {tag} is available; this start stays on {name} (install it on the Engine page)",
                    v.label
                )));
                p
            }
            Some(tag) => {
                emit(Event::Step(format!("Updating {} to {tag}", v.label)));
                let updated = engines::install(home, &v, &sys, &mut progress(&mut *emit));
                match updated {
                    Ok(p2) => p2,
                    Err(e) => {
                        emit(Event::Log(format!(
                            "update failed, staying on {name}: {e:#}"
                        )));
                        p
                    }
                }
            }
            None => p,
        },
        None => {
            emit(Event::Step(
                "Installing the engine (checked against the SHA-256 GitHub publishes)".into(),
            ));
            engines::install(home, &v, &sys, &mut progress(&mut *emit))?
        }
    };
    start_with(home, r, &sys, &server_bin, v.kind, v.label, None, emit)
}

/// Download what is missing, plan, start and wait until the engine answers.
#[allow(clippy::too_many_arguments)]
fn start_with(
    home: &Path,
    r: &crate::recipe::Recipe,
    sys: &system::SysInfo,
    server_bin: &Path,
    engine_kind: &str,
    engine_label: &str,
    plugin: Option<&crate::plugin::PluginEngine>,
    emit: &mut dyn FnMut(Event),
) -> Result<Server> {
    crate::orphans::wait_for_port(home, 8080, Duration::from_secs(12));
    let parts = r.model.files.len();
    for (i, x) in r.fetches().iter().enumerate() {
        let path = home.join("models").join(&x.local);
        let f = &x.file;
        if path.exists() {
            continue;
        }
        let what = if i >= parts {
            format!("{} (draft model)", r.title)
        } else if parts > 1 {
            format!("{} (part {} of {parts})", r.title, i + 1)
        } else {
            r.title.clone()
        };
        let size = if i >= parts && x.size > 0 {
            format!(" ({})", system::gib(x.size))
        } else if parts == 1 && i == 0 && r.model.size > 0 {
            format!(" ({})", system::gib(r.model.size))
        } else {
            String::new()
        };
        // say it in words when an earlier download is picked up (seen on 3 Nov: the bar started at 45 % without a word)
        let have = std::fs::metadata(crate::download::part_path(&path))
            .map(|m| m.len())
            .unwrap_or(0);
        if have > 0 {
            emit(Event::Step(format!(
                "Resuming the download of {what}: {} already here",
                system::gib(have)
            )));
        } else {
            emit(Event::Step(format!("Downloading {what}{size}")));
        }
        let url = format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            x.repo, x.revision, f
        );
        let sha = x.sha256.as_deref();
        crate::download::fetch(&url, &path, sha, &mut |d, t| {
            emit(Event::Progress {
                label: format!(
                    "model {}",
                    Path::new(f)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
                done: d,
                total: t,
            })
        })?;
        if let Some(s) = sha {
            emit(Event::Step("Checked: the file matches its SHA-256".into()));
            crate::locate::remember_checked(home, &path, s);
        }
    }
    let path = r.main_file(home);
    emit(Event::Step("Choosing settings for this machine".into()));
    let size = if r.model.size > 0 {
        r.model.size
    } else {
        r.model
            .files
            .iter()
            .map(|f| {
                std::fs::metadata(r.file_path(home, f))
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .sum()
    };
    // an engine with its own start command fills its template; every other engine gets the llama-server arguments
    let (program, mut pl, health) = match plugin.filter(|e| !e.command.is_empty()) {
        Some(e) => {
            let port = plan::free_port(8080);
            let ctx = r.run.context.unwrap_or(8192);
            let filled =
                crate::plugin::fill_command(e, server_bin, &path, "127.0.0.1", port, ctx, &r.id);
            let first = std::path::PathBuf::from(&filled[0]);
            let program = if first.is_absolute() {
                first
            } else {
                crate::launch::which(&filled[0])
                    .with_context(|| format!("{} is not on the PATH", filled[0]))?
            };
            let env = e.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let pl = plan::Plan {
                env,
                args: filled[1..].to_vec(),
                port,
                ctx,
            };
            (program, pl, e.health.clone())
        }
        None => {
            let mut pl = plan::auto_plan(sys, engine_kind, &path, size, &r.id);
            if let Some(e) = plugin {
                pl.args.extend(e.args.iter().cloned());
                for (k, v) in &e.env {
                    pl.env.push((k.clone(), v.clone()));
                }
            }
            let health = plugin
                .map(|e| e.health.clone())
                .unwrap_or_else(|| "/health".into());
            (server_bin.to_path_buf(), pl, health)
        }
    };
    r.apply(&mut pl, &home.join("models"));
    // the engine port: 8080, or the next free one when something else holds it (and then say so)
    if pl.port != 8080 {
        emit(Event::Step(format!(
            "Port 8080 is taken, so the engine uses port {}",
            pl.port
        )));
    }
    let command = pl.command_line(&program);
    emit(Event::Step(format!(
        "Starting the engine (context {} tokens)",
        pl.ctx
    )));
    let mut srv = Server::start(&program, &pl, &home.join("logs"))?;
    srv.health = health;
    srv.wait_ready(Duration::from_secs(900), &mut |l| {
        emit(Event::Log(l.to_string()))
    })?;
    // a recipe can ask for a log line before it counts as ready (an expert store that fills after the server is up)
    if let Some(want) = r.run.ready_log.as_deref() {
        emit(Event::Step(format!(
            "Warming up (waiting for \u{201c}{want}\u{201d} in the log)"
        )));
        let t0 = std::time::Instant::now();
        loop {
            if crate::server::stop_requested() {
                anyhow::bail!("stopped while the model was warming up"); // dropping the Server ends the engine
            }
            let log = std::fs::read_to_string(&srv.log).unwrap_or_default();
            if log.contains(want) {
                break;
            }
            if t0.elapsed() > Duration::from_secs(900) {
                anyhow::bail!("the engine log never showed \u{201c}{want}\u{201d} (15 min)");
            }
            if let Some(st) = srv.child.try_wait()? {
                anyhow::bail!("the engine stopped while warming up ({st})");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    emit(Event::Ready {
        base: srv.base.clone(),
        engine: engine_label.into(),
        model: r.id.clone(),
        command,
    });
    Ok(srv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_names_the_gpu_memory_and_what_was_measured() {
        let r = crate::recipe::builtin()
            .into_iter()
            .find(|r| r.id == "katy-27b-dflash")
            .unwrap();
        let sys = system::SysInfo {
            os: "linux".into(),
            arch: "x64".into(),
            cpu: String::new(),
            cores: 8,
            ram_bytes: 64 << 30,
            disk_free_bytes: 500 << 30,
            avx2: true,
            gpus: vec![],
            gpu_usable_bytes: 8 << 30,
        };
        let t = too_big_text(&r, &sys);
        assert!(t.contains("8.0 GiB GPU memory"), "{t}");
        assert!(t.contains("asks for 24 GB RAM, 12 GB GPU memory"), "{t}");
    }
}
