# Shared tail of the foreign-map ship scripts (sourced, not run).
# The caller sets STEM, T (scratch dir, cwd), and defines `import_final <project>`
# that fills a fresh project; src_pak/<stem>_src.json (+ optional _server) must exist.
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
P=$R5ED_PROJECTS/$STEM
TEMPLATES=${TEMPLATES:-$R5ED_SCRATCH/templates}
[ -d "$TEMPLATES/material" ] || "$PY" "$R/scripts/lib/export_templates.py" "$TEMPLATES" || exit 1
export R5ED_INDEX_PAKS=${STEM}_src
# The bake uses every logical CPU unless RAYON_NUM_THREADS is set by the caller (it was pinned to 6).
[ -n "${RAYON_NUM_THREADS:-}" ] && export RAYON_NUM_THREADS
step() { echo "== $1 $(date +%T)"; }
pak() {  # a pak counts as built only with repak's "built pak" line and no ERROR
  $REPAK "$1" 2>&1 | tr '\r' '\n' | grep -E "built pak|ERROR" > "$2"; cat "$2"
  grep -q "built pak" "$2" && ! grep -q "ERROR" "$2"
}

ship_tail() {
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

  step "src paks"
  rm -rf src_pak/out src_pak/out_server
  pak src_pak/${STEM}_src.json srcpak.log || { echo "SRC PAK FAILED"; return 1; }
  cp -f src_pak/out/${STEM}_src.rpak src_pak/out/${STEM}_src*.starpak $G/paks/Win64/ 2>/dev/null
  if [ -f src_pak/${STEM}_src_server.json ]; then
    pak src_pak/${STEM}_src_server.json srcpak_server.log || { echo "SRC SERVER PAK FAILED"; return 1; }
    cp -f src_pak/out_server/${STEM}_src.rpak $G/paks/Win64_server/
  fi

  step "project"
  if [ -f "$P/map.json" ]; then mv "$P" "$T/project_prev_$(date +%H%M%S)"; fi
  "$EXE" new "$P" --stem "$STEM" >/dev/null && import_final "$P" || { echo "IMPORT FAILED"; return 1; }
  if [ -n "${DISPLAY:-}" ]; then
    py -c "
import json,sys
p=sys.argv[1]+'/map.json'; d=json.load(open(p)); d['meta']['display_name']=sys.argv[2]
json.dump(d,open(p,'w'),indent=1)" "$P" "$DISPLAY" || return 1
  fi

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

  if [ -n "${PREBUILT:-}" ]; then
    # A bake done elsewhere (the Linux bake box): this machine only packs it. No build runs here,
    # whatever changed since; the map ships exactly as baked.
    step "prebuilt bake (no build)"
    [ -f "$PREBUILT/maps/$STEM.bsp" ] || { echo "PREBUILT $PREBUILT has no maps/$STEM.bsp"; return 1; }
    rm -rf "$P/build" && cp -r "$PREBUILT" "$P/build" || { echo "PREBUILT COPY FAILED"; return 1; }
    echo "using the bake from $PREBUILT"
  else
    step "build + bake"
    # BUILD_EXTRA: extra build arguments (e.g. --src-pak <src_pak json> so authored materials resolve).
    py $R/scripts/h2/capped_idle.py 8 build.log "$EXE" build "$P" --bake final --install ${BUILD_EXTRA:-}
    grep -q "PROGRESS 100" build.log || { echo "BUILD FAILED"; grep -E "^Error|panicked|error:" build.log | head -5; return 1; }
  fi

  step "package"
  local B=$P/build
  # The packager keys its RSX exports (cache/, export/) by pak build and reuses them;
  # only its generated outputs are cleared, or every ship re-extracts from scratch.
  (cd "$B/package" 2>/dev/null && rm -rf material mdl paks server shader shaderset texture vpk wraps ./*.json ./*.orig ./*.rpak)
  py $R/scripts/h2/capped_idle.py 8 package.log "$EXE" package "$P"
  [ -f "$B/package/$STEM.json" ] || { echo "PACKAGE FAILED"; tail -5 package.log; return 1; }
  py $R/scripts/source/check_package.py package.log || { echo "PACKAGE INCOMPLETE -- NOT SHIPPED"; return 1; }

  step "shader closure"
  (cd "$B/package" && cp $STEM.json $STEM.json.orig && rm -rf shader shaderset && py -c "
import json,sys;m=json.load(open(sys.argv[1]+'.json.orig'))
m['files']=[f for f in m['files'] if f['_type'] not in ('shdr','shds')]
json.dump(m,open(sys.argv[1]+'.json','w'),indent=1)" $STEM)
  H2_PAK_SCRATCH=$R5ED_SCRATCH/paks_dec py $R/scripts/h2/close_shaders.py "$B/package" $STEM "$INDEX" "$G" $REPAK > close_shaders.log 2>&1
  grep -q "repak exit 0" close_shaders.log || { echo "SHADER CLOSURE FAILED"; tail -5 close_shaders.log; return 1; }

  if [ -f "$T/src_pak/${STEM}_src_server.json" ]; then
    step "server models"
    py $R/scripts/source/merge_server_models.py "$B/package" $STEM "$T/src_pak/${STEM}_src_server.json" $REPAK \
      || { echo "SERVER MODELS FAILED"; return 1; }
  fi

  step "gate"
  py $R/scripts/h2/gate_pak.py "$B/package/$STEM.rpak" "$B/package/$STEM.json" || { echo "GATE FAILED -- NOT DEPLOYED"; return 1; }
  [ -n "${SKIP_DEPLOY:-}" ] && { echo "gate green; SKIP_DEPLOY set"; return 0; }

  if [ -n "${MOD_ID:-}" ]; then
    # As a mod (stem mp_<namespace>__<name>): MOD_ID=Team.Name MOD_DIR=<mod folder>.
    step "mod"
    taskkill //F //IM r5apex.exe >/dev/null 2>&1; taskkill //F //IM r5apex_ds.exe >/dev/null 2>&1; taskkill //F //IM r5apex_dx12.exe >/dev/null 2>&1
    rm -f $G/paks/Win64/${STEM}_src.rpak $G/paks/Win64/${STEM}_src*.starpak $G/paks/Win64_server/${STEM}_src.rpak
    local MOVE_ARGS=()
    if [ -n "${MOVEMENT:-}" ]; then
      local MOVE_FILE=$MOVEMENT; [ -f "$MOVE_FILE" ] || MOVE_FILE=$R/scripts/source/mod_templates/movement/$MOVEMENT.json
      [ -f "$MOVE_FILE" ] || { echo "NO MOVEMENT PRESET $MOVEMENT"; return 1; }
      MOVE_ARGS=(--movement "$MOVE_FILE")
      [ -n "${INVINCIBLE:-}" ] && MOVE_ARGS+=(--invincible)
      [ -n "${NO_MELEE:-}" ] && MOVE_ARGS+=(--no-melee)
    fi
    py $R/scripts/source/mod_pack.py "$P" "${MOD_DIR:?MOD_DIR}" --id "$MOD_ID" --display "${DISPLAY:-$STEM}" ${BLURB:+--blurb "$BLURB"} --install "$G" "${MOVE_ARGS[@]}" || return 1
    step "done"
    return 0
  fi

  step "deploy"
  taskkill //F //IM r5apex.exe >/dev/null 2>&1; taskkill //F //IM r5apex_ds.exe >/dev/null 2>&1; taskkill //F //IM r5apex_dx12.exe >/dev/null 2>&1
  cp -f "$B/package/$STEM.rpak" "$B/package/paks/Win64/${STEM}_client_perm.rpak" "$B/package/paks/Win64/${STEM}_client_temp.rpak" $G/paks/Win64/
  cp -f "$B/package/paks/Win64_server/$STEM.rpak" $G/paks/Win64_server/
  cp -f "$B/package/vpk/englishserver_$STEM.bsp.pak000_dir.vpk" "$B/package/vpk/server_$STEM.bsp.pak000_000.vpk" $G/vpk/
  rm -f $G/paks/Win64/${STEM}_src.rpak $G/paks/Win64/${STEM}_src*.starpak $G/paks/Win64_server/${STEM}_src.rpak
  "$EXE" register "$P" --install "$G" > register.log 2>&1 || { echo "REGISTER FAILED"; tail -3 register.log; return 1; }
  ls -la $G/paks/Win64/$STEM* $G/paks/Win64_server/$STEM* $G/vpk/*$STEM* | awk '{print $5, $NF}'
  step "done"
}
