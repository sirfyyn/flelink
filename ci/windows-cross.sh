#!/usr/bin/env bash
# The Windows program, cross-compiled on Linux with MinGW in a throw-away container: does it build for Windows at all.
# Like ci/linux.sh the source goes in with `docker cp` (the runner's workspace is not a path the Docker daemon can see).
# The release builds stay MSVC builds on Windows; this only catches code that does not compile there.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
image="${FLE_CI_IMAGE:-rust:1-bookworm}"

cat > /tmp/flelink-win-inside.sh <<'INSIDE'
set -euo pipefail
apt-get update -qq && apt-get install -y -qq gcc-mingw-w64-x86-64 >/dev/null
rustup target add x86_64-pc-windows-gnu >/dev/null
cd /src
cargo build --release --target x86_64-pc-windows-gnu -p flelink
ls -l /target/x86_64-pc-windows-gnu/release/flelink.exe
INSIDE

c="$(docker create --memory=8g \
  -v flelink-ci-cargo:/usr/local/cargo/registry -v flelink-ci-target-win:/target \
  -e CARGO_TARGET_DIR=/target -e CARGO_TERM_COLOR=never -e CARGO_INCREMENTAL=0 \
  "$image" bash /inside.sh)"
trap 'docker rm -f "$c" >/dev/null 2>&1 || true' EXIT
docker cp "$here/." "$c:/src"
docker cp /tmp/flelink-win-inside.sh "$c:/inside.sh"
docker start -a "$c"
