# Engine adapter

FLE Link talks to an inference engine through one small interface. Today there are two adapters (FLE, upstream llama.cpp);
a third engine is a new file, not a rewrite.

## Why

FLE Link is the layer between an engine and the tools that use it. The engine moves faster than the layer, and it may
be replaced. So nothing outside an adapter may know an engine's flags, binary names, environment variables or log lines.

## What an adapter provides

```rust
pub trait EngineAdapter {
    /// "fle", "llama-cpp", ... (stable id, shown in the UI, stored in settings)
    fn id(&self) -> &'static str;

    /// Builds this engine for the machine: which variants exist (cuda, metal, vulkan, cpu), where they come from,
    /// how they are checked (SHA-256), what they unpack to.
    fn variants(&self, sys: &SysInfo) -> Vec<Variant>;
    fn install(&self, v: &Variant, dir: &Path, progress: &mut dyn FnMut(Progress)) -> Result<Installed>;
    fn find(&self, root: &Path) -> Option<Installed>;          // server binary + version

    /// What this engine can do (drives the plan and hides options it cannot honour).
    fn capabilities(&self, e: &Installed) -> Capabilities;      // mtp, speculative types, lora, expert offload, ...

    /// The command that serves `plan` (arguments and environment). Pure, so `flelink doctor` can print it.
    fn command(&self, plan: &Plan, e: &Installed) -> Command;

    /// Ready, loading, failed. Default: HTTP /health. Adapters may read the log for phases and hints.
    fn health(&self, port: u16) -> Health;
    fn phase_from_log(&self, line: &str) -> Option<Phase>;
    fn hint_from_log(&self, tail: &[String]) -> Option<Hint>;

    /// API surface the proxy may forward ("/v1/chat/completions", "/props", ...). Others answer 501.
    fn api(&self) -> ApiSurface;
}
```

## Rules

1. **Plan is engine-neutral.** `Plan` holds model, context, GPU split, speculation wish, "no thinking", and so on. The
   adapter translates it to flags. The FLE policy variables live in the FLE adapter only.
2. **Fallback is a list of adapters**, not a special case: "FLE, then llama.cpp" is configuration.
3. **No engine on PATH.** Engines live in FLE Link's folder, or at a path the user names.
4. **Conformance tests per adapter** run against a fake server that speaks the same HTTP, so a new adapter proves itself
   without a GPU: starts, becomes ready, answers a chat, streams, survives a crash, stops cleanly.

## Where it is today

`engines.rs` (variants, install, find), `serve.rs` (process, health), `launch.rs` (plan to command) and
`flelink-api/src/engine.rs` (phases, hints, restart) hold the FLE and upstream rules together. Step 1 is to move them
behind the trait without changing behaviour; the existing comparison tests must stay green. Step 2 adds the
capabilities struct. Step 3 is a third adapter as a proof (for example a plain `llama-server` at a user path).
