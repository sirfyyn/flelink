//! A quant cut to how a model is used (Forge mode "usage", 4 Nov 2026, decision 774): the FLE usage profile says how often
//! each expert of each layer is picked, the GPU cache budget says which of them sit on the GPU. Experts on the GPU cost
//! almost no decode speed per byte, experts read on the CPU cost speed with every byte. So the bits go where they are
//! cheap: a layer whose traffic the cache serves can keep more bits, a layer whose experts are mostly read on the CPU gets
//! fewer.
//!
//! Stage 1 (this file): one type per layer and role (gate, up, down), because a GGUF expert tensor has one type for all
//! its experts. The plan already keeps a type per expert (`Plan::experts`), all equal for now; FLE v1.2.6 plans split
//! hot/cold expert tensors, and then the same plan can give each expert its own type.
//!
//! The profile format (FLE src/llama-moecache.cpp, version 1, text): header lines `FLE-MOE-PROFILE v1`, `model_size N`,
//! `model_desc …`, `n_layer N`, `steps N`, `layers N`; then per layer `L <il> <n_expert> <slot_bytes>` and a line of
//! n_expert counts. FLE refuses a profile whose header names another model, so a profile taken on one quant is rewritten
//! for the new one (`Profile::for_model`): the counts are per expert and do not depend on the quant.
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct LayerUse {
    pub il: u32,
    pub slot_bytes: u64,
    pub counts: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub model_size: u64,
    pub model_desc: String,
    pub n_layer: u32,
    pub steps: u64,
    pub layers: Vec<LayerUse>,
}

impl Profile {
    pub fn parse(text: &str) -> Result<Profile, String> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        if lines.next() != Some("FLE-MOE-PROFILE v1") {
            return Err("not an FLE usage profile (version 1)".into());
        }
        let (mut model_size, mut model_desc, mut n_layer, mut steps) =
            (0u64, String::new(), 0u32, 0u64);
        let mut layers = vec![];
        let mut pending: Option<(u32, usize, u64)> = None;
        for l in lines {
            if let Some((il, n, slot)) = pending.take() {
                let counts: Vec<u64> = l
                    .split_whitespace()
                    .map(|x| {
                        x.parse()
                            .map_err(|_| format!("layer {il}: bad count {x:?}"))
                    })
                    .collect::<Result<_, _>>()?;
                if counts.len() != n {
                    return Err(format!(
                        "layer {il}: {} counts for {n} experts",
                        counts.len()
                    ));
                }
                layers.push(LayerUse {
                    il,
                    slot_bytes: slot,
                    counts,
                });
                continue;
            }
            let (k, v) = l.split_once(' ').unwrap_or((l, ""));
            match k {
                "model_size" => model_size = v.trim().parse().map_err(|_| "bad model_size")?,
                "model_desc" => model_desc = v.trim().to_string(),
                "n_layer" => n_layer = v.trim().parse().map_err(|_| "bad n_layer")?,
                "steps" => steps = v.trim().parse().map_err(|_| "bad steps")?,
                "layers" => {}
                "L" => {
                    let f: Vec<&str> = v.split_whitespace().collect();
                    if f.len() != 3 {
                        return Err(format!("bad layer line {l:?}"));
                    }
                    let p = |s: &str| {
                        s.parse::<u64>()
                            .map_err(|_| format!("bad layer line {l:?}"))
                    };
                    pending = Some((p(f[0])? as u32, p(f[1])? as usize, p(f[2])?));
                }
                _ => return Err(format!("unknown profile line {l:?}")),
            }
        }
        if pending.is_some() {
            return Err("the profile ends inside a layer".into());
        }
        Ok(Profile {
            model_size,
            model_desc,
            n_layer,
            steps,
            layers,
        })
    }

    /// The same counts for another model file (FLE checks size, description and shape; slot bytes follow the new types).
    pub fn for_model(
        &self,
        model_size: u64,
        model_desc: &str,
        slot_bytes: &BTreeMap<u32, u64>,
    ) -> String {
        let mut s = format!(
            "FLE-MOE-PROFILE v1\nmodel_size {model_size}\nmodel_desc {model_desc}\nn_layer {}\nsteps {}\nlayers {}\n",
            self.n_layer,
            self.steps,
            self.layers.len()
        );
        for l in &self.layers {
            let slot = slot_bytes.get(&l.il).copied().unwrap_or(l.slot_bytes);
            s += &format!("L {} {} {slot}\n", l.il, l.counts.len());
            s += &l
                .counts
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            s.push('\n');
        }
        s
    }
}

