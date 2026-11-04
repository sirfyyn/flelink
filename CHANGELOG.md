# Changelog

## Unreleased

- **Forge: optimize for my usage.** In Forge → Recipe (quantize), a card takes FLE's usage profile of a recipe, the GPU
  expert cache and the quant you run now, and plans the expert type per layer and role: layers the cache serves keep
  more bits, layers read on the CPU fewer, at the reference's error. It shows what each plan reads on the CPU per token
  next to the reference, takes the types into the recipe's rules, and copies the note a model card needs. The plan now
  compares every candidate by what it really reads (more bits in a hot layer make its slots bigger, so the cache holds
  fewer experts), so it is never slower than an even quant.
- **The app picks folders like any app:** "Add folder" on the first run and the models folder in Settings get a
  "Choose…" button that opens the system's folder picker (the app allows its page this one call and nothing else). In a
  browser the text field stays.
- **Clearer model cards:** a recipe that does not fit says why in numbers ("measured with 120 GB RAM and a 32 GB GPU;
  this computer has 8 GB RAM shared with the GPU"), a long note shows its first sentence and unfolds the rest, and a
  download that stops for lack of disk space offers a folder on another drive. The first-run headline is one sentence
  per language, so Japanese, Korean and Chinese keep their word order.
- **The Studio in eight languages:** English, German, French, Spanish, Chinese (simplified), Japanese, Korean and
  Russian, from the browser's language or Settings → Appearance → Language. The pages a beginner sees are translated (the
  first-run view, chat, models, engine, settings, hints; German also most advanced settings); chat messages, code, logs
  and model descriptions stay as they are, and a text without a translation stays English.
- **Fewer things that stop a first try:** the macOS app is signed ad hoc (Apple Silicon opens it instead of calling it
  damaged; the installer avoids every warning, flelink.run says what a browser download asks); loading shows the time
  left; the Thinking chip says "No thinking in this model" for models without one; on a small machine the fit rule keeps
  a quarter of the RAM for the system (8 GiB before), so a 1 GB model no longer shows "too big" on an 8 GB Mac
  (DEVIATIONS.md D13); the big FLE quants fold away where none fits; the first-run view shows the free space and the way
  to another folder; a CUDA driver error names the driver needed; an engine the system killed for lack of memory says so.
- **A first-run view with three ways to a model:** download one that fits this computer (the smallest fitting recipes,
  one click), use one already on it (FLE Link lists what it finds in the models folder, the Hugging Face cache, LM Studio
  and Ollama, each with a Use button), or connect to a server that already runs. "Add folder" names a folder of GGUF
  files; it is searched from then on, by the Studio and by recipes. Ollama's models are found by their names.
- **A simpler first look:** the sidebar shows Chat, Models, Engine and Settings; Experiments, Expert cache and the plugin
  pages (Agent, Forge) sit in "Lab", folded until opened. Without a model the chat hides its Thinking, Tools and Sampling
  chips and the Engine page its tuner. The live numbers explain themselves on hover (drafts accepted, expert cache hits,
  prompt reading, slots).
- **Share on GitHub:** `flelink bench --task pagoda --share` and the Forge's "Share on GitHub" open a discussion in
  sirfyyn/flelink (Benchmarks, Community quants) with the result filled in: the machine, the server build, every run, or
  the portable recipe with its check. You read it and post it with your own account; FLE Link uploads nothing. A post too
  long for a link goes to the clipboard.
- **Forge, quants cut to your own use (first part):** reads an FLE usage profile, rewrites it for a new quant, and
  plans the bits per layer and role from the profile and your GPU cache budget: layers whose experts the cache serves keep
  more bits, layers read on the CPU fewer, at the error of a reference quant. The plan already keeps a type per expert for
  the split expert tensors FLE plans next. The page and the build follow.
- **Clearer words:** `flelink bench --task` says "Nothing answers at …" when no server runs, `flelink ask` without a
  question says so before loading the model (and reads one from a pipe), `flelink recipes` keeps its columns with long
  ids, and the DeepSeek recipe's title and note name the RAM expert store and the FLE Link number on v1.2.4.

## 0.1.5 (2026-11-04)

