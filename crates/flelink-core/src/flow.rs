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
    if r.model.size > 0
        && catalog::fit_size(r.model.size, r.needs_fle(), &sys) == catalog::Fit::TooBig
    {
        bail!(
            "{} does not fit this machine ({} RAM, {} free on disk)",
            r.title,
            system::gib(sys.ram_bytes),
            system::gib(sys.disk_free_bytes)
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
    let existing = engines::installed(home)
        .into_iter()
        .find(|(n, _)| n.starts_with(v.id));
    let server_bin = match existing {
        Some((_, p)) => p,
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
    let parts = r.model.files.len();
    for (i, f) in r.model.files.iter().enumerate() {
        let path = r.file_path(home, f);
        if path.exists() {
            continue;
        }
        let what = if parts > 1 {
            format!("{} (part {} of {parts})", r.title, i + 1)
        } else {
            r.title.clone()
        };
        let size = if parts == 1 && r.model.size > 0 {
            format!(" ({})", system::gib(r.model.size))
        } else {
            String::new()
        };
        emit(Event::Step(format!("Downloading {what}{size}")));
        let url = format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            r.model.repo, r.model.revision, f
        );
        let sha = r.model.sha256.get(f).map(|s| s.as_str());
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
    let mut pl = plan::auto_plan(&sys, v.kind, &path, size, &r.id);
    r.apply(&mut pl);
    let command = pl.command_line(&server_bin);
    emit(Event::Step(format!(
        "Starting the engine (context {} tokens)",
        pl.ctx
    )));
    let mut srv = Server::start(&server_bin, &pl, &home.join("logs"))?;
    srv.wait_ready(Duration::from_secs(900), &mut |l| {
        emit(Event::Log(l.to_string()))
    })?;
    emit(Event::Ready {
        base: srv.base.clone(),
        engine: v.label.into(),
        model: r.id.clone(),
        command,
    });
    Ok(srv)
}