/// The three expert roles; `rows` and `cols` per expert (cols is the row length the type packs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Gate,
    Up,
    Down,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Gate, Role::Up, Role::Down];
    pub fn tensor(self) -> &'static str {
        match self {
            Role::Gate => "ffn_gate_exps",
            Role::Up => "ffn_up_exps",
            Role::Down => "ffn_down_exps",
        }
    }
}

/// An expert type the plan may choose, with its bits per weight.
#[derive(Debug, Clone)]
pub struct Choice {
    pub name: &'static str,
    pub bpw: f64,
}

/// From the smallest to the source's own MXFP4 (4.25 bpw; more bits than the source has buy nothing).
pub const CHOICES: [Choice; 7] = [
    Choice {
        name: "iq1_m",
        bpw: 1.75,
    },
    Choice {
        name: "iq2_xxs",
        bpw: 2.0625,
    },
    Choice {
        name: "iq2_xs",
        bpw: 2.3125,
    },
    Choice {
        name: "iq2_s",
        bpw: 2.5,
    },
    Choice {
        name: "iq3_xxs",
        bpw: 3.0625,
    },
    Choice {
        name: "iq3_s",
        bpw: 3.4375,
    },
    Choice {
        name: "iq4_xs",
        bpw: 4.25,
    },
];

/// How much error a role adds per unit of quantization noise. Measured per model at night (KL rise per bit on sample
/// layers); until then down projections count more, as is usual.
#[derive(Debug, Clone)]
pub struct Sensitivity {
    pub by_role: BTreeMap<Role, f64>,
}

impl Default for Sensitivity {
    fn default() -> Self {
        Sensitivity {
            by_role: [(Role::Gate, 1.0), (Role::Up, 1.0), (Role::Down, 1.6)]
                .into_iter()
                .collect(),
        }
    }
}

/// Quantization noise falls by about a factor of four per bit.
fn noise(bpw: f64) -> f64 {
    4f64.powf(-bpw)
}

pub struct Shape {
    /// weights of one expert in one role
    pub weights: u64,
}

pub struct Inputs<'a> {
    pub profile: &'a Profile,
    /// weights per expert and role (DeepSeek-V4.1: 5120 × 2304 each)
    pub expert_weights: u64,
    /// the GPU expert cache in MiB
    pub budget_mib: u64,
    pub sens: Sensitivity,
    /// the error to reach, in the same units as `Plan::error` (e.g. the error of a reference quant's types)
    pub target_error: f64,
}

#[derive(Debug, Clone)]
pub struct Plan {
    /// type per layer and role
    pub layers: BTreeMap<u32, BTreeMap<Role, &'static str>>,
    /// type per layer, role and expert (stage 1: equal within a layer; stage 2 with split tensors: per expert)
    pub experts: BTreeMap<u32, BTreeMap<Role, Vec<&'static str>>>,
    /// share of each layer's expert traffic that is read on the CPU
    pub cpu_share: BTreeMap<u32, f64>,
    /// expected expert bytes read on the CPU per token
    pub cpu_bytes_per_token: f64,
    pub error: f64,
}

fn bytes(weights: u64, bpw: f64) -> f64 {
    weights as f64 * bpw / 8.0
}

/// Which experts the cache holds: the most used ones over all layers until the budget is full (FLE admits by frequency).
fn cpu_shares(p: &Profile, slot: &BTreeMap<u32, f64>, budget: f64) -> BTreeMap<u32, f64> {
    let mut all: Vec<(u64, u32)> = p
        .layers
        .iter()
        .flat_map(|l| l.counts.iter().map(move |c| (*c, l.il)))
        .collect();
    all.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut used = 0.0;
    let mut cached: BTreeMap<u32, u64> = BTreeMap::new();
    for (c, il) in all {
        let s = slot[&il];
        if used + s > budget {
            continue;
        }
        used += s;
        *cached.entry(il).or_default() += c;
    }
    p.layers
        .iter()
        .map(|l| {
            let total: u64 = l.counts.iter().sum();
            let hit = cached.get(&l.il).copied().unwrap_or(0);
            (
                l.il,
                if total == 0 {
                    1.0
                } else {
                    1.0 - hit as f64 / total as f64
                },
            )
        })
        .collect()
}

