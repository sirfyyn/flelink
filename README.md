<p align="center">
  <img src="site/logo.svg" alt="FLE Link" width="96">
</p>

<h1 align="center">FLE Link</h1>

<p align="center"><b>Big models. One command.</b><br>
DeepSeek V4.1 Flash (763B) at about 50 tokens/s on one 32 GB GPU with 128 GB RAM, with the <a href="https://github.com/sirfyyn/FastLocalExperts">FLE engine</a>.<br>
The layer between an inference engine and everything that talks to it.</p>

<p align="center">
  <a href="https://flelink.run">flelink.run</a> ·
  <a href="#install">Install</a> ·
  <a href="#flagship-models">Models</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="CHANGELOG.md">Changelog</a>
</p>

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
| **68 s** | the same on an 8 GB M2 MacBook, model download included (FLE Link 0.1.3, fresh home, 3 Nov 2026) |
| **91 tok/s** | a 125B MoE on one 32 GB GPU with the FLE engine and its MTP draft head, 2.5× plain llama.cpp (`flelink run` starts it without the head so far) |
| **50 tok/s** | DeepSeek-V4.1-Flash (763B, 2-bit quant ISTA GSQ-RCO IQ2_XXS, 232 GiB) on one 32 GB GPU with 128 GB RAM and the DSpark draft (FLE v1.2.5: 49.8 / 50.8 / 50.6; through FLE Link 49.9 median) (`flelink run deepseek-v41-ista-dspark`). The draft does not change the output: the same answer every run, 37.6 tok/s without it by hand, 37.2 through FLE Link. The 2-bit quant itself is lossy: its next token agrees with the original weights 83.5 to 85.2 % of the time |

**Best performance measured on Linux.** The speed figures in this README were measured on Linux; macOS and Windows run
the same recipes.

## Install

```bash
curl -fsSL https://flelink.run/install.sh | sh     # macOS, Linux
irm https://flelink.run/install.ps1 | iex          # Windows
```

