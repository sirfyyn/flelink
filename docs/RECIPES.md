# Recipes

A recipe is one TOML file that says which model to run, which engine it wants, how to start it and how to chat with it.
It is what you share when you found settings that work: `flelink run https://…/my.recipe.toml` reproduces them on
another machine. A recipe never runs code; it only names files to download and values for the engine.

## Where FLE Link finds them

Later places replace earlier ones with the same `id`:

1. built in (the catalog: `flelink recipes` lists them)
2. `<data>/recipes/*.toml` (your own)
3. `<data>/plugins/<plugin>/recipes/*.toml` (brought by a plugin)
4. a file or URL on the command line: `flelink run my.recipe.toml`, `flelink run https://…`

`<data>` is the FLE Link data folder (`flelink system` shows it).

## Format

```toml
id = "qwen38-fle-long"                # letters, digits, . _ - ; also the name for `flelink run`
title = "Qwen3.8-Flash-Next FLE, 64k context"
note = "For long documents on a 32 GB card."

[model]
repo = "sirfyyn/Qwen3.8-Flash-Next-FLE-GGUF"       # Hugging Face owner/name
revision = "main"                                   # optional
files = [                                           # every part; the first is passed to the engine
  "IQ3_M/Qwen3.8-Flash-Next-FLE-IQ3_M-00001-of-00002.gguf",
  "IQ3_M/Qwen3.8-Flash-Next-FLE-IQ3_M-00002-of-00002.gguf",
]
size = 88872241728                                  # bytes of all files: the fit check runs before any download
[model.sha256]                                      # optional, per file
"IQ3_M/Qwen3.8-Flash-Next-FLE-IQ3_M-00001-of-00002.gguf" = "…"

[engine]
prefer = ["fle"]       # engine ids in order; empty: the best for this machine
strict = true          # only these may run it (the model needs FLE's expert offloading)

[run]
context = 65536                            # tokens; default: sized from free memory
args = ["--cache-reuse", "256"]            # appended after the automatic arguments; the last value of a flag wins
env = { FLE_MOE_CACHE_BIAS = "0.002" }     # overrides the automatic environment

[requires]             # shown to people; the fit check uses model.size
ram_gib = 100
vram_gib = 24

[chat]                 # defaults for the chat in the Studio
system = "You are a careful assistant."
thinking = false
tools = false
temperature = 0.6
top_p = 0.95
```

Checks when a recipe is read: the id and the repo are plain names, every file is a `.gguf` path inside the repo (no `..`,
no absolute path), environment names are letters, digits and `_`, the context is between 512 and 1,048,576, and unknown
keys are refused (a typo does not silently do nothing). A file that fails is skipped with the reason; `flelink recipes`
prints it.

## Commands

```bash
flelink recipes                 # every recipe, whether it fits this machine, where it comes from
flelink recipe ID               # print it as TOML (to share, or to save a changed copy in <data>/recipes)
flelink run ID                  # download what is missing, start, serve on /v1
flelink run my.recipe.toml      # the same from a file
flelink ask ID "question"       # one answer, then everything stops
```

## Plugins

A plugin is a folder in `<data>/plugins/<id>/` with recipes and, later, engine adapters and a Studio page. Plugins are
files, not code that runs inside FLE Link; see docs/ENGINE-ADAPTER.md for the adapter part.