/// For one weight λ between speed and error: per layer and role the type with the least CPU bytes + λ · error.
fn choose(
    inp: &Inputs,
    share: &BTreeMap<u32, f64>,
    lambda: f64,
) -> BTreeMap<u32, BTreeMap<Role, &'static Choice>> {
    let w = inp.expert_weights;
    inp.profile
        .layers
        .iter()
        .map(|l| {
            let roles = Role::ALL
                .iter()
                .map(|r| {
                    let s = inp.sens.by_role[r];
                    let best = CHOICES
                        .iter()
                        .min_by(|a, b| {
                            let f = |c: &Choice| {
                                share[&l.il] * bytes(w, c.bpw) + lambda * s * noise(c.bpw)
                            };
                            f(a).partial_cmp(&f(b)).unwrap()
                        })
                        .unwrap();
                    (*r, best)
                })
                .collect();
            (l.il, roles)
        })
        .collect()
}

pub fn plan(inp: &Inputs) -> Plan {
    let budget = (inp.budget_mib << 20) as f64;
    let w = inp.expert_weights;
    let p = inp.profile;
    let err_of = |c: &BTreeMap<u32, BTreeMap<Role, &'static Choice>>| -> f64 {
        c.values()
            .flat_map(|r| {
                r.iter()
                    .map(|(role, ch)| inp.sens.by_role[role] * noise(ch.bpw))
            })
            .sum()
    };
    type Pick = BTreeMap<u32, BTreeMap<Role, &'static Choice>>;
    let slots = |c: &Pick| -> BTreeMap<u32, f64> {
        c.iter()
            .map(|(il, r)| (*il, r.values().map(|ch| bytes(w, ch.bpw)).sum()))
            .collect()
    };
    let k = |l: &LayerUse| -> f64 {
        if p.steps == 0 {
            0.0
        } else {
            l.counts.iter().sum::<u64>() as f64 / p.steps as f64
        }
    };
    // what an assignment really reads on the CPU: its own slot sizes decide what the cache holds
    let real = |c: &Pick| -> (f64, BTreeMap<u32, f64>) {
        let slot = slots(c);
        let share = cpu_shares(p, &slot, budget);
        let b = p
            .layers
            .iter()
            .map(|l| k(l) * share[&l.il] * slot[&l.il])
            .sum();
        (b, share)
    };
    let uniform = |gu: &'static Choice, down: &'static Choice| -> Pick {
        p.layers
            .iter()
            .map(|l| {
                (
                    l.il,
                    Role::ALL
                        .iter()
                        .map(|r| (*r, if *r == Role::Down { down } else { gu }))
                        .collect(),
                )
            })
            .collect()
    };
    // candidates: every even assignment (down the same or one step up), and the λ choice for the cache each candidate
    // leaves, settled over a few rounds. More bits in a hot layer make its slots bigger, so the cache holds fewer experts;
    // only the real cost of each candidate says which one is fastest.
    let mut cands: Vec<Pick> = vec![];
    for (i, c) in CHOICES.iter().enumerate() {
        cands.push(uniform(c, c));
        if let Some(d) = CHOICES.get(i + 1) {
            cands.push(uniform(c, d));
        }
    }
    let seeds: Vec<Pick> = cands.clone();
    for seed in seeds {
        let mut slot = slots(&seed);
        for _ in 0..6 {
            let share = cpu_shares(p, &slot, budget);
            // the smallest λ (fastest for this cache) whose error stays within the target
            let (mut lo, mut hi) = (0.0f64, 1e30f64);
            for _ in 0..200 {
                let mid = if hi > 1e29 {
                    (lo * 10.0).max(1.0)
                } else {
                    (lo + hi) / 2.0
                };
                if err_of(&choose(inp, &share, mid)) <= inp.target_error {
                    hi = mid;
                } else {
                    lo = mid;
                }
                if hi < 1e29 && (hi - lo) <= hi * 1e-6 {
                    break;
                }
            }
            let c = choose(inp, &share, hi);
            slot = slots(&c);
            cands.push(c);
        }
    }
    let ok: Vec<&Pick> = cands
        .iter()
        .filter(|c| err_of(c) <= inp.target_error * (1.0 + 1e-9))
        .collect();
    let best: Pick = if ok.is_empty() {
        uniform(&CHOICES[CHOICES.len() - 1], &CHOICES[CHOICES.len() - 1])
    } else {
        (*ok.iter()
            .min_by(|a, b| real(a).0.partial_cmp(&real(b).0).unwrap())
            .unwrap())
        .clone()
    };
    let (cpu_bytes_per_token, share) = real(&best);
    let layers: BTreeMap<u32, BTreeMap<Role, &'static str>> = best
        .iter()
        .map(|(il, r)| (*il, r.iter().map(|(role, c)| (*role, c.name)).collect()))
        .collect();
    let experts = p
        .layers
        .iter()
        .map(|l| {
            (
                l.il,
                layers[&l.il]
                    .iter()
                    .map(|(r, t)| (*r, vec![*t; l.counts.len()]))
                    .collect(),
            )
        })
        .collect();
    Plan {
        error: err_of(&best),
        layers,
        experts,
        cpu_share: share,
        cpu_bytes_per_token,
    }
}

