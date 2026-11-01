# Engine adapter

FLE Link talks to an inference engine through one small interface. Today there are two adapters (FLE, upstream llama.cpp);
a third engine is a new file, not a rewrite.

## Why

FLE Link is the layer between an engine and the tools that use it. The engine moves faster than the layer, and it may
be replaced. So nothing outside an adapter may know an engine's flags, binary names, environment variables or log lines.

## What exists already

Profiles carry `engine` (`fle`, `upstream`, `custom`), `binary` (the path of a `llama-server`), `args` and `env`. That field
is the key: `custom` becomes the generic adapter for any engine that behaves like `llama-server`, and a new engine gets its
own id next to the three. Old profiles stay valid. For `custom` two optional fields are added: `health` (default `/health`)
and `capabilities` (default: what plain `llama-server` offers).

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

`crates/flelink-core/src/adapter.rs` holds the three adapters (`fle`, `upstream`, `custom`) with their identity, names,
capabilities, health path, release (repo, default tag, whether it updates itself) and how its log reads (which lines
mark a loading phase, which errors get which hint; the Studio's engine supervisor asks the running engine's adapter). Core and Studio ask it which
engines FLE Link installs, where from and whether the FLE rules apply, instead of comparing ids.

`crates/flelink-core/src/assets.rs` says which archive of a release is the engine for a variant and platform (name
templates, CUDA version read from the name); the terminal's install and the Studio's both choose with it.

Still outside: `launch.rs` in both crates (plan to command:
two ports of two Python specifications, each held by its own comparison tests) and the FLE expert cache and draft acceptance lines that
`flelink-api/src/engine.rs` reads. They move behind the trait one at a time, with the comparison tests green.

Engines from plugins (docs/PLUGINS.md) are the third kind of adapter, as data: a release or a path, arguments or a
start template, a health path. They start from the terminal and from the Studio.
