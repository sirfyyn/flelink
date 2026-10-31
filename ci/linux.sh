#!/usr/bin/env bash
# The Linux checks (fmt, clippy, tests with the Python comparison) in a throw-away container. Same steps as .github/workflows/ci.yml.
# On titan, builds run in the measurement frame: cores 16-31, never 0-15 (Rhea). FLE_CI_CPUSET passes that on to the container.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cpuset="${FLE_CI_CPUSET:-16-31}"
image="${FLE_CI_IMAGE:-rust:1-bookworm}"
cache="${FLE_CI_CACHE:-$HOME/.cache/flelink-ci}"
mkdir -p "$cache/cargo" "$cache/target" "$cache/spec"

docker run --rm --cpuset-cpus="$cpuset" --memory=24g \
  -v "$here":/src:ro -v "$cache/cargo":/usr/local/cargo/registry -v "$cache/target":/target -v "$cache/spec":/spec \
  -e CARGO_TARGET_DIR=/target -e CARGO_TERM_COLOR=never \
  "$image" bash -euo pipefail -c '
    rustup component add clippy rustfmt >/dev/null
    apt-get update -qq && apt-get install -y -qq python3 git >/dev/null
    # the Python specifications at their pinned commits (public repositories)
    fetch() { # name url sha
      if [ ! -d /spec/$1/.git ]; then git clone -q "$2" /spec/$1; fi
      git -C /spec/$1 fetch -q origin "$3" 2>/dev/null || git -C /spec/$1 fetch -q origin
      git -C /spec/$1 checkout -q "$3"
    }
    fetch studio https://github.com/sirfyyn/fle-studio.git "$(cat /src/STUDIO_PIN)"
    fetch engine https://github.com/sirfyyn/FastLocalExperts.git "$(cat /src/ENGINE_PIN)"
    rm -rf /work && mkdir /work && cp -a /src/. /work/ && cd /work
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings
    FLE_STUDIO_SRC=/spec/studio/src FLE_ENGINE_SRC=/spec/engine FLE_STUDIO_PYTHON=python3 cargo test --all
  '
