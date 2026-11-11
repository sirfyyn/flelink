# Changelog

## Unreleased

## 0.1.13 (2026-11-11)

- **No second agent.** An FLE Durable that an FLE Link left behind when it was killed is stopped before a new one
  starts; two agents on one queue would run events twice.
- **Events wait for the model** (FLE Durable 0.2.2): while no model is loaded, what the agenda, a webhook or another
  source hands the agent stays in the queue and goes on when a model runs again. Until now such an event was used up
  after three failed tries. The Agent page says so, and its "Waiting" card works again (it stayed empty since 0.1.8).
- **Updates by themselves**, if you turn it on in Settings: only while nobody is served. No model is loaded, or it has
  been quiet for 15 minutes and "Load this model when FLE Link starts" brings the model back.
- **Terminal interface**: the model list is the first screen (m shows the machine), models already on this machine
  have rows of their own, Ctrl+E in the chat goes back to choose another model.
- `flelink --help` lists the commands by importance and says where things are.
- Engine log: the two lines a DFlash or DSpark draft causes during memory fitting ("failed to measure the memory of
  the extra model") are no longer shown as errors, with a note why.
- Attaching to another server is refused while a start waits for GPU memory; before, it took the start's place.
- The System page counts the machine's logical processors also when FLE Link is bound to some of them.
- The first run measured again (README): 13 to 15 s without the download instead of 22 s with 0.1.3.
- Inside: `server.rs` is split into `server/agent_routes.rs`, `forge_routes.rs`, `update_routes.rs` and `proxy.rs`; the
  comparison tests hold on a machine whose GPUs are busy and with Python 3.14.

## 0.1.12 (2026-11-10)

- **Every page in seven languages.** French, Spanish, Russian, Chinese, Japanese and Korean covered about a quarter of
  the texts (the first look); they now cover all pages, about 870 more texts each: settings, engine, agent, lab, expert
  cache, tuner, Forge, remote access, updates. German got the same for the lab pages and Forge. What stays English:
  names, key names and messages that go to the model.

## 0.1.11 (2026-11-10)

A critical pass over the app, the command line, the terminal interface and the agent.

- **The desktop app updates itself.** On macOS and for the Linux AppImage, "Update now" downloads the app of the new
  release, checks it against the signed checksum list, puts it in the place of the running one and starts it. Until now
  every update meant a new download by hand. An app from a package (deb, rpm) or the Windows setup still points to the
  release page. `FLE_UPDATE_FORCE=1` offers the newest release again although it is the running version, to try the whole
  way.
- **The MCP hub reaches the agent again** (FLE Durable 0.2.1): since 0.1.8 the hub from Settings only reached the chat.
  FLE Durable 0.2.1 also introduces itself to the model as the FLE Durable agent.
- **The agent follows the model.** FLE Durable reads the model's name and context window once, at its start. An agent
  that came up before the model (start at login, or the app opened without a model) kept a 4,096-token window and
  1,024-token answers until someone restarted it by hand. FLE Link now starts it again when a model is ready that is not
  the one it was started with.
- **The agent chat says when it cannot answer.** Without a loaded model the status said "Ready", a message got no
  answer, now or later, and nothing said why. The chat now shows that no model is loaded and does not send; a turn the
  model server refused shows its reason. Approvals can be decided in the chat (the buttons never appeared).
- **`flelink update`**: the newest version from the terminal, checked against the signed release list like the button in
  the Studio; `--check` only says whether there is one. A Studio that runs as a service on macOS or Linux can now update
  itself (it is replaced in place), and one started by hand in a desktop terminal is no longer taken for a service.
- An update stops the agent before FLE Link starts again; before, the old agent kept running next to the new one.
- **`flelink connect`** prints the address of the running Studio or app also while no model is loaded (it fell back to
  port 8642, which is wrong when the app had to take another port).
- **Terminal interface**: the About box says why a model does not fit (disk, memory or GPU, with the numbers) and shows the
  whole note; an error no longer stays when another row is chosen.
- `flelink system` also lists a llama-server found elsewhere on the machine (PATH, Homebrew), the one `doctor` uses.
- **German**: about 480 more texts and 100 patterns, so chat, models, engine, settings, remote access, the agent chat and the
  Agent page are in German throughout (Lab, expert cache, tuner and Forge are still mostly English).
  `ci/i18n_gaps.py` lists texts without a translation.
- The Agent page is in the main navigation, not under Lab.
- Settings → About shows FLE Link's version (it showed the page API version, 0.4.1, and an empty "Python" row); Remote
  access in the desktop app shows where it listens (it said ":0").
- Messages say "FLE Link" where they still said "FLE Studio".

## 0.1.10 (2026-11-10)

- **The desktop app starts once.** A second start brings the open window to the front; before, each start ran its own server and agent on the same data.
- **No Sign in button for a tool server that needs none.** A web MCP server that answers without a sign-in (the FYOS hub in the home network) showed one, and pressing it ended in an error.

## 0.1.9 (2026-11-10)

- **The desktop app opens sign-ins and links in your browser.** In 0.1.8 Connect for Google and Hugging Face, and every link to another site, did nothing in the app.
- **The app finds Node when started from the Finder or at login.** macOS gives such an app a short PATH; FLE Link now also looks in /opt/homebrew/bin, /usr/local/bin, nvm, Volta, fnm and asdf, and hands that Node to npm and the agent, so installing FLE Durable works.

## 0.1.8 (2026-11-10)

- **The agent is FLE Durable 0.2.0,** the full runner instead of the small core: browser and web tools, sub-agents, tickets, calendar and events of its own, computer use through FLE Link, and calendars mirrored from your accounts. FLE Link offers the update on the Agent page.
- **No engine left behind.** Stopping `flelink run` (Ctrl+C, SIGTERM) while the model loads or warms up now ends the engine too; a start right after a stop waits for port 8080 instead of moving to another one.
- **Accounts with a click.** Settings has Connect buttons for Hugging Face, GitHub and Google; pasting a token still works. With Google connected, your main calendar appears on the Agent page and for the agent, and a calendar that cannot be read says so. iCloud calendars come in the same way, with an app-specific password from Apple.
- **Computer use, when you allow it.** The agent can see your screen and use your mouse and keyboard while you allow it on the Agent page, for 30 minutes at a time. Off at every start of FLE Link; not on Windows yet.
- **Tickets, calendar and events on the Agent page.** What you and the agent keep track of, in one card: add and change tickets, plan and remove calendar entries, read what came in. An agent without them, or one reached with a limited token, simply does not show the card.
- **Always on.** `flelink service on`, or the switch on the Agent page: FLE Link starts at login with the last model and the agent, and comes back if it stops. No administrator rights; launchd on macOS, systemd for the user on Linux, a logon task on Windows.
- **`flelink bench --task` points to `--share`.** A run without it now ends with a one-line hint, and the README explains the option.
- **A start right after a stop waits for the GPU memory.** The Studio does so within 30 s of stopping an engine, `flelink run` for recipes with the RAM expert store: until the used memory stops falling, at most 15 s.
- **DeepSeek recipe notes** give the FLE v1.2.5 numbers and name the version the recipes need.
- **New recipe `katy-27b-dflash`** for a 12 to 32 GB GPU: a dense 27B model with the DFlash2 draft, partly on the CPU below 20 GB.
- **`flelink run` is easier to call.** A part of a recipe id is enough when only one recipe has it (`flelink run katy`), several matches are named, and without a model it lists the recipes.
- **A recipe that does not fit says why:** the error names the GPU memory too and what the recipe was measured with.
- **`flelink doctor` on an Apple Silicon Mac** names the Metal GPU instead of failing the NVIDIA check.
- **`flelink --help` is shorter:** `demo` and `fetch-sidecars` still work but are no longer listed.
- **Recipes from outside can no longer load code.** `run.env` takes only the engine's own switches (`FLE_`, `GGML_`, `LLAMA_`, two `CUDA_` names), and a recipe from a URL or a file shows what it sets and asks before it starts (`--yes` skips that).
- **The engine no longer changes under a recipe.** `flelink run` and `ask` stay on the installed FLE unless the recipe needs a newer one and say when a newer release is there.
- **Updates and FLE engines are checked against a signature.** Releases carry a signature made with a key that is not on GitHub; FLE Link refuses an update without one and an FLE engine newer than v1.2.6 (docs/RELEASING.md).
- **A coding agent no longer pays for a model stuck in a loop.** Through the Studio's `/v1` (OpenAI chat and Anthropic format), an answer that repeats itself is ended with the normal "cut short" reason, by the same rules as the chat page. Tool calls are not read. `FLE_NO_LOOP_GUARD=1` turns it off.
- **`flelink code`: a coding agent for the folder you are in.** It starts FLE Durable for this folder, with the running model or a recipe you name, and works in the terminal: the answer as it comes, the tools it uses, questions before steps that cannot be undone. `AGENTS.md` or `CLAUDE.md` become its instructions, each folder keeps its conversation, and `/direct` talks to the model itself. FLE Durable is fetched and set up when it is missing (or `flelink code --setup`); it needs git and Node 22.19+.
- **The agent is one click away.** The Agent page explains FLE Durable, installs it and starts it. While it answers, the chat is your agent, with a switch to talk to the model directly. It can also be a FLE Durable that runs elsewhere, by address and token.
- **Automatic settings no longer shift the expert choice:** `FLE_MOE_CACHE_BIAS` is gone from what `flelink run` sets by itself.

## 0.1.7 (2026-11-06)

- **A model with some parts already on the disk is no longer turned down for the disk space of all of them.** The fit check counted the whole
  model once as soon as one part was missing (a recipe gave only a total size), so `flelink run` of Rin said "does not fit this machine" with
  147 GiB free although only the 6 MB of part 1 were missing. Recipes can now list the bytes per file (`[model.sizes]`); then only the missing
  parts count. The Rin recipe has them.
- **The terminal interface lists the recipes too.** Besides the catalog models, the model list of `flelink` now shows the built-in recipes that
  are not in the catalog (DeepSeek V4.1 Flash with and without the draft model, Rin) with the same verdict (fits, tight, too big) and
  `[downloaded]` mark; the fit check runs before anything is downloaded, and the search row stays last.
- **The GPU is read only while a page is open.** An answer through the proxy (Claude Code, an agent, a script, `flelink bench`) used to wake the
  whole live sampler, including the `nvidia-smi` stream, which took about 16 % of a core with spikes to 67 % from a 16-thread engine: the same
  DeepSeek V4.1 Flash recipe measured 47.1 against 49.7 tokens per second directly (5 %, same answer, same accepted drafts). Now an answer only
  keeps the engine's `/metrics` and `/slots` going; the GPU stream starts when a Studio page asks for the live data and ends when it stops.
- **Rin, the 2-bit DeepSeek V4.1 Flash, is a recipe.** "Rin - Deepseek V4.1 Flash 1017 + DSpark" starts the split model (a hot half
  resident on the GPU, a cold half served from RAM) with FLE's DSpark draft: about 53 tokens per second on one 32 GB GPU with 120 GB
  RAM, measured with FLE v1.2.6. It needs FLE v1.2.6 or newer and sets no GPU cache budget, because the hot half is already on the
  card and a cache on top would run out of memory. Leave a few seconds between two starts: the GPU memory has to be free first. A recipe can now name a usage profile
  (`run.profile_file`) that is one of the model repo's files: it is fetched with the model and copied to the recipe's profile the
  first time, so Rin's store starts warm (without the profile Rin ran at 30.9 instead of 53 tokens per second).
- **A split model (Rin) keeps the RAM expert store.** The plan did not count the `_exps_hot` and `_exps_cold` tensors of the split
  layout as experts, took the model for small and pinned all of it in RAM (`--load-mode none`); the engine then logged "store off" and
  never reached its ready line. Both halves now count as experts.
- **FLE's own status is available at `GET /api/engine/fle-status`.** It passes through `/fle/status` (schema 1) of FLE v1.2.6 and
  newer: what the engine decided at start (usage profile, GPU cache, RAM store, warnings) and its live counters. Another engine, or an
  older FLE, answers `{"available": false}`. The engine page of the Studio shows it as a card "FLE engine": decode and prefill speed,
  draft acceptance, usage profile, GPU cache, RAM store, CUDA version and the engine's warnings; with another engine the card stays hidden.
- **A start that fails for lack of memory says so.** FLE v1.2.6 reports `ram_too_small` (how much the RAM expert store needs and how
  much this process may use); the engine log of a failed start now names both numbers and what to change.
- **The live sampler no longer slows the engine down.** Through FLE Link, FLE v1.2.5 decoded 35.6 tokens per second where the
  same engine started by hand did 37.6. The first answer through the proxy wakes the live sampler, and it started a new
  `nvidia-smi` process every two seconds; an engine that keeps all its CPU threads busy loses a core for a moment with every
  started process, and the other threads wait for it. Now one long-lived `nvidia-smi -l 3` serves the GPU readings (it ends when
  nobody watches), and while a response runs the engine is asked for `/metrics` every second tick and for `/slots` every sixth.
  With this change, three runs through FLE Link gave 37.2, 37.4 and 35.3 tokens per second (median 37.2); one run in three is
  still slow, the cause of that is being looked at.
- **The repeat check also catches a loop whose sentences differ only in their numbers.** The first measurement of ISTA IQ2_XXS
  found no exact loop in 23 runs, but one greedy answer thinking a Pagoda in three.js fell into "Need maybe add
  `rocks.push(createRockCluster(1, -21.0, 1, 0.00005));` etc." (48 of the last 50 sentences, only the numbers changing) and used
  all 16000 tokens. Chat now also turns every number into # and stops when one sentence shape fills at least 40 % of the recent
  sentences (14 times or more). In the answers that did not loop, the most frequent shape came 5 times in 44 at most.
  `tests/loop-guard.test.mjs` runs it on the real answer tails.
- **A failed start says why, with a number:** FLE v1.2.5 and later print `[fle-status-error]` with a code before they stop.
  FLE Link shows it in plain words, for example "The GPU cache budget is too large for this card and context: at most 14200 MiB
  fit" with the setting to change, instead of "The engine exited (code 1)".

## 0.1.6 (2026-11-04)

- **A model that repeats itself is stopped:** a 2-bit model sometimes writes the same passage over and over (reports of 100k
  tokens). Chat now notices it in the text as it arrives (a piece of 12 to 480 characters five times in a row, at least 400
  characters) and stops the answer with a note. It reads text only, so no engine setting changes and an answer that does
  not loop is never touched. Settings → Appearance → Repeat check turns it off (kept in this browser).
- **DeepSeek recipes for FLE v1.2.5** (the engine is out: 49.8 / 50.8 / 50.6 tok/s on three runs, the same text each time):
  a fixed GPU cache (no uploads while decoding), a read-only usage profile, so a start gives the same answer, and for the
  DSpark recipe five draft tokens, a 15.5 GB expert cache and `FLE_DRAFT_UBATCH=512`. The recipes ask for engine v1.2.5, which
  FLE Link installs by itself on the first start. The speed through FLE Link is measured next; until then the numbers on
  flelink.run stay those of v1.2.4.
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
