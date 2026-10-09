#!/usr/bin/env bash
# A ported Apex Mobile level (port_level.py output) -> the S21 install, by ship_glb.sh's steps:
# objmat builds the materials from world.glb's embedded textures, the src pak is packed, and the
# project's own map.json and meshes (spawns, air-wall clip, sun, lightmap scales) are kept instead
# of a fresh import. Everything after that is ship_common.sh's ship_tail.
#
#   scripts/apexm/ship_level.sh <port_level.py out dir> [stem]
#   SKIP_DEPLOY=1 stops after the gate; MOD_ID/MOD_DIR ship it as a mod, as ship_common.sh documents.
#
# To bake on a Linux box instead of here: run until "src paks", then
#   r5ed-compile build <P> --install --bake none --src-pak <T>/src_pak/<stem>_src.json --write-assets tables.json
# send the project + tables.json; put the build/ that comes back at <out dir>/project/build (or point
# PREBUILT=<that build dir> at it) and run this again: no build runs, it goes straight to package.
set -uo pipefail
OUT=$(cd "$1" && pwd)
STEM=${2:-$(py -c "import json,sys;print(json.load(open(sys.argv[1]))['meta']['stem'])" "$OUT/project/map.json")}
DISPLAY=${DISPLAY:-$(py -c "import json,sys;print(json.load(open(sys.argv[1]))['meta']['display_name'])" "$OUT/project/map.json")}
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
T=$R5ED_SCRATCH/source-port/$STEM
mkdir -p "$T" && cd "$T" || exit 1
. "$(dirname "${BASH_SOURCE[0]}")/../source/ship_common.sh"
PREFIX=world/obj/$STEM

step "materials"
rm -rf src_pak
py $R/scripts/source/objmat.py $TEMPLATES "$OUT/world.glb" "$T/src_pak/assets" --stem "$STEM" --prefix "$PREFIX" \
   --out-dir "$T/src_pak/out" --only "$OUT/import_report.json" || exit 1

# The install cannot read the src pak, so the build reads its materials from the manifest.
export BUILD_EXTRA="--src-pak $T/src_pak/${STEM}_src.json"
# A bake that came back from the box rides in with the project.
[ -z "${PREBUILT:-}" ] && [ -f "$OUT/project/build/maps/$STEM.bsp" ] && export PREBUILT="$OUT/project/build"
import_final() { cp -f "$OUT/project/map.json" "$1/" && rm -rf "$1/meshes" && cp -r "$OUT/project/meshes" "$1/"; }
ship_tail || exit 1
