//! The Forge usage plan without a running Studio, for a reference whose expert types come as a list (e.g. read on another
//! machine): `cargo run -p flelink-api --example usage_plan -- <profile.prof> <types.tsv> <budget MiB> [rules-out.txt]`.
//! types.tsv: `name<TAB>ne0 ne1 ne2<TAB>TYPE` per expert tensor. Writes llama-quantize `regex=type` rules when asked.
use flelink_api::forge::usage::{self, Role};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: usage_plan <profile.prof> <types.tsv> <budget MiB> [rules-out.txt]");
        std::process::exit(2);
    }
    let prof = usage::Profile::parse(&std::fs::read_to_string(&a[1]).unwrap()).unwrap();
    let list = std::fs::read_to_string(&a[2]).unwrap();
    let tensors = list.lines().filter_map(|l| {
        let f: Vec<&str> = l.split('\t').collect();
        (f.len() == 3).then(|| {
            (
                f[0].to_string(),
                f[1].split(' ').map(|x| x.parse().unwrap()).collect(),
                f[2].to_string(),
            )
        })
    });
    let (types, weights) = usage::types_of(tensors);
    let budget: u64 = a[3].parse().unwrap();
    let sens = usage::Sensitivity::implied_by(&types);
    let target = usage::error_of(&types, &sens);
    let plan = usage::plan(&usage::Inputs {
        profile: &prof,
        expert_weights: weights,
        budget_mib: budget,
        sens: sens.clone(),
        target_error: target,
        reference: Some(&types),
    });
    let reference = usage::cpu_bytes_of(&prof, weights, budget, &types);
    println!(
        "budget {budget} MiB: reference reads {:.1} MB/token on the CPU, plan {:.1} MB/token ({:.2}x less); error {:.4} vs {:.4}",
        reference / 1e6,
        plan.cpu_bytes_per_token / 1e6,
        reference / plan.cpu_bytes_per_token.max(1.0),
        plan.error,
        target
    );
    println!("layer  gate      up        down      | reference           | GPU serves");
    for (il, r) in &plan.layers {
        let was = |role: Role| types[il].get(&role).cloned().unwrap_or_default();
        println!(
            "{il:>5}  {:<9} {:<9} {:<9} | {:<7} {:<7} {:<7} | {:>3.0} %",
            r[&Role::Gate],
            r[&Role::Up],
            r[&Role::Down],
            was(Role::Gate),
            was(Role::Up),
            was(Role::Down),
            (1.0 - plan.cpu_share[il]) * 100.0
        );
    }
    if let Some(out) = a.get(4) {
        let rules: String = plan
            .rules()
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                format!(
                    "{}={}\n",
                    x["pattern"].as_str().unwrap(),
                    x["type"].as_str().unwrap()
                )
            })
            .collect();
        std::fs::write(out, rules).unwrap();
    }
}
