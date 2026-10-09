"""Lift the menu and ceremony rooms out of an imported stock map.

    py extract_basement.py <imported project map.json> <out json>

Every shipped map carries, away from the playable space, the targets and lights
character select, the victory ceremony, scene capture and the victory podium
look up by name. This writes those objects grouped by room, each position
relative to the room's anchor, for rcomp to place under any map that lacks them.
Brush entities are left out: their `*N` model slots exist only in the source map.
"""
import json
import math
import sys

# anchor script name, radius in units
ROOMS = {
    "char_select": ("target_char_sel_camera_new", 1500),
    "victory": ("target_victory_gcards_ref", 1500),
    "scene_capture": ("scene_capture_room_1_ref", 3500),
    "podium": ("podium_info_target", 1500),
}
DROP_CLASSES = {"ambient_generic", "envmap_volume", "fog_volume", "trigger_soundscape", "trigger_out_of_bounds"}


def script_name(o):
    kv = o["kind"].get("keyvalues", {})
    return kv.get("script_name") or kv.get("targetname")


def main():
    doc = json.load(open(sys.argv[1], encoding="utf-8"))
    objs = doc["objects"]
    out = {"source": doc.get("meta", {}).get("name", sys.argv[1]), "rooms": {}}
    for room, (anchor_name, radius) in ROOMS.items():
        anchor = next(o for o in objs if script_name(o) == anchor_name)
        a = anchor["transform"]["position"]
        items = []
        for o in objs:
            k = o["kind"]
            if k["type"] not in ("entity", "light"):
                continue
            if math.dist(o["transform"]["position"], a) > radius:
                continue
            kv = k.get("keyvalues", {})
            if k.get("classname") in DROP_CLASSES or str(kv.get("model", "")).startswith("*"):
                continue
            t = dict(o["transform"])
            t["position"] = [p - q for p, q in zip(t["position"], a)]
            items.append({"name": o["name"], "transform": t, "kind": k})
        out["rooms"][room] = items
        print(room, len(items), "objects")
    json.dump(out, open(sys.argv[2], "w", encoding="utf-8"), indent=1)


if __name__ == "__main__":
    main()
