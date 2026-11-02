use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("flelink/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(20))
        // per read, not for the whole file: a connection that delivers nothing for a minute counts as broken
        .timeout(std::time::Duration::from_secs(60))
        .build()?)
}

fn hf_token() -> Option<String> {
    std::env::var("HF_TOKEN").ok().or_else(|| {
        let p = dirs::home_dir()?.join(".cache/huggingface/token");
        fs::read_to_string(p).ok().map(|s| s.trim().to_string())
    })
}

/// How often a broken connection is picked up again by itself, and how long it waits before each try.
const RETRIES: u32 = 5;
fn pause(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(
        if cfg!(test) || std::env::var_os("FLE_TEST_FAST_RETRY").is_some() {
            10
        } else {
            1000 << attempt.min(4)
        },
    )
}

/// One request: `Ok(true)` when the server says the part file is already complete (416).
enum Fail {
    /// worth trying again: the connection broke, the data stopped coming
    Again(anyhow::Error),
    Stop(anyhow::Error),
}

fn once(
    c: &reqwest::blocking::Client,
    url: &str,
    part: &Path,
    have: &mut u64,
    hasher: &mut Sha256,
    hashing: bool,
    progress: &mut dyn FnMut(u64, u64),
) -> std::result::Result<bool, Fail> {
    let mut req = c.get(url);
    if let Some(t) = hf_token().filter(|_| url.contains("huggingface.co")) {
        req = req.bearer_auth(t);
    }
    if *have > 0 {
        req = req.header("Range", format!("bytes={have}-"));
    }
    // a connection that cannot be made at all is not retried: no network should say so at once
    let mut resp = req.send().map_err(|e| {
        let e = anyhow::Error::new(e);
        if *have > 0 {
            Fail::Again(e)
        } else {
            Fail::Stop(e)
        }
    })?;
    let status = resp.status().as_u16();
    if status == 416 {
        return Ok(true);
    }
    if status == 401 || status == 403 {
        return Err(Fail::Stop(anyhow::anyhow!(
            "access denied ({status}): gated repo? Accept its terms on Hugging Face and set HF_TOKEN"
        )));
    }
    if matches!(status, 429 | 500..=599) && *have > 0 {
        return Err(Fail::Again(anyhow::anyhow!(
            "download failed: HTTP {status} for {url}"
        )));
    }
    if !resp.status().is_success() {
        return Err(Fail::Stop(anyhow::anyhow!(
            "download failed: HTTP {status} for {url}"
        )));
    }
    if *have > 0 && status == 200 {
        // server ignored Range: start over
        *have = 0;
        *hasher = Sha256::new();
        fs::remove_file(part).ok();
    }
    let total = *have + resp.content_length().unwrap_or(0);
    let stop = |e: std::io::Error| Fail::Stop(e.into());
    let mut out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(part)
        .map_err(stop)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut last = std::time::Instant::now();
    loop {
        let n = match resp.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                out.flush().ok();
                progress(*have, total);
                return Err(Fail::Again(e.into()));
            }
        };
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(stop)?;
        if hashing {
            hasher.update(&buf[..n]);
        }
        *have += n as u64;
        if last.elapsed().as_millis() > 150 {
            progress(*have, total);
            last = std::time::Instant::now();
        }
    }
    out.flush().map_err(stop)?;
    progress(*have, total);
    if total > 0 && *have < total {
        return Err(Fail::Again(anyhow::anyhow!(
            "connection ended early ({have} of {total} bytes); run again to resume"
        )));
    }
    Ok(false)
}

/// Download `url` to `dest`, resuming from `dest.part`, verifying SHA-256 when known. `progress(done, total)`.
/// A connection that breaks or stalls on the way is picked up again where it stopped, up to five times in a row.
pub fn fetch(
    url: &str,
    dest: &Path,
    sha256: Option<&str>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    fs::create_dir_all(dest.parent().context("no parent")?)?;
    let part = dest.with_extension(format!(
        "{}part",
        dest.extension()
            .map(|e| format!("{}.", e.to_string_lossy()))
            .unwrap_or_default()
    ));
    let mut have = part.metadata().map(|m| m.len()).unwrap_or(0);
    let mut hasher = Sha256::new();
    if have > 0 && sha256.is_some() {
        let mut f = File::open(&part)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    let c = client()?;
    let mut tries = 0;
    loop {
        let before = have;
        match once(
            &c,
            url,
            &part,
            &mut have,
            &mut hasher,
            sha256.is_some(),
            progress,
        ) {
            Ok(true) => {
                fs::rename(&part, dest)?;
                return Ok(());
            }
            Ok(false) => break,
            Err(Fail::Stop(e)) => return Err(e),
            Err(Fail::Again(e)) => {
                // progress since the last break starts the count again: only a connection that keeps failing gives up
                if have > before {
                    tries = 0;
                }
                tries += 1;
                if tries > RETRIES {
                    return Err(e.context(format!(
                        "the download stopped after {RETRIES} tries in a row without progress; run again to resume"
                    )));
                }
                std::thread::sleep(pause(tries));
            }
        }
    }
    if let Some(want) = sha256 {
        let got = hex::encode(hasher.finalize());
        if !got.eq_ignore_ascii_case(want) {
            let bad = dest.with_extension("bad");
            fs::rename(&part, &bad).ok();
            bail!(
                "checksum mismatch (got {got}); file moved to {}",
                bad.display()
            );
        }
    }
    fs::rename(&part, dest)?;
    Ok(())
}
