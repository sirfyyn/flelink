//! Forge jobs: a recipe turned into steps that run in the background, with progress, a log, cancel and resume. Port of
//! `fle_studio_forge/jobs.py` and `steps.py`.
//!
//! A job is a list of steps (importance matrix, build, router statistics, pruning, reference logits, KL divergence,
//! checksums), run one after the other in one thread; one job runs at a time because every step is a large job for the
//! disk and the memory. The state of a job is `job.json` in its folder, saved after every change, so a job that was
//! cancelled, failed or interrupted by closing FLE Link can be resumed: finished steps are not repeated, and a step that
//! writes a model continues at the last finished tensor.
use super::build::{self, Stop};
use super::tools::Tool;
use super::{ggufio, reap, recipe, verify};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const LOG_KEEP: usize = 4000;

fn title(step: &str) -> &'static str {
    match step {
        "imatrix" => "Importance matrix",
        "build" => "Build the model",
        "reap_table" => "Router statistics",
        "reap_prune" => "Prune experts",
        "kld_base" => "Reference probabilities",
        "kld" => "KL divergence",
        _ => "Checksums and files",
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn plan_steps(r: &Value) -> Vec<&'static str> {
    let mut s = vec![];
    if r["mode"] == "quantize" && r["quantize"]["imatrix"] == true && r["imatrix"]["file"] == "" {
        s.push("imatrix");
    }
    s.push("build");
    if r["reap"]["enabled"] == true {
        if r["reap"]["table"] == "" {
            s.push("reap_table");
        }
        s.push("reap_prune");
    }
    if r["verify"]["enabled"] == true {
        if r["verify"]["base_logits"] == "" {
            s.push("kld_base");
        }
        s.push("kld");
    }
    s.push("finish");
    s
}

fn needed_tools(r: &Value) -> Vec<&'static str> {
    let steps = plan_steps(r);
    let mut out = vec![];
    if steps.contains(&"imatrix") {
        out.push("llama-imatrix");
    }
    if r["mode"] == "quantize" {
        out.push("llama-quantize");
    }
    if steps.contains(&"kld") {
        out.push("llama-perplexity");
    }
    out
}

pub struct Job {
    pub folder: PathBuf,
    pub data: Mutex<Value>,
    log: Mutex<VecDeque<Value>>,
    seq: Mutex<u64>,
    pub cancel: AtomicBool,
    pub running: AtomicBool,
    child: Mutex<Option<std::process::Child>>,
}

impl Job {
    fn new(folder: PathBuf, data: Value) -> Job {
        Job {
            folder,
            data: Mutex::new(data),
            log: Mutex::new(VecDeque::new()),
            seq: Mutex::new(0),
            cancel: AtomicBool::new(false),
            running: AtomicBool::new(false),
            child: Mutex::new(None),
        }
    }

