#!/usr/bin/env bash
# An r5ed project -> a standalone map mod, under a mod stem, installed into the
# S21 install; the old loose deploy of the source stem is removed only once the
# mod is gated and installed.
#
#   scripts/mod_map.sh <source project> <Team.Name> "<display>" <mod dir> [src pak stems...]
#   e.g. mod_map.sh "$R5ED_PROJECTS/mp_rr_smoke" Me.Smoke "Smoke" mods/Smoke
#
# The mod stem is mp_<team_name>__<old name without mp_rr_>. BLURB sets the description.
set -uo pipefail
SRC=$1; MOD_ID=$2; DISPLAY=$3; MOD_DIR=$4; shift 4; SRC_PAKS=("$@")
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
OLD=$(basename "$SRC")
NS=$(echo "$MOD_ID" | tr 'A-Z.' 'a-z_')
STEM="mp_${NS}__${OLD#mp_rr_}"
P=$R5ED_PROJECTS/$STEM
T=$R5ED_SCRATCH/mod-map/$STEM
INDEX_PAKS=$(IFS=,; echo "${SRC_PAKS[*]:-}")
export R5ED_INDEX_PAKS=$INDEX_PAKS
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
echo "$OLD -> $STEM ($MOD_ID)"

step "project"
[ -f "$SRC/map.json" ] || { echo "NO PROJECT $SRC"; exit 1; }
if [ -d "$P" ]; then mv "$P" "$T/project_prev_$(date +%H%M%S)" || exit 1; fi
mkdir -p "$P" || exit 1
# Everything but the old build output: its RSX export cache is keyed by pak build and reused below.
(cd "$SRC" && tar cf - --exclude=./build .) | (cd "$P" && tar xf -) || { echo "COPY FAILED"; exit 1; }
if [ -d "$SRC/build/package/cache" ]; then mkdir -p "$P/build/package" && cp -r "$SRC/build/package/cache" "$P/build/package/"; fi
py -c "
import json,sys
p=sys.argv[1]+'/map.json'; d=json.load(open(p)); d['meta']['stem']=sys.argv[2]; d['meta']['display_name']=sys.argv[3]
json.dump(d,open(p,'w'),indent=1)" "$P" "$STEM" "$DISPLAY" || exit 1

step "stage install"
# The deployed old-stem paks would be indexed as donors of stock assets (RSX
# cannot export from an r5ed-built pak), so they leave the install for the
# build and come back if anything fails. SRC_PAK_DIR holds the map's own
# source paks, present only while the build indexes them.
PREV=$T/prev_deploy
mkdir -p "$PREV/Win64" "$PREV/Win64_server"
for f in $OLD.rpak ${OLD}_client_perm.rpak ${OLD}_client_temp.rpak; do
  [ -f "$G/paks/Win64/$f" ] && { mv "$G/paks/Win64/$f" "$PREV/Win64/" || { echo "CANNOT MOVE $f (game running?)"; exit 1; }; }
