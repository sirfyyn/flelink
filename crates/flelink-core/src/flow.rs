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

/// Install the engine for this machine if needed, download the model if needed (resumable, checked), start the engine
/// with automatic settings and wait until it answers. `model_id` is a recipe: a built-in or user recipe id, a `.toml`
/// file or an `https://` URL.
pub fn ensure_and_start(
    home: &Path,
    model_id: &str,
    emit: &mut dyn FnMut(Event),
) -> Result<Server> {
    let r = crate::recipe::find(home, model_id)?;
    ensure_and_start_recipe(home, &r, emit)
}

pub fn ensure_and_start_recipe(
    home: &Path,
    r: &crate::recipe::Recipe,
    emit: &mut dyn FnMut(Event),
) -> Result<Server> {
    let sys = system::detect(home);
    // copies already on this machine first: the fit check then counts only what is really missing
    crate::locate::adopt(r, home, &home.join("models"), &mut |e| emit(e));
    if r.fit(&home.join("models"), &sys) == catalog::Fit::TooBig {
        bail!(
            "{} does not fit this machine ({} RAM, {} free on disk)",
            r.title,
            system::gib(sys.ram_bytes),
            system::gib(sys.disk_free_bytes)
        );
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
        let server_bin = crate::plugin::server(home, &e, &sys.os, &sys.arch, &mut |l, d, t| {
            emit(Event::Progress {
                label: l.into(),
                done: d,
                total: t,
            })
        })?;
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
        // a newer FLE release replaces the installed one for this start; the old folder stays (a failed update keeps it)
        Some((name, p)) => match engines::update_available(home, &v, &name) {
            Some(tag) => {
                emit(Event::Step(format!("Updating {} to {tag}", v.label)));
                match engines::install(home, &v, &sys, &mut |l, d, t| {
                    emit(Event::Progress {
                        label: l.into(),
                        done: d,
                        total: t,
                    })
                }) {
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
            engines::install(home, &v, &sys, &mut |l, d, t| {
                emit(Event::Progress {
                    label: l.into(),
                    done: d,
                    total: t,
                })
            })?
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
        emit(Event::Step(format!("Downloading {what}{size}")));
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
    emit(Event::Ready {
        base: srv.base.clone(),
        engine: engine_label.into(),
        model: r.id.clone(),
        command,
    });
    Ok(srv)
}