- **DeepSeek recipes read prompts in windows of 8 tokens instead of 16:** with 16, FLE v1.2.4 sends each window down a
  per-expert CUDA path that computes a repeated null slot with a real expert's row, so prompts came out slightly wrong
  (nothing crashed; found in FLE's code review, fixed in FLE v1.2.5). With 8 every window stays on the path that computes
  correctly, at practically the same speed (59 tokens in 1.56 s instead of 1.45 s). Update to get it.
- **A recipe runs the engine as it would run by hand:** the Studio no longer adds the model's suggested sampling
  (`--temp`, `--top-p`, `--top-k`, `--min-p`, `--presence-penalty`) or a large prompt cache in RAM (`--cache-ram`) to a
  recipe's command, and a flag the recipe sets appears once. Those sampling flags were the server's defaults for every
  `/v1` request that set none, so a client got other answers through FLE Link than from the engine itself. The Studio's
  chat sends the model's suggested values with each request instead ("Model defaults").
- **A recipe start no longer waits for checksums:** copies of the model already on this machine that FLE Link has not
  read before are checked in the background (120 GB take over a minute), and the recipe starts once they match. Before,
  the Studio's request waited for the whole read (seen with IQ1_M on 4 Nov 2026).
- **A practical test you can compare:** `flelink bench --task pagoda` measures a real answer (a three.js scene, up to
  8000 tokens) at temperature 0 without thinking, through any `/v1`: decode and prompt speed, length, whether the answer is
  complete, and the machine and server build in the result file. `--answers DIR` keeps the scenes.

- **A recipe that needs a newer FLE updates the engine itself in the Studio too:** starting it installs the newer
  release (progress on the Engine page) and starts the recipe once it is in, like `flelink run`. Before, the Studio
  refused with "Update the engine on the Engine page".

## 0.1.4 (2026-11-03)

- **DeepSeek-V4.1 ISTA on FLE v1.2.4** (`min_version v1.2.4`, older engines are refused with the reason): the RAM
  expert store (90 GB), the CPU pool and the prefill windows, exact (no cache bias). FLE Link waits for the store's warm
  start before it says ready. Measured through FLE Link on one 32 GB GPU with 120 GB RAM: a 3100-token three.js answer at
  **31.6 tok/s**, its 59-token prompt in 2.5 s (16.4 s before the windows). FLE's release measures 33.3 tok/s on the same
  kind of answer (n=6).
- **Two more DeepSeek recipes:** `deepseek-v41-ista-dspark` with the DSpark drafter (lossless at temperature 0; 41.9
  tok/s in FLE's release, one run) and `deepseek-v41-ista-iq1m`, a smaller quant (faster, less exact; not yet measured on
  v1.2.4). The cache-bonus recipe is gone: the bonus slows the store down.
- **The Studio installs engines without GitHub's API too:** when the hourly API limit is used up, the Engine page reads
  the release page and its checksums, like the CLI (it stopped with "Could not reach GitHub: Forbidden").
- **The chat hides a stray `</think>`** at the start of an answer without thinking.
- **Demo mode** (`flelink studio --demo`, `FLE_DEMO=1` for the app): `flelink demo chat|page|preview|new|scroll|wait` drives
  the real Studio from outside, the way a person would, and `flelink demo record start|stop` records the screen with the
  system's recorder. For videos of the Studio without a browser automation.
- **Recipes can set the chat's answer length** (`[chat] max_tokens`), like temperature and thinking.
- **The checksum decides, not the name:** a file FLE Link has read once (or downloaded and verified) is found again by
  its SHA-256 under any name, so two recipes that share a part (the DeepSeek Engram file of IQ2_XXS and IQ1_M) keep one
  copy instead of downloading 110 GB twice.
- **Recipes for measured settings:** `[run] unset_env` drops automatic values FLE Link would set, `[run] profile` brings
  a usage profile so the first start is warm, and `[run] ready_log` makes FLE Link wait for a log line (an expert store
  that fills after the server is up) before it says ready. Found in a benchmark through FLE Link on 3 Nov 2026: without the
  profile FLE started cold, and "ready" came before the 90 GB expert store was filled.
- **Says what it does when something gets in the way:** a download that picks up an earlier one says "Resuming the
  download … already here", a checked file says so, and when port 8080 is taken the engine's new port is named
  (all three ran silently before; seen while recording the video on 3 Nov 2026).
- **A data folder of its own keeps its own settings:** with `FLE_HOME` set, the config is `<FLE_HOME>/config.toml`
  instead of `~/.config/fle/config.toml` (`--config` and `FLE_CONFIG` still win). A test home or a container no longer
  reads or changes the user's settings.

## 0.1.3 (2026-11-03)

- **Upstream llama.cpp installs without GitHub's API too:** its releases have no `SHA256SUMS`, so when the hourly API
  limit is used up FLE Link reads the files and their sha256 from the release page instead. Before, a first run on a
  shared address stopped with "GitHub answered 403" (seen on a fresh Mac, 3 Nov 2026).
- **Installers** take the newest tag from the `/releases/latest` redirect, the API only as a fallback.

## 0.1.2 (2026-11-03)

- **DeepSeek-V4.1 ISTA on FLE v1.2.3:** the cache bonus only after layer 14, the CPU side scheduled asynchronously, two
  requests at once again (v1.2.3 fixes the q8_0 abort; the one-slot workaround is gone). Recipes can name the oldest
  engine they work with (`min_version`); the Studio refuses an older one with the reason, `flelink run` updates it.
- **MCP hub:** one hub address in Settings gives the chat and the agent their tools through a few meta tools, so many
  tools do not slow the model down.
- **GitHub sign-in:** a token in Settings, checked with GitHub and kept private; the agent works in your repos with it
  (`git`, `gh`), and the hub's GitHub plugin gets it.
- **Measured:** DeepSeek-V4.1 ISTA through FLE Link on FLE v1.2.3: 29.1 tok/s for one request, two at once about 39
  tok/s together (one 32 GB GPU, 120 GB RAM).
- **Engines install without GitHub's API** when its hourly limit is used up (a shared address): the release page and its
  `SHA256SUMS` give the files and checksums.
- **An attached server that comes back** no longer stays "not answering" when no page is open.
- **The chat sees only the hub's meta tools**, not every plugin tool a hub may list (136 on the FYOS hub); Settings shows
  whether the hub answers.

## 0.1.1 (2026-11-02)

- **Agent:** a new built-in page runs [FLE Durable](https://github.com/sirfyyn/fle-durable) next to your model, an always-on local agent that works from events: an
  agenda you and the agent plan (once, daily, on certain days, every few minutes), webhooks (`POST /api/p/agent/events`),
  one durable queue, approvals for destructive steps. FLE Link starts and stops it, gives it the engine's model and window,
  and keeps its token to itself. Needs Node 22.19+ and FLE Durable ([docs/AGENT.md](docs/AGENT.md)).

- **Coding agents on the local model:** the engine answers the OpenAI API (Chat Completions and Responses) and the
  Anthropic API (Messages) on one address. `flelink connect [claude|codex|opencode|pi|api|mcp]` finds the running model
  and prints the setting for each tool; the Engine page shows the same with a copy button. The desktop app takes port
  8642 first, so the address stays the same.
- **MCP server:** `flelink mcp` lets an agent run FLE Link: what fits, download, load (installing the engine first on a
  fresh machine), stop, recent requests, engine log, settings, connect, one test prompt. It finds the Studio or the app
  through `<data>/studio.json` (mode 0600) and starts a Studio when none runs.
- **Live preview in the chat:** HTML and SVG blocks get a Preview button; the page runs under the code in a sandboxed
  frame without access to the Studio.
- **Recent requests** on the Engine page: every answer from any program (the chat, Claude Code, Codex, apps) with API,
  tokens, speed and duration; Anthropic and Responses traffic now counts in the live numbers too.
- **First start on a fresh machine:** Start in the Studio installs the engine that fits by itself, then starts.
- **Downloads pick themselves up again:** a broken or stalled connection (60 s without data) resumes where it stopped,
  up to five tries in a row without progress. Checking a model already on disk shows its progress.
- **Looks:** the logo works while the engine starts or answers, live numbers count up, pages fade in, buttons answer;
  the terminal shows a bar with speed and time left and colours (not with `NO_COLOR`); the full-screen terminal uses the
  brand colours and rounded frames; new macOS 26 icon and the cube logo everywhere.
- **Calmer Models page:** it no longer rebuilds every second (clicks could get lost) and shows its recipes on the first
  visit. Small models whose tool call the engine cannot read answer without tools from then on, without a failed try first.
- **`flelink run` stops politely:** SIGTERM, ten seconds, then SIGKILL.
- **Docker:** `ghcr.io/sirfyyn/flelink`, the Studio on port 8642 with the data in `/data`, built and tested with every
  release.
- **No foreign update notice:** in a data folder taken over from FLE Studio, FLE Link showed the Python Studio's own
  update answer ("FLE Link 0.4.0 is available"). It keeps its answer in its own file now (`flelink-update-check.json`).
- **The installers offer the desktop app:** on a machine with a screen `install.sh` and `install.ps1` ask whether to
  add the app too (`--app` / `--no-app`), checked against `SHA256SUMS`, which every release now carries for all its files.
- **DeepSeek-V4.1 with the hot experts in RAM:** recipe `deepseek-v41-ista` (decision 735: 22 GB expert cache, hot experts
  locked in RAM after a profile from the first run, large batches); needs the FLE engine after v1.2.1. 29.3 tok/s on one 32 GB
  GPU with 120 GB RAM on tasks the profile has not seen; one request at a time, since FLE v1.2.2 aborts when two share the
  q8_0 cache. Recipes can name a
  per-recipe profile file (`{profile}`) and a lock size from the memory (`{hot_mlock_gb}`); FLE Link raises the limit for
  locked memory before such an engine starts and says in the engine log when the system does not allow enough.

## 0.1.0 (2026-11-01)

First version of FLE Link: the layer between an inference engine and the tools that use it.

- **One file, one command.** `flelink` (short name `fle`) looks at the machine, installs the engine that fits (FLE or plain
  llama.cpp, checked against SHA-256), downloads a model that fits (resumable, checked), chooses the start settings and opens a
  chat. Commands: `system`, `doctor`, `plan`, `models`, `run`, `ask`, `serve`, `engines`, `inspect`, `verify`, `config`,
  `fetch-sidecars`, `report`, `bench`, `studio`.
- **Browser interface** (`flelink studio`): the FLE Studio 0.4.1 page under the FLE Link name, served by a Rust server that answers the same `/api` and
  `/v1` as the Python original (compared request by request in the tests; deliberate differences in `DEVIATIONS.md`).
- **Speed test in the browser** (Experiments): the three requests of FLE Studio against the running engine, stored with the
  exact command, compared with the last run of the same setting (median and 95 % interval, the same numbers as the Python
  Studio). **Tuner** ("Best for my machine" on the Engine page): tries threads, expert cache or `--n-cpu-moe`, batch and
  context one at a time, keeps only what is faster with a 95 % interval and saves the winner as a launch profile.
  **A/B/A comparison** of two settings with drift check. **Flag help:** every part of the start command explained in
  one sentence on the Engine page. **Expert cache view:** hit rate over time and per layer, fill, hot experts, read
  from the FLE engine's log. **Quality next to speed:** KL divergence of a quantization
  against its reference (with the engine's `llama-perplexity`; the reference run is kept and reused) and a knowledge probe
  (48 own questions in English and German, CC0; MKQA can be imported on request), also right after a speed test.
- **MCP servers as chat tools:** the `mcpServers` JSON in Settings, stdio programs and Streamable HTTP servers; their tools
  appear as `server__tool`, ask before each call unless the server is trusted, and the last tool list is remembered so the
  prompt stays the same while a server starts. Servers of the older HTTP+SSE revision are found by themselves. **Sign
  in** to remote servers that want it (OAuth with PKCE, discovery and client registration; tokens in `<data>/oauth/`, mode
  0600, refreshed by themselves). **MCP Apps:** a tool with a `ui://` view shows it in the chat, in a frame under the
  policy its resource asks for; the app may call only its own server's tools, with the same approval as the model.
- **Updates from inside:** once a day FLE Link asks GitHub for a newer release (and a newer FLE engine), shows the notes as
  plain text and, for the file the installer put in place, updates in one click: archive checked against the release's
  SHA-256, the new program beside the old one, the same arguments again. The app, a cargo install, a source build or a
  system folder get the command or page that fits. Turn it off in Settings.
- **Remote access:** one FLE Link computes for another. The host shows a pairing code (Settings or `flelink pair-code`,
  10 minutes, one use); the client proves it with an HMAC bound to the certificate it pinned and gets a token of its own.
  Rights per client (chat, look and measure; admin for changes, installs confirmed on the client), revoke any time. Hosts
  by address (pinned TLS) or SSH target (`ssh -N -L`, keys only, kept alive); everything under `/h/<host>/` is passed on,
  streams included; a mirrored host shows its chats offline.
- **Forge** (its own page, `/forge`): build your own quant with the FLE recipe. Hybrids that take whole expert layers
  from other quants of the same model (the layer choice for a size budget is computed exactly, with a memory preview for
  your RAM and GPU), quantization with `llama-quantize` and an importance matrix, expert pruning (REAP, with the FLE
  engine's `llama-fle-reap` or from the importance matrix), the KL-divergence check against the reference, and a recipe
  file that others can rebuild. Jobs run in the background, survive a restart and resume where they stopped; a finished
  model goes into the library with a start profile. The built files are byte for byte those of the FLE Studio Forge.
- **Desktop app** (Tauri 2, `app/`): the same server inside the app on a free loopback port; closing the window stops the
  server and the model. On Windows the engine ends with the app, also after a crash.
- **Host mode:** `flelink studio --host 0.0.0.0` for another machine to look at: access key, https with a certificate of its
  own (fingerprint printed), `--no-tls` for a trusted network.
- **Recipes with a draft model:** `[draft]` downloads a draft model with the model and puts its path where `run.args`
  says `{draft}`; `{physical_cores}` is the core count without SMT; `model.save_as` stores a part under the name the
  loader expects. Built in: `deepseek-v41-ista-dspark`, DeepSeek-V4.1-Flash (ISTA IQ2_XXS) with FLE's DSpark draft model
  and the measured start command (FLE engine v1.2.1 or newer).
- **Models already on the machine are used:** before a download, a recipe looks in the models folder, `FLE_MODEL_DIRS`, the
  Hugging Face cache, LM Studio and Ollama for the same file (name, and SHA-256 where known) and links it instead.
- **Engine adapters:** `fle`, `upstream` and `custom` (any llama-server-compatible binary) behind one interface.
- **One data folder** for the terminal, the Studio and the app (`FLE Link` in the user's app data, `flelink` on Linux). A model
  `flelink ask` downloaded shows up in the Studio, and an engine is installed once. Until that folder exists, an older `FLE
  Studio` or `FLE` folder is used as it is.
- **Engines left behind are cleaned up.** Each running engine has a note in `<data>/run/`; before an engine starts, one whose
  program died (`kill -9`, a crash) is stopped instead of loading the model a second time next to it.
- `doctor`, `plan`, `report`, `fetch-sidecars` and `serve` take catalog names (`flelink doctor llama-3.2-1b`) and find the engines
  FLE Link installed; without an NVIDIA GPU or with a plain llama.cpp, `auto` means plain llama.cpp settings, not a failure.
- The Studio asks once more without tools when a small model writes a tool call the engine cannot read, and says so under the
  answer.
- **Recipes and plugins:** a recipe (TOML) is a model with its engine, start settings and chat defaults (also started from the
  Studio's Models page), shared as a file or
  URL (`flelink recipes`, `flelink run my.recipe.toml`, docs/RECIPES.md). A plugin is a folder that brings engines (from a
  GitHub release, your own build or a program on the PATH, with its own start template if it does not speak like
  llama-server) and recipes (`flelink plugins`, docs/PLUGINS.md); it is data, not code. Recipes with a plugin engine start
  from the terminal and from the Studio.
- **The FLE engine updates itself:** once a day FLE Link asks for a newer FLE release and installs it next to the old one
  (`FLE_NO_UPDATE=1` turns it off). The newest installed build of an engine is the one that runs.
- **Any GGUF from Hugging Face:** search, open a repo, see per quant whether it fits this machine, download (resumable,
  checked against the repo's SHA-256).
- GPU `auto` takes the card with the most free memory and keeps a `CUDA_VISIBLE_DEVICES` it was started with (D12, D13).
- Not in this version: pairing and per-client rights, the client that passes everything through to a host, systemd service
  control, MCP, Forge, Tuner, updates. They answer `501` instead of a 404 that looks like a bug; the Studio hides
  Experiments and Expert cache until the server has them.