done
[ -f "$G/paks/Win64_server/$OLD.rpak" ] && { mv "$G/paks/Win64_server/$OLD.rpak" "$PREV/Win64_server/" || exit 1; }
SRC_COPIED=()
if [ -n "${SRC_PAK_DIR:-}" ]; then
  for f in "$SRC_PAK_DIR"/out/*.rpak "$SRC_PAK_DIR"/out/*.starpak; do
    [ -f "$f" ] || continue
    cp -f "$f" "$G/paks/Win64/" || exit 1; SRC_COPIED+=("paks/Win64/$(basename "$f")")
  done
  for f in "$SRC_PAK_DIR"/out_server/*.rpak; do
    [ -f "$f" ] || continue
    cp -f "$f" "$G/paks/Win64_server/" || exit 1; SRC_COPIED+=("paks/Win64_server/$(basename "$f")")
  done
fi
unstage() { for f in "${SRC_COPIED[@]}"; do rm -f "$G/$f"; done; }
restore() { unstage; cp -n "$PREV"/Win64/* "$G/paks/Win64/" 2>/dev/null; cp -n "$PREV"/Win64_server/* "$G/paks/Win64_server/" 2>/dev/null; echo "   previous deploy restored"; }
trap 'rc=$?; [ $rc -ne 0 ] && [ -z "${INSTALLED:-}" ] && restore; rm -rf "$LOCK"' EXIT
py -c "
import json,sys
import os
f=sys.argv[1]
if not os.path.isfile(f): sys.exit(0)
d=json.load(open(f)); n=len(d['assets'])
d['assets']=[r for r in d['assets'] if not any(r['pak'].startswith(s) for s in sys.argv[2:])]
for k in [k for k in d['paks'] if any(k.startswith(s) for s in sys.argv[2:])]: d['paks'].pop(k)
json.dump(d,open(f,'w')); print('index rows purged', n-len(d['assets']))" "$INDEX" "$OLD." "${OLD}_client" "$STEM" || exit 1

step "build + bake"
# The client names its capture dir after the map load name, so a capture of the old stem moves with it.
if [ -f "$G/platform/cubemaps/$OLD/manifest.json" ] && [ ! -e "$G/platform/cubemaps/$STEM" ]; then
  cp -r "$G/platform/cubemaps/$OLD" "$G/platform/cubemaps/$STEM" || exit 1
  echo "cubemap capture copied $OLD -> $STEM"
fi
if [ -f "$G/platform/cubemaps/$STEM/manifest.json" ]; then export R5ED_CUBEMAP_CAPTURE="$G/platform/cubemaps/$STEM"; echo "cubemaps from capture $R5ED_CUBEMAP_CAPTURE"; fi
py $R/scripts/h2/capped_idle.py 8 build.log "$EXE" build "$P" --bake final --install
grep -q "PROGRESS 100" build.log || { echo "BUILD FAILED"; grep -E "^Error|panicked|error:" build.log | head -5; exit 1; }

step "package"
B=$P/build
py $R/scripts/h2/capped_idle.py 8 package.log "$EXE" package "$P"
[ -f "$B/package/$STEM.json" ] || { echo "PACKAGE FAILED"; tail -5 package.log; exit 1; }
py $R/scripts/source/check_package.py package.log || { echo "PACKAGE INCOMPLETE -- NOT SHIPPED"; exit 1; }

step "shader closure"
cd "$B/package" && cp $STEM.json $STEM.json.orig && rm -rf shader shaderset
py -c "
import json,sys;m=json.load(open(sys.argv[1]+'.json.orig'))
m['files']=[f for f in m['files'] if f['_type'] not in ('shdr','shds')]
json.dump(m,open(sys.argv[1]+'.json','w'),indent=1)" $STEM
# PACKAGE_EXTRA: dirs of prebuilt assets nothing in the map references (e.g. sky particles).
[ -n "${PACKAGE_EXTRA:-}" ] && { py $R/scripts/h3/pak_extra.py "$STEM.json" $PACKAGE_EXTRA || { echo "PACKAGE EXTRA FAILED"; exit 1; }; }
cd "$T"
H2_PAK_SCRATCH=$R5ED_SCRATCH/paks_dec py $R/scripts/h2/close_shaders.py "$B/package" $STEM "$INDEX" "$G" $REPAK > close_shaders.log 2>&1
grep -q "repak exit 0" close_shaders.log || { echo "SHADER CLOSURE FAILED"; tail -5 close_shaders.log; exit 1; }

step "gate"
py $R/scripts/h2/gate_pak.py "$B/package/$STEM.rpak" "$B/package/$STEM.json" || { echo "GATE FAILED -- NOT INSTALLED"; exit 1; }

step "mod"
unstage
taskkill //F //IM r5apex.exe >/dev/null 2>&1; taskkill //F //IM r5apex_ds.exe >/dev/null 2>&1; taskkill //F //IM r5apex_dx12.exe >/dev/null 2>&1
py $R/scripts/source/mod_pack.py "$P" "$MOD_DIR" --id "$MOD_ID" --display "$DISPLAY" ${BLURB:+--blurb "$BLURB"} --install "$G" || exit 1
INSTALLED=1

step "retire loose $OLD"
for f in "$PREV"/Win64/* "$PREV"/Win64_server/*; do [ -f "$f" ] && echo "  moved aside $(basename "$f") -> $PREV"; done
for f in paks/Win64/${OLD}_loadscreen.rpak vpk/englishserver_$OLD.bsp.pak000_dir.vpk vpk/server_$OLD.bsp.pak000_000.vpk; do
  [ -f "$G/$f" ] && mv "$G/$f" "$PREV/" && echo "  moved aside $f -> $PREV"
done
# `register` named the loose stem in the install's maps block and launcher list;
# only those two exact lines go (both files are backed up beside the run).
cp "$G/platform/playlists_r5_patch.txt" "$T/playlists_r5_patch.before.txt" && cp "$G/platform/r5f_wip_maps.txt" "$T/r5f_wip_maps.before.txt" || exit 1
py -c "
import re,sys
old=sys.argv[1]
for path,pat in ((sys.argv[2], r'^\s*'+re.escape(old)+r' 1\s*$'), (sys.argv[3], r'^'+re.escape(old)+r'\s+=.*$')):
    lines=open(path,encoding='utf-8').read().split('\n')
    keep=[l for l in lines if not re.match(pat,l)]
    print(f'  {path}: removed {len(lines)-len(keep)} line(s)')
    open(path,'w',encoding='utf-8',newline='').write('\n'.join(keep))" "$OLD" "$G/platform/playlists_r5_patch.txt" "$G/platform/r5f_wip_maps.txt" || exit 1
step "done"
