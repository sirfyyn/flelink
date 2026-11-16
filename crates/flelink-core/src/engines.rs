use crate::download::{client, fetch};
use crate::system::SysInfo;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The llama.cpp release FLE is tested with (same pin as FLE Studio); where each engine comes from is its adapter's.
pub use crate::adapter::LLAMA_TAG;

#[derive(Debug, Clone)]
pub struct Variant {
    pub id: &'static str,
    pub kind: &'static str, // fle | upstream
    pub backend: &'static str,
    pub label: &'static str,
    pub why: &'static str,
}

/// Engine choices for this machine, best first. Same rules as FLE Studio's engines.py.
pub fn variants(s: &SysInfo) -> Vec<Variant> {
    let mut v = Vec::new();
    let nvidia = s.gpus.iter().any(|g| g.vendor == "nvidia");
    if s.os == "macos" {
        if s.arch == "arm64" {
            v.push(Variant {
                id: "upstream-metal",
                kind: "upstream",
                backend: "metal",
                label: "llama.cpp for Apple Silicon (Metal)",
                why: "Runs the model on the Apple GPU with unified memory.",
            });
        } else {
            v.push(Variant {
                id: "upstream-macos-x64",
                kind: "upstream",
                backend: "cpu",
                label: "llama.cpp for Intel Macs",
                why: "CPU build for Intel Macs.",
            });
        }
        return v;
    }
    if nvidia && s.arch == "x64" {
        if s.avx2 {
            v.push(Variant {
                id: "fle-cuda",
                kind: "fle",
                backend: "cuda",
                label: "FLE engine (NVIDIA CUDA)",
                why: "The FLE build: GPU expert cache, MTP drafting, pinned expert loading.",
            });
        }
        v.push(Variant {
            id: "upstream-cuda",
            kind: "upstream",
            backend: "cuda",
            label: "llama.cpp (NVIDIA CUDA)",
            why: "Upstream llama.cpp on the NVIDIA GPU.",
        });
    }
    if nvidia || s.gpus.iter().any(|g| g.vendor == "other") {
        v.push(Variant {
            id: "upstream-vulkan",
            kind: "upstream",
            backend: "vulkan",
            label: "llama.cpp (Vulkan)",
            why: "Works on AMD, Intel and NVIDIA GPUs.",
        });
    }
    v.push(Variant {
        id: "upstream-cpu",
        kind: "upstream",
        backend: "cpu",
        label: "llama.cpp (CPU)",
        why: "Works everywhere; slow for big models.",
    });
    v
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

fn release(repo: &str, tag: Option<&str>) -> Result<Release> {
    let url = match tag {
        Some(t) => format!("https://api.github.com/repos/{repo}/releases/tags/{t}"),
        None => format!("https://api.github.com/repos/{repo}/releases/latest"),
    };
    let r = client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()?;
    let code = r.status();
    if code.is_success() {
        return Ok(r.json()?);
    }
    // 403/429: GitHub's API allows 60 requests an hour per address without sign-in (a shared office or lab address uses
    // them up quickly). The release page and its SHA256SUMS have no such limit and name every file with its checksum.
    if code.as_u16() == 403 || code.as_u16() == 429 {
        if let Ok(rel) = release_from_page(repo, tag) {
            return Ok(rel);
        }
    }
    bail!("GitHub answered {code} for {url}")
}

/// A release as GitHub's release list shows it.
#[derive(Deserialize)]
struct Listed {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// A plain release tag (`v1.2.6`, `v1.3`), no test build (`v1.3.0-rc1`).
fn stable(tag: &str) -> bool {
    tag.strip_prefix('v').is_some_and(|t| {
        !t.is_empty()
            && t.split('.').count() <= 3
            && t.split('.')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    })
}

/// The newest of `tags` that the recipe's engine spec admits (its line and `max_version`); only plain release tags.
pub fn pick_tag<'a>(
    tags: impl IntoIterator<Item = &'a str>,
    spec: &crate::recipe::EngineSpec,
) -> Option<String> {
    tags.into_iter()
        .filter(|t| stable(t) && crate::recipe::EngineLine::of(t).is_some() && spec.admits(t))
        .max_by_key(|t| crate::recipe::engine_version(t))
        .map(str::to_string)
}

/// The release tags a release feed (`github.com/<repo>/releases.atom`) names, newest first. GitHub serves it without the
/// API's hourly limit; it lists published releases only (no drafts).
fn parse_atom_tags(xml: &str, repo: &str) -> Vec<String> {
    let mark = format!("/{repo}/releases/tag/");
    let mut out: Vec<String> = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&mark) {
        rest = &rest[i + mark.len()..];
        let end = rest
            .find(|c: char| c == '"' || c == '<' || c.is_whitespace())
            .unwrap_or(rest.len());
        let t = &rest[..end];
        if !t.is_empty() && !t.contains('/') && !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// The newest release of `repo` that the recipe's engine spec admits (the FLE line, `max_version`). The FLE engine has two
/// lines without shared code, so "the latest release" may belong to the other one: the release list is read instead
/// (`/releases`, drafts and test builds left out), or, when GitHub's API refuses (403/429), the release feed and the
/// chosen release's page.
pub fn release_for(repo: &str, spec: &crate::recipe::EngineSpec) -> Result<String> {
    Ok(release_in_line(repo, spec)?.tag_name)
}

fn release_in_line(repo: &str, spec: &crate::recipe::EngineSpec) -> Result<Release> {
    let what = || {
        format!(
            "no FLE release of the {} line ({}{}) in {repo}",
            spec.line,
            spec.line.range(),
            spec.max_version
                .as_deref()
                .map(|m| format!(", up to {m}"))
                .unwrap_or_default()
        )
    };
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
    let r = client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()?;
    let code = r.status();
    if code.is_success() {
        let list: Vec<Listed> = r.json()?;
        let ok: Vec<&Listed> = list.iter().filter(|l| !l.draft && !l.prerelease).collect();
        let tag = pick_tag(ok.iter().map(|l| l.tag_name.as_str()), spec).with_context(what)?;
        let l = list.into_iter().find(|l| l.tag_name == tag).unwrap();
        return Ok(Release {
            tag_name: l.tag_name,
            assets: l.assets,
        });
    }
    if code.as_u16() == 403 || code.as_u16() == 429 {
        let feed = client()?
            .get(format!("https://github.com/{repo}/releases.atom"))
            .send()?;
        if feed.status().is_success() {
            let tags = parse_atom_tags(&feed.text()?, repo);
            let tag = pick_tag(tags.iter().map(String::as_str), spec).with_context(what)?;
            return release_from_page(repo, Some(&tag));
        }
    }
    bail!("GitHub answered {code} for {url}")
}

/// [`release_from_page`] in the API's shape (`tag_name`, `assets` with `name`, `browser_download_url`, `digest`), for the
/// Studio's engine install when GitHub's API refuses (403/429).
pub fn release_page_json(repo: &str, tag: Option<&str>) -> Result<serde_json::Value> {
    let r = release_from_page(repo, tag)?;
    let assets: Vec<serde_json::Value> = r
        .assets
        .iter()
        .map(|a| serde_json::json!({"name": a.name, "browser_download_url": a.browser_download_url, "digest": a.digest}))
        .collect();
    Ok(serde_json::json!({"tag_name": r.tag_name, "assets": assets}))
}

/// A release without the API: the tag from `/releases/latest` (it redirects to `/releases/tag/<tag>`), the files and
/// their SHA-256 from the release's `SHA256SUMS`, or, when a release has none (upstream llama.cpp), from the asset list
/// GitHub shows on the release page (`/releases/expanded_assets/<tag>`, every file with its sha256 digest).
fn release_from_page(repo: &str, tag: Option<&str>) -> Result<Release> {
    let c = client()?;
    let tag = match tag {
        Some(t) => t.to_string(),
        None => {
            let r = c
                .get(format!("https://github.com/{repo}/releases/latest"))
                .send()?;
            let path = r.url().path().to_string();
            path.rsplit_once("/releases/tag/")
                .map(|(_, t)| t.to_string())
                .filter(|t| !t.is_empty())
                .context("the release page did not name a tag")?
        }
    };
    let base = format!("https://github.com/{repo}/releases/download/{tag}");
    let sums = c.get(format!("{base}/SHA256SUMS")).send()?;
    let assets = if sums.status().is_success() {
        parse_sums(&sums.text()?, &base)
    } else {
        let page = c
            .get(format!(
                "https://github.com/{repo}/releases/expanded_assets/{tag}"
            ))
            .send()?;
        if !page.status().is_success() {
            bail!("no SHA256SUMS and no asset list in release {tag}");
        }
        parse_expanded(&page.text()?, repo, &tag)
    };
    if assets.is_empty() {
        bail!("release {tag} names no files with a checksum");
    }
    Ok(Release {
        tag_name: tag,
        assets,
    })
}

/// `<sha256>  <name>` lines (sha256sum's format, `*` before binary names allowed) as release assets.
fn parse_sums(text: &str, base: &str) -> Vec<Asset> {
    text.lines()
        .filter_map(|l| {
            let (sha, name) = l.trim().split_once(char::is_whitespace)?;
            let name = name.trim().trim_start_matches('*');
            (sha.len() == 64
                && sha.chars().all(|c| c.is_ascii_hexdigit())
                && !name.is_empty()
                && !name.contains('/'))
            .then(|| Asset {
                name: name.to_string(),
                browser_download_url: format!("{base}/{name}"),
                digest: Some(format!("sha256:{}", sha.to_lowercase())),
            })
        })
        .collect()
}

/// The asset list of a release page: each download link `/<repo>/releases/download/<tag>/<name>` with the first
/// `sha256:<hex>` after it (before the next link). Files without a digest are left out, so nothing unchecked is installed.
fn parse_expanded(html: &str, repo: &str, tag: &str) -> Vec<Asset> {
    let prefix = format!("href=\"/{repo}/releases/download/{tag}/");
    let mut out: Vec<Asset> = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(&prefix) {
        rest = &rest[i + prefix.len()..];
        let Some(end) = rest.find('"') else { break };
        let name = &rest[..end];
        let block = &rest[end..rest[end..].find(&prefix).map_or(rest.len(), |j| end + j)];
        let sha = block
            .find("sha256:")
            .and_then(|k| block.get(k + 7..k + 7 + 64));
        let ok = |n: &str| !n.is_empty() && !n.contains('/') && !n.contains("..");
        if let Some(sha) = sha.filter(|h| h.chars().all(|c| c.is_ascii_hexdigit())) {
            if ok(name) && !out.iter().any(|a| a.name == name) {
                out.push(Asset {
                    name: name.to_string(),
                    browser_download_url: format!(
                        "https://github.com/{repo}/releases/download/{tag}/{name}"
                    ),
                    digest: Some(format!("sha256:{}", sha.to_lowercase())),
                });
            }
        }
    }
    out
}

/// The engine archive of a variant for this platform (crate::assets), the newest CUDA build when there are several.
fn pick<'a>(assets: &'a [Asset], v: &Variant, os: &str, arch: &str) -> Option<&'a Asset> {
    let pattern = crate::assets::asset_patterns(v.id, os, arch)
        .ok()?
        .remove(0);
    assets
        .iter()
        .filter_map(|a| {
            let cuda = pattern.matches(&a.name)?;
            let key: Vec<u64> = cuda
                .unwrap_or_default()
                .split('.')
                .map(|x| x.parse().unwrap_or(0))
                .collect();
            Some((key, a))
        })
        .max_by(|x, y| x.0.cmp(&y.0).then_with(|| x.1.name.cmp(&y.1.name)))
        .map(|(_, a)| a)
}

