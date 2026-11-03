#!/bin/sh
# FLE Link installer for macOS and Linux (POSIX sh).
#
#   curl -fsSL https://flelink.run/install.sh | sh
#
# What it does: downloads ONE file (the `flelink` program for this machine), checks its SHA-256, puts it in ~/.local/bin and
# tells you how to start. On a machine with a screen it offers the desktop app too (macOS: ~/Applications/FLE Link.app,
# Linux: an AppImage with a menu entry), also checked against its SHA-256. No Python, no uv, no git, no sudo; it writes
# only below your home directory.
#
# Options:  --yes   --bin-dir DIR   --app (with the desktop app, no question)   --no-app
# Environment: FLE_BIN_DIR, FLE_REPO (default sirfyyn/flelink), FLE_URL_BASE (default: the latest release)
set -eu

REPO="${FLE_REPO:-sirfyyn/flelink}"
BASE="${FLE_URL_BASE:-}"
BIN_DIR="${FLE_BIN_DIR:-$HOME/.local/bin}"
YES=0
APP=ask

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --yes|-y) YES=1 ;;
    --app) APP=yes ;;
    --no-app) APP=no ;;
    --bin-dir) shift; BIN_DIR="${1:?--bin-dir needs a folder}" ;;
    -h|--help) sed -n '2,14p' "$0" 2>/dev/null || true; exit 0 ;;
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
  # the newest release from the redirect of /releases/latest (no API: its 60 requests an hour per address run out on
  # shared networks), the API only when that gives nothing
  tag="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" 2>/dev/null | sed -n 's|.*/releases/tag/\([^/?#]*\).*|\1|p')"
  [ -n "$tag" ] || tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
  [ -n "$tag" ] || die "no release found in $REPO yet"
  BASE="https://github.com/$REPO/releases/download/$tag"
fi
version="$(printf '%s' "$BASE" | sed -n 's|.*/v\([0-9][^/]*\)$|\1|p')"
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

# --- the desktop app: the same Studio in a window -------------------------------------------------------------------------
if [ "$APP" = ask ]; then
  APP=no
  screen=0
  if [ "$os" = Darwin ] || [ -n "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then screen=1; fi
  # the script itself comes through a pipe, so the question goes to the terminal
  if [ "$screen" = 1 ] && [ "$YES" = 0 ] && (exec </dev/tty) 2>/dev/null; then
    printf '\nAlso install the desktop app (the Studio in its own window)? [y/N] '
    read -r answer </dev/tty || answer=""
    case "$answer" in y|Y|yes|j|J|ja) APP=yes ;; esac
  fi
fi
if [ "$APP" = yes ]; then
  case "$os:$arch" in
    Darwin:arm64)  app="FLE.Link_aarch64.app.tar.gz" ;;
    Darwin:x86_64) app="FLE.Link_x64.app.tar.gz" ;;
    Linux:*)       [ -n "$version" ] || die "cannot tell the release version for the app; use --no-app"; app="FLE.Link_${version}_amd64.AppImage" ;;
  esac
  say "Downloading the desktop app ($app)"
  curl -fsSL -o "$tmp/SHA256SUMS" "$BASE/SHA256SUMS" || die "checksum list missing: $BASE/SHA256SUMS"
  curl -fSL --progress-bar -o "$tmp/$app" "$BASE/$app" || die "download failed: $BASE/$app"
  want="$(grep " $app\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)"
  got="$(sha "$tmp/$app")"
  [ -n "$want" ] && [ "$want" = "$got" ] || die "checksum mismatch for $app; the app was not installed (the program is)"
  say "Checksum ok"
  if [ "$os" = Darwin ]; then
    mkdir -p "$HOME/Applications"
    rm -rf "$HOME/Applications/FLE Link.app"
    tar -xzf "$tmp/$app" -C "$HOME/Applications"
    say "Installed $HOME/Applications/FLE Link.app"
  else
    mkdir -p "$HOME/.local/share/applications"
    install -m 0755 "$tmp/$app" "$BIN_DIR/flelink-app"
    cat > "$HOME/.local/share/applications/flelink.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=FLE Link
Comment=Local models, wired up in one command
Exec=$BIN_DIR/flelink-app
Terminal=false
Categories=Development;Utility;
DESKTOP
    say "Installed $BIN_DIR/flelink-app and a menu entry (the AppImage needs libfuse2 on some distributions)"
  fi
fi

case ":$PATH:" in *":$BIN_DIR:"*) ;; *) printf '\nAdd this to your shell profile so you can type `flelink`:\n  export PATH="%s:$PATH"\n' "$BIN_DIR" ;; esac
printf '\nDone. Start with:\n'
printf '  flelink          the terminal interface (first run sets everything up)\n'
printf '  flelink studio   the browser interface\n'
printf '  flelink system   what this machine can do\n'
if [ "$APP" = yes ]; then printf '  the app          FLE Link in Applications / your app menu\n'; fi