impl Plan {
    /// The Forge's quantize rules (`quantize.rules`: one regex per tensor and its type).
    pub fn rules(&self) -> Value {
        let mut out = vec![];
        for (il, roles) in &self.layers {
            for (role, t) in roles {
                out.push(json!({"pattern": format!(r"^blk\.{il}\.{}\.weight$", role.tensor()), "type": t}));
            }
        }
        json!(out)
    }
}

/// The error a given assignment (e.g. a reference quant's types per layer and role) has in the plan's units.
pub fn error_of(types: &BTreeMap<u32, BTreeMap<Role, String>>, sens: &Sensitivity) -> f64 {
    types
        .values()
        .flat_map(|r| {
            r.iter().map(|(role, t)| {
                let bpw = CHOICES
                    .iter()
                    .find(|c| c.name == t.to_lowercase())
                    .map_or(2.0, |c| c.bpw);
                sens.by_role[role] * noise(bpw)
            })
        })
        .sum()
}

/// The note every model built this way carries (Max 04.11.2026: say that the bits follow a usage profile, and on what).
pub fn model_card_note(profile_sha: &str, budget_mib: u64, hardware: &str) -> String {
    format!(
        "The bits of this quant are cut to one usage profile (FLE profile sha256 {profile_sha}, GPU expert cache {budget_mib} MiB): \
         experts the profile used often enough to sit on the GPU keep more bits, experts read on the CPU fewer. On other \
         prompts or another cache size it can be slower or less exact than a quant with even bits. Built and measured on: {hardware}."
    )
}

fn bpw_of(t: &str) -> f64 {
    CHOICES
        .iter()
        .find(|c| c.name == t.to_lowercase())
        .map_or(2.0, |c| c.bpw)
}

/// The expert types of a quant per layer and role, and the weights of one expert in one role (from its tensor shapes).
pub fn types_of(
    tensors: impl Iterator<Item = (String, Vec<u64>, String)>,
) -> (BTreeMap<u32, BTreeMap<Role, String>>, u64) {
    let mut types: BTreeMap<u32, BTreeMap<Role, String>> = BTreeMap::new();
    let mut weights = 0;
    for (name, shape, ty) in tensors {
        let Some(rest) = name.strip_prefix("blk.") else {
            continue;
        };
        let Some((il, tail)) = rest.split_once('.') else {
            continue;
        };
        let Ok(il) = il.parse::<u32>() else { continue };
        let Some(role) = Role::ALL
            .into_iter()
            .find(|r| tail == format!("{}.weight", r.tensor()))
        else {
            continue;
        };
        if shape.len() == 3 {
            weights = shape[0] * shape[1];
        }
        types.entry(il).or_default().insert(role, ty.to_lowercase());
    }
    (types, weights)
}

