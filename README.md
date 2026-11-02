<p align="center">
  <img src="site/logo.svg" alt="FLE Link" width="96">
</p>

<h1 align="center">FLE Link</h1>

<p align="center"><b>Local models, wired up in one command.</b><br>
The layer between an inference engine and everything that talks to it.</p>

<p align="center">
  <a href="https://flelink.run">flelink.run</a> ·
  <a href="#install">Install</a> ·
  <a href="#flagship-models">Models</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="CHANGELOG.md">Changelog</a>
</p>

<!-- demo: a first run from nothing to an answer (recording goes here) -->

FLE Link reads your machine, installs the engine that fits, downloads a model that fits, picks the start settings and serves an
OpenAI- and Anthropic-compatible API, a browser Studio and a desktop app. The engine underneath can be swapped:
[FLE (Fast Local Experts)](https://github.com/sirfyyn/FastLocalExperts) for big mixture-of-experts models on small GPUs, the
upstream llama.cpp as fallback, or any llama-server compatible build of your own.

```text
 your tools (chat page, editor, agent)  ──/v1──►  FLE Link  ──►  engine (FLE, llama.cpp, your build)
```

Above it, [FLE Durable](https://github.com/sirfyyn/fle-durable) is the agent: always on, works from events (its agenda,
webhooks, your messages), and FLE Link starts it with your model on the Agent page.

| | |
|---|---|
| **66 s** | from nothing to the first answer on Linux with an NVIDIA card |
| **92 s** | the same on an 8 GB M2 MacBook, model download included |
| **91 tok/s** | a 125B MoE on one 32 GB GPU with the FLE engine, 2.5× plain llama.cpp |
| **23.2 tok/s** | DeepSeek-V4.1-Flash (232 GiB) on the same 32 GB GPU, with FLE's DSpark draft model |

## Install

```bash
curl -fsSL https://flelink.run/install.sh | sh     # macOS, Linux
irm https://flelink.run/install.ps1 | iex          # Windows
```

One file. No Python, no git, no sudo. The program is checked against its SHA-256 and lands in your user folder.
On a machine with a screen the installer asks whether to add the desktop app (macOS: `~/Applications/FLE Link.app`, Linux:
an AppImage with a menu entry, Windows: its setup), checked against the release's `SHA256SUMS`; `--app` / `--no-app`
(Windows: `$env:FLE_APP = 'yes'`) answer without asking. The app is also on the [releases page](https://github.com/sirfyyn/flelink/releases). The builds are unsigned: on macOS,
right-click the app and choose Open once; on Windows, choose More info, then Run anyway.

```bash
flelink                  # terminal interface; the first run sets everything up
flelink ask MODEL "…"    # one question, then everything is cleaned up
flelink run MODEL        # serve a model on http://127.0.0.1:8080/v1 until Ctrl+C
flelink studio           # browser interface on http://127.0.0.1:8642
flelink system           # what this machine can do, and which engine fits
flelink models           # known models and whether each fits
flelink doctor MODEL     # check model, engine, memory, disk
flelink recipes          # every recipe (built in, yours, from plugins) and whether it fits
flelink pair-code        # let another FLE Link use this machine (see Remote access)
fle                      # short name, same program
```

## Flagship models

| Model | Size | Measured | Notes |
|---|---|---|---|
| [Qwen3.8-Flash-Next · FLE](https://huggingface.co/sirfyyn/Qwen3.8-Flash-Next-FLE-GGUF) | 125B MoE, 88.9 GB | decode **91 tok/s** (Strata 74, llama.cpp 36) | one 32 GB RTX PRO 4500, 120 GB RAM, same machine for all three (6 Oct 2026); smaller than Unsloth IQ4_XS at the same KL divergence |
| [DeepSeek-V4.1-Flash-1017 · ISTA-DASLab GSQ-RCO](https://huggingface.co/ISTA-DASLab/DeepSeek-V4.1-Flash-1017-GSQ-RCO-GGUF) + [DSpark draft](https://huggingface.co/sirfyyn/DeepSeek-V4.1-Flash-1017-DSpark-FLE-GGUF) | 763B MoE, IQ2_XXS 232 GiB + 5.2 GB draft | decode **23.2 tok/s** (two rounds of three prompts: 22.9 and 23.6), 72–89 % of the drafts accepted | `flelink run deepseek-v41-ista-dspark`; one 32 GB RTX PRO 4500, 120 GB RAM, 15 GB expert cache, after a warm-up, no CPU pinning, rest from NVMe (1 Nov 2026); FLE engine v1.2.1 |

Small models such as Llama 3.2 1B run on any laptop and are in the built-in catalog (`flelink models`). Any other GGUF on
Hugging Face can be searched, checked for fit and downloaded from the Studio's Models page.

## How it works

1. **It reads the machine:** CPU, memory, GPU, free disk, and which engine build fits.
2. **It checks fit before downloading:** no 80 GB download that ends in an out-of-memory error. Only what is really missing
   counts, and a recipe's measured requirements come before the rule of thumb. Downloads resume and are checked against SHA-256.
3. **It uses what you already have:** before a download it looks in the models folder, `FLE_MODEL_DIRS`, the Hugging Face
   cache, LM Studio and Ollama for the same file (checked by SHA-256 where known) and links it instead.
4. **It serves, and shows the command:** every start is a plain llama-server command you can copy and run without FLE Link.

Everything lives in one data folder shared by the terminal, the Studio and the app. An engine left behind by a crash is
stopped before a second one loads.

## Connect your tools

The engine answers the OpenAI API (Chat Completions and Responses) and the Anthropic API (Messages) on the same address, so
coding agents use the local model in the format they speak natively. `flelink connect` finds the running model and prints the
setting for each tool; the Engine page of the Studio shows the same with a copy button.

```sh
flelink connect            # Claude Code, Codex CLI, OpenCode, pi and the plain addresses
flelink connect claude     # one tool
```

For Claude Code it prints `ANTHROPIC_BASE_URL=http://127.0.0.1:8642` (the Studio and the app) or `:8080` (`flelink run`)
with the model name; Codex gets a provider with `wire_api = "responses"`, OpenCode and pi an OpenAI-compatible provider. No
key is checked on this machine.

## Live preview in the chat

An HTML or SVG block in an answer gets a Preview button: the page runs right under the code, in a sandboxed frame from the
server (`POST /api/preview`, served like an MCP app at `/app-frame/<id>`), with its own opaque origin, no access to the
Studio or its API, scripts and styles only from the common CDNs, and no network requests of its own. "Open in a tab" shows it
full size.

## Let a coding agent drive it (MCP)

`flelink mcp` is an MCP server on stdin/stdout. An agent sees what fits the machine, downloads and loads a model (installing
the engine first on a fresh machine), stops it, reads the engine log, changes settings, tests the model with one prompt and
gets the `connect` setting. It works through the Studio's own API; when no Studio or app runs, it starts one in the
background (the Studio and the app say where they are in `<data>/studio.json`, readable only by your user).

```sh
claude mcp add flelink -- flelink mcp          # Claude Code
```

Codex (`~/.codex/config.toml`): `[mcp_servers.flelink]` with `command = "flelink"` and `args = ["mcp"]`. OpenCode:
`"mcp": {"flelink": {"type": "local", "command": ["flelink", "mcp"]}}`. Tools: `status`, `system`, `recipes`,
`library`, `download`, `downloads`, `load`, `stop`, `log`, `settings`, `connect`, `ask`.

## Recipes

A recipe is one TOML file: model, engine, start values, chat defaults, and optionally a draft model for speculative decoding.
It never runs code. Keep the settings that work for you in `<data>/recipes/`, share the file, and `flelink run my.recipe.toml`
reproduces them elsewhere. Format and places: [docs/RECIPES.md](docs/RECIPES.md). Plugins add recipes and engines as files:
[docs/PLUGINS.md](docs/PLUGINS.md).

## In the Studio

- **Chat** with live tokens per second, prompt speed and context fill.
- **Engine**: the plan with a note for every choice, the log in plain words, profiles, one sentence per start flag.
- **Experiments**: speed test, a tuner ("Best for my machine") that keeps a change only when it is faster with a 95 %
  interval that excludes zero, A/B/A comparisons with a drift check, KL divergence and a knowledge probe next to speed.
- **Expert cache**: hit rate over time and per layer, read from the FLE engine's log.
- **Forge**: build your own quant: hybrids from whole expert layers of other quants (exact choice for a size budget, memory
  preview), quantization with an importance matrix, expert pruning, KL-divergence check, a recipe file others can rebuild.
- **Agent**: [FLE Durable](https://github.com/sirfyyn/fle-durable) next to your model, an always-on local agent that works from events. You plan things on its
  agenda (once, daily, on certain days, every few minutes), it plans its own follow-ups, any tool can hand it an event by
  webhook; destructive steps wait for your approval. Needs Node 22.19+ and FLE Durable: [docs/AGENT.md](docs/AGENT.md).
- **Tools**: MCP servers (stdio and Streamable HTTP, OAuth sign-in, MCP Apps) as chat tools, asked before each call unless trusted.
- **Updates** in one click, checked against the release's SHA-256.

## Remote access

One FLE Link computes for another. The machine with the GPU shows a pairing code (Settings or `flelink pair-code`); the
client enters it and gets its own token, with rights per client and revocable any time. Hosts are reached by address (pinned
TLS) or through an SSH tunnel (keys only).

## Host mode

```bash
flelink studio --host 0.0.0.0
```

Opens the Studio to your network: other machines sign in with an access key, over https with a certificate of its own whose
SHA-256 fingerprint is printed at start, together with the address to open. API clients send `Authorization: Bearer KEY`.
`--no-tls` serves plain http for a network you trust.

## Tested by breaking it

`tests/fulltest/fulltest.py` runs every command twice, once as intended and once with something broken on purpose: a download
killed halfway, no network, cut-off and random model files, the engine killed, the program killed, ports taken, wrong keys,
broken requests. It runs on macOS and Linux against a release build:

```bash
python3 tests/fulltest/fulltest.py --bin target/release/flelink --work /tmp/ft
```

The unit and end-to-end tests run on Linux, macOS and Windows for every pull request.

## Layout

| Path | What |
|---|---|
| `crates/flelink-core` | machine inspection, GGUF reading, launch plan, engine install, downloads, supervisor, orphaned-engine cleanup, the settings for coding agents (`connect.rs`) |
| `crates/flelink-api` | the `/api` the Studio talks to, and the `/v1` proxy |
| `crates/flelink` | the program: CLI, terminal interface, `studio`, the MCP server (`mcp.rs`) |
| `web/studio` | the Studio page (embedded into the binary) |
| `web/plugins` | the built-in pages: Agent, Forge, Updates, Remote access |
| `crates/flelink-core/recipes` | built-in recipes that need more than the catalog (a draft model, engine flags) |
| `app/` | the desktop app (Tauri 2) |
| `site/` | flelink.run |
| `tests/fulltest` | the fault test suite |
| `STUDIO_PIN`, `ENGINE_PIN` | commits of the Python specifications the comparison tests run against |
| `DEVIATIONS.md` | where this program deliberately differs from the Python original, and why |

## Develop

```bash
cargo build --release -p flelink
FLE_STUDIO_SRC=…/fle-studio/src FLE_ENGINE_SRC=…/FastLocalExperts FLE_STUDIO_PYTHON=python3.13 cargo test --all
```

Without the two source folders the comparison tests skip themselves. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Roadmap

More engine adapters (vLLM, ik_llama.cpp) · faster long-prompt reading with FLE · LoRA training. Until then, adapters load
with `--arg=--lora`.

## Licence

MIT. Nothing leaves your computer except the downloads you start (engine from GitHub, models from Hugging Face); no account,
no telemetry.
