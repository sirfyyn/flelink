# How FLE Link works

What happens between `flelink run <model>` and the first answer, with the rules and numbers the code uses. Every step can
be read in the terminal (`flelink system`, `flelink models`, `flelink plan`, `flelink doctor`) and on the Studio's Engine page.

## 1. Read the machine

`flelink system` collects CPU (cores, AVX2), RAM, the GPUs with the memory each may use (on a Mac: the share of unified
memory the GPU may take), and the free disk in the models folder. From that it lists the engine builds that run here, best
first: CUDA on NVIDIA, Metal on Apple Silicon, Vulkan for other GPUs, CPU (AVX2 or not) without one. The FLE engine is offered
where it has a build; plain llama.cpp everywhere.

## 2. Decide whether a model fits, before downloading

A model needs about **1.25 × its size** (weights plus KV cache and compute buffers). That is compared with:

- the memory the GPU may use (or half the RAM, whichever is larger) for ordinary models;
- the RAM for FLE models whose experts live in RAM (the FLE engine keeps only the busy experts on the card).

Up to 80 % of that memory: **fits**. Up to 100 %: **tight** (it runs, slower). Above: **too big**. The disk must hold the
model plus 512 MB. A recipe can say what it was measured with (`[requires]`); that comes before the rule of thumb, and up to a
fifth less is still "tight". Only files that are really missing count against the disk.

## 3. Use what is already there

Before a download FLE Link looks for the same file in the models folder, the folders in `FLE_MODEL_DIRS`, the Hugging Face
cache, LM Studio's folders and Ollama's blobs. A file counts as the same when its name matches and, where the recipe gives a
SHA-256, its contents match too; that check reads the file once (with a progress bar) and is remembered by path, size and
modification time. A found file is linked, never moved or changed.

## 4. Download what is missing

Downloads go to a `.part` file and continue where they stopped, also after a restart. A connection that breaks or stays
silent for 60 seconds is picked up again by itself; only five breaks in a row without a single new byte stop it ("run again to
resume"). Every file with a known SHA-256 is checked; a damaged one is moved aside as `.bad`. Engines come from GitHub
releases and are checked against the checksums the release publishes.

## 5. Choose the start settings

The plan is one llama-server command, shown in full:

- **Context:** the memory left after the model (size × 1.15), at about 150 MB per 1,000 tokens, between 2,048 and 32,768
  tokens, rounded down to a multiple of 1,024.
- **GPU:** all layers on the GPU (`-ngl 99`) when there is one.
- **FLE engine:** its policy variables (`FLE_MOE_*`: an expert cache sized to the card, admission and eviction, profile
  saving) and `-fit on`.
- **Recipes** replace any of this with values that were measured (`docs/RECIPES.md`); profiles keep a command of your own.

"Best for my machine" (the tuner) then tries threads, expert cache or `--n-cpu-moe`, batch size and context one at a time,
measures each several times, and keeps a change only when it is faster with a 95 % interval that excludes zero. The winner
is saved as a profile, so the next start is the same.

## 6. Start and watch

Before an engine starts, engines left behind by a program that died (a crash, `kill -9`) are found through
`<data>/run/engine-<pid>` and stopped, so a model is never loaded twice. The engine log is shown with its loading phases in
plain words. `flelink run` stops the engine politely on Ctrl+C or SIGTERM (ten seconds, then SIGKILL); quitting the app or
the Studio does the same.

## 7. Serve

The engine answers three API shapes on one address: OpenAI Chat Completions and Responses (`/v1/chat/completions`,
`/v1/responses`) and Anthropic Messages (`/v1/messages`). `flelink run` serves on port 8080, the Studio and the app pass
`/v1` through on 8642 and count every answer (tokens, speed, which program asked). `flelink connect` prints the setting for
Claude Code, Codex, OpenCode and pi; `flelink mcp` lets an agent run all of the above itself. Other machines reach the Studio
in host mode with an access key, or through pairing (Remote access).

## Where things live

One data folder for the terminal, the Studio and the app (macOS `~/Library/Application Support/FLE Link`, Linux
`~/.local/share/flelink`, Windows `%LOCALAPPDATA%\FLE Link`; a folder of FLE Studio is taken over; `FLE_HOME` moves it):

| Path | What |
|---|---|
| `models/` | model files, one folder per repository |
| `engines/` | installed engine builds |
| `recipes/`, `plugins/` | your recipes and plugins |
| `profiles.json` | launch profiles |
| `models/profiles/` | expert profiles of the FLE engine (which experts are busy) |
| `chats/`, `bench/`, `tuning/` | chats, speed tests, tuner results |
| `cache/checked-files.json` | SHA-256 of files found elsewhere |
| `run/` | which program started which engine |
| `studio.json` | where a running Studio or app is (mode 0600) |