/// The numbers of the version at the end of a folder name (`fle-cuda-v1.2.0` -> [1, 2, 0], `upstream-metal-b11540` ->
/// [11540]), for ordering builds of one variant.
pub fn version_key(name: &str) -> Vec<u64> {
    let tail = name.rsplit_once('-').map(|(_, t)| t).unwrap_or(name);
    tail.trim_start_matches(['v', 'b'])
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// The newest installed build of a variant (by its version, not by name order).
pub fn newest_installed(dir: &Path, variant_id: &str) -> Option<(String, PathBuf)> {
    newest_installed_where(dir, variant_id, |_| true)
}

/// The newest installed build of a variant that a recipe may use: for an FLE variant only builds of the recipe's line
/// (and up to its `max_version`), so a moe build and a dense build can sit side by side, each in its own folder
/// (`fle-cuda-v1.2.6`, `fle-cuda-v1.3.0`), and each recipe starts its own.
pub fn newest_installed_for(
    dir: &Path,
    v: &Variant,
    spec: &crate::recipe::EngineSpec,
) -> Option<(String, PathBuf)> {
    if v.kind != "fle" {
        return newest_installed(dir, v.id);
    }
    newest_installed_where(dir, v.id, |n| spec.admits(n))
}

fn newest_installed_where(
    dir: &Path,
    variant_id: &str,
    keep: impl Fn(&str) -> bool,
) -> Option<(String, PathBuf)> {
    installed(dir)
        .into_iter()
        .filter(|(n, _)| n.starts_with(&format!("{variant_id}-")) && keep(n))
        .max_by_key(|(n, _)| version_key(n))
}

/// A newer FLE release than `installed_name` within the recipe's line (and up to its `max_version`), if there is one.
/// Asks GitHub at most once a day per line (the answer is kept in `<data>/engines/.latest-<variant>-<line>`); offline, on
/// any error and with FLE_NO_UPDATE set it says nothing. Only the FLE engine follows its releases (its adapter says so):
/// upstream llama.cpp is pinned (LLAMA_TAG).
pub fn update_available(
    dir: &Path,
    v: &Variant,
    installed_name: &str,
    spec: &crate::recipe::EngineSpec,
) -> Option<String> {
    let rel = crate::adapter::release_of(v.kind).filter(|r| r.self_updates)?;
    if std::env::var_os("FLE_NO_UPDATE").is_some_and(|x| !x.is_empty()) {
        return None;
    }
    let max = spec
        .max_version
        .as_deref()
        .map(|m| format!("-max{m}"))
        .unwrap_or_default();
    let memo = dir
        .join("engines")
        .join(format!(".latest-{}-{}{max}", v.id, spec.line));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cached = std::fs::read_to_string(&memo).ok().and_then(|t| {
        let (when, tag) = t.trim().split_once(' ')?;
        let when: u64 = when.parse().ok()?;
        (now.saturating_sub(when) < 86_400).then(|| tag.to_string())
    });
    let tag = match cached {
        Some(t) => t,
        None => {
            let t = match rel.default_tag {
                Some(pinned) => release(rel.repo, Some(pinned)).ok()?.tag_name,
                None => release_for(rel.repo, spec).ok()?,
            };
            let _ = std::fs::write(&memo, format!("{now} {t}"));
            t
        }
    };
    newer_in_line(installed_name, &tag, spec).then_some(tag)
}

/// Is release `tag` an update of the installed build `installed_name` for this recipe (same line, newer, within
/// `max_version`)?
pub fn newer_in_line(installed_name: &str, tag: &str, spec: &crate::recipe::EngineSpec) -> bool {
    spec.admits(tag) && version_key(&format!("x-{tag}")) > version_key(installed_name)
}

/// Install a server from any GitHub release (an engine a plugin brings): the archive whose name contains every token goes
/// to `engines/<id>-<tag>/`, checked against the SHA-256 GitHub publishes. Returns the path of `binary` inside it.
pub fn install_from_release(
    dir: &Path,
    id: &str,
    repo: &str,
    tag: Option<&str>,
    tokens: &[String],
    binary: &str,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<PathBuf> {
    let rel = release(repo, tag)?;
    let asset = rel
        .assets
        .iter()
        .filter(|a| a.name.ends_with(".tar.gz") || a.name.ends_with(".zip"))
        .filter(|a| tokens.iter().all(|t| a.name.contains(t.as_str())))
        .max_by_key(|a| a.name.clone())
        .with_context(|| format!("no asset with {tokens:?} in {repo} {}", rel.tag_name))?;
    let target = dir.join("engines").join(format!("{id}-{}", rel.tag_name));
    if let Some(p) = find_file(&target, binary) {
        return Ok(p);
    }
    let archive = dir.join("downloads").join(&asset.name);
    let sha = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"));
    let label = format!("engine {}", asset.name);
    fetch(&asset.browser_download_url, &archive, sha, &mut |d, t| {
        progress(&label, d, t)
    })?;
    std::fs::create_dir_all(&target)?;
    extract(&archive, &target)?;
    let _ = std::fs::remove_file(&archive);
    let server = find_file(&target, binary)
        .with_context(|| format!("{binary} not found in {}", asset.name))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).ok();
    }
    Ok(server)
}

