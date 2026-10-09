#!/usr/bin/env bash
# A Source map (Hammer .vmf + its content folder) end to end into the S21 install:
# import -> models -> materials -> src paks -> build + bake -> package -> shader
# closure -> gate -> deploy. The gate failing stops everything before the install
# is touched.
#
#   scripts/source/ship.sh <stem> <vmf> <content root> [more content roots...]
#   e.g. ship.sh mp_rr_surf_castle ".../surf_castle.vmf" ".../csgo"
#
# SKIP_DEPLOY=1 stops after the gate. DISPLAY sets the map's display name.
# As a mod (stem must be mp_<namespace>__<name>): MOD_ID=Team.Name MOD_DIR=<mod folder>
# packages the gated build with mod_pack.py and installs it under mods/ instead
# of copying it into the base install.
set -uo pipefail
STEM=$1; VMF=$2; shift 2; CONTENT=("$@")
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
T=$R5ED_SCRATCH/source-port/$STEM
P=$R5ED_PROJECTS/$STEM
TEMPLATES=${TEMPLATES:-$R5ED_SCRATCH/templates}
[ -d "$TEMPLATES/material" ] || "$PY" "$R/scripts/lib/export_templates.py" "$TEMPLATES" || exit 1
export R5ED_INDEX_PAKS=${STEM}_src
mkdir -p "$T"
cd "$T" || exit 1
step() { echo "== $1 $(date +%T)"; }
# One ship at a time: every ship edits the shared asset index and the install's
# paks, and closes the game. A second run waits here.
LOCK=$R5ED_SCRATCH/r5ed-ship.lock
until mkdir "$LOCK" 2>/dev/null; do
  owner=$(cut -d' ' -f1 "$LOCK/owner" 2>/dev/null)
  if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then echo "clearing a stale ship lock ($owner)"; rm -rf "$LOCK"; continue; fi
  echo "waiting for the ship lock ($(cat "$LOCK/owner" 2>/dev/null))"; sleep 30
done
echo "$$ $(basename "$0") ${STEM:-} $(date +%T)" > "$LOCK/owner"
trap 'rm -rf "$LOCK"' EXIT

step "import (report)"
rm -rf probe && "$EXE" new probe --stem "$STEM" >/dev/null && "$EXE" import-vmf probe "$VMF" > import_report.json || { echo "IMPORT FAILED"; exit 1; }

step "models"
py $R/scripts/source/srcprops.py import_report.json props --content "${CONTENT[@]}" \
   --assets "$T/src_pak/assets" --assets-server "$T/src_pak/assets_server" --stem "$STEM" --out-dir-server "$T/src_pak/out_server" || exit 1

step "materials"
py $R/scripts/source/srcmat.py $TEMPLATES "$T/src_pak/assets" --content "${CONTENT[@]}" --import-report import_report.json \
   --used props/used.json --stem "$STEM" --out-dir "$T/src_pak/out" --sizes-out texture_sizes.json || exit 1

step "src paks"
rm -rf src_pak/out src_pak/out_server
pak() {  # a pak counts as built only with repak's "built pak" line and no ERROR
  $REPAK "$1" 2>&1 | tr '\r' '\n' | grep -E "built pak|ERROR" > "$2"; cat "$2"
  grep -q "built pak" "$2" && ! grep -q "ERROR" "$2"
}
pak src_pak/${STEM}_src.json srcpak.log || { echo "SRC PAK FAILED"; exit 1; }
pak src_pak/${STEM}_src_server.json srcpak_server.log || { echo "SRC SERVER PAK FAILED"; exit 1; }
cp -f src_pak/out/${STEM}_src.rpak src_pak/out/${STEM}_src*.starpak $G/paks/Win64/
cp -f src_pak/out_server/${STEM}_src.rpak $G/paks/Win64_server/

