"""Authors the Valhalla map's environment into map.json: the tower pulse beams,
man cannon and waterfall effects, the fog aimed at the sun, the colour grade,
the sun and the bake settings. Idempotent: every object is matched by name and
rewritten, so re-running after a fresh h2import needs no hand editing.

usage: h3env.py <project dir>
"""

import json
import math
import sys
import uuid
from pathlib import Path

# H3 riverworld.sky_atm_parameters AtmosphereSettings[*]: all four entries agree
# on the sun, and OverrideRealSunValues is set.
SUN_PITCH, SUN_YAW = 45.0, 25.0
# AtmosphereSettings haze colour.
HAZE = (0.54, 0.58, 0.96)

FOG = {
    # The directional glow is aimed at the sun; the distance fog is the H3 haze.
    "fogangles": f"{SUN_PITCH:g} {SUN_YAW:g} 0",
    "use_angles": "1",
    "fogdirhalfangle": "40",
    "fogdircolorstrength": "0.3",
    "fogdircolor": "255 243 229",
    "fogcolor": " ".join(str(round(c * 255)) for c in HAZE),
    "fogcolorstrength": "0.55",
    "fogdensity": "0.3",
    "fogdistoffset": "1500",
    "foghalfdisttop": "6000",
    "foghalfdistbottom": "10000",
    "fogztop": "65000",
    "fogzbottom": "-800",
    "fogenable": "1",
    "vl_intensity": "0.08",
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

# H3 scenery palette levels\multi\riverworld\fx\* -- in Apex units, but VERIFIED
# against the dump geometry (bounds.py/near.py/falls.py), not taken on faith from
# the TagTool positions: the tower_pulse points sit at the tower BASE (z 149/238)
# while the spires run to z 2076 (north) / 2169 (south), so a beam at the
# TagTool point is buried inside 1400 units of rock; and the waterfall_base
# points are ~2000 units west of the real vahalla_waterfall sheet
# (x 1549..2425, y -5960..-4903, z 76..2142, faces +X, yaw ~10).
FX = [
    # Beam: just above each spire top. Scale 2 -- the stock rays are sized for
    # the Gardens POI spire, ours run 2000+ units and H3's column goes to the sky.
    ("fx_tower_beam_n", "P_moon_POI_gardens_tower_rays", (-124.0, 3950.0, 2090.0), -89.65, "2"),
    ("fx_tower_beam_s", "P_moon_POI_gardens_tower_rays", (-124.0, -5050.0, 2180.0), 90.2, "2"),
    # South falls: sheet on the curtain's lower third, mist + foam at the plunge
    # pool (sheet bottom z 76), fan mist at the top (z 2142).
    ("fx_falls_sheet", "P_moon_bio_waterfall_modular_01", (1974.0, -5360.0, 300.0), 190.0, "1"),
    ("fx_falls_mist", "P_moon_bio_falls_bottom_mist_lrg", (1974.0, -5300.0, 110.0), 190.0, "1"),
    ("fx_falls_foam", "P_env_moon_wf_fallingFoam", (1974.0, -5250.0, 110.0), 190.0, "1"),
    ("fx_falls_top_mist", "P_env_moon_wf_fanMist", (1974.0, -5500.0, 2050.0), 0.0, "1"),
    # North: no sheet mesh exists -- the river drops 75 -> -37 into the lake at
    # y 1800..2600, so foam + splash go on the drop.
    ("fx_drop_foam", "P_env_moon_wf_fallingFoam", (300.0, 2200.0, 10.0), 0.0, "1"),
    ("fx_drop_splash", "P_env_moon_river_splash_small_01", (300.0, 2200.0, 10.0), 0.0, "1"),
]

FX_KEYS = {
    "editorclass": "info_particle_system_clientside",
    "warmup_time": "0",
    "warmup_and_pause": "0",
    "VisibilityFlags": "7",
    "start_active": "1",
    "sound_enabled": "0",
    "kill_if_over_limit": "0",
    "kill_for_replay": "0",
    "in_skybox": "0",
    "ClientSide": "1",
    "scale": "1",
    "model": "mdl/dev/editor_effect_node_256.rmdl",
}

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

# Fine trims keep their own texel; the ground, cliffs and distant terrain drop to
# 16 so the bake carries the strata and the snow line.
TERRAIN_TEXEL = 16.0
DEFAULT_TEXEL = 8.0
TERRAIN_WORDS = ("ground", "cliff", "distant", "snow", "water", "rock")


def yaw_quat(deg):
    r = math.radians(deg) / 2
    return [0.0, 0.0, math.sin(r), math.cos(r)]


def transform(pos, rot):
    return {"position": [float(v) for v in pos], "rotation": [float(v) for v in rot],
            "scale": [1.0, 1.0, 1.0]}


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
        name = o["name"].lower()
        o["kind"]["lightmap_scale"] = TERRAIN_TEXEL if any(w in name for w in TERRAIN_WORDS) else DEFAULT_TEXEL

    fog = by_name["fog"]
    fog["kind"]["keyvalues"].update(FOG)
    fog["transform"] = transform([0, 0, 0], [0, 0, 0, 1])

    sun = by_name["sun"]
    pitch, yaw = math.radians(SUN_PITCH), math.radians(SUN_YAW)
    to_sun = [math.cos(pitch) * math.cos(yaw), math.cos(pitch) * math.sin(yaw), math.sin(pitch)]
    cross = [-to_sun[1], to_sun[0], 0.0]
    q = [*cross, 1.0 + to_sun[2]]
    n = math.sqrt(sum(v * v for v in q))
    sun["transform"]["rotation"] = [v / n for v in q]
    sun["kind"]["intensity"] = 1.3

    # FX entities are fully owned by this script: drop the whole set first so a
    # renamed or removed effect cannot linger at a stale position.
    doc["objects"] = [o for o in doc["objects"] if not o["name"].startswith("fx_")]
    by_name = {o["name"]: o for o in doc["objects"]}
    for name, effect, pos, y, scale in FX:
        kv = dict(FX_KEYS, effect_name=effect, targetname=name, scale=scale)
        obj = by_name.get(name)
        if obj is None:
            obj = {"id": str(uuid.uuid4()), "name": name, "layer": layers["entities"], "parent": None,
                   "transform": transform(pos, yaw_quat(y)), "hidden": False, "locked": False,
                   "kind": {"type": "entity", "classname": "info_particle_system", "keyvalues": kv,
                            "partition": "fx", "links": {}}}
            doc["objects"].append(obj)
        else:
            obj["transform"] = transform(pos, yaw_quat(y))
            obj["kind"]["keyvalues"] = kv

    grade_name = "color_correction"
    kv = {"spawnflags": "0", "minfalloff": "-1", "maxweight": "1", "maxfalloff": "-1",
          "fadeOutDuration": "0", "fadeInDuration": "0", "filename": GRADE}
    obj = by_name.get(grade_name)
    if obj is None:
        doc["objects"].append({"id": str(uuid.uuid4()), "name": grade_name,
                               "layer": layers["entities"], "parent": None,
                               "transform": transform([0, 0, 0], [0, 0, 0, 1]),
                               "hidden": False, "locked": False,
                               "kind": {"type": "entity", "classname": "color_correction", "keyvalues": kv,
                                        "partition": "env", "links": {}}})
    else:
        obj["kind"]["keyvalues"] = kv

    path.write_text(json.dumps(doc, indent=1))
    print(f"{len(FX)} fx entities, grade {GRADE}, sun pitch {SUN_PITCH} yaw {SUN_YAW}, "
          f"bake texel {BAKE['texel_size']} bounces {BAKE['bounces']}")


if __name__ == "__main__":
    main()