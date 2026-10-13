"""Authors The Pit's environment into map.json: fog, colour grade, sun and
bake settings. Idempotent: every object is matched by name and rewritten.

usage: thepitenv.py <project dir>
"""

import json
import math
import sys
import uuid
from pathlib import Path

# Enclosed UNSC training facility: high neutral sun (light enters through
# the roof glass and courtyard openings, no long directional shadows).
SUN_PITCH, SUN_YAW = 65.0, 200.0

FOG = {
    "fogangles": f"{SUN_PITCH:g} {SUN_YAW:g} 0",
    "use_angles": "1",
    "fogdirhalfangle": "40",
    "fogdircolorstrength": "0.2",
    "fogdircolor": "235 240 245",
    "fogcolor": "150 155 165",
    "fogcolorstrength": "0.35",
    "fogdensity": "0.25",
    "fogdistoffset": "800",
    "foghalfdisttop": "4000",
    "foghalfdistbottom": "6000",
    "fogztop": "65000",
    "fogzbottom": "-800",
    "fogenable": "1",
    "vl_intensity": "0.05",
    "vl_color": "1.00 0.99 0.95",
    "vl_scatter": "0.8",
    "vl_in_shadow_scatter": "0",
    "vl_distFalloff": "1500",
    "enable_character_fog": "0",
    "disableWhenVfogEnabled": "0",
    "forceontosky": "0",
    "minfadetime": "3",
    "spawnflags": "0",
    "scale": "1",
    "targetname": "env_fog_controller_baseline",
}

GRADE = "materials/correction/hdr_overcast_test.raw_hdr"

BAKE = {
    "texel_size": 16.0,
    "bounces": 3,
    "samples": 256,
    "sky_color": [0.55, 0.6, 0.68],
    "sky_intensity": 1.2,
    "ambient_occlusion": True,
    "probe_spacing": 512.0,
    "cubemap_spacing": 4096.0,
}

# Small indoor map: everything keeps a fine texel.
MESH_TEXEL = 8.0


def quat_between(a, b):
    ax, ay, az = a
    bx, by, bz = b
    cx, cy, cz = ay * bz - az * by, az * bx - ax * bz, ax * by - ay * bx
    q = [cx, cy, cz, 1.0 + ax * bx + ay * by + az * bz]
    n = math.sqrt(sum(v * v for v in q))
    return [v / n for v in q]


def main():
    proj = Path(sys.argv[1])
    path = proj / "map.json"
    doc = json.loads(path.read_text())
    layers = {l["name"]: l["id"] for l in doc["layers"]}
    by_name = {o["name"]: o for o in doc["objects"]}

    doc["settings"]["bake"].update(BAKE)
    for o in doc["objects"]:
        if o["kind"].get("type") != "mesh":
            continue
        o["kind"]["lightmap_scale"] = MESH_TEXEL

    pitch, yaw = math.radians(SUN_PITCH), math.radians(SUN_YAW)
    to_sun = [math.cos(pitch) * math.cos(yaw), math.cos(pitch) * math.sin(yaw), math.sin(pitch)]
    sun = by_name["sun"]
    sun["transform"]["rotation"] = quat_between([0, 0, -1], [-v for v in to_sun])
    sun["kind"]["intensity"] = 1.1

    kv = dict(FOG)
    fog = by_name.get("fog")
    if fog is None:
        doc["objects"].append({"id": str(uuid.uuid4()), "name": "fog",
                               "layer": layers["entities"], "parent": None,
                               "transform": {"position": [0, 0, 0], "rotation": [0, 0, 0, 1],
                                             "scale": [1.0, 1.0, 1.0]},
                               "hidden": False, "locked": False,
                               "kind": {"type": "entity", "classname": "env_fog_controller",
                                        "keyvalues": kv, "partition": "env", "links": {}}})
    else:
        fog["kind"]["keyvalues"].update(kv)

    grade_name = "color_correction"
    kv = {"spawnflags": "0", "minfalloff": "-1", "maxweight": "1", "maxfalloff": "-1",
          "fadeOutDuration": "0", "fadeInDuration": "0", "filename": GRADE}
    obj = by_name.get(grade_name)
    if obj is None:
        doc["objects"].append({"id": str(uuid.uuid4()), "name": grade_name,
                               "layer": layers["entities"], "parent": None,
                               "transform": {"position": [0, 0, 0], "rotation": [0, 0, 0, 1],
                                             "scale": [1.0, 1.0, 1.0]},
                               "hidden": False, "locked": False,
                               "kind": {"type": "entity", "classname": "color_correction",
                                        "keyvalues": kv, "partition": "env", "links": {}}})
    else:
        obj["kind"]["keyvalues"] = kv

    path.write_text(json.dumps(doc, indent=1))
    print(f"grade {GRADE}, sun pitch {SUN_PITCH} yaw {SUN_YAW}, mesh texel {MESH_TEXEL}")


if __name__ == "__main__":
    main()
