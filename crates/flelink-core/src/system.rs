use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Gpu {
    pub vendor: String, // nvidia | apple | other
    pub name: String,
    pub vram_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct SysInfo {
    pub os: String,   // macos | linux | windows
    pub arch: String, // arm64 | x64
    pub cpu: String,
    pub cores: usize,
    pub ram_bytes: u64,
    pub disk_free_bytes: u64,
    pub avx2: bool,
    pub gpus: Vec<Gpu>,
    /// Memory the GPU may use: unified memory limit on Apple Silicon, VRAM of the biggest NVIDIA card otherwise.
    pub gpu_usable_bytes: u64,
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[cfg(target_arch = "x86_64")]
fn has_avx2() -> bool {
    std::is_x86_feature_detected!("avx2")
}
#[cfg(not(target_arch = "x86_64"))]
fn has_avx2() -> bool {
    false
}

/// Free space in the file system of `dir` (created if missing); 0 when it cannot be told.
pub fn free_bytes(dir: &Path) -> u64 {
    let _ = std::fs::create_dir_all(dir);
    run("df", &["-k", &dir.to_string_lossy()])
        .and_then(|o| {
            o.lines().nth(1).and_then(|l| {
                l.split_whitespace()
                    .nth(3)
                    .and_then(|v| v.parse::<u64>().ok())
            })
        })
        .map(|k| k * 1024)
        .unwrap_or(0)
}

/// Cores without SMT siblings (the engine's `-t` wants these); the logical count when the system does not say.
pub fn physical_cores() -> usize {
    let logical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let found = match std::env::consts::OS {
        "linux" => {
            let text = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
            let mut cores = std::collections::BTreeSet::new();
            for block in text.split("\n\n") {
                let get = |key: &str| {
                    block.lines().find_map(|l| {
                        l.strip_prefix(key)
                            .and_then(|r| r.trim_start().strip_prefix(':'))
                            .map(|v| v.trim().to_string())
                    })
                };
                if let (Some(p), Some(c)) = (get("physical id"), get("core id")) {
                    cores.insert((p, c));
                }
            }
            cores.len()
        }
        "macos" => run("sysctl", &["-n", "hw.physicalcpu"])
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0),
        "windows" => run(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfCores -Sum).Sum",
            ],
        )
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0),
        _ => 0,
    };
    if found == 0 || found > logical {
        logical
    } else {
        found
    }
}

/// Before an engine that locks hot experts in RAM starts (`FLE_MOE_HOT_MLOCK_GB` in its environment): raise this process's
/// soft limit for locked memory to the hard limit, so the engine inherits it. The hard limit itself needs the system's
/// permission; when it is below what the engine wants, the returned text says how to allow it (the engine then runs without
/// the lock, slower).
#[allow(clippy::unnecessary_cast)] // rlim_t is not u64 everywhere
pub fn allow_memlock(env: &[(String, String)]) -> Option<String> {
    let gb: u64 = env
        .iter()
        .find(|(k, _)| k == "FLE_MOE_HOT_MLOCK_GB")
        .and_then(|(_, v)| v.trim().parse().ok())
        .filter(|g| *g > 0)?;
    #[cfg(unix)]
    {
        let mut r = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: plain getrlimit/setrlimit on this process with a valid struct
        unsafe {
            if libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut r) != 0 {
                return None;
            }
            r.rlim_cur = r.rlim_max;
            libc::setrlimit(libc::RLIMIT_MEMLOCK, &r);
        }
        let want = gb << 30;
        if r.rlim_max != libc::RLIM_INFINITY && (r.rlim_max as u64) < want {
            return Some(format!(
                "FLE Link: the system allows only {} MiB of locked memory, the hot expert lock wants {gb} GiB; it runs without the lock (slower). Allow it once (as root): a line `<user> - memlock unlimited` in /etc/security/limits.d/fle.conf, then log in again.",
                r.rlim_max as u64 >> 20
            ));
        }
        None
    }
    #[cfg(not(unix))]
    {
        let _ = gb;
        None
    }
}

