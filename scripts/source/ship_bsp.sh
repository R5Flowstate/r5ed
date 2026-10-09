#!/usr/bin/env bash
# A compiled Source map (.bsp or .bsp.bz2) -> an installed S21 map mod, in one call:
# decompress -> bsp2vmf (VMF + pakfile content root) -> ship.sh as a mod.
#
#   scripts/source/ship_bsp.sh <map.bsp[.bz2]> <ModName> "<Display>" [more content roots...]
#   e.g. ship_bsp.sh ".../surf_cannons_kg_v3.bsp.bz2" SurfCannons "Surf Cannons"
#
# Mod id <MOD_TEAM>.<ModName> (MOD_TEAM defaults to Community), stem mp_<team>_<modname>__<map name up to
# its _kg/_v suffix> (SHORT= overrides). MODS_DIR is where the mod folder goes.
# surf_* maps get the surf movement preset, invincibility and no melee unless
# MOVEMENT/INVINCIBLE/NO_MELEE are set (MOVEMENT=none for no preset).
# BLURB sets the mod description (credit the map author there).
set -uo pipefail
SRC=$1; NAME=$2; DISPLAY_NAME=$3; shift 3; EXTRA=("$@")
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
W=$R5ED_SCRATCH/surfmaps
mkdir -p "$W/conv" || exit 1

base=$(basename "$SRC"); base=${base%.bz2}; base=${base%.bsp}; base=${base// /_}
BSP="$W/$base.bsp"
if [ ! -f "$BSP" ]; then
  case "$SRC" in
    *.bz2) bzip2 -dc "$SRC" > "$BSP" || { rm -f "$BSP"; echo "DECOMPRESS FAILED"; exit 1; } ;;
    *) cp "$SRC" "$BSP" || exit 1 ;;
  esac
fi
py $R/scripts/source/bsp2vmf.py "$BSP" "$W/conv/$base" || { echo "BSP2VMF FAILED"; exit 1; }

TEAM=${MOD_TEAM:-Community}
ns="$(echo "${TEAM}_$NAME" | tr 'A-Z' 'a-z')"
short=${SHORT:-$(echo "$base" | tr 'A-Z' 'a-z' | sed -E 's/_(kg|v[0-9]+|b[0-9]+|fix|final|beta|rc[0-9]*)(_.*)?$//' | tr -c 'a-z0-9_\n' '_')}
stem="mp_${ns}__${short}"
[ ${#stem} -le 63 ] || { echo "STEM TOO LONG: $stem"; exit 1; }

if [[ "$base" == surf_* ]]; then
  : "${MOVEMENT:=surf}" "${INVINCIBLE:=1}" "${NO_MELEE:=1}"
fi
[ "${MOVEMENT:-}" = "none" ] && MOVEMENT=""
echo "$base -> $stem ($TEAM.$NAME)"
STEM=$stem DISPLAY="$DISPLAY_NAME" MOD_ID="$TEAM.$NAME" MOD_DIR="${MODS_DIR:-$R5ED_PROJECTS/mods}/$NAME" \
  MOVEMENT="${MOVEMENT:-}" INVINCIBLE="${INVINCIBLE:-}" NO_MELEE="${NO_MELEE:-}" \
  BLURB="${BLURB:-$DISPLAY_NAME. Community map ported from Source.}" \
  bash $R/scripts/source/ship.sh "$stem" "$W/conv/$base/$base.vmf" "$W/conv/$base/content" "${EXTRA[@]}"
