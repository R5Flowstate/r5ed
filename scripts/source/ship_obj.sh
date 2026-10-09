#!/usr/bin/env bash
# A Wavefront OBJ map (.obj + .mtl + images) end to end into the S21 install:
# import -> image materials -> src pak -> build + bake -> package -> shader
# closure -> gate -> deploy.
#
#   scripts/source/ship_obj.sh <stem> <obj> <scale> [up=y|z]
#   e.g. ship_obj.sh mp_rr_summoners_rift ".../summoner_rift.obj" 262
#
# The .mtl is the one next to the .obj with the same name. SKIP_DEPLOY=1 stops after the gate.
set -uo pipefail
STEM=$1; OBJ=$2; SCALE=$3; UP=${4:-y}
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
T=$R5ED_SCRATCH/source-port/$STEM
mkdir -p "$T" && cd "$T" || exit 1
. "$(dirname "${BASH_SOURCE[0]}")/ship_common.sh"
# The map name, not the whole stem: RSX exports land under
# build/package/cache/rsx/material-<hash>/<material path>, and a mod stem in
# both the project dir and the material path ran a long cutout twin's export
# past Windows' 260-character limit.
PREFIX=world/obj/${STEM##*__}
MTL="${OBJ%.*}.mtl"

py -c "import json,sys;json.dump({'scale':float(sys.argv[1]),'up':sys.argv[2],'material_prefix':sys.argv[3]},open('obj_options.json','w'))" \
   "$SCALE" "$UP" "$PREFIX"

step "import (report)"
rm -rf probe && "$EXE" new probe --stem "$STEM" >/dev/null && "$EXE" import-obj probe "$OBJ" --options obj_options.json > import_report.json \
  || { echo "IMPORT FAILED"; exit 1; }

step "materials"
rm -rf src_pak
py $R/scripts/source/objmat.py $TEMPLATES "$MTL" "$T/src_pak/assets" --stem "$STEM" --prefix "$PREFIX" \
   --out-dir "$T/src_pak/out" --only import_report.json || exit 1

import_final() { "$EXE" import-obj "$1" "$OBJ" --options obj_options.json > import_final.json; }
ship_tail || exit 1
