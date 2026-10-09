#!/usr/bin/env bash
# A DCC model (FBX/DAE/STL/PLY/3DS/BLEND) end to end into the S21 install:
# blender -> glb -> import -> image materials -> src pak -> build + bake ->
# package -> shader closure -> gate -> deploy.
#
#   scripts/source/ship_glb.sh <stem> <model> <scale> [up=y|z]
#   e.g. ship_glb.sh mp_rr_castle ".../castle.fbx" 1
#
# The scale is baked into the GLB, so import-glb runs at scale 1 with the same up.
# SKIP_DEPLOY=1 stops after the gate.
set -uo pipefail
STEM=$1; MODEL=$2; SCALE=$3; UP=${4:-y}
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
T=$R5ED_SCRATCH/source-port/$STEM
mkdir -p "$T" && cd "$T" || exit 1
. "$(dirname "${BASH_SOURCE[0]}")/ship_common.sh"
PREFIX=world/obj/$STEM
GLB="$T/source.glb"

step "convert"
py $R/scripts/source/blender_convert.py "$MODEL" "$GLB" --up "$UP" --scale "$SCALE" || exit 1

py -c "import json,sys;json.dump({'scale':1.0,'up':sys.argv[1],'material_prefix':sys.argv[2]},open('obj_options.json','w'))" \
   "$UP" "$PREFIX"

step "import (report)"
rm -rf probe && "$EXE" new probe --stem "$STEM" >/dev/null && "$EXE" import-glb probe "$GLB" --options obj_options.json > import_report.json \
  || { echo "IMPORT FAILED"; exit 1; }

step "materials"
rm -rf src_pak
py $R/scripts/source/objmat.py $TEMPLATES "$GLB" "$T/src_pak/assets" --stem "$STEM" --prefix "$PREFIX" \
   --out-dir "$T/src_pak/out" --only import_report.json || exit 1

import_final() { "$EXE" import-glb "$1" "$GLB" --options obj_options.json > import_final.json; }
ship_tail || exit 1
