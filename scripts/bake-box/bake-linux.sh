#!/usr/bin/env bash
# r5ed map BAKE on a Linux CPU box. Native Rust build, no Wine, no GPU, no
# game paks, no RSX. Windows keeps everything except the CPU-hot bake.
#
# The box compiles from an asset table Windows wrote, so it never opens a
# game root: `build --assets tables.json --install` takes the table and only
# adds the default sky. A table matches the document it was written from --
# rewrite it on Windows after any edit that adds a material or model.
#
# Layout (all under $BOX, default ~/r5ed-bake):
#   $BOX/r5ed-src/      repo checkout (this script lives in scripts/bake-box/)
#   $BOX/jobs/<stem>/   per map, copied from Windows:
#                         project/map.json + project/meshes/ + tables.json
#
# Usage:  BOX=~/r5ed-bake ~/r5ed-bake/r5ed-src/scripts/bake-box/bake-linux.sh <stem>
# Output: $BOX/jobs/<stem>/project/build/{maps,dedi} (bsp lumps + .ent).
# Copy project/build back to Windows and continue the ship at "package".
set -uo pipefail
STEM=$1
BOX=${BOX:-$HOME/r5ed-bake}
JOB=$BOX/jobs/$STEM
SRC=$BOX/r5ed-src
export RAYON_NUM_THREADS=${RAYON_NUM_THREADS:-$(nproc)}
export TMP=$JOB/tmp TEMP=$JOB/tmp
mkdir -p "$TMP"

step() { echo "== $1 $(date +%T)"; }
fail() { echo "BAKE-BOX: $1"; exit 1; }

[ -d "$SRC/crates" ] || fail "clone the repo to $SRC first"
[ -f "$JOB/project/map.json" ] || fail "copy $STEM project/ (map.json + meshes/) to $JOB/project first"
# The full install table (optional, one per game build) and the map's own
# table merge in that order, the map's entries winning.
ASSETS=()
[ -f "$BOX/full.json" ] && ASSETS+=("$BOX/full.json")
[ -f "$JOB/tables.json" ] && ASSETS+=("$JOB/tables.json")
[ ${#ASSETS[@]} -gt 0 ] || fail "copy $STEM tables.json (build --write-assets on Windows) to $JOB, or full.json to $BOX, first"
ASSET_LIST=$(IFS=,; echo "${ASSETS[*]}")

step "toolchain"
command -v cargo >/dev/null || {
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  . "$HOME/.cargo/env"
}
# Every run, so a pulled checkout never bakes with an older compiler; a no-op
# build takes a second, the first one ~5-10 min.
step "cargo build -p r5ed-compile"
(cd "$SRC" && cargo build --release -p r5ed-compile) || fail "cargo build failed"

EXE=$SRC/target/release/r5ed-compile

step "build --bake final from $ASSET_LIST (threads: $RAYON_NUM_THREADS)"
cd "$JOB" || fail "cd $JOB"
nice -n 10 "$EXE" build "$JOB/project" --assets "$ASSET_LIST" --install --bake final > build.log 2>&1
grep -q "PROGRESS 100" build.log || {
  echo "BUILD FAILED -- first lines of the cause:";
  grep -E "^Error|panicked|error:|not in the asset table|unresolved" build.log | head -5;
  echo "(unresolved -> the table lacks a name the map draws: rewrite tables.json on Windows with the map's _src pak installed)";
  exit 1;
}
echo "BAKE OK -- copy $JOB/project/build back to Windows (ship continues at package)"
ls "$JOB/project/build"
