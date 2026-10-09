"""Merge h3dump geometry-side gameplay.json with the TagTool scenario dump
(gameplay_tags.tsv) into the h2import.py contract: player_starts carry
TagTool yaw facings (matched by position), plus H3 weapon/vehicle/equipment
placements, trigger volumes and sandbox palettes for later stages."""

import json
import math
import sys
from pathlib import Path

DUMP = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("dump")


def load_tsv():
    inst, pal, loc, trig, soft, sand = [], {}, [], [], [], {}
    for line in (DUMP / "gameplay_tags.tsv").read_text().splitlines():
        f = line.split("\t")
        k = f[0]
        if k == "PAL":
            pal.setdefault(f[1], {})[int(f[2])] = f[3]
        elif k == "INST":
            inst.append({
                "block": f[1], "palette": int(f[2]), "tag": f[3],
                "position": [float(f[4]), float(f[5]), float(f[6])],
                "yaw": float(f[7]), "pitch": float(f[8]), "roll": float(f[9]),
                "spawn_time": f[10], "team": f[11],
            })
        elif k == "LOC":
            loc.append({"position": [float(f[1]), float(f[2]), float(f[3])],
                        "facing": math.radians(float(f[4]))})
        elif k == "TRIG":
            trig.append({"name": f[1], "position": [float(f[2]), float(f[3]), float(f[4])],
                         "extents": [float(f[5]), float(f[6]), float(f[7])], "kill": f[8]})
        elif k == "SOFT":
            soft.append({"name": f[1], "type": f[2]})
        elif k == "SAND":
            sand.setdefault(f[1], []).append({"object": f[3], "name": f[4]})
    return inst, pal, loc, trig, soft, sand


def main():
    raw = json.loads((DUMP / "gameplay.json").read_text())
    inst, pal, loc, trig, soft, sand = load_tsv()

    # TagTool yaw for every Reclaimer spawn marker, matched by position.
    scen = [r for r in inst if r["block"] == "Scenery"]
    by_pos = {}
    for r in scen:
        by_pos.setdefault(tuple(round(v, 3) for v in r["position"]), []).append(r)
    starts = []
    for s in raw["player_starts"]:
        key = tuple(round(v, 3) for v in s["position"])
        cands = by_pos.get(key, [])
        yaw = cands[0]["yaw"] if cands else math.degrees(s["facing"])
        starts.append({"position": s["position"], "facing": math.radians(yaw),
                       "model": s["model"], "group": s["group"]})

    def block(name):
        return [r for r in inst if r["block"] == name]

    merged = {
        "map": raw["map"],
        "scenario": raw["scenario"],
        "player_starts": starts,
        "player_start_single": loc,
        "netgame_flags": [],
        "netgame_equipment": [
            {"position": r["position"], "orientation": [r["yaw"], r["pitch"], r["roll"]],
             "collection": r["tag"], "spawn_time": r["spawn_time"], "team": r["team"]}
            for r in block("Equipment")],
        "vehicles": [r for r in block("Vehicles")],
        "weapons": [r for r in block("Weapons")],
        "scenery": scen,
        "crates": [r for r in inst if r["block"] == "Crates"],
        "trigger_volumes": trig,
        "soft_ceilings": soft,
        "sandbox": sand,
        "palettes": pal,
    }
    (DUMP / "gameplay_h3raw.json").write_text(json.dumps(raw, indent=1))
    (DUMP / "gameplay.json").write_text(json.dumps(merged, indent=1))
    print(f"starts={len(starts)} weapons={len(merged['weapons'])} "
          f"vehicles={len(merged['vehicles'])} equipment={len(merged['netgame_equipment'])} "
          f"trig={len(trig)}")


if __name__ == "__main__":
    main()