step "project"
if [ -f "$P/map.json" ]; then mv "$P" "$T/project_prev_$(date +%H%M%S)"; fi
# Only props whose model converted are placed; the engine refuses a map naming a missing model.
py -c "
import json
o=json.load(open('texture_sizes.json')); m=json.load(open('props/models.json')); r=json.load(open('props/props_report.json'))
o['models']=sorted(k.lower() for k in m if k not in r['failed'])
json.dump(o,open('import_options.json','w'),indent=1)"
"$EXE" new "$P" --stem "$STEM" >/dev/null && "$EXE" import-vmf "$P" "$VMF" --options import_options.json > import_final.json || { echo "IMPORT FAILED"; exit 1; }
py -c "import json;r=json.load(open('import_final.json'))['report'];print('props left out (no model):', r['missing_models']); print('TELEPORTS WITH NO DESTINATION (they do nothing in Source either):', r['unresolved_teleports']) if r.get('unresolved_teleports') else None; print('MOVERS FROZEN AS STATIC BRUSHES:', r['frozen_movers']) if r.get('frozen_movers') else None; print('BULLET BLOCKERS LEFT OUT:', r['bullet_blockers']) if r.get('bullet_blockers') else None"
py -c "
import json,sys
p=sys.argv[1]+'/map.json'; d=json.load(open(p)); d['meta']['display_name']=sys.argv[2]
json.dump(d,open(p,'w'),indent=1)" "$P" "${DISPLAY:-$STEM}"

# The deployed map pak must not stand in for these assets while the build indexes.
rm -f $G/paks/Win64/$STEM.rpak $G/paks/Win64/${STEM}_client_perm.rpak $G/paks/Win64/${STEM}_client_temp.rpak $G/paks/Win64_server/$STEM.rpak
py -c "
import json,sys
import os
f=sys.argv[1]
if not os.path.isfile(f): sys.exit(0)
d=json.load(open(f)); n=len(d['assets'])
d['assets']=[r for r in d['assets'] if not r['pak'].startswith(sys.argv[2])]
for k in [k for k in d['paks'] if k.startswith(sys.argv[2])]: d['paks'].pop(k)
json.dump(d,open(f,'w')); print('index rows purged', n-len(d['assets']))" "$INDEX" "$STEM"

step "build + bake"
# A client cubemap_capture of this map replaces the baked cubemaps it covers.
if [ -f "$G/platform/cubemaps/$STEM/manifest.json" ]; then export R5ED_CUBEMAP_CAPTURE="$G/platform/cubemaps/$STEM"; echo "cubemaps from capture $R5ED_CUBEMAP_CAPTURE"; fi
py $R/scripts/h2/capped_idle.py 8 build.log "$EXE" build "$P" --bake final --install
grep -q "PROGRESS 100" build.log || { echo "BUILD FAILED"; grep -E "^Error|panicked|error:" build.log | head -5; exit 1; }

