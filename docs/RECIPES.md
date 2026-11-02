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

[model.save_as]                                     # optional: store a file under another name (same folder)
"Other-Name.gguf" = "Model-00003-of-00003.gguf"     # for a last part the publisher uploaded under its own name

[draft]                                # optional: a draft model for speculative decoding, downloaded with the model
repo = "owner/drafter-GGUF"            # empty: the model's repo
file = "drafter.gguf"
sha256 = "…"                           # optional
size = 5216519072                      # optional, shown before the download

[engine]
prefer = ["fle"]       # engine ids in order; empty: the best for this machine
strict = true          # only these may run it (the model needs FLE's expert offloading)
min_version = "v1.2.3" # the oldest engine release the settings work with; the Studio refuses an older one with the reason


[run]
context = 65536                            # tokens; default: sized from free memory
args = ["--cache-reuse", "256"]            # appended after the automatic arguments; the last value of a flag wins
                                           # {draft}: path of the draft model; {physical_cores}: cores without SMT
                                           # in args and env also {profile}: <models>/profiles/<id>.prof (a file the
                                           # engine keeps between starts), {hot_mlock_gb}: memory minus 60 GiB, at most 75
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

Built-in recipes with a draft model or engine flags of their own are TOML files in `crates/flelink-core/recipes/`
(`deepseek-v41-ista`: DeepSeek-V4.1-Flash with the hot experts locked in RAM; `deepseek-v41-ista-dspark`: the same model
with FLE's DSpark draft model, for the FLE engine v1.2.1). An engine that locks memory (`FLE_MOE_HOT_MLOCK_GB`) gets the
system's limit for locked memory; when that is too low, the engine log says how to allow it.

Checks when a recipe is read: the id and the repo are plain names, every file is a `.gguf` path inside the repo (no `..`,
no absolute path), environment names are letters, digits and `_`, the context is between 512 and 1,048,576, and unknown
keys are refused (a typo does not silently do nothing). A file that fails is skipped with the reason; `flelink recipes`
prints it.

## Models you already have

Before a recipe downloads a file, FLE Link looks for it on this machine: the models folder, the folders in
`FLE_MODEL_DIRS` (separated like PATH, e.g. `FLE_MODEL_DIRS=/mnt/models:/data/gguf`), the Hugging Face cache, LM Studio's
folders and Ollama's blobs. A file with the same name is taken when its SHA-256 matches (from the recipe, or in the Studio
from Hugging Face's file list); it is read once for that and remembered. Without a checksum the name (and the size, when
known) decides. The copy is linked into the models folder, never moved or changed.

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
