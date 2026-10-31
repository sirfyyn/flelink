# FLE Link

**Run local AI models. One file, one command.**
FLE Link sits between an inference engine and the tools that talk to it (a chat page, your editor, an agent). It looks at the
machine, installs the engine that fits, downloads a model that fits, chooses the start settings, and serves an
OpenAI-compatible API. Nothing to type, no variables to set.

```text
 harness (chat page, editor, agent)  ──►  FLE Link  ──►  inference engine (FLE, llama.cpp, …)
```

The engine is swappable. [FLE (Fast Local Experts)](https://github.com/sirfyyn/FastLocalExperts) is the one it ships with today,
and the upstream llama.cpp is the fallback.

```bash
flelink            # the terminal interface (first run sets everything up)
flelink studio     # the browser interface at http://127.0.0.1:8642
flelink system     # what this machine can do, and which engine fits it
flelink doctor MODEL
flelink models     # known models and whether each fits
flelink run MODEL  # load a model and serve it until Ctrl+C
flelink ask MODEL "question"
```

Install (one file, no Python, no `git`, no `sudo`):

```bash
curl -fsSL https://flelink.run/install.sh | sh     # macOS, Linux
irm https://flelink.run/install.ps1 | iex          # Windows
```

## Layout

| Path | What |
|---|---|
| `crates/flelink-core` | machine inspection, GGUF reading, launch plan, engine install, downloads, supervisor |
| `crates/flelink-api` | the `/api` the browser page talks to, and the `/v1` proxy |
| `crates/flelink` | the program: CLI, terminal interface, `studio` |
| `web/studio` | the browser page (embedded into the binary) |
| `STUDIO_PIN`, `ENGINE_PIN` | commits of the Python specifications the comparison tests run against |
| `DEVIATIONS.md` | where this program deliberately differs from the Python original, and why |

## Develop

```bash
cargo build --release -p flelink
FLE_STUDIO_SRC=…/fle-studio/src FLE_ENGINE_SRC=…/FastLocalExperts FLE_STUDIO_PYTHON=python3.13 cargo test --all
```

Without the two source folders the comparison tests skip themselves. Training (LoRA/QLoRA) is not part of FLE Link;
adapters can be loaded with `--arg=--lora`.

MIT licence.