step "package"
B=$P/build
# The packager keys its RSX exports (cache/, export/) by pak build and reuses them;
# only its generated outputs are cleared, or every ship re-extracts from scratch.
(cd "$B/package" 2>/dev/null && rm -rf material mdl paks server shader shaderset texture vpk wraps ./*.json ./*.orig ./*.rpak)
py $R/scripts/h2/capped_idle.py 8 package.log "$EXE" package "$P"
[ -f "$B/package/$STEM.json" ] || { echo "PACKAGE FAILED"; tail -5 package.log; exit 1; }
py $R/scripts/source/check_package.py package.log || { echo "PACKAGE INCOMPLETE -- NOT SHIPPED"; exit 1; }

step "shader closure"
cd "$B/package" && cp $STEM.json $STEM.json.orig && rm -rf shader shaderset
py -c "
import json,sys;m=json.load(open(sys.argv[1]+'.json.orig'))
m['files']=[f for f in m['files'] if f['_type'] not in ('shdr','shds')]
json.dump(m,open(sys.argv[1]+'.json','w'),indent=1)" $STEM
cd "$T"
H2_PAK_SCRATCH=$R5ED_SCRATCH/paks_dec py $R/scripts/h2/close_shaders.py "$B/package" $STEM "$INDEX" "$G" $REPAK > close_shaders.log 2>&1
grep -q "repak exit 0" close_shaders.log || { echo "SHADER CLOSURE FAILED"; tail -5 close_shaders.log; exit 1; }

step "server models"
py $R/scripts/source/merge_server_models.py "$B/package" $STEM "$T/src_pak/${STEM}_src_server.json" $REPAK || { echo "SERVER MODELS FAILED"; exit 1; }

step "gate"
py $R/scripts/h2/gate_pak.py "$B/package/$STEM.rpak" "$B/package/$STEM.json" || { echo "GATE FAILED -- NOT DEPLOYED"; exit 1; }
[ -n "${SKIP_DEPLOY:-}" ] && { echo "gate green; SKIP_DEPLOY set"; exit 0; }

if [ -n "${MOD_ID:-}" ]; then
  step "mod"
  taskkill //F //IM r5apex.exe >/dev/null 2>&1; taskkill //F //IM r5apex_ds.exe >/dev/null 2>&1; taskkill //F //IM r5apex_dx12.exe >/dev/null 2>&1
  rm -f $G/paks/Win64/${STEM}_src.rpak $G/paks/Win64/${STEM}_src*.starpak $G/paks/Win64_server/${STEM}_src.rpak
  # MOVEMENT names a preset under mod_templates/movement (surf, ...) or a JSON path.
  MOVE_ARGS=()
  if [ -n "${MOVEMENT:-}" ]; then
    MOVE_FILE=$MOVEMENT; [ -f "$MOVE_FILE" ] || MOVE_FILE=$R/scripts/source/mod_templates/movement/$MOVEMENT.json
    [ -f "$MOVE_FILE" ] || { echo "NO MOVEMENT PRESET $MOVEMENT"; exit 1; }
    MOVE_ARGS=(--movement "$MOVE_FILE")
    [ -n "${INVINCIBLE:-}" ] && MOVE_ARGS+=(--invincible)
    [ -n "${NO_MELEE:-}" ] && MOVE_ARGS+=(--no-melee)
  fi
  py $R/scripts/source/mod_pack.py "$P" "${MOD_DIR:?MOD_DIR}" --id "$MOD_ID" --display "${DISPLAY:-$STEM}" ${BLURB:+--blurb "$BLURB"} --install "$G" "${MOVE_ARGS[@]}" || exit 1
  step "done"
  exit 0
fi

step "deploy"
taskkill //F //IM r5apex.exe >/dev/null 2>&1; taskkill //F //IM r5apex_ds.exe >/dev/null 2>&1; taskkill //F //IM r5apex_dx12.exe >/dev/null 2>&1
cp -f "$B/package/$STEM.rpak" "$B/package/paks/Win64/${STEM}_client_perm.rpak" "$B/package/paks/Win64/${STEM}_client_temp.rpak" $G/paks/Win64/
cp -f "$B/package/paks/Win64_server/$STEM.rpak" $G/paks/Win64_server/
cp -f "$B/package/vpk/englishserver_$STEM.bsp.pak000_dir.vpk" "$B/package/vpk/server_$STEM.bsp.pak000_000.vpk" $G/vpk/
rm -f $G/paks/Win64/${STEM}_src.rpak $G/paks/Win64/${STEM}_src*.starpak $G/paks/Win64_server/${STEM}_src.rpak
"$EXE" register "$P" --install "$G" > register.log 2>&1 || { echo "REGISTER FAILED"; tail -3 register.log; exit 1; }
ls -la $G/paks/Win64/$STEM* $G/paks/Win64_server/$STEM* $G/vpk/*$STEM* | awk '{print $5, $NF}'
step "done"