/// The first file called `name` (or `name.exe` on Windows) below `root`.
pub fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let exe = format!("{name}.exe");
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .is_some_and(|n| n == name || (cfg!(windows) && n == exe.as_str()))
            {
                return Some(p);
            }
        }
    }
    None
}

/// Where an installed engine's llama-server lives, if there is one.
pub fn installed(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("engines")) {
        for e in rd.flatten() {
            if let Some(p) = find_server(&e.path()) {
                out.push((e.file_name().to_string_lossy().to_string(), p));
            }
        }
    }
    out.sort();
    out
}

pub fn find_server(root: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == name) {
                return Some(p);
            }
        }
    }
    None
}

/// The signed SHA-256 of an asset of FLE release `tag` (see [`crate::signature::fle_sha`]).
pub fn fle_signed_sha(tag: &str, asset: &str) -> Result<Option<String>> {
    let c = client()?;
    let get = |name: &str| -> Option<Vec<u8>> {
        let url = format!(
            "https://github.com/{}/releases/download/{tag}/{name}",
            crate::adapter::FLE_REPO
        );
        let r = c.get(&url).send().ok()?;
        r.status().is_success().then_some(())?;
        r.bytes().ok().map(|b| b.to_vec())
    };
    crate::signature::fle_sha(tag, asset, &get)
}