/// Expert bytes per token a given assignment reads on the CPU with this profile and cache (for a reference quant).
pub fn cpu_bytes_of(
    p: &Profile,
    expert_weights: u64,
    budget_mib: u64,
    types: &BTreeMap<u32, BTreeMap<Role, String>>,
) -> f64 {
    let w = expert_weights;
    let slot: BTreeMap<u32, f64> = p
        .layers
        .iter()
        .map(|l| {
            let s = types.get(&l.il).map_or(3.0 * bytes(w, 2.0625), |r| {
                r.values().map(|t| bytes(w, bpw_of(t))).sum()
            });
            (l.il, s)
        })
        .collect();
    let share = cpu_shares(p, &slot, (budget_mib << 20) as f64);
    if p.steps == 0 {
        return 0.0;
    }
    p.layers
        .iter()
        .map(|l| {
            let k = l.counts.iter().sum::<u64>() as f64 / p.steps as f64;
            k * share[&l.il] * slot[&l.il]
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> Profile {
        // two layers of four experts: layer 0 is concentrated (a cache of two experts serves most of it), layer 1 even
        Profile::parse(
            "FLE-MOE-PROFILE v1\nmodel_size 1000\nmodel_desc test IQ2_XXS\nn_layer 2\nsteps 100\nlayers 2\n\
             L 0 4 10\n90 90 10 10\nL 1 4 10\n50 50 50 50\n",
        )
        .unwrap()
    }

    #[test]
    fn the_profile_reads_and_is_rewritten_for_a_new_model_with_the_same_counts() {
        let p = profile();
        assert_eq!(p.layers.len(), 2);
        assert_eq!(p.layers[0].counts, vec![90, 90, 10, 10]);
        let again =
            Profile::parse(&p.for_model(2000, "new quant", &[(0, 77)].into_iter().collect()))
                .unwrap();
        assert_eq!(again.model_size, 2000);
        assert_eq!(again.model_desc, "new quant");
        assert_eq!(again.layers[0].slot_bytes, 77);
        assert_eq!(again.layers[1].counts, p.layers[1].counts);
        assert!(
            Profile::parse("FLE-MOE-PROFILE v1\nL 0 2 1\n1\n").is_err(),
            "too few counts"
        );
    }

    #[test]
    fn the_layer_the_cache_serves_keeps_more_bits_at_the_same_error() {
        let p = profile();
        let w = 1 << 20;
        // a cache for two experts of the middle size: layer 0's two hot ones fit
        let slot = 3.0 * bytes(w, 2.3125);
        let even: BTreeMap<u32, BTreeMap<Role, String>> = [0, 1]
            .into_iter()
            .map(|il| {
                (
                    il,
                    Role::ALL
                        .iter()
                        .map(|r| (*r, "iq2_xs".to_string()))
                        .collect(),
                )
            })
            .collect();
        let sens = Sensitivity::default();
        let inp = Inputs {
            profile: &p,
            expert_weights: w,
            budget_mib: ((2.0 * slot) as u64 >> 20) + 1,
            sens: sens.clone(),
            target_error: error_of(&even, &sens),
        };
        let plan = plan(&inp);
        assert!(plan.error <= inp.target_error * 1.000001);
        let bpw = |il: u32, r: Role| {
            CHOICES
                .iter()
                .find(|c| c.name == plan.layers[&il][&r])
                .unwrap()
                .bpw
        };
        assert!(
            plan.cpu_share[&0] < plan.cpu_share[&1],
            "{:?}",
            plan.cpu_share
        );
        assert!(
            bpw(0, Role::Down) >= bpw(1, Role::Down),
            "{:?}",
            plan.layers
        );
        assert!(
            bpw(1, Role::Down) >= bpw(1, Role::Gate),
            "down counts more: {:?}",
            plan.layers
        );
        assert_eq!(plan.experts[&0][&Role::Up].len(), 4);
        assert_eq!(plan.rules().as_array().unwrap().len(), 6);
        assert!(model_card_note("abc", 15000, "RTX PRO 4500").contains("usage profile"));
        // the reference's traffic is computed the same way as the plan's
        let planned: BTreeMap<u32, BTreeMap<Role, String>> = plan
            .layers
            .iter()
            .map(|(il, r)| (*il, r.iter().map(|(k, v)| (*k, v.to_string())).collect()))
            .collect();
        let again = cpu_bytes_of(&p, w, inp.budget_mib, &planned);
        assert!((again - plan.cpu_bytes_per_token).abs() <= plan.cpu_bytes_per_token * 1e-9);
        assert!(plan.cpu_bytes_per_token <= cpu_bytes_of(&p, w, inp.budget_mib, &even) * 1.000001);
        let (t, wt) = types_of(
            [
                (
                    "blk.3.ffn_down_exps.weight".to_string(),
                    vec![2304, 5120, 256],
                    "IQ2_XS".to_string(),
                ),
                (
                    "blk.3.attn_q.weight".to_string(),
                    vec![5120, 4096],
                    "Q8_0".to_string(),
                ),
            ]
            .into_iter(),
        );
        assert_eq!(t[&3][&Role::Down], "iq2_xs");
        assert_eq!(t[&3].len(), 1);
        assert_eq!(wt, 2304 * 5120);
    }
}
