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
OpenAI-compatible API, a browser Studio and a desktop app. The engine underneath can be swapped:
[FLE (Fast Local Experts)](https://github.com/sirfyyn/FastLocalExperts) for big mixture-of-experts models on small GPUs, the
upstream llama.cpp as fallback, or any llama-server compatible build of your own.

```text
 your tools (chat page, editor, agent)  ──/v1──►  FLE Link  ──►  engine (FLE, llama.cpp, your build)
```

| | |
|---|---|
| **66 s** | from nothing to the first answer on Linux with an NVIDIA card |
| **92 s** | the same on an 8 GB M2 MacBook, model download included |
| **91 tok/s** | a 125B MoE on one 32 GB GPU with the FLE engine, 2.5× plain llama.cpp |

## Install

```bash
curl -fsSL https://flelink.run/install.sh | sh     # macOS, Linux
irm https://flelink.run/install.ps1 | iex          # Windows
```

One file. No Python, no git, no sudo. The program is checked against its SHA-256 and lands in your user folder.
The desktop app is on the [releases page](https://github.com/sirfyyn/flelink/releases). The builds are unsigned: on macOS,
right-click the app and choose Open once; on Windows, choose More info, then Run anyway.

```bash
flelink                  # terminal interface; the first run sets everything up
flelink ask MODEL "…"    # one question, then everything is cleaned up
flelink run MODEL        # serve a model on http://127.0.0.1:8080/v1 until Ctrl+C
flelink studio           # browser interface on http://127.0.0.1:8642
flelink system           # what this machine can do, and which engine fits
flelink models           # known models and whether each fits
flelink doctor MODEL     # check model, engine, memory, disk
fle                      # short name, same program
```

## Flagship models

| Model | Size | Measured | Notes |
|---|---|---|---|
| [Qwen3.8-Flash-Next · FLE](https://huggingface.co/sirfyyn/Qwen3.8-Flash-Next-FLE-GGUF) | 125B MoE, 88.9 GB | decode **91 tok/s** (Strata 74, llama.cpp 36) | one 32 GB RTX PRO 4500, 120 GB RAM, same machine for all three (6 Oct 2026); smaller than Unsloth IQ4_XS at the same KL divergence |
| [DeepSeek-V4.1-Flash-1017 · ISTA-DASLab GSQ-RCO](https://huggingface.co/ISTA-DASLab/DeepSeek-V4.1-Flash-1017-GSQ-RCO-GGUF) | 763B MoE, IQ2_XXS 232 GiB | decode **23.3–23.5 tok/s** (two runs) | one 32 GB RTX PRO 4500, 18 GB expert cache, RAM capped at 102 GB, rest from NVMe (1 Nov 2026); FLE engine main, in FLE Link with the next engine release |

Small models such as Llama 3.2 1B run on any laptop and are in the built-in catalog (`flelink models`). Any other GGUF on
Hugging Face can be searched, checked for fit and downloaded from the Studio's Models page.

## How it works

1. **It reads the machine:** CPU, memory, GPU, free disk, and which engine build fits.
2. **It checks fit before downloading:** no 80 GB download that ends in an out-of-memory error. Downloads resume and are
   checked against SHA-256.
3. **It serves, and shows the command:** every start is a plain llama-server command you can copy and run without FLE Link.

Everything lives in one data folder shared by the terminal, the Studio and the app. An engine left behind by a crash is
stopped before a second one loads.

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

## Layout

| Path | What |
|---|---|
| `crates/flelink-core` | machine inspection, GGUF reading, launch plan, engine install, downloads, supervisor, orphaned-engine cleanup |
| `crates/flelink-api` | the `/api` the Studio talks to, and the `/v1` proxy |
| `crates/flelink` | the program: CLI, terminal interface, `studio` |
| `web/studio` | the Studio page (embedded into the binary) |
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

Plugins (recipes and engine adapters as files) ·
quantization for your machine · LoRA training. Until then, adapters load with `--arg=--lora`.

## Licence

MIT. Nothing leaves your computer except the downloads you start (engine from GitHub, models from Hugging Face); no account,
no telemetry.
