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
- Not in this version: pairing and per-client rights, the client that passes everything through to a host, systemd service
  control, MCP, Forge, Tuner, Hugging Face search, updates. They answer `501` instead of a 404 that looks like a bug; the
  Studio hides Experiments, Expert cache and the Hugging Face search until the server has them.
