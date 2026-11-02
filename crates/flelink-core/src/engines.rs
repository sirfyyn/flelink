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

/// A release without the API: the tag from `/releases/latest` (it redirects to `/releases/tag/<tag>`), the files and
/// their SHA-256 from the release's `SHA256SUMS`.
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
    if !sums.status().is_success() {
        bail!("no SHA256SUMS in release {tag}");
    }
    let assets = parse_sums(&sums.text()?, &base);
    if assets.is_empty() {
        bail!("SHA256SUMS of {tag} names no files");
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
    installed(dir)
        .into_iter()
        .filter(|(n, _)| n.starts_with(&format!("{variant_id}-")))
        .max_by_key(|(n, _)| version_key(n))
}

/// A newer FLE release than `installed_name`, if there is one. Asks GitHub at most once a day (the answer is kept in
/// `<data>/engines/.latest-<variant>`); offline, on any error and with FLE_NO_UPDATE set it says nothing. Only the FLE
/// engine follows its releases (its adapter says so): upstream llama.cpp is pinned (LLAMA_TAG).
pub fn update_available(dir: &Path, v: &Variant, installed_name: &str) -> Option<String> {
    let rel = crate::adapter::release_of(v.kind).filter(|r| r.self_updates)?;
    if std::env::var_os("FLE_NO_UPDATE").is_some_and(|x| !x.is_empty()) {
        return None;
    }
    let memo = dir.join("engines").join(format!(".latest-{}", v.id));
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
            let t = release(rel.repo, rel.default_tag).ok()?.tag_name;
            let _ = std::fs::write(&memo, format!("{now} {t}"));
            t
        }
    };
    let have = version_key(installed_name);
    (version_key(&format!("x-{tag}")) > have).then_some(tag)
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

/// Download and unpack an engine; returns the path of its llama-server. Checks the SHA-256 GitHub publishes.
pub fn install(
    dir: &Path,
    v: &Variant,
    s: &SysInfo,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<PathBuf> {
    let r = crate::adapter::release_of(v.kind)
        .with_context(|| format!("FLE Link does not install the {} engine", v.kind))?;
    let (repo, tag) = (r.repo, r.default_tag);
    let rel = release(repo, tag)?;
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
}
