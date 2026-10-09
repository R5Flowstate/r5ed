"""Gate: every guid reference slot of a built pak resolves, in the pak itself or in the
paks the game always has loaded.

    py gate_guidrefs.py <pak> <client|dedi>

client: the resident client paks (common*, startup*, ui*) of R5ED_GAME_ROOT/paks/Win64
dedi:   every pak in R5ED_GAME_ROOT/paks/Win64_server
The asset lists come from RSX and are cached in R5ED_SCRATCH, keyed by the paks' sizes
and times. Walks the pak's guid-ref table, the one the engine's consistency check walks.
"""
import csv
import hashlib
import os
import struct
import subprocess
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import r5edenv  # noqa: E402
import rpak  # noqa: E402

RESIDENT = ("common", "startup", "ui")


def universe(side):
    root = r5edenv.path("R5ED_GAME_ROOT") / "paks"
    if side == "client":
        paks = sorted(p for p in (root / "Win64").glob("*.rpak") if p.name.startswith(RESIDENT))
    else:
        paks = sorted((root / "Win64_server").glob("*.rpak"))
    if not paks:
        raise SystemExit(f"no {side} paks under {root}")
    key = hashlib.sha1("|".join(f"{p.name}:{p.stat().st_size}:{p.stat().st_mtime_ns}" for p in paks).encode()).hexdigest()[:16]
    out = r5edenv.scratch("guid_lists") / f"{side}_{key}.csv"
    if not out.is_file():
        rsx = r5edenv.get("R5ED_RSX")
        tmp = out.with_suffix(".tmp.csv")
        subprocess.run([rsx, "-nogui", "-nocachedb", "--list", str(tmp), "--listformat", "csv", *map(str, paks)],
                       cwd=out.parent, capture_output=True, check=True)
        tmp.replace(out)
    known = {}
    with open(out, newline="", encoding="utf-8", errors="replace") as f:
        for row in csv.DictReader(f):
            try:
                known[int(row["guid"], 16)] = row.get("type", "")
            except (KeyError, ValueError):
                pass
    return known


def owner_of(pk, page, off):
    best = None
    for a in pk.assets:
        if a.head.index == page and a.head.offset <= off < a.head.offset + a.header_size:
            return a
        if a.data.index == page and a.data.offset <= off:
            if best is None or a.data.offset > best.data.offset:
                best = a
    return best


def main():
    if len(sys.argv) != 3 or sys.argv[2] not in ("client", "dedi"):
        raise SystemExit(__doc__)
    path, side = sys.argv[1], sys.argv[2]
    known = universe(side)
    pk = rpak.open_pak(path)
    inpak = {a.guid for a in pk.assets}
    bad, seen = [], Counter()
    for p in pk.guid_refs:
        try:
            buf, o = pk.src(p)
            g, = struct.unpack_from("<Q", buf, o)
        except Exception:
            continue
        if g == 0 or g in known or g in inpak:
            continue
        bad.append((p, g))
        seen[g] += 1
    print("%s: assets %d  guid-ref slots %d  UNRESOLVED %d (%d distinct)"
          % (os.path.basename(path), len(pk.assets), len(pk.guid_refs), len(bad), len(seen)))
    if bad:
        by = Counter()
        for p, g in bad[:4000]:
            a = owner_of(pk, p.index, p.offset)
            by[(a.guid, a.type) if a else (0, "?")] += 1
        for (ag, ty), n in by.most_common(15):
            print("  owner 0x%016X %-6s %d unresolved" % (ag, ty, n))
        for g, n in seen.most_common(15):
            print("  missing 0x%016X x%d" % (g, n))
    print("  asset types:", dict(Counter(a.type for a in pk.assets)))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