One file. No Python, no git, no sudo. The program is checked against its SHA-256 and lands in your user folder.
On a machine with a screen the installer asks whether to add the desktop app (macOS: `~/Applications/FLE Link.app`, Linux:
an AppImage with a menu entry, Windows: its setup), checked against the release's `SHA256SUMS`; `--app` / `--no-app`
(Windows: `$env:FLE_APP = 'yes'`) answer without asking. The app is also on the [releases page](https://github.com/sirfyyn/flelink/releases). The apps carry no Apple or Microsoft developer signature (the release files themselves are signed with minisign): on macOS,
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
flelink code             # work on the project in this folder with the agent (FLE Durable)
flelink service on       # start FLE Link at login, with the last model and the agent
flelink connect          # the settings for Claude Code, Codex, OpenCode, pi
flelink update           # install the newest FLE Link (checked against the signed release list)
flelink pair-code        # let another FLE Link use this machine (see Remote access)
fle                      # short name, same program
```

### Docker

```bash
docker run -d --name flelink -p 127.0.0.1:8642:8642 -v flelink:/data ghcr.io/sirfyyn/flelink
docker logs flelink        # the access key: requests from outside the container need it
```

The Studio on port 8642, settings, engines and models in the volume `/data`. NVIDIA GPUs: add `--gpus all` (NVIDIA Container
Toolkit on the host; not tested yet). Built and smoke-tested with every release (`.github/workflows/docker.yml`).

## Flagship models

| Model | Size | Measured | Notes |
|---|---|---|---|
| [Qwen3.8-Flash-Next · FLE](https://huggingface.co/sirfyyn/Qwen3.8-Flash-Next-FLE-GGUF) | 125B MoE, 88.9 GB | decode **91 tok/s** (Strata 74, llama.cpp 36) | one 32 GB RTX PRO 4500, 128 GB RAM, same machine for all three (6 Oct 2026); smaller than Unsloth IQ4_XS at the same KL divergence |
| [DeepSeek-V4.1-Flash-1017 · ISTA-DASLab GSQ-RCO](https://huggingface.co/ISTA-DASLab/DeepSeek-V4.1-Flash-1017-GSQ-RCO-GGUF) | 763B MoE, IQ2_XXS 232 GiB | with the [DSpark draft](https://huggingface.co/sirfyyn/DeepSeek-V4.1-Flash-1017-DSpark-FLE-GGUF): **about 50 tok/s** (the draft does not change the output at temperature 0: same answer every run); 37.6 without it by hand, 37.2 through FLE Link | `flelink run deepseek-v41-ista-dspark` / `deepseek-v41-ista`; one 32 GB RTX PRO 4500, 128 GB RAM (an expert store in RAM of about 90 GB, an unlimited memlock limit), Ryzen 9 9950X3D, Linux, FLE engine v1.2.5 (5 Nov 2026). `deepseek-v41-ista-iq1m`: the IQ1_M quant for speed, less exact |
| [Rin - Deepseek V4.1 Flash 1017](https://huggingface.co/sirfyyn/Rin-Deepseek-V4.1-Flash-1017-FLE-GGUF) | 763B MoE, 2-bit, split hot/cold | `flelink run rin-deepseek-v41-dspark`: 53.4 tok/s (about +6 % over ISTA IQ2_XXS), KL divergence 0.29 against the original (ISTA IQ2_XXS 0.25); needs FLE v1.2.6 or newer | built from ISTA DASLab's GSQ-RCO quants: the hot half stays on the GPU, the cold half is served from RAM; lossy, see the model card |

Small models such as Llama 3.2 1B run on any laptop and are in the built-in catalog (`flelink models`). Any other GGUF on
Hugging Face can be searched, checked for fit and downloaded from the Studio's Models page.

All figures in this table: best performance measured on Linux.

## How it works

1. **It reads the machine:** CPU, memory, GPU, free disk, and which engine build fits.
2. **It checks fit before downloading:** no 80 GB download that ends in an out-of-memory error. Only what is really missing
   counts, and a recipe's measured requirements come before the rule of thumb. Downloads resume and are checked against SHA-256.
3. **It uses what you already have:** before a download it looks in the models folder, `FLE_MODEL_DIRS`, the Hugging Face
   cache, LM Studio and Ollama for the same file (checked by SHA-256 where known) and links it instead.
4. **It serves, and shows the command:** every start is a plain llama-server command you can copy and run without FLE Link.

The rules and numbers behind each step: [docs/HOW-IT-WORKS.md](docs/HOW-IT-WORKS.md). Everything lives in one data folder shared by the terminal, the Studio and the app. An engine left behind by a crash is
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

## Tools through a hub, GitHub for the agent

Settings → **MCP hub**: the address of an MCP hub (the FYOS hub, `fyos-hub`, on this computer or another one) and its token.
The chat and the agent then see the hub's few meta tools (search a tool, call it) instead of every tool of every plugin, so
adding many tools does not fill the model's context or slow it down.

Settings → **GitHub**: a token (best a fine-grained one, only for the repos you choose, with Contents, Pull requests and
Issues read and write). FLE Link checks it with GitHub and keeps it in `<data>/secrets/github.json` (only you can read it;
the page never shows it again). The agent gets it as `GH_TOKEN` and as git's credential for github.com, so `git` and `gh`
work in your repos; requests to the hub carry it as `X-GitHub-Token` for the hub's GitHub plugin.

## Recent requests

The Engine page lists the last answers through FLE Link from any program, the chat in the Studio as well as Claude Code,
Codex or an app: who asked (from the User-Agent), which API (OpenAI, Anthropic, Responses), prompt and output tokens, speed
and duration (`GET /api/live/requests`, the MCP tool `requests`). The OpenAI format brings the engine's own timings; for
the other two the speed is the output over the wall clock, marked with ~.

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
`library`, `download`, `downloads`, `load`, `stop`, `requests`, `log`, `settings`, `connect`, `ask`.

## A practical test you can compare

Tokens per second on a short paragraph say little about a real answer. `flelink bench --task pagoda` sends a real task
(a voxel pagoda in three.js, one HTML file, the prompt of the DeepSeek numbers above) at temperature 0 without a thinking
phase, to any OpenAI-compatible server: the engine, the Studio's `/v1` (its token is taken from the running Studio), or
another runtime.

```bash
flelink bench --task pagoda --url http://127.0.0.1:8080 --runs 3 --answers ./answers --output pagoda.json
```

Each run reports decode and prompt speed from the server's own timings, the answer's length and whether it is complete
(the HTML ends before the token limit of 8000). The result file names the machine (CPU, RAM, GPUs), the server build and
the model file, so two people can compare their files; `--answers` keeps each scene to open in a browser.

Add `--share` to post your result in [Benchmarks](https://github.com/sirfyyn/flelink/discussions/categories/benchmarks):
it opens a filled-in form in your browser, FLE Link uploads nothing, and you read the text before you post it. The big-model
numbers in this README come from one machine, so a result from different hardware helps most.

## Demo mode: record the real Studio

For videos and demos the Studio can take its steps from outside, done the way a person would: `flelink studio --demo` (the
desktop app: `FLE_DEMO=1`), then

```bash
flelink demo record start pagoda.mp4 --window    # the system's screen recorder (wf-recorder, ffmpeg x11grab, screencapture)
flelink demo chat "Code a voxel pagoda in three.js" --max-tokens 8000 --temperature 0 --wait
flelink demo page /engine                        # the requests list with tokens and speed
flelink demo preview                             # Preview on the last answer with a web page
flelink demo record stop
```

The page types the message, sends it and shows the answer as it streams, with the live numbers. Without `--demo` the page
does not ask for steps at all. A step is one of a few fixed actions (`chat`, `page`, `preview`, `new`, `scroll`) with plain
values; nothing runs code.

## Recipes

A recipe is one TOML file: model, engine, start values, chat defaults, and optionally a draft model for speculative decoding.
A recipe sets only the engine's own switches, but it does choose the files and the engine arguments, so treat one from someone else like a program: `flelink run my.recipe.toml` shows what it sets and asks first. Keep the settings that work for you in `<data>/recipes/` and share the file to reproduce them elsewhere. Format and places: [docs/RECIPES.md](docs/RECIPES.md). Plugins add recipes and engines as files:
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
  webhook; destructive steps wait for your approval. It keeps tickets, a calendar and a list of events with you, and
  while a model is loaded the chat talks to it. FLE Link installs it with one click; it needs git and Node 22.19+:
  [docs/AGENT.md](docs/AGENT.md).
- **Accounts** (Settings): sign in to Hugging Face (gated models), GitHub (the agent's `git` and `gh`), Google and iCloud
  (your calendars, read only, mirrored to the agent). Tokens stay on this computer in files only you can read.
- **Computer use**: off at every start. On the Agent page you allow the agent to see the screen and use mouse and
  keyboard for 30 minutes (macOS, Linux with X11 or Wayland).
- **Tools**: MCP servers (stdio and Streamable HTTP, OAuth sign-in, MCP Apps) as chat tools, asked before each call unless trusted.
- **Updates** in one click or with `flelink update`: the release's checksum list is signed (minisign), and an update
  without a valid signature is refused. The desktop app replaces itself the same way (macOS, Linux AppImage).

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

MIT. No telemetry and no account of ours. FLE Link talks to the internet when you download (engine from GitHub, models
from Hugging Face), once a day for the update check (api.github.com; switchable in Settings), and for what you connect
yourself: an account under Settings, web search or a web MCP server. [Privacy](https://flelink.run/privacy.html).
