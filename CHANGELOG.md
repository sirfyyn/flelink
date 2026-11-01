# Changelog

## 0.1.0 (not yet released)

First version of FLE Link: the layer between an inference engine and the tools that use it.

- **One file, one command.** `flelink` (short name `fle`) looks at the machine, installs the engine that fits (FLE or plain
  llama.cpp, checked against SHA-256), downloads a model that fits (resumable, checked), chooses the start settings and opens a
  chat. Commands: `system`, `doctor`, `plan`, `models`, `run`, `ask`, `serve`, `engines`, `inspect`, `verify`, `config`,
  `fetch-sidecars`, `report`, `bench`, `studio`.
- **Browser interface** (`flelink studio`): the FLE Studio 0.4.1 page under the FLE Link name, served by a Rust server that answers the same `/api` and
  `/v1` as the Python original (compared request by request in the tests; deliberate differences in `DEVIATIONS.md`).
- **Desktop app** (Tauri 2, `app/`): the same server inside the app on a free loopback port; closing the window stops the
  server and the model. On Windows the engine ends with the app, also after a crash.
- **Host mode:** `flelink studio --host 0.0.0.0` for another machine to look at: access key, https with a certificate of its
  own (fingerprint printed), `--no-tls` for a trusted network.
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
- **Recipes and plugins:** a recipe (TOML) is a model with its engine, start settings and chat defaults, shared as a file or
  URL (`flelink recipes`, `flelink run my.recipe.toml`, docs/RECIPES.md). A plugin is a folder that brings engines (from a
  GitHub release or your own build) and recipes (`flelink plugins`, docs/PLUGINS.md); it is data, not code.
- **The FLE engine updates itself:** once a day FLE Link asks for a newer FLE release and installs it next to the old one
  (`FLE_NO_UPDATE=1` turns it off). The newest installed build of an engine is the one that runs.
- **Any GGUF from Hugging Face:** search, open a repo, see per quant whether it fits this machine, download (resumable,
  checked against the repo's SHA-256).
- GPU `auto` takes the card with the most free memory and keeps a `CUDA_VISIBLE_DEVICES` it was started with (D12, D13).
- Not in this version: pairing and per-client rights, the client that passes everything through to a host, systemd service
  control, MCP, Forge, Tuner, updates. They answer `501` instead of a 404 that looks like a bug; the Studio hides
  Experiments and Expert cache until the server has them.