    pub fn id(&self) -> String {
        self.data.lock().unwrap()["id"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    pub fn state(&self) -> String {
        self.data.lock().unwrap()["state"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    fn work(&self) -> PathBuf {
        let w = self.folder.join("work");
        let _ = std::fs::create_dir_all(&w);
        w
    }

    pub fn save(&self) {
        let mut d = self.data.lock().unwrap();
        d["updated"] = json!(now());
        let tmp = self.folder.join("job.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(&*d).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, self.folder.join("job.json"));
        }
    }

    fn add_log(&self, text: &str, kind: &str) {
        {
            let mut seq = self.seq.lock().unwrap();
            *seq += 1;
            let mut log = self.log.lock().unwrap();
            log.push_back(json!({"seq": *seq, "t": now(), "text": text, "kind": kind}));
            if log.len() > LOG_KEEP {
                log.pop_front();
            }
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.folder.join("log.txt"))
        {
            let _ = writeln!(f, "{text}");
        }
    }
}

/// What a step may do: log, report progress, run a tool, ask whether the job was cancelled.
struct Ctx<'a> {
    job: &'a Arc<Job>,
    step: usize,
    recipe: Value,
    tools: HashMap<String, Tool>,
}

impl Ctx<'_> {
    fn log(&self, text: &str, kind: &str) {
        self.job.add_log(text, kind);
    }

    fn progress(&self, fraction: f64, message: &str) {
        {
            let mut d = self.job.data.lock().unwrap();
            let s = &mut d["steps"][self.step];
            s["progress"] = json!(fraction.clamp(0.0, 1.0));
            if !message.is_empty() {
                s["message"] = json!(message);
            }
        }
        self.job.save();
    }

    fn cancelled(&self) -> bool {
        self.job.cancel.load(Ordering::SeqCst)
    }

    fn artifact(&self, k: &str) -> String {
        let d = self.job.data.lock().unwrap();
        match &d["artifacts"][k] {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            v => v.to_string(),
        }
    }

    fn tool(&self, name: &str) -> Result<Tool, Stop> {
        self.tools.get(name).filter(|t| t.ok()).cloned().ok_or_else(|| {
            Stop::Failed(format!("{name} was not found. Set the folder of your llama.cpp tools on the Forge page, or install an engine on the Engine page (its folder is searched too)."))
        })
    }

    /// Run a tool, stream its output into the log, stop it when the job is cancelled; its exit code.
    fn run(
        &self,
        tool: &Tool,
        args: &[String],
        on_line: &mut dyn FnMut(&str),
    ) -> Result<i32, Stop> {
        let (prog, mut pre) = super::tools::command_for(&tool.path);
        pre.extend(args.iter().cloned());
        let shown = std::iter::once(prog.clone())
            .chain(pre.iter().cloned())
            .map(|w| {
                if w.contains(' ') {
                    format!("\"{w}\"")
                } else {
                    w
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        self.log(&format!("$ {shown}"), "cmdline");
        let mut cmd = std::process::Command::new(&prog);
        cmd.args(&pre)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        super::tools::configure(&mut cmd, &tool.path);
        let mut child = cmd
            .spawn()
            .map_err(|e| Stop::Failed(format!("could not start {}: {e}", tool.name)))?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        for pipe in [
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(pipe).lines().map_while(Result::ok) {
                    let _ = tx.send(line);
                }
            });
        }
        drop(tx);
        *self.job.child.lock().unwrap() = Some(child);
        let mut cancel_at: Option<Instant> = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => {
                    let line = line.trim_end().to_string();
                    if !line.is_empty() {
                        let low = line.to_lowercase();
                        self.log(
                            &line,
                            if low.contains("error") || low.contains("failed") {
                                "err"
                            } else {
                                ""
                            },
                        );
                        on_line(&line);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(_) => {}
            }
            if self.cancelled() {
                let mut c = self.job.child.lock().unwrap();
                if let Some(ch) = c.as_mut() {
                    match cancel_at {
                        None => {
                            #[cfg(unix)]
                            unsafe {
                                libc::kill(ch.id() as i32, libc::SIGINT);
                            }
                            #[cfg(not(unix))]
                            let _ = ch.kill();
                            cancel_at = Some(Instant::now());
                        }
                        Some(t) if t.elapsed() > Duration::from_secs(10) => {
                            let _ = ch.kill();
                        }
                        _ => {}
                    }
                }
            }
        }
        let code = {
            let mut c = self.job.child.lock().unwrap();
            let code = c
                .as_mut()
                .and_then(|ch| ch.wait().ok())
                .and_then(|s| s.code())
                .unwrap_or(-1);
            *c = None;
            code
        };
        if self.cancelled() {
            return Err(Stop::Cancelled);
        }
        Ok(code)
    }
}

fn need_file(path: &str, what: &str) -> Result<String, Stop> {
    let p = ggufio::path_buf(path);
    if path.is_empty() || !p.is_file() {
        return Err(Stop::Failed(if path.is_empty() {
            format!("{what} is not set")
        } else {
            format!("{what}: no file at {path:?}")
        }));
    }
    Ok(p.to_string_lossy().to_string())
}

fn free_check(folder: &Path, need: u64, what: &str) -> Result<(), Stop> {
    let _ = std::fs::create_dir_all(folder);
    let free = flelink_core::system::free_bytes(folder);
    if free > 0 && free < need + (1 << 30) {
        return Err(Stop::Failed(format!(
            "{what} needs about {:.1} GB in {}, but only {:.1} GB are free",
            need as f64 / 1e9,
            folder.display(),
            free as f64 / 1e9
        )));
    }
    Ok(())
}

fn threads(n: &Value) -> Vec<String> {
    match n.as_i64() {
        Some(n) if n > 0 => vec!["-t".into(), n.to_string()],
        _ => vec![],
    }
}

fn source_path(r: &Value, sid: &Value) -> String {
    r["sources"][sid.as_str().unwrap_or("")]["path"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn chunk_progress<'a>(ctx: &'a Ctx<'a>, total: i64) -> impl FnMut(&str) + 'a {
    let re = Regex::new(r"\[(\d+)\]").unwrap();
    let mut seen = 0i64;
    move |line: &str| {
        for m in re.captures_iter(line) {
            seen = seen.max(m[1].parse().unwrap_or(0));
        }
        if total > 0 && seen > 0 {
            ctx.progress(
                (seen as f64 / total as f64).min(0.99),
                &format!("chunk {seen} of {total}"),
            );
        }
    }
}

fn step_imatrix(ctx: &Ctx) -> Result<Value, Stop> {
    let r = &ctx.recipe;
    let im = &r["imatrix"];
    let tool = ctx.tool("llama-imatrix")?;
    let model = need_file(
        &source_path(r, &r["quantize"]["source"]),
        "the source model",
    )?;
    let corpus = need_file(
        im["corpus"].as_str().unwrap_or(""),
        "the calibration text (imatrix.corpus)",
    )?;
    let out = ctx.job.work().join("imatrix.gguf"); // .gguf makes llama-imatrix write the format the Forge reads back
    let _ = std::fs::remove_file(&out);
    let mut args: Vec<String> = vec![
        "-m".into(),
        model,
        "-f".into(),
        corpus,
        "-o".into(),
        out.to_string_lossy().into(),
        "-c".into(),
        im["context"].to_string(),
    ];
    if im["chunks"].as_i64().unwrap_or(0) > 0 {
        args.extend(["--chunks".into(), im["chunks"].to_string()]);
    }
    if im["gpu_layers"].as_i64().unwrap_or(0) > 0 {
        args.extend(["-ngl".into(), im["gpu_layers"].to_string()]);
    }
    args.extend(threads(&im["threads"]));
    let total_re = Regex::new(r"(?i)computing over (\d+) chunks").unwrap();
    let done_re = Regex::new(r"\[(\d+)\]-?\d").unwrap();
    let eta_re = Regex::new(r"(?i)ETA\s+([\d.]+)\s*(minutes|seconds|hours)").unwrap();
    let (mut total, mut done) = (im["chunks"].as_i64().unwrap_or(0), 0i64);
    let code = ctx.run(&tool, &args, &mut |line| {
        if let Some(m) = total_re.captures(line) {
            total = m[1].parse().unwrap_or(total);
        }
        for m in done_re.captures_iter(line) {
            done = done.max(m[1].parse().unwrap_or(0));
        }
        if total > 0 && done > 0 {
            let eta = eta_re.captures(line).map_or(String::new(), |m| {
                format!(", about {} {} left", &m[1], &m[2])
            });
            ctx.progress(
                done as f64 / total as f64,
                &format!("chunk {done} of {total}{eta}"),
            );
        }
    })?;
    if code != 0 || !out.is_file() {
        return Err(Stop::Failed(format!(
            "llama-imatrix exited with code {code} and wrote no matrix (see the log)"
        )));
    }
    Ok(json!({"imatrix": out.to_string_lossy()}))
}

fn step_build(ctx: &Ctx) -> Result<Value, Stop> {
    let r = &ctx.recipe;
    let final_ = PathBuf::from(ctx.artifact("final"));
    let out = if r["reap"]["enabled"] == true {
        ctx.job.work().join("built.gguf")
    } else {
        final_
    };
    if r["mode"] == "hybrid" {
        let h = &r["hybrid"];
        if h["layers"].as_object().is_none_or(|o| o.is_empty())
            && h["rules"].as_array().is_none_or(|a| a.is_empty())
        {
            return Err(Stop::Failed("The recipe takes nothing from a donor, so the result would be a copy of the base. Choose layers on the Recipe step first.".into()));
        }
        for (sid, s) in r["sources"].as_object().into_iter().flatten() {
            if s["role"] != "reference" {
                need_file(s["path"].as_str().unwrap_or(""), &format!("source {sid}"))?;
            }
        }
        let base_id = h["base"].as_str().unwrap_or("");
        let base =
            ggufio::open_model(&ggufio::path_buf(&source_path(r, &h["base"]))).map_err(|e| e.0)?;
        let mut models: HashMap<String, ggufio::Model> =
            HashMap::from([(base_id.to_string(), base.clone())]);
        let donors: std::collections::HashSet<String> = h["layers"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, v)| v.as_str().map(String::from))
            .chain(
                h["rules"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x["source"].as_str().map(String::from)),
            )
            .filter(|s| s != base_id)
            .collect();
        for sid in donors {
            let m = ggufio::open_model(&ggufio::path_buf(&source_path(r, &json!(sid))))
                .map_err(|e| e.0)?;
            models.insert(sid, m);
        }
        let pick = h["layers"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_string())))
            .collect();
        let rules = super::plan::parse_rules(&h["rules"]).map_err(|e| e.0)?;
        let plan =
            super::plan::assemble(&base, &models, &pick, base_id, &rules).map_err(|e| e.0)?;
        let size = super::plan::predicted_file_size(&base, &models, &plan);
        ctx.log(
            &format!("The result will be {:.2} GB", size as f64 / 1e9),
            "",
        );
        free_check(out.parent().unwrap_or(Path::new(".")), size, "The hybrid")?;
        let result = build::build_hybrid(
            r,
            &out,
            &mut |f, m| ctx.progress(f, m),
            &|| ctx.cancelled(),
            &|t| ctx.log(t, ""),
        )?;
        ctx.log(
            &format!(
                "Wrote {}: {:.2} GB, SHA-256 {}...",
                out.file_name().unwrap_or_default().to_string_lossy(),
                result["size"].as_f64().unwrap_or(0.0) / 1e9,
                &result["sha256"].as_str().unwrap_or("")[..16]
            ),
            "ok",
        );
        return Ok(
            json!({"model": out.to_string_lossy(), "model_sha256": result["sha256"], "model_size": result["size"]}),
        );
    }
    let q = &r["quantize"];
    let tool = ctx.tool("llama-quantize")?;
    let source = need_file(&source_path(r, &q["source"]), "the source model")?;
    let part = out.with_file_name(format!(
        "{}.part",
        out.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _ = std::fs::remove_file(&part);
    if let Some(d) = out.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut args: Vec<String> = vec![];
    if q["imatrix"] == true {
        let file = r["imatrix"]["file"]
            .as_str()
            .filter(|f| !f.is_empty())
            .map(String::from)
            .unwrap_or_else(|| ctx.artifact("imatrix"));
        args.extend([
            "--imatrix".into(),
            need_file(&file, "the importance matrix")?,
        ]);
    }
    if q["allow_requantize"] == true {
        args.push("--allow-requantize".into());
    }
    for (key, flag) in [
        ("output_type", "--output-tensor-type"),
        ("embedding_type", "--token-embedding-type"),
    ] {
        if let Some(v) = q[key].as_str().filter(|v| !v.is_empty()) {
            args.extend([flag.to_string(), v.to_string()]);
        }
    }
    for rule in q["rules"].as_array().into_iter().flatten() {
        args.extend([
            "--tensor-type".into(),
            format!(
                "{}={}",
                rule["pattern"].as_str().unwrap_or(""),
                rule["type"].as_str().unwrap_or("")
            ),
        ]);
    }
    args.extend([
        source,
        part.to_string_lossy().into(),
        q["type"].as_str().unwrap_or("").to_string(),
    ]);
    if r["imatrix"]["threads"].as_i64().unwrap_or(0) > 0 {
        args.push(r["imatrix"]["threads"].to_string());
    }
    let prog = Regex::new(r"^\[\s*(\d+)\s*/\s*(\d+)\s*\]").unwrap();
    let t0 = Instant::now();
    let code = ctx.run(&tool, &args, &mut |line| {
        if let Some(m) = prog.captures(line) {
            let (n, total): (f64, f64) = (m[1].parse().unwrap_or(0.0), m[2].parse().unwrap_or(0.0));
            if total > 0.0 {
                let eta = if n > 1.0 {
                    format!(
                        ", about {:.0} min left",
                        t0.elapsed().as_secs_f64() * (total - n) / n / 60.0
                    )
                } else {
                    String::new()
                };
                ctx.progress(n / total, &format!("tensor {n} of {total}{eta}"));
            }
        }
    });
    let ok = matches!(code, Ok(0)) && part.is_file();
    if ok {
        std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
    } else {
        let _ = std::fs::remove_file(&part);
    }
    let code = code?;
    if !ok {
        return Err(Stop::Failed(format!(
            "llama-quantize exited with code {code} (see the log)"
        )));
    }
    Ok(
        json!({"model": out.to_string_lossy(), "model_size": std::fs::metadata(&out).map_or(0, |m| m.len())}),
    )
}

fn step_reap_table(ctx: &Ctx) -> Result<Value, Stop> {
    let r = &ctx.recipe;
    let rp = &r["reap"];
    let model = need_file(
        &ctx.artifact("model"),
        "the model built by the previous step",
    )?;
    let out = ctx.job.work().join("reap.tsv");
    if let Some(tool) = ctx.tools.get("llama-fle-reap").filter(|t| t.ok()).cloned() {
        let corpus = need_file(
            r["imatrix"]["corpus"].as_str().unwrap_or(""),
            "the calibration text (imatrix.corpus)",
        )?;
        let c = r["imatrix"]["context"].to_string();
        let mut args: Vec<String> = vec![
            "-m".into(),
            model,
            "-f".into(),
            corpus,
            "-c".into(),
            c.clone(),
            "-b".into(),
            c.clone(),
            "-ub".into(),
            c,
        ];
        if rp["chunks"].as_i64().unwrap_or(0) > 0 {
            args.extend(["--chunks".into(), rp["chunks"].to_string()]);
        }
        args.extend(threads(&rp["threads"]));
        if rp["gpu_layers"].as_i64().unwrap_or(0) > 0 {
            args.extend(["-ngl".into(), rp["gpu_layers"].to_string()]);
            if tool.feature("cmoe") {
                args.push("-cmoe".into());
            }
            if tool.feature("fit") {
                args.extend(["-fit".into(), "off".into()]);
            }
        }
        args.extend(["-o".into(), out.to_string_lossy().into()]);
        let _ = std::fs::remove_file(&out);
        let total = rp["chunks"].as_i64().unwrap_or(0);
        let mut prog = chunk_progress(ctx, total);
        let code = ctx.run(&tool, &args, &mut prog)?;
        if code != 0 || !out.is_file() {
            return Err(Stop::Failed(format!(
                "llama-fle-reap exited with code {code} and wrote no table (see the log)"
            )));
        }
        return Ok(json!({"reap_table": out.to_string_lossy()}));
    }
    let matrix = r["imatrix"]["file"]
        .as_str()
        .filter(|f| !f.is_empty())
        .map(String::from)
        .unwrap_or_else(|| ctx.artifact("imatrix"));
    if matrix.is_empty() || !Path::new(&matrix).is_file() {
        return Err(Stop::Failed("Pruning needs router statistics. Install an FLE engine (it ships llama-fle-reap), give a table of your own, or compute an importance matrix of this model first (imatrix.file).".into()));
    }
    ctx.log("llama-fle-reap was not found; deriving routing counts and input energy from the importance matrix (a weaker signal than the real REAP statistics)", "warn");
    let table = reap::table_from_imatrix(Path::new(&matrix))?;
    reap::write_table(&table, &out).map_err(|e| e.to_string())?;
    Ok(json!({"reap_table": out.to_string_lossy()}))
}

fn step_reap_prune(ctx: &Ctx) -> Result<Value, Stop> {
    let r = &ctx.recipe;
    let rp = &r["reap"];
    let model_path = need_file(
        &ctx.artifact("model"),
        "the model built by the previous step",
    )?;
    let table_file = rp["table"]
        .as_str()
        .filter(|t| !t.is_empty())
        .map(String::from)
        .unwrap_or_else(|| ctx.artifact("reap_table"));
    let table_path = need_file(&table_file, "the router statistics table")?;
    let final_ = PathBuf::from(ctx.artifact("final"));
    let keep = rp["keep"].as_u64().unwrap_or(0);
    let table = reap::load_table(Path::new(&table_path))?;
    let model = ggufio::open_model(Path::new(&model_path)).map_err(|e| e.0)?;
    let size = reap::predicted_prune_size(&model, keep);
    ctx.log(
        &format!("The pruned model will be {:.2} GB", size as f64 / 1e9),
        "",
    );
    free_check(
        final_.parent().unwrap_or(Path::new(".")),
        size,
        "The pruned model",
    )?;
    let result = reap::prune(
        Path::new(&model_path),
        &final_,
        &table,
        keep,
        rp["score"].as_str().unwrap_or("reap"),
        &mut |f, m| ctx.progress(f, m),
        &|| ctx.cancelled(),
        &|t| ctx.log(t, ""),
    )?;
    let built = ctx.job.work().join("built.gguf");
    if built.is_file() && Path::new(&model_path) == built {
        let _ = std::fs::remove_file(&built);
    }
    ctx.log(
        &format!(
            "Wrote {}: {:.2} GB",
            final_.file_name().unwrap_or_default().to_string_lossy(),
            result["size"].as_f64().unwrap_or(0.0) / 1e9
        ),
        "ok",
    );
    Ok(
        json!({"model": final_.to_string_lossy(), "model_sha256": result["sha256"], "model_size": result["size"], "reap_kept": result["keep"]}),
    )
}

fn perplexity_args(ctx: &Ctx, model: &str) -> Result<Vec<String>, Stop> {
    let v = &ctx.recipe["verify"];
    let text = need_file(
        v["text"].as_str().unwrap_or(""),
        "the text for the comparison (verify.text)",
    )?;
    let mut args: Vec<String> = vec![
        "-m".into(),
        model.into(),
        "-f".into(),
        text,
        "-c".into(),
        v["context"].to_string(),
    ];
    if v["chunks"].as_i64().unwrap_or(0) > 0 {
        args.extend(["--chunks".into(), v["chunks"].to_string()]);
    }
    args.extend(threads(&v["threads"]));
    if v["gpu_layers"].as_i64().unwrap_or(0) > 0 {
        args.extend(["-ngl".into(), v["gpu_layers"].to_string()]);
    }
    Ok(args)
}

fn step_kld_base(ctx: &Ctx) -> Result<Value, Stop> {
    let v = &ctx.recipe["verify"];
    let tool = ctx.tool("llama-perplexity")?;
    let reference = need_file(
        &source_path(&ctx.recipe, &v["reference"]),
        "the reference model",
    )?;
    let out = ctx.job.work().join("base.kld");
    let _ = std::fs::remove_file(&out);
    let mut args = perplexity_args(ctx, &reference)?;
    args.extend(["--kl-divergence-base".into(), out.to_string_lossy().into()]);
    ctx.log(
        "The reference probabilities can take several GB of disk space for a long text.",
        "",
    );
    let mut prog = chunk_progress(ctx, v["chunks"].as_i64().unwrap_or(0));
    let code = ctx.run(&tool, &args, &mut prog)?;
    if code != 0 || !out.is_file() {
        return Err(Stop::Failed(format!(
            "llama-perplexity exited with code {code} and stored no probabilities (see the log)"
        )));
    }
    Ok(json!({"base_logits": out.to_string_lossy()}))
}

fn step_kld(ctx: &Ctx) -> Result<Value, Stop> {
    let r = &ctx.recipe;
    let v = &r["verify"];
    let tool = ctx.tool("llama-perplexity")?;
    let model = need_file(
        &ctx.artifact("model"),
        "the model built by the previous step",
    )?;
    let base_file = v["base_logits"]
        .as_str()
        .filter(|b| !b.is_empty())
        .map(String::from)
        .unwrap_or_else(|| ctx.artifact("base_logits"));
    let base = need_file(&base_file, "the reference probabilities")?;
    let mut args = perplexity_args(ctx, &model)?;
    args.extend([
        "--kl-divergence".into(),
        "--kl-divergence-base".into(),
        base,
    ]);
    let mut lines: Vec<String> = vec![];
    let mut prog = chunk_progress(ctx, v["chunks"].as_i64().unwrap_or(0));
    let code = ctx.run(&tool, &args, &mut |l| {
        lines.push(l.to_string());
        prog(l);
    })?;
    let stats = verify::parse(&lines.join("\n"));
    if code != 0 || stats["complete"] != true {
        let note = if code != 0 {
            format!(" (exit code {code})")
        } else {
            String::new()
        };
        return Err(Stop::Failed(format!(
            "llama-perplexity printed no KL-divergence statistics{note}; see the log"
        )));
    }
    let size = std::fs::metadata(&model).map_or(0, |m| m.len());
    let params: Option<u64> = ggufio::open_model(Path::new(&model))
        .ok()
        .map(|m| m.tensors.values().map(|t| t.n_elements()).sum());
    let reference = r["sources"][v["reference"].as_str().unwrap_or("")]["file"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let text = Path::new(v["text"].as_str().unwrap_or(""))
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let card = verify::card(
        &stats,
        r["name"].as_str().unwrap_or(""),
        size,
        params,
        &reference,
        &text,
        v["context"].as_i64().unwrap_or(0),
        v["chunks"].as_i64().unwrap_or(0),
        &tool.version,
    );
    let _ = std::fs::write(
        ctx.job.folder.join("card.json"),
        serde_json::to_string_pretty(&card).unwrap_or_default(),
    );
    ctx.job.data.lock().unwrap()["result"]["card"] = card.clone();
    let top1 = card["top1"].as_f64().map_or(String::new(), |t| {
        format!(", top-1 agreement {:.2} %", t * 100.0)
    });
    ctx.log(
        &format!(
            "KL divergence {:.5}{top1}",
            card["kld"].as_f64().unwrap_or(0.0)
        ),
        "ok",
    );
    Ok(json!({"kld": card["kld"]}))
}

fn step_finish(ctx: &Ctx) -> Result<Value, Stop> {
    let r = &ctx.recipe;
    let final_ = PathBuf::from(ctx.artifact("final"));
    if !final_.is_file() {
        return Err(Stop::Failed(format!(
            "the result {} is missing",
            final_.display()
        )));
    }
    let size = std::fs::metadata(&final_).map_or(0, |m| m.len());
    let known = (Path::new(&ctx.artifact("model")) == final_)
        .then(|| ctx.artifact("model_sha256"))
        .filter(|s| !s.is_empty());
    let sha = match known {
        Some(s) => s,
        None => {
            ctx.log("Computing the SHA-256 of the result", "");
            let mut last = Instant::now();
            ggufio::sha256_file(&final_, &mut |done| {
                if last.elapsed() > Duration::from_millis(500) {
                    ctx.progress(
                        done as f64 / size.max(1) as f64,
                        &format!("{:.1} of {:.1} GB", done as f64 / 1e9, size as f64 / 1e9),
                    );
                    last = Instant::now();
                }
            })
            .map_err(|e| e.to_string())?
        }
    };
    let mut portable = recipe::portable(r).map_err(|e| e.0)?;
    let file = final_
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    portable["output"] = json!({"file": file, "sha256": sha, "size": size});
    let stem = final_
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let recipe_file = final_.with_file_name(format!("{stem}.recipe.json"));
    std::fs::write(&recipe_file, recipe::dumps(&portable)).map_err(|e| e.to_string())?;
    let card = ctx.job.data.lock().unwrap()["result"]["card"].clone();
    let manifest = json!({"studio": env!("CARGO_PKG_VERSION"), "created": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "file": file, "size": size, "sha256": sha, "name": r["name"], "mode": r["mode"],
        "sources": r["sources"].as_object().into_iter().flatten().map(|(k, s)| (k.clone(), json!({"file": s["file"], "size": s["size"]}))).collect::<Map<_, _>>(),
        "tools": ctx.tools.iter().filter(|(_, t)| t.ok()).map(|(n, t)| (n.clone(), json!(t.version))).collect::<Map<_, _>>(),
        "card": card});
    let manifest_file = final_.with_file_name(format!("{stem}.forge.json"));
    std::fs::write(
        &manifest_file,
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    {
        let mut d = ctx.job.data.lock().unwrap();
        let res = &mut d["result"];
        res["model"] = json!(final_.to_string_lossy());
        res["size"] = json!(size);
        res["sha256"] = json!(sha);
        res["recipe_file"] = json!(recipe_file.to_string_lossy());
        res["manifest"] = json!(manifest_file.to_string_lossy());
    }
    ctx.log(&format!("SHA-256 {sha}"), "ok");
    ctx.log(
        &format!(
            "Recipe saved as {}",
            recipe_file
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ),
        "ok",
    );
    Ok(json!({"final_sha256": sha, "recipe_file": recipe_file.to_string_lossy()}))
}

fn run_step(id: &str, ctx: &Ctx) -> Result<Value, Stop> {
    match id {
        "imatrix" => step_imatrix(ctx),
        "build" => step_build(ctx),
        "reap_table" => step_reap_table(ctx),
        "reap_prune" => step_reap_prune(ctx),
        "kld_base" => step_kld_base(ctx),
        "kld" => step_kld(ctx),
        _ => step_finish(ctx),
    }
}

pub struct ForgeJobs {
    dir: PathBuf,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

impl ForgeJobs {
    pub fn new(data_dir: &Path) -> ForgeJobs {
        let dir = data_dir.join("forge").join("jobs");
        let _ = std::fs::create_dir_all(&dir);
        let mut jobs = HashMap::new();
        let mut folders: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        folders.sort();
        for folder in folders {
            let Some(mut data) = std::fs::read_to_string(folder.join("job.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            else {
                continue;
            };
            if data["state"] == "queued" || data["state"] == "running" {
                data["state"] = json!("interrupted");
                for s in data["steps"].as_array_mut().into_iter().flatten() {
                    if s["state"] == "running" {
                        s["state"] = json!("interrupted");
                    }
                }
            }
            let job = Arc::new(Job::new(folder.clone(), data));
            job.save();
            if let Ok(t) = std::fs::read_to_string(folder.join("log.txt")) {
                let lines: Vec<&str> = t.lines().collect();
                for l in &lines[lines.len().saturating_sub(LOG_KEEP)..] {
                    let mut seq = job.seq.lock().unwrap();
                    *seq += 1;
                    job.log
                        .lock()
                        .unwrap()
                        .push_back(json!({"seq": *seq, "t": 0.0, "text": l, "kind": ""}));
                }
            }
            jobs.insert(job.id(), job);
        }
        ForgeJobs {
            dir,
            jobs: Mutex::new(jobs),
        }
    }

    pub fn running(&self) -> Option<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .find(|j| j.running.load(Ordering::SeqCst))
            .cloned()
    }

    /// Normalise a recipe and say what would run and what is missing, without starting anything.
    pub fn check(&self, r: &Value, tools: &HashMap<String, Tool>) -> Result<Value, String> {
        let norm = recipe::normalize(r).map_err(|e| e.0)?;
        let mut problems = recipe::validate_sources(&norm);
        for (sid, s) in norm["sources"].as_object().into_iter().flatten() {
            let p = s["path"].as_str().unwrap_or("");
            if !p.is_empty() && !ggufio::path_buf(p).is_file() {
                problems.push(format!("source {sid}: no file at {p}"));
            }
        }
        let missing = super::tools::missing_for(tools, &needed_tools(&norm));
        let steps = plan_steps(&norm);
        let mut feat = vec![];
        if norm["mode"] == "quantize" {
            if let Some(t) = tools.get("llama-quantize") {
                if norm["quantize"]["rules"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
                    && !t.feature("tensor_type")
                {
                    feat.push("this llama-quantize has no --tensor-type; the per-tensor rules cannot be used".to_string());
                }
                if norm["quantize"]["allow_requantize"] == true && !t.feature("allow_requantize") {
                    feat.push("this llama-quantize has no --allow-requantize".into());
                }
            }
        }
        if steps.contains(&"kld")
            && tools
                .get("llama-perplexity")
                .is_some_and(|t| !t.feature("kl_divergence"))
        {
            feat.push("this llama-perplexity has no --kl-divergence".into());
        }
        let missing_text = |p: &Value| {
            p.as_str()
                .is_none_or(|p| p.is_empty() || !ggufio::path_buf(p).is_file())
        };
        if steps.contains(&"imatrix") && missing_text(&norm["imatrix"]["corpus"]) {
            problems.push("The calibration text (imatrix.corpus) is not set or not a file".into());
        }
        if steps.contains(&"reap_table")
            && tools.get("llama-fle-reap").is_some_and(|t| t.ok())
            && missing_text(&norm["imatrix"]["corpus"])
        {
            problems.push("Pruning measures the router on the calibration text (imatrix.corpus), which is not set".into());
        }
        if steps.contains(&"kld") && missing_text(&norm["verify"]["text"]) {
            problems
                .push("The text for the comparison (verify.text) is not set or not a file".into());
        }
        let ok = problems.is_empty() && missing.is_empty() && feat.is_empty();
        problems.extend(feat);
        Ok(
            json!({"recipe": norm, "steps": steps.iter().map(|s| json!({"id": s, "title": title(s)})).collect::<Vec<_>>(),
            "problems": problems, "missing_tools": missing, "ok": ok}),
        )
    }

    fn unique_output(&self, out_dir: &Path, name: &str) -> PathBuf {
        let taken: Vec<String> = self
            .jobs
            .lock()
            .unwrap()
            .values()
            .map(|j| {
                j.data.lock().unwrap()["artifacts"]["final"]
                    .as_str()
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        let mut n = 1;
        let mut cand = out_dir.join(format!("{name}.gguf"));
        loop {
            let partial = cand.with_file_name(format!(
                "{}.part",
                cand.file_name().unwrap_or_default().to_string_lossy()
            ));
            if !cand.exists()
                && !taken.contains(&cand.to_string_lossy().to_string())
                && !partial.exists()
            {
                return cand;
            }
            n += 1;
            cand = out_dir.join(format!("{name}-{n}.gguf"));
        }
    }

    /// Err((409, …)) when another job runs, Err((400, …)) for a recipe that cannot run.
    pub fn create(
        self: &Arc<Self>,
        r: &Value,
        tools: HashMap<String, Tool>,
        out_dir: &Path,
    ) -> Result<Value, (u16, String)> {
        let info = self.check(r, &tools).map_err(|e| (400, e))?;
        if info["ok"] != true {
            let mut parts: Vec<String> = info["problems"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str().map(String::from))
                .collect();
            parts.extend(
                info["missing_tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.as_str().map(|t| format!("{t} is missing"))),
            );
            return Err((400, parts.join("; ")));
        }
        if self.running().is_some() {
            return Err((
                409,
                "Another Forge job is running. Wait for it or cancel it first.".into(),
            ));
        }
        let norm = info["recipe"].clone();
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).expect("random bytes");
        let id = hex::encode(b);
        let folder = self.dir.join(&id);
        std::fs::create_dir_all(&folder).map_err(|e| (500, e.to_string()))?;
        let _ = std::fs::create_dir_all(out_dir);
        let final_ = self.unique_output(out_dir, norm["name"].as_str().unwrap_or("my-quant"));
        let steps: Vec<Value> = info["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                json!({"id": s["id"], "title": s["title"], "state": "pending", "progress": 0.0,
            "message": "", "started": null, "finished": null, "error": "", "outputs": {}})
            })
            .collect();
        let data = json!({"id": id, "name": norm["name"], "recipe": norm, "created": now(), "updated": now(), "state": "queued", "error": "",
            "artifacts": {"final": final_.to_string_lossy()}, "result": {}, "steps": steps});
        let job = Arc::new(Job::new(folder, data));
        self.jobs.lock().unwrap().insert(id.clone(), job.clone());
        job.save();
        let names: Vec<String> = info["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["id"].as_str().map(String::from))
            .collect();
        job.add_log(
            &format!(
                "Job {id}: {} ({}), steps: {}",
                norm["name"].as_str().unwrap_or(""),
                norm["mode"].as_str().unwrap_or(""),
                names.join(", ")
            ),
            "",
        );
        self.start(&job, tools);
        Ok(self.public(&job))
    }

    fn start(self: &Arc<Self>, job: &Arc<Job>, tools: HashMap<String, Tool>) {
        job.cancel.store(false, Ordering::SeqCst);
        {
            let mut d = job.data.lock().unwrap();
            d["state"] = json!("running");
            d["error"] = json!("");
        }
        job.save();
        job.running.store(true, Ordering::SeqCst);
        let job = job.clone();
        std::thread::Builder::new()
            .name(format!("forge-{}", job.id()))
            .spawn(move || {
                Self::run(&job, tools);
                job.running.store(false, Ordering::SeqCst);
            })
            .expect("forge thread");
    }

    fn run(job: &Arc<Job>, tools: HashMap<String, Tool>) {
        let n = job.data.lock().unwrap()["steps"]
            .as_array()
            .map_or(0, Vec::len);
        let result = (|| -> Result<(), Stop> {
            for i in 0..n {
                let (id, done) = {
                    let d = job.data.lock().unwrap();
                    (
                        d["steps"][i]["id"].as_str().unwrap_or("").to_string(),
                        d["steps"][i]["state"] == "done",
                    )
                };
                if done {
                    continue;
                }
                if job.cancel.load(Ordering::SeqCst) {
                    return Err(Stop::Cancelled);
                }
                let recipe = job.data.lock().unwrap()["recipe"].clone();
                {
                    let mut d = job.data.lock().unwrap();
                    let s = &mut d["steps"][i];
                    s["state"] = json!("running");
                    s["started"] = json!(now());
                    s["progress"] = json!(0.0);
                    s["error"] = json!("");
                    s["message"] = json!("");
                }
                job.save();
                job.add_log(&format!("== {}", title(&id)), "head");
                let ctx = Ctx {
                    job,
                    step: i,
                    recipe,
                    tools: tools.clone(),
                };
                match run_step(&id, &ctx) {
                    Ok(outputs) => {
                        let mut d = job.data.lock().unwrap();
                        let s = &mut d["steps"][i];
                        s["state"] = json!("done");
                        s["progress"] = json!(1.0);
                        s["finished"] = json!(now());
                        s["outputs"] = outputs.clone();
                        for (k, v) in outputs.as_object().into_iter().flatten() {
                            if v.is_string() || v.is_number() {
                                d["artifacts"][k] = v.clone();
                            }
                        }
                    }
                    Err(Stop::Cancelled) => {
                        job.data.lock().unwrap()["steps"][i]["state"] = json!("cancelled");
                        return Err(Stop::Cancelled);
                    }
                    Err(Stop::Failed(e)) => {
                        let mut d = job.data.lock().unwrap();
                        let s = &mut d["steps"][i];
                        s["state"] = json!("failed");
                        s["error"] = json!(e.chars().take(600).collect::<String>());
                        s["finished"] = json!(now());
                        return Err(Stop::Failed(e));
                    }
                }
                job.save();
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                job.data.lock().unwrap()["state"] = json!("done");
                job.add_log("Done", "ok");
            }
            Err(Stop::Cancelled) => {
                job.data.lock().unwrap()["state"] = json!("cancelled");
                job.add_log("Cancelled. Resume continues where it stopped.", "warn");
            }
            Err(Stop::Failed(e)) => {
                {
                    let mut d = job.data.lock().unwrap();
                    d["state"] = json!("failed");
                    d["error"] = json!(e.chars().take(600).collect::<String>());
                }
                job.add_log(&format!("Failed: {e}"), "err");
            }
        }
        job.save();
    }

    pub fn resume(
        self: &Arc<Self>,
        id: &str,
        tools: HashMap<String, Tool>,
    ) -> Result<Value, String> {
        let job = self.get_job(id).ok_or("Not found")?;
        let st = job.state();
        if !["failed", "cancelled", "interrupted"].contains(&st.as_str()) {
            return Err(format!("A job that is {st} cannot be resumed"));
        }
        if self.running().is_some() {
            return Err("Another Forge job is running".into());
        }
        for s in job.data.lock().unwrap()["steps"]
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            if s["state"] != "done" {
                s["state"] = json!("pending");
                s["error"] = json!("");
                s["progress"] = json!(0.0);
                s["message"] = json!("");
            }
        }
        job.add_log("Resuming", "");
        self.start(&job, tools);
        Ok(self.public(&job))
    }

    pub fn cancel(&self, id: &str) -> Option<Value> {
        let job = self.get_job(id)?;
        job.cancel.store(true, Ordering::SeqCst);
        Some(self.public(&job))
    }

    pub fn delete(&self, id: &str, keep_model: bool) -> Result<(), String> {
        let job = self.get_job(id).ok_or("Not found")?;
        if job.running.load(Ordering::SeqCst) {
            return Err("Cancel the job before deleting it".into());
        }
        let _ = std::fs::remove_dir_all(&job.folder);
        self.jobs.lock().unwrap().remove(id);
        if !keep_model {
            let f = PathBuf::from(
                job.data.lock().unwrap()["artifacts"]["final"]
                    .as_str()
                    .unwrap_or(""),
            );
            if f.file_name().is_some() {
                let name = f.file_name().unwrap().to_string_lossy().to_string();
                for p in [
                    f.clone(),
                    f.with_file_name(format!("{name}.part")),
                    f.with_file_name(format!("{name}.part.json")),
                ] {
                    if p.is_file() {
                        let _ = std::fs::remove_file(p);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn get_job(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().get(id).cloned()
    }

    pub fn public(&self, job: &Arc<Job>) -> Value {
        let mut d = job.data.lock().unwrap().clone();
        d["log_seq"] = json!(*job.seq.lock().unwrap());
        d["running"] = json!(job.running.load(Ordering::SeqCst));
        d
    }

    pub fn list(&self) -> Vec<Value> {
        let mut jobs: Vec<Arc<Job>> = self.jobs.lock().unwrap().values().cloned().collect();
        jobs.sort_by(|a, b| {
            let c = |j: &Arc<Job>| j.data.lock().unwrap()["created"].as_f64().unwrap_or(0.0);
            c(b).total_cmp(&c(a))
        });
        jobs.iter()
            .map(|j| {
                let mut d = self.public(j);
                if let Some(o) = d.as_object_mut() {
                    o.remove("recipe");
                }
                d
            })
            .collect()
    }

    pub fn log_since(&self, id: &str, since: u64) -> Option<Value> {
        let job = self.get_job(id)?;
        let lines: Vec<Value> = job
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|x| x["seq"].as_u64().unwrap_or(0) > since)
            .cloned()
            .collect();
        let next = *job.seq.lock().unwrap();
        Some(json!({"lines": lines, "next": next}))
    }
}
