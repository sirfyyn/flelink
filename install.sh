#!/bin/sh
# FLE Link installer for macOS and Linux (POSIX sh).
#
#   curl -fsSL https://flelink.run/install.sh | sh
#
# What it does: downloads ONE file (the `flelink` program for this machine), checks its SHA-256, puts it in ~/.local/bin and
# tells you how to start. No Python, no uv, no git, no sudo; it writes only below your home directory.
#
# Options:  --yes   --bin-dir DIR
# Environment: FLE_BIN_DIR, FLE_REPO (default sirfyyn/flelink), FLE_URL_BASE (default: the latest release)
set -eu

REPO="${FLE_REPO:-sirfyyn/flelink}"
BASE="${FLE_URL_BASE:-}"
BIN_DIR="${FLE_BIN_DIR:-$HOME/.local/bin}"
YES=0

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --yes|-y) YES=1 ;;
    --bin-dir) shift; BIN_DIR="${1:?--bin-dir needs a folder}" ;;
    -h|--help) sed -n '2,12p' "$0" 2>/dev/null || true; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
  shift
done

os="$(uname -s)"; arch="$(uname -m)"
case "$os:$arch" in
  Darwin:arm64)           target="aarch64-apple-darwin" ;;
  Darwin:x86_64)          target="x86_64-apple-darwin" ;;
  Linux:x86_64|Linux:amd64) target="x86_64-unknown-linux-gnu" ;;
  *) die "no build for $os $arch yet (macOS arm64/x64 and Linux x64 are available; Windows: install.ps1)" ;;
esac

have curl || die "curl is needed"
if have sha256sum; then sha() { sha256sum "$1" | cut -d' ' -f1; }
elif have shasum;  then sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
else die "sha256sum or shasum is needed to check the download"; fi

if [ -z "$BASE" ]; then
  tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
  [ -n "$tag" ] || die "no release found in $REPO yet"
  BASE="https://github.com/$REPO/releases/download/$tag"
fi
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT INT TERM
file="flelink-$target.tar.gz"
say "Downloading $file"
curl -fSL --progress-bar -o "$tmp/$file" "$BASE/$file" || die "download failed: $BASE/$file"
curl -fsSL -o "$tmp/$file.sha256" "$BASE/$file.sha256" || die "checksum file missing: $BASE/$file.sha256"
want="$(cut -d' ' -f1 < "$tmp/$file.sha256")"
got="$(sha "$tmp/$file")"
[ "$want" = "$got" ] || die "checksum mismatch (wanted $want, got $got); nothing was installed"
say "Checksum ok"

mkdir -p "$BIN_DIR"
tar -xzf "$tmp/$file" -C "$tmp"
[ -f "$tmp/flelink" ] || die "the archive has no 'flelink' program"
install -m 0755 "$tmp/flelink" "$BIN_DIR/flelink"
say "Installed $BIN_DIR/flelink"
# the short name `fle`: a link to the same file, unless another program of that name is already there
if [ ! -e "$BIN_DIR/fle" ] || [ -L "$BIN_DIR/fle" ]; then ln -sf flelink "$BIN_DIR/fle"; say "Installed $BIN_DIR/fle (short name)"; fi

case ":$PATH:" in *":$BIN_DIR:"*) ;; *) printf '\nAdd this to your shell profile so you can type `flelink`:\n  export PATH="%s:$PATH"\n' "$BIN_DIR" ;; esac
printf '\nDone. Start with:\n'
printf '  flelink          the terminal interface (first run sets everything up)\n'
printf '  flelink studio   the browser interface\n'
printf '  flelink system   what this machine can do\n'
