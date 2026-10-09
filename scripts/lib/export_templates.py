"""Export the stock S21 materials the porting scripts rebuild foreign materials on.

    py scripts/lib/export_templates.py [out dir]      (default $R5ED_SCRATCH/templates)

The template names are read out of the porting scripts themselves (every
("<material path>", "<shader type>") pair they declare). Each is found through
r5ed-compile's asset index of the install and exported with RSX, with its depth and
colour-pass materials. The index is built first if it does not exist yet.
"""
import collections
import json
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE.parent
sys.path.insert(0, str(HERE))
import r5edenv  # noqa: E402
from guid import string_to_guid  # noqa: E402

PAIR = re.compile(r'\("((?:world|models)/[a-z0-9_/]+)", "(wldc|wld|wld_bm|rgdp|rgdc|sknp|skn)"\)')


def wanted() -> set[tuple[str, str]]:
    out = set()
    for f in SCRIPTS.rglob("*.py"):
        if "tests" in f.parts:
            continue
        out.update(PAIR.findall(f.read_text(encoding="utf-8", errors="replace")))
    return out


def compile_bin() -> Path:
    return Path(r5edenv.get("R5ED_BIN", required=False) or SCRIPTS.parent / "target" / "release")


def asset_index() -> Path:
    index = compile_bin() / "r5ed-cache" / "index" / "assets.json"
    if index.is_file():
        return index
    exe = next((compile_bin() / n for n in ("r5ed-compile.exe", "r5ed-compile") if (compile_bin() / n).is_file()), None)
    if exe is None:
        raise SystemExit(f"r5ed-compile not found in {compile_bin()}: cargo build --release, or set R5ED_BIN")
    print("indexing the install (first run only, several minutes)")
    probe = r5edenv.scratch("index_probe") / "probe"
    subprocess.run([str(exe), "new", str(probe), "--stem", "mp_rr_index_probe"], check=True, capture_output=True)
    subprocess.run([str(exe), "brush", str(probe), "--min", "0,0,0", "--max", "64,64,64"], check=True, capture_output=True)
    r = subprocess.run([str(exe), "build", str(probe), "--install", "--bake", "none"], capture_output=True, text=True)
    if not index.is_file():
        sys.stdout.write(r.stderr[-2000:])
        raise SystemExit("building the asset index failed")
    return index


def main() -> int:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else r5edenv.scratch("templates")
    out.mkdir(parents=True, exist_ok=True)
    pairs = wanted()
    by_guid = {string_to_guid(f"material/{n}_{t}.rpak"): f"{n}_{t}" for n, t in pairs}
    rows = json.loads(asset_index().read_text(encoding="utf-8"))["assets"]
    pick = collections.defaultdict(set)
    for r in rows:
        if r.get("kind") == "matl" and r["guid"] in by_guid and r["guid"] not in {g for s in pick.values() for g in s}:
            pick[r["pak"]].add(r["guid"])
    found = {g for s in pick.values() for g in s}
    rsx = r5edenv.get("R5ED_RSX")
    paks = r5edenv.path("R5ED_GAME_ROOT") / "paks" / "Win64"
    for pak, guids in sorted(pick.items()):
        lst = out / f"_g_{pak}.txt"
        lst.write_text("\n".join("%016x" % g for g in sorted(guids)), encoding="ascii")
        subprocess.run([rsx, "-nogui", "-nocachedb", "-export", "--exporttypes", "matl", "--texturenames", "guid",
                        "--exportguids", str(lst), "--exportdir", str(out), str(paks / pak)],
                       cwd=out, capture_output=True, timeout=1800)
        print(f"{pak}: {len(guids)}")
    missing = sorted(by_guid[g] for g in by_guid if g not in found
                     or not list((out / "material").rglob(f"{Path(by_guid[g]).name}.json")))
    if missing:
        # a class whose template is absent fails only when a port actually needs it
        print("WARNING not in this install:", ", ".join(missing))
    print(f"{len(pairs) - len(missing)}/{len(pairs)} templates in {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
