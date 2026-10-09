# Sourced by the ship scripts: r5ed.config.json and the environment give every
# tool and folder (scripts/lib/r5edenv.py lists them); a missing one stops here.
R="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PY="${PYTHON:-py}"
eval "$("$PY" "$R/scripts/lib/r5edenv.py")" || { echo "cannot read the r5ed settings"; exit 1; }
for v in R5ED_GAME_ROOT R5ED_RSX R5ED_REPAK R5ED_REVPK R5ED_PROJECTS R5ED_SCRATCH; do
  [ -n "${!v:-}" ] || { echo "$v is not set (copy r5ed.config.example.json to r5ed.config.json)"; exit 1; }
done
G="$R5ED_GAME_ROOT"
REPAK="$R5ED_REPAK"
# r5ed-compile and its asset cache (r5ed-cache/ beside the exe); a cargo release build by default
BIN="${R5ED_BIN:-$R/target/release}"
EXE="$BIN/r5ed-compile.exe"
[ -f "$EXE" ] || EXE="$BIN/r5ed-compile"
INDEX="$BIN/r5ed-cache/index/assets.json"
TMP="$R5ED_SCRATCH/tmp"; TEMP="$TMP"; mkdir -p "$TMP"
export TMP TEMP R5ED_GAME_ROOT R5ED_REPAK R5ED_REVPK R5ED_RSX