/// Download and unpack an engine; returns the path of its llama-server. With a recipe's engine spec an FLE engine is the
/// newest release of the recipe's line (see [`release_for`]); without one, the latest release. An FLE release is checked
/// against its signed checksums, any other against the SHA-256 GitHub publishes.
pub fn install(
    dir: &Path,
    v: &Variant,
    s: &SysInfo,
    spec: Option<&crate::recipe::EngineSpec>,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<PathBuf> {
    let r = crate::adapter::release_of(v.kind)
        .with_context(|| format!("FLE Link does not install the {} engine", v.kind))?;
    let (repo, tag) = (r.repo, r.default_tag);
    let rel = match spec.filter(|_| tag.is_none() && repo == crate::adapter::FLE_REPO) {
        Some(sp) => release_in_line(repo, sp)?,
        None => release(repo, tag)?,
    };
    let asset = pick(&rel.assets, v, &s.os, &s.arch).with_context(|| {
        format!(
            "no {} build for {}-{} in {repo} {}",
            v.id, s.os, s.arch, rel.tag_name
        )
    })?;
    let target = dir
        .join("engines")
        .join(format!("{}-{}", v.id, rel.tag_name));
    if let Some(p) = find_server(&target) {
        return Ok(p);
    }
    let archive = dir.join("downloads").join(&asset.name);
    let signed = if repo == crate::adapter::FLE_REPO {
        fle_signed_sha(&rel.tag_name, &asset.name)?
    } else {
        None
    };
    let sha = signed.as_deref().or(asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:")));
    let label = format!("engine {}", asset.name);
    fetch(&asset.browser_download_url, &archive, sha, &mut |d, t| {
        progress(&label, d, t)
    })?;
    std::fs::create_dir_all(&target)?;
    extract(&archive, &target)?;
    let _ = std::fs::remove_file(&archive); // unpacked; the archive would only take space (up to a few hundred MB)
    let server = find_server(&target).context("llama-server not found in the archive")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).ok();
    }
    Ok(server)
}

