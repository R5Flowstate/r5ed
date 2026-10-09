#!/usr/bin/env bash
# Builds the editor and drives every frontend feature in a real browser.
#
#   scripts/smoke.sh [--no-build] [--keep] [-- <smoke.mjs args>]
#
# The host needs a game install and the asset tool to start. Neither is needed
# to exercise the editor: an empty game root has no paks, so the asset tool is
# never run, and the stub below only has to exist. The compiler CLI is the real
# one, so the build jobs run for real.
#
# Headless Chromium loses the WebGPU device on the first getCurrentTexture(),
# so the browser runs headed under Xvfb with Vulkan on SwiftShader.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD=1
KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) BUILD=0 ;;
    --keep) KEEP=1 ;;
    --) shift; break ;;
    *) echo "smoke: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done

WORK="${R5ED_SMOKE_DIR:-$REPO/target/smoke}"
PORT="${R5ED_SMOKE_PORT:-7797}"

if [ "$BUILD" = 1 ]; then
  rustup target add wasm32-unknown-unknown >/dev/null
  if ! command -v wasm-bindgen >/dev/null; then
    cargo install wasm-bindgen-cli --version 0.2.100 --locked
  fi
  python3 "$REPO/scripts/build_wasm.py"
  npm --prefix "$REPO/web" install --no-audit --no-fund
  npm --prefix "$REPO/web" run build
  # A debug host reads web/dist from disk on every request, so a later
  # `npm run build` is picked up without rebuilding the host.
  cargo build -p r5ed-host --manifest-path "$REPO/Cargo.toml"
  cargo build --release -p r5ed-compile --manifest-path "$REPO/Cargo.toml"
fi

rm -rf "$WORK"
mkdir -p "$WORK/home/.config/r5ed" "$WORK/game" "$WORK/projects" "$WORK/cache"
printf '#!/bin/sh\nexit 0\n' > "$WORK/rsx"
chmod +x "$WORK/rsx"
cat > "$WORK/home/.config/r5ed/host.json" <<EOF
{
  "game_root": "$WORK/game",
  "projects_root": "$WORK/projects",
  "cache_root": "$WORK/cache",
  "port": $PORT,
  "tools": { "rsx": "$WORK/rsx", "compile": "$REPO/target/release/r5ed-compile" }
}
EOF

cd "$REPO"
HOME="$WORK/home" APPDATA="" "$REPO/target/debug/r5ed-host" --no-browser > "$WORK/host.log" 2>&1 &
HOST_PID=$!
cleanup() { if [ "$KEEP" = 0 ]; then kill "$HOST_PID" 2>/dev/null || true; fi; }
trap cleanup EXIT

for _ in $(seq 1 100); do
  grep -q '^r5ed http' "$WORK/host.log" 2>/dev/null && break
  sleep 0.1
done
URL="$(sed -n 's/^r5ed \(http[^ ]*\)$/\1/p' "$WORK/host.log" | head -n1)"
if [ -z "$URL" ]; then
  cat "$WORK/host.log" >&2
  echo "smoke: the host did not start" >&2
  exit 1
fi
echo "smoke: host at $URL"

export R5ED_URL="$URL" R5ED_PROJECTS="$WORK/projects" R5ED_SMOKE_OUT="$WORK/out"
xvfb-run -a -s "-screen 0 1920x1080x24" node "$REPO/web/smoke/smoke.mjs" "$@"
if [ "$KEEP" = 1 ]; then
  echo "smoke: host left running (pid $HOST_PID) at $URL"
fi
