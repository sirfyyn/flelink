//! Small statistics for comparing speed measurements. Port of `fle_studio/stats.py` (the one-sample part the speed test
//! uses): the median of repeated runs with a 95 % interval, Student's t for few runs, a percentile bootstrap of the median
//! from 8 runs on. The bootstrap draws with Python's own generator (Mersenne Twister, `random.Random(seed).randrange`), so
//! the same numbers give the same interval as in the Python Studio.
use serde_json::{json, Value};

pub const BOOTSTRAP_MIN: usize = 8;
pub const BOOTSTRAP_DRAWS: usize = 2000;
pub const SEED: u32 = 20261011;

// ------------------------------------------------------------------------------------------------ Python's random

/// MT19937 as CPython seeds it from an int (`init_by_array` with the int's 32-bit words).
pub struct PyRandom {
    mt: [u32; 624],
    i: usize,
}

impl PyRandom {
    pub fn new(seed: u32) -> PyRandom {
        let mut mt = [0u32; 624];
        mt[0] = 19_650_218;
        for i in 1..624 {
            mt[i] = 1_812_433_253u32
                .wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        let key = [seed];
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..624.max(key.len()) {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= 624 {
                mt[0] = mt[623];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..623 {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1_566_083_941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= 624 {
                mt[0] = mt[623];
                i = 1;
            }
        }
        mt[0] = 0x8000_0000;
        PyRandom { mt, i: 624 }
    }

    fn next_u32(&mut self) -> u32 {
        if self.i >= 624 {
            for k in 0..624 {
                let y = (self.mt[k] & 0x8000_0000) | (self.mt[(k + 1) % 624] & 0x7fff_ffff);
                let mut v = self.mt[(k + 397) % 624] ^ (y >> 1);
                if y & 1 != 0 {
                    v ^= 0x9908_b0df;
                }
                self.mt[k] = v;
            }
            self.i = 0;
        }
        let mut y = self.mt[self.i];
        self.i += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `randrange(n)` for 0 < n < 2^32: `getrandbits(n.bit_length())` until it is below n.
    pub fn randrange(&mut self, n: usize) -> usize {
        let k = usize::BITS - n.leading_zeros();
        loop {
            let r = (self.next_u32() >> (32 - k)) as usize;
            if r < n {
                return r;
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ t distribution

/// ln Γ(x) for x > 0 (Lanczos, g = 7; relative error about 1e-15).
fn lgamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - lgamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = G[0];
    let t = x + 7.5;
    for (i, g) in G.iter().enumerate().skip(1) {
        a += g / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

fn betacf(a: f64, b: f64, x: f64) -> f64 {
    let tiny = 1e-300;
    let fix = |v: f64| if v.abs() > tiny { v } else { tiny };
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 / fix(1.0 - qab * x / qap);
    let mut h = d;
    for m in 1..300 {
        let m = f64::from(m);
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 / fix(1.0 + aa * d);
        c = fix(1.0 + aa / c);
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 / fix(1.0 + aa * d);
        c = fix(1.0 + aa / c);
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < 3e-14 {
            break;
        }
    }
    h
}

fn betainc(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front = (lgamma(a + b) - lgamma(a) - lgamma(b) + a * x.ln() + b * (-x).ln_1p()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        front * betacf(a, b, x) / a
    } else {
        1.0 - front * betacf(b, a, 1.0 - x) / b
    }
}

pub fn t_cdf(t: f64, df: f64) -> f64 {
    let x = df / (df + t * t);
    let tail = 0.5 * betainc(df / 2.0, 0.5, x);
    if t > 0.0 {
        1.0 - tail
    } else {
        tail
    }
}

/// The t value with `t_cdf(t, df) == p` (bisection), for 0 < p < 1.
pub fn t_quantile(p: f64, df: f64) -> f64 {
    if p == 0.5 {
        return 0.0;
    }
    if p < 0.5 {
        return -t_quantile(1.0 - p, df);
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    while t_cdf(hi, df) < p {
        hi *= 2.0;
        if hi > 1e9 {
            break;
        }
    }
    for _ in 0..100 {
        let mid = (lo + hi) / 2.0;
        if t_cdf(mid, df) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

// ------------------------------------------------------------------------------------------------ one sample

pub fn median(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn mean(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len() as f64
}

fn stdev(xs: &[f64]) -> f64 {
    let m = mean(xs);
    (xs.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (xs.len() - 1) as f64).sqrt()
}

/// Python's `round(x, n)` for the values here (half to even on the decimal value).
pub fn round_to(x: f64, n: i32) -> f64 {
    let f = 10f64.powi(n);
    let y = x * f;
    let r = y.round();
    let r = if (y - y.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        r - y.signum()
    } else {
        r
    };
    r / f
}

pub fn t_interval(xs: &[f64], level: f64) -> Option<(f64, f64)> {
    if xs.len() < 2 {
        return None;
    }
    let m = mean(xs);
    let sem = stdev(xs) / (xs.len() as f64).sqrt();
    let q = t_quantile(0.5 + level / 2.0, (xs.len() - 1) as f64);
    Some((m - q * sem, m + q * sem))
}

pub fn bootstrap_interval(xs: &[f64], level: f64, draws: usize, seed: u32) -> Option<(f64, f64)> {
    if xs.len() < 2 {
        return None;
    }
    let mut rng = PyRandom::new(seed);
    let n = xs.len();
    let mut stats: Vec<f64> = (0..draws)
        .map(|_| {
            let s: Vec<f64> = (0..n).map(|_| xs[rng.randrange(n)]).collect();
            median(&s)
        })
        .collect();
    stats.sort_by(f64::total_cmp);
    let alpha = (1.0 - level) / 2.0;
    Some((
        stats[(alpha * draws as f64) as usize],
        stats[(draws - 1).min(((1.0 - alpha) * draws as f64) as usize)],
    ))
}

/// Median and 95 % interval of repeated runs; `lo`/`hi` are null below 3 runs.
pub fn summarize(values: &[f64]) -> Value {
    let xs: Vec<f64> = values.iter().copied().filter(|v| !v.is_nan()).collect();
    let mut out = json!({"n": xs.len(), "median": null, "mean": null, "lo": null, "hi": null, "method": null});
    if xs.is_empty() {
        return out;
    }
    out["median"] = json!(round_to(median(&xs), 4));
    out["mean"] = json!(round_to(mean(&xs), 4));
    if xs.len() >= 3 {
        let (ci, method) = if xs.len() >= BOOTSTRAP_MIN {
            (
                bootstrap_interval(&xs, 0.95, BOOTSTRAP_DRAWS, SEED),
                "bootstrap",
            )
        } else {
            (t_interval(&xs, 0.95), "t")
        };
        out["method"] = json!(method);
        if let Some((lo, hi)) = ci {
            out["lo"] = json!(round_to(lo, 4));
            out["hi"] = json!(round_to(hi, 4));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_is_pythons() {
        // python3 -c "import random; r=random.Random(20261011); print([r.randrange(10) for _ in range(12)])"
        let mut r = PyRandom::new(SEED);
        let got: Vec<usize> = (0..12).map(|_| r.randrange(10)).collect();
        assert_eq!(got, PY_RANDRANGE_10);
    }

    const PY_RANDRANGE_10: [usize; 12] = [7, 5, 7, 5, 4, 0, 0, 5, 7, 0, 0, 5];

    #[test]
    fn summaries_match_python() {
        // from fle_studio.stats.summarize (Python Studio 055f4aa)
        let cases: [(&[f64], Value); 4] = [
            (
                &[10.0, 11.0, 12.0],
                json!({"n": 3, "median": 11.0, "mean": 11.0, "lo": 8.5159, "hi": 13.4841, "method": "t"}),
            ),
            (
                &[10.0, 11.0, 12.0, 10.5, 11.5, 12.5, 9.5, 13.0],
                json!({"n": 8, "median": 11.25, "mean": 11.25, "lo": 10.0, "hi": 12.5, "method": "bootstrap"}),
            ),
            (
                &[
                    23.31, 23.5, 23.42, 23.38, 23.47, 23.29, 23.51, 23.44, 23.36, 23.4,
                ],
                json!({"n": 10, "median": 23.41, "mean": 23.408, "lo": 23.355, "hi": 23.47, "method": "bootstrap"}),
            ),
            (
                &[5.5, 7.25, 6.0, 6.5],
                json!({"n": 4, "median": 6.25, "mean": 6.3125, "lo": 5.1246, "hi": 7.5004, "method": "t"}),
            ),
        ];
        for (v, want) in cases {
            assert_eq!(summarize(v), want, "{v:?}");
        }
    }

    #[test]
    fn t_quantiles_match_the_table() {
        assert!((t_quantile(0.975, 2.0) - 4.302_652_7).abs() < 1e-6);
        assert!((t_quantile(0.975, 9.0) - 2.262_157_2).abs() < 1e-6);
        assert!((t_quantile(0.975, 1e6) - 1.959_966).abs() < 1e-4);
    }

    #[test]
    fn summaries_have_an_interval_from_three_runs() {
        assert!(summarize(&[1.0, 2.0])["lo"].is_null());
        let s = summarize(&[10.0, 11.0, 12.0]);
        assert_eq!(s["method"], "t");
        assert_eq!(s["median"], 11.0);
        let s = summarize(&[10.0, 11.0, 12.0, 10.5, 11.5, 12.5, 9.5, 13.0]);
        assert_eq!(s["method"], "bootstrap");
    }
}