fn safe(dest: &Path, rel: &Path) -> Result<PathBuf> {
    if rel.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        bail!("unsafe path in archive: {}", rel.display());
    }
    Ok(dest.join(rel))
}

fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive.to_string_lossy();
    if name.ends_with(".zip") {
        let mut z = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
        for i in 0..z.len() {
            let mut f = z.by_index(i)?;
            let out = safe(dest, Path::new(f.name()))?;
            if f.is_dir() {
                std::fs::create_dir_all(&out)?;
            } else {
                std::fs::create_dir_all(out.parent().unwrap())?;
                std::io::copy(&mut f, &mut std::fs::File::create(&out)?)?;
            }
        }
        return Ok(());
    }
    let mut t = tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(archive)?));
    for e in t.entries()? {
        let mut e = e?;
        let rel = e.path()?.to_path_buf();
        let out = safe(dest, &rel)?;
        if e.header().entry_type().is_symlink() || e.header().entry_type().is_hard_link() {
            let link = e.link_name()?.map(|l| l.to_path_buf()).unwrap_or_default();
            safe(out.parent().unwrap_or(dest), &link)?; // the link must stay inside too
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        e.unpack(&out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::{Gpu, SysInfo};

    fn s(os: &str, arch: &str, nvidia: bool, avx2: bool) -> SysInfo {
        SysInfo {
            os: os.into(),
            arch: arch.into(),
            cpu: "x".into(),
            cores: 8,
            ram_bytes: 64 << 30,
            disk_free_bytes: 100 << 30,
            avx2,
            gpus: if nvidia {
                vec![Gpu {
                    vendor: "nvidia".into(),
                    name: "g".into(),
                    vram_bytes: Some(32 << 30),
                }]
            } else {
                vec![]
            },
            gpu_usable_bytes: 0,
        }
    }
    #[test]
    fn apple_silicon_gets_metal_and_only_that() {
        let v = variants(&s("macos", "arm64", false, false));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, "upstream-metal");
    }
    #[test]
    fn nvidia_with_avx2_gets_the_fle_engine_first() {
        let v = variants(&s("linux", "x64", true, true));
        assert_eq!(v[0].id, "fle-cuda");
        assert!(
            v.iter().any(|x| x.id == "upstream-cuda")
                && v.iter().any(|x| x.id == "upstream-vulkan")
        );
    }
    #[test]
    fn nvidia_without_avx2_skips_the_fle_engine() {
        assert_eq!(
            variants(&s("linux", "x64", true, false))[0].id,
            "upstream-cuda"
        );
    }
    #[test]
    fn no_gpu_gets_the_cpu_build() {
        assert_eq!(
            variants(&s("linux", "x64", false, true))[0].id,
            "upstream-cpu"
        );
    }
    #[test]
    fn archive_paths_must_stay_inside() {
        let d = Path::new("/tmp/x");
        assert!(safe(d, Path::new("llama/bin/llama-server")).is_ok());
        assert!(safe(d, Path::new("../evil")).is_err());
        assert!(safe(d, Path::new("a/../../evil")).is_err());
        assert!(safe(d, Path::new("/etc/passwd")).is_err());
    }
    #[test]
    fn asset_choice_matches_the_platform_and_skips_cudart() {
        let a = |n: &str| Asset {
            name: n.into(),
            browser_download_url: String::new(),
            digest: None,
        };
        let assets = vec![
            a("llama-b1-bin-macos-arm64.tar.gz"),
            a("llama-b1-bin-macos-x64.tar.gz"),
            a("llama-b1-bin-ubuntu-x64.tar.gz"),
            a("llama-b1-bin-ubuntu-cuda-12.8-x64.tar.gz"),
            a("cudart-llama-bin-win-cuda-12.4-x64.zip"),
            a("llama-b1-bin-win-cuda-12.4-x64.zip"),
        ];
        let m = variants(&s("macos", "arm64", false, false)).remove(0);
        assert_eq!(
            pick(&assets, &m, "macos", "arm64").unwrap().name,
            "llama-b1-bin-macos-arm64.tar.gz"
        );
        let c = variants(&s("windows", "x64", true, false))
            .into_iter()
            .find(|v| v.id == "upstream-cuda")
            .unwrap();
        assert_eq!(
            pick(&assets, &c, "windows", "x64").unwrap().name,
            "llama-b1-bin-win-cuda-12.4-x64.zip"
        );
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn versions_order_by_number_not_by_name() {
        assert!(version_key("fle-cuda-v1.10.0") > version_key("fle-cuda-v1.9.3"));
        assert!(version_key("fle-cuda-v1.2.0") > version_key("fle-cuda-v1.1.0"));
        assert_eq!(version_key("upstream-metal-b11540"), vec![11540]);
        let home = std::env::temp_dir().join(format!("fle-versions-{}", std::process::id()));
        for n in [
            "fle-cuda-v1.9.0",
            "fle-cuda-v1.10.0",
            "upstream-cuda-b11540",
        ] {
            let d = home.join("engines").join(n);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join(if cfg!(windows) {
                    "llama-server.exe"
                } else {
                    "llama-server"
                }),
                "",
            )
            .unwrap();
        }
        assert_eq!(
            newest_installed(&home, "fle-cuda").unwrap().0,
            "fle-cuda-v1.10.0"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    fn fle_variant() -> Variant {
        Variant {
            id: "fle-cuda",
            kind: "fle",
            backend: "cuda",
            label: "FLE engine (NVIDIA CUDA)",
            why: "",
        }
    }

    #[test]
    fn both_lines_sit_side_by_side_and_each_recipe_takes_its_own() {
        use crate::recipe::{EngineLine, EngineSpec};
        let home = std::env::temp_dir().join(format!("fle-lines-{}", std::process::id()));
        for n in [
            "fle-cuda-v1.2.5",
            "fle-cuda-v1.2.6",
            "fle-cuda-v1.3.0",
            "upstream-cuda-b11540",
        ] {
            let d = home.join("engines").join(n).join("bin");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join(if cfg!(windows) {
                    "llama-server.exe"
                } else {
                    "llama-server"
                }),
                "",
            )
            .unwrap();
        }
        let v = fle_variant();
        let moe = EngineSpec::default();
        let dense = EngineSpec {
            line: EngineLine::Dense,
            ..EngineSpec::default()
        };
        assert_eq!(
            newest_installed(&home, "fle-cuda").unwrap().0,
            "fle-cuda-v1.3.0"
        );
        assert_eq!(
            newest_installed_for(&home, &v, &moe).unwrap().0,
            "fle-cuda-v1.2.6"
        );
        assert_eq!(
            newest_installed_for(&home, &v, &dense).unwrap().0,
            "fle-cuda-v1.3.0"
        );
        let capped = EngineSpec {
            max_version: Some("v1.2.5".into()),
            ..EngineSpec::default()
        };
        assert_eq!(
            newest_installed_for(&home, &v, &capped).unwrap().0,
            "fle-cuda-v1.2.5"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_update_stays_in_the_line() {
        use crate::recipe::{EngineLine, EngineSpec};
        let moe = EngineSpec::default();
        let dense = EngineSpec {
            line: EngineLine::Dense,
            ..EngineSpec::default()
        };
        let tags = [
            "v1.3.1",
            "v1.3.0",
            "v1.2.7",
            "v1.2.6",
            "v1.4.0-rc1",
            "v1.2.10",
            "nightly",
        ];
        assert_eq!(pick_tag(tags, &moe).as_deref(), Some("v1.2.10"));
        assert_eq!(pick_tag(tags, &dense).as_deref(), Some("v1.3.1"));
        assert_eq!(pick_tag(["v1.2.6"], &dense), None);
        let capped = EngineSpec {
            max_version: Some("v1.2.7".into()),
            ..EngineSpec::default()
        };
        assert_eq!(pick_tag(tags, &capped).as_deref(), Some("v1.2.7"));
        assert!(newer_in_line("fle-cuda-v1.2.6", "v1.2.7", &moe));
        assert!(
            !newer_in_line("fle-cuda-v1.2.6", "v1.3.0", &moe),
            "dense is no update of moe"
        );
        assert!(newer_in_line("fle-cuda-v1.3.0", "v1.3.1", &dense));
        assert!(!newer_in_line("fle-cuda-v1.3.1", "v1.3.1", &dense));
    }

    #[test]
    fn the_release_feed_names_its_tags() {
        let xml = r#"<feed><entry><link rel="alternate" type="text/html" href="https://github.com/sirfyyn/FastLocalExperts/releases/tag/v1.3.0"/>
            <id>tag:github.com,2008:Repository/1/v1.3.0</id></entry>
            <entry><link rel="alternate" type="text/html" href="https://github.com/sirfyyn/FastLocalExperts/releases/tag/v1.2.6"/></entry>
            <entry><link href="https://github.com/sirfyyn/FastLocalExperts/releases/tag/v1.2.6"/></entry></feed>"#;
        assert_eq!(
            parse_atom_tags(xml, "sirfyyn/FastLocalExperts"),
            vec!["v1.3.0".to_string(), "v1.2.6".to_string()]
        );
    }

    /// Live: the newest release of each line (`cargo test -p flelink-core -- --ignored line_live`).
    #[test]
    #[ignore]
    fn line_live_fle_releases() {
        use crate::recipe::EngineSpec;
        let t = release_for(crate::adapter::FLE_REPO, &EngineSpec::default()).unwrap();
        assert!(t.starts_with("v1.2."), "{t}");
    }
}

#[cfg(test)]
mod sums_tests {
    use super::*;

    #[test]
    fn sha256sums_become_assets_with_their_checksum() {
        let text = "35a728b2ad5c71759edcd60cd0916cc87886d5657340d0589c20e8ea2dda0d4e  fle-d90513f3-bin-ubuntu-cuda-12.8-x64.tar.gz\n\
                    53EEDF7A7E63D35E8986615753308B0C9A3BAE80554D7B2B80E61ED1DAD2CFD3 *fle-win.zip\nnot a line\n\
                    abcd  ../evil\n";
        let a = parse_sums(text, "https://github.com/o/r/releases/download/v1.2.3");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].browser_download_url, "https://github.com/o/r/releases/download/v1.2.3/fle-d90513f3-bin-ubuntu-cuda-12.8-x64.tar.gz");
        assert_eq!(a[1].name, "fle-win.zip");
        assert!(a[1].digest.as_deref().unwrap().starts_with("sha256:53eedf"));
    }

    #[test]
    fn the_release_page_gives_files_with_their_digest() {
        // shape of github.com/<repo>/releases/expanded_assets/<tag>: link, then the digest (shown more than once)
        let h = "a".repeat(64);
        let k = "B".repeat(64);
        let html = format!(
            "<a href=\"/ggml-org/llama.cpp/releases/download/b1/llama-b1-bin-macos-arm64.zip\">x</a>\
             <span>sha256:{h}</span><clipboard-copy value=\"sha256:{h}\">\
             <a href=\"/ggml-org/llama.cpp/releases/download/b1/no-digest.zip\">y</a>\
             <a href=\"/ggml-org/llama.cpp/releases/download/b1/llama-b1-bin-win-cpu-x64.zip\">z</a><span>sha256:{k}</span>\
             <a href=\"/ggml-org/llama.cpp/archive/refs/tags/b1.zip\">source</a>"
        );
        let a = parse_expanded(&html, "ggml-org/llama.cpp", "b1");
        assert_eq!(a.len(), 2, "the file without a digest is left out");
        assert_eq!(a[0].name, "llama-b1-bin-macos-arm64.zip");
        assert_eq!(a[0].browser_download_url, "https://github.com/ggml-org/llama.cpp/releases/download/b1/llama-b1-bin-macos-arm64.zip");
        assert_eq!(a[0].digest.as_deref(), Some(format!("sha256:{h}").as_str()));
        assert_eq!(
            a[1].digest.as_deref(),
            Some(format!("sha256:{}", "b".repeat(64)).as_str())
        );
    }

    /// Live: the upstream release page without the API (`cargo test -p flelink-core -- --ignored page_live`).
    #[test]
    #[ignore]
    fn page_live_upstream_llama_cpp() {
        let r = release_from_page("ggml-org/llama.cpp", Some("b11540")).unwrap();
        assert!(r.assets.len() > 20, "{} files", r.assets.len());
        assert!(r.assets.iter().any(|a| a.name.contains("macos-arm64")));
    }
}
