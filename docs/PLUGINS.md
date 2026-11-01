# Plugins

A plugin is a folder in `<data>/plugins/<id>/`. It brings engines and recipes. It is data, not code that runs inside FLE
Link: `plugin.toml` says where an engine's server comes from and how it is started, `recipes/` holds recipes
(docs/RECIPES.md). Copy the folder to share it.

```text
<data>/plugins/ik/
  plugin.toml
  recipes/
    qwen-ik.toml          # engine.prefer = ["ik-llama"]
```

## plugin.toml

```toml
id = "ik"                       # must match the folder name
title = "ik_llama.cpp"
version = "0.1.0"
description = "A llama.cpp fork with faster MoE kernels."

[[engine]]
id = "ik-llama"                 # what recipes name in engine.prefer; not fle, upstream or custom
label = "ik_llama.cpp"
args = ["-fmoe"]                # appended to the automatic arguments
env = { }                       # environment for the server

[engine.release]                # a GitHub release FLE Link downloads (SHA-256 checked) and unpacks
repo = "owner/ik_llama.cpp"
tag = ""                        # empty: the latest release
binary = "llama-server"         # the server inside the archive
[engine.release.assets]         # per system: words the archive name must contain
"linux-x64" = ["ubuntu", "x64", "cuda"]
"windows-x64" = ["win", "cuda", "x64"]
"macos-arm64" = ["macos", "arm64"]
```

Instead of `[engine.release]` an engine can name a server you built: `path = "/opt/ik/build/bin/llama-server"`
(absolute). Exactly one of the two.

## Rules

- **llama-server compatible.** An engine from a plugin must take `-m`, `-c`, `--host`, `--port`, answer `/health` and
  serve `/v1`. That covers llama.cpp forks. An engine with its own front end needs a built-in adapter
  (docs/ENGINE-ADAPTER.md).
- **Its own runtime.** The archive must contain what the server needs (for CUDA: the runtime libraries), or the system must
  have it. FLE Link adds the CUDA runtime only for its built-in engines.
- **Checked when read.** Plain ids, absolute paths, `owner/name` repos, plain environment names, unknown keys refused. A
  plugin that fails is skipped with the reason (`flelink plugins` prints it); the others keep working. Two plugins cannot
  define the same engine id: the first (by folder name) wins, the second is reported.
- **No code.** A plugin cannot run anything inside FLE Link. The server it names runs as its own process, like the
  built-in engines.

## Commands

```bash
flelink plugins           # plugins, their engines and recipes, and what was skipped
flelink recipes           # recipes from plugins show the plugin as their source
flelink run RECIPE        # a recipe that prefers a plugin engine installs and starts it
```

## Not yet

A Studio page per plugin and routes under `/api/p/<id>/` (the Python Studio had them as Python code) need a sandboxed
form first; until then plugins are engines and recipes.
