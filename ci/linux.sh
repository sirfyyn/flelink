#!/usr/bin/env bash
# The Linux checks (fmt, clippy, tests with the Python comparison) in a throw-away container. Same steps as .github/workflows/ci.yml.
# On titan the runner pod keeps builds inside the measurement frame by itself (job containers on cores 24-31, dockerd limited
# to 6 CPUs and 10 GiB), and the daemon in it only sees a few cores: so no --cpuset here. FLE_CI_CPUSET sets one if wanted.
# The source goes in with `docker cp`, not a bind mount: the runner's workspace is not a path the Docker daemon can see.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cpuset="${FLE_CI_CPUSET:-}"
image="${FLE_CI_IMAGE:-rust:1-bookworm}"

cat > /tmp/flelink-ci-inside.sh <<'INSIDE'
set -euo pipefail
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
cd /src
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
FLE_STUDIO_SRC=/spec/studio/src FLE_ENGINE_SRC=/spec/engine FLE_STUDIO_PYTHON=python3 cargo test --all
INSIDE

c="$(docker create ${cpuset:+--cpuset-cpus="$cpuset"} --memory=8g \
  -v flelink-ci-cargo:/usr/local/cargo/registry -v flelink-ci-target:/target -v flelink-ci-spec:/spec \
  -e CARGO_TARGET_DIR=/target -e CARGO_TERM_COLOR=never \
  "$image" bash /inside.sh)"
trap 'docker rm -f "$c" >/dev/null 2>&1 || true' EXIT
docker cp "$here/." "$c:/src"
docker cp /tmp/flelink-ci-inside.sh "$c:/inside.sh"
docker start -a "$c"
