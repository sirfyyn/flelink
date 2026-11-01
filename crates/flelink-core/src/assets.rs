//! Which file of an engine release is the engine for this machine: small name templates per variant (`{n}` digits,
//! `{cuda}` a CUDA version), the same for the terminal and the Studio. A loose match on parts of the name would take the
//! wrong archive the day a release adds one with a similar name; these say exactly what an archive is called.

/// `windows` and `macos` (the terminal's names) as `win` and `mac` (the Studio's); `linux` stays.
fn os_name(os: &str) -> &str {
    match os {
        "windows" | "win" => "win",
        "linux" => "linux",
        _ => "mac",
    }
}

/// A release asset name against one of the small patterns below. Each pattern is a template: `{n}` is digits, `{cuda}` a
/// CUDA version (digits and dots); everything else must match exactly.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern(Vec<Part>);
#[derive(Debug, Clone, PartialEq)]
enum Part {
    Lit(String),
    Digits,
    AnyRun, // `.+`
    Cuda,
}

impl Pattern {
    /// Returns the CUDA version the name carries, `Some(None)` for a match without one, `None` for no match.
    pub fn matches(&self, name: &str) -> Option<Option<String>> {
        fn go(parts: &[Part], s: &str, cuda: &mut Option<String>) -> bool {
            match parts.split_first() {
                None => s.is_empty(),
                Some((Part::Lit(l), rest)) => s
                    .strip_prefix(l.as_str())
                    .is_some_and(|r| go(rest, r, cuda)),
                Some((Part::Digits, rest)) => {
                    let n = s.bytes().take_while(u8::is_ascii_digit).count();
                    (1..=n).rev().any(|k| go(rest, &s[k..], cuda))
                }
                Some((Part::AnyRun, rest)) => {
                    (1..=s.len()).any(|k| s.is_char_boundary(k) && go(rest, &s[k..], cuda))
                }
                Some((Part::Cuda, rest)) => {
                    let n = s
                        .bytes()
                        .take_while(|b| b.is_ascii_digit() || *b == b'.')
                        .count();
                    (1..=n).rev().any(|k| {
                        let mut c2 = Some(s[..k].to_string());
                        if go(rest, &s[k..], &mut c2) {
                            *cuda = c2;
                            true
                        } else {
                            false
                        }
                    })
                }
            }
        }
        let mut cuda = None;
        go(&self.0, name, &mut cuda).then_some(cuda)
    }
}

fn pat(parts: Vec<Part>) -> Pattern {
    Pattern(parts)
}
fn lit(s: &str) -> Part {
    Part::Lit(s.to_string())
}

/// The release assets of a variant; the first is the engine, CUDA builds add their runtime.
pub fn asset_patterns(variant: &str, os: &str, arch: &str) -> Result<Vec<Pattern>, String> {
    let os = os_name(os);
    let ext = if os == "win" { ".zip" } else { ".tar.gz" };
    let plat = match os {
        "linux" => "ubuntu",
        "win" => "win",
        _ => "macos",
    };
    Ok(vec![match variant {
        "upstream-metal" | "upstream-macos-x64" => pat(vec![
            lit("llama-b"),
            Part::Digits,
            lit(&format!("-bin-macos-{arch}{ext}")),
        ]),
        "upstream-cpu" => {
            let cpu = if os == "win" { "cpu-" } else { "" };
            pat(vec![
                lit("llama-b"),
                Part::Digits,
                lit(&format!("-bin-{plat}-{cpu}{arch}{ext}")),
            ])
        }
        "upstream-vulkan" => pat(vec![
            lit("llama-b"),
            Part::Digits,
            lit(&format!("-bin-{plat}-vulkan-{arch}{ext}")),
        ]),
        "upstream-cuda" => pat(vec![
            lit("llama-b"),
            Part::Digits,
            lit(&format!("-bin-{plat}-cuda-")),
            Part::Cuda,
            lit(&format!("-{arch}{ext}")),
        ]),
        "fle-cuda" => pat(vec![
            lit("fle-"),
            Part::AnyRun,
            lit(&format!("-bin-{plat}-cuda-")),
            Part::Cuda,
            lit(&format!("-{arch}{ext}")),
        ]),
        other => return Err(format!("unknown engine {other}")),
    }])
}

pub fn cudart_pattern(os: &str, arch: &str, cuda: &str) -> Pattern {
    if os_name(os) == "win" {
        pat(vec![lit(&format!(
            "cudart-llama-bin-win-cuda-{cuda}-{arch}.zip"
        ))])
    } else {
        pat(vec![
            lit("cudart-llama-b"),
            Part::Digits,
            lit(&format!("-bin-ubuntu-cuda-{cuda}-{arch}.tar.gz")),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fle_release_names_match_and_carry_their_cuda_version() {
        let p = asset_patterns("fle-cuda", "linux", "x64")
            .unwrap()
            .remove(0);
        assert_eq!(
            p.matches("fle-c80bda92-bin-ubuntu-cuda-12.8-x64.tar.gz"),
            Some(Some("12.8".into()))
        );
        assert_eq!(p.matches("fle-c80bda92-bin-win-cuda-13.4-x64.zip"), None);
        let w = asset_patterns("fle-cuda", "windows", "x64")
            .unwrap()
            .remove(0);
        assert_eq!(
            w.matches("fle-c80bda92-bin-win-cuda-13.4-x64.zip"),
            Some(Some("13.4".into()))
        );
        // the runtime package is not the engine
        let u = asset_patterns("upstream-cuda", "win", "x64")
            .unwrap()
            .remove(0);
        assert_eq!(u.matches("cudart-llama-bin-win-cuda-12.4-x64.zip"), None);
        assert!(cudart_pattern("windows", "x64", "12.4")
            .matches("cudart-llama-bin-win-cuda-12.4-x64.zip")
            .is_some());
        assert!(asset_patterns("nope", "linux", "x64").is_err());
    }
}