pub fn detect(dir: &Path) -> SysInfo {
    let os = std::env::consts::OS.to_string();
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        a => a,
    }
    .to_string();
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let (mut cpu, mut ram) = (String::new(), 0u64);
    if os == "macos" {
        cpu = run("sysctl", &["-n", "machdep.cpu.brand_string"]).unwrap_or_default();
        ram = run("sysctl", &["-n", "hw.memsize"])
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
    } else if os == "linux" {
        let mi = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        ram = mi
            .lines()
            .find(|l| l.starts_with("MemTotal"))
            .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024;
        let ci = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        cpu = ci
            .lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split(':').nth(1))
            .unwrap_or("")
            .trim()
            .to_string();
    }
    if os == "windows" {
        // PowerShell is on every supported Windows; wmic is gone from newer ones
        let ps = |cmd: &str| {
            run(
                "powershell",
                &["-NoProfile", "-NonInteractive", "-Command", cmd],
            )
        };
        ram = ps("(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        cpu = ps("(Get-CimInstance Win32_Processor | Select-Object -First 1).Name")
            .unwrap_or_default();
    }
    let avx2 = has_avx2(); // the CPU says so itself, on every OS
    let mut gpus = Vec::new();
    if let Some(out) = run(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ],
    ) {
        for line in out.lines() {
            let mut p = line.split(',');
            let name = p.next().unwrap_or("").trim().to_string();
            let mib: u64 = p.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            gpus.push(Gpu {
                vendor: "nvidia".into(),
                name,
                vram_bytes: Some(mib * 1024 * 1024),
            });
        }
    }
    let apple = os == "macos" && arch == "arm64";
    if apple {
        gpus.push(Gpu {
            vendor: "apple".into(),
            name: format!("{} GPU", cpu),
            vram_bytes: None,
        });
    }
    let gpu_usable_bytes = if apple {
        (ram as f64 * 0.667) as u64 // what Metal lets one process use (recommendedMaxWorkingSetSize is about 2/3)
    } else {
        gpus.iter().filter_map(|g| g.vram_bytes).max().unwrap_or(0)
    };
    let disk_free_bytes = free_bytes(dir);
    SysInfo {
        os,
        arch,
        cpu,
        cores,
        ram_bytes: ram,
        disk_free_bytes,
        avx2,
        gpus,
        gpu_usable_bytes,
    }
}

pub fn gib(b: u64) -> String {
    format!("{:.1} GiB", b as f64 / 1073741824.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gib_formats() {
        assert_eq!(gib(1 << 30), "1.0 GiB");
        assert_eq!(gib(3 << 29), "1.5 GiB");
    }
    #[test]
    fn detect_here_finds_memory_and_a_writable_disk() {
        let d = std::env::temp_dir().join("fle-test-system");
        let s = detect(&d);
        assert!(s.ram_bytes > 0 && s.cores > 0 && s.disk_free_bytes > 0);
        assert!(!s.os.is_empty() && !s.arch.is_empty());
    }
}

#[cfg(all(test, unix))]
mod memlock_tests {
    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn the_lock_limit_is_raised_or_explained() {
        assert_eq!(super::allow_memlock(&[]), None);
        assert_eq!(
            super::allow_memlock(&[("FLE_MOE_HOT_MLOCK_GB".into(), "0".into())]),
            None
        );
        let note = super::allow_memlock(&[("FLE_MOE_HOT_MLOCK_GB".into(), "60".into())]);
        let mut r = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut r) };
        // the soft limit now equals the hard one, and a hard limit below 60 GiB is explained
        assert_eq!(r.rlim_cur, r.rlim_max);
        let enough = r.rlim_max == libc::RLIM_INFINITY || (r.rlim_max as u64) >= 60 << 30;
        assert_eq!(note.is_none(), enough, "{note:?}");
        if let Some(n) = note {
            assert!(n.contains("memlock unlimited"), "{n}");
        }
    }
}
