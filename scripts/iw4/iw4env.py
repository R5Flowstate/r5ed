"""Carries an IW4 map's own environment into an r5ed project: its primary lights,
its exponential fog, its film colour grade and a sky tint taken from the fog colour.
Idempotent: every object it owns is named iw4_* and rewritten on each run.

usage: iw4env.py <project dir> <map dump dir> <world dir> --grade-out <file.raw_hdr>

  <map dump dir>  the OAT dump of the map zone (vision/<map>.vision, when the map
                  has one, and maps/createart/<map>_art.gsc)
  <world dir>     the <map>.d3dbsp.world dump (comworld.json)

The grade is baked for the S21 post chain: the volume holds
tonemap^-1(film(srgb(tonemap(x)))), so the stock tonemap shows the S21 frame
through MW2's own film pass. The grade file has to ship with the map at
materials/correction/<stem>.raw_hdr, the path the colour correction names.
"""

import argparse
import json
import math
import re
import sys
import uuid
from pathlib import Path

import numpy as np

import os

sys.path.insert(0, os.environ.get("CC_RAW_HDR_DIR",
                                  str(Path(__file__).resolve().parents[3] / "bspconv-cafe" / "tools" / "tf2fx")))
import cc_raw_hdr as cc  # noqa: E402

# ComWorld primary light types.
LIGHT_SPOT, LIGHT_OMNI = 2, 3
# rcomp reads a local light's brightness as value / 255 at the light, falling off as
# 1 / (1 + d / range)^2 under a window that reaches zero at 2 * range; at d = range
# that leaves 0.25 * 0.879 of it.
AT_RANGE = 0.25 * (1 - 0.5 ** 4) ** 2


def vision(path):
    out = {}
    for line in Path(path).read_text().splitlines():
        m = re.match(r'\s*(\w+)\s+"([^"]*)"', line)
        if m:
            out[m.group(1)] = [float(v) for v in m.group(2).split()]
    return out


def exp_fog(art):
    """setExpFog( start, halfway, r, g, b, max opacity, transition )."""
    m = re.search(r"setExpFog\(([^)]*)\)", Path(art).read_text())
    if not m:
        return None
    v = [float(x) for x in m.group(1).split(",")]
    return {"start": v[0], "halfway": v[1], "color": v[2:5], "opacity": v[5]}


def film(v, neutral_tint=False):
    """MW2's alternate film pass (r_filmAltShader, the default): desaturation runs
    from the dark value at black to the main one at white, the tint is a quadratic
    Bezier from the dark tint through the medium one to the light one, and contrast
    and brightness scale and bias the tinted colour about mid grey."""
    g = lambda k, d: v.get(k, d)
    contrast, brightness = g("r_filmContrast", [1.0])[0], g("r_filmBrightness", [0.0])[0]
    desat, desat_dark = g("r_filmDesaturation", [0.0])[0], g("r_filmDesaturationDark", [0.0])[0]
    dark = np.array(g("r_filmDarkTint", [1, 1, 1]))
    mid = np.array(g("r_filmMediumTint", [1, 1, 1]))
    light = np.array(g("r_filmLightTint", [1, 1, 1]))
    invert = g("r_filmInvert", [0])[0] != 0
    if neutral_tint:
        luma = lambda t: np.full(3, float(t @ np.array([0.299, 0.587, 0.114])))
        dark, mid, light = luma(dark), luma(mid), luma(light)

    def apply(c):
        lum = (c * np.array([0.299, 0.587, 0.114])).sum(axis=-1, keepdims=True)
        d = desat_dark + (desat - desat_dark) * lum
        grey = c + (lum - c) * d
        tint = dark + 2.0 * (mid - dark) * lum + (dark - 2.0 * mid + light) * lum * lum
        out = grey * tint * contrast + brightness + 0.5 * (1.0 - contrast)
        return 1.0 - out if invert else out

    return apply


def bake_grade(v, dst, linear_end=0.5, neutral_tint=False):
    grade = film(v, neutral_tint)

    def frame(x, _lut=None):
        shown = cc.srgb_encode(cc.s21_tonemap(x, linear_end))
        return cc.srgb_decode(np.clip(grade(shown), 0.0, 1.0))

    cc.tf2_frame = frame
    p = np.arange(cc.S21_SIZE) / (cc.S21_SIZE - 1.0)
    b, g, r = np.meshgrid(p, p, p, indexing="ij")
    x = cc.pq_decode(np.stack([r, g, b], axis=-1)) / cc.SCENE_TO_PQ
    h = cc.s21_tonemap_inverse(frame(x), linear_end, 1.0 / cc.SCENE_TO_PQ)
    # Node sampling only: the least-squares refit diverges on this curve and maps
    # bright warm surfaces to pink.
    codes = cc.pq_encode(h * cc.SCENE_TO_PQ)
    blob = cc.pack(codes)
    Path(dst).write_bytes(blob)
    cc.verify(None, blob, linear_end)
    return len(blob)


def quat_between(a, b):
    a, b = np.asarray(a, float), np.asarray(b, float)
    a, b = a / np.linalg.norm(a), b / np.linalg.norm(b)
    d = float(a @ b)
    if d < -0.999999:
        axis = np.cross(a, [1.0, 0.0, 0.0])
        if np.linalg.norm(axis) < 1e-6:
            axis = np.cross(a, [0.0, 1.0, 0.0])
        axis /= np.linalg.norm(axis)
        return [*axis.tolist(), 0.0]
    q = np.array([*np.cross(a, b), 1.0 + d])
    return (q / np.linalg.norm(q)).tolist()


def local_light(i, pl, layer):
    """A ComWorld spot or omni light. MW2 stores `dir` pointing back at the light
    and lights linearly (light_point_linear) or quadratically to `radius`; the
    brightness matches MW2's value halfway out, and range = radius / 2 ends the
    light where MW2's does."""
    color = np.array(pl["color"], float)
    peak = float(color.max())
    if peak <= 0.0 or pl["radius"] <= 0.0:
        return None
    half = 0.25 if "quadratic" in pl.get("defName", "") else 0.5
    spot = pl["type"] == LIGHT_SPOT
    outer = math.degrees(math.acos(max(-1.0, min(1.0, pl["cosHalfFovOuter"])))) if spot else 0.0
    inner = math.degrees(math.acos(max(-1.0, min(1.0, pl["cosHalfFovInner"])))) if spot else 0.0
    forward = [-c for c in pl["dir"]]
    return {"id": str(uuid.uuid4()), "name": f"iw4_light_{i}", "layer": layer, "parent": None,
            "transform": {"position": [float(c) for c in pl["origin"]],
                          "rotation": quat_between([0, 0, -1], forward) if spot else [0.0, 0.0, 0.0, 1.0],
                          "scale": [1.0, 1.0, 1.0]},
            "hidden": False, "locked": False,
            "kind": {"type": "light", "kind": "spot" if spot else "point",
                     # rcomp decodes a local light's colour from gamma.
                     "color": [float((c / peak) ** (1 / 2.2)) for c in color],
                     "intensity": round(255.0 * half * peak / AT_RANGE, 1),
                     "range": float(pl["radius"]) / 2.0, "inner_angle": min(inner, outer), "outer_angle": outer,
                     "size": [0.0, 0.0], "cast_shadows": bool(pl.get("canUseShadowMap", 1)),
                     "baked": True, "realtime": False, "keyvalues": {}}}


# Props that draw a lit fixture and the baked point light each one gets: range and
# brightness, warm fluorescent white. Small pools around each fixture, not area light:
# MW2 lit these with its lightmap alone.
FIXTURES = {
    "com_two_light_fixture_on": (110.0, 420.0), "me_lightfluohang_on": (110.0, 420.0),
    "me_lightfluohang_double_on": (130.0, 480.0), "bc_militarytentceilinglight_on": (100.0, 380.0),
    "ma_light_modern_on": (100.0, 380.0), "usa_light_exposed_can_on": (90.0, 340.0),
    "com_lightbox_on": (80.0, 300.0), "com_cafe_light_part1_on": (90.0, 340.0),
    "ch_stagelight01_on": (130.0, 480.0), "com_studio_light_on": (130.0, 480.0),
    "me_streetlightlone_on": (160.0, 520.0), "me_streetlightdual_on": (160.0, 520.0),
    "ch_street_wall_light_01_on": (120.0, 440.0),
}
# World surfaces that are lit fixtures: each connected panel gets a small point light
# just in front of it.
WORLD_FIXTURES = {
    "wc/me_office_ceiling_light_2_on": (110.0, 420.0), "wc/ap_light_round_embedded": (90.0, 340.0),
    "wc/ap_light_embedded_round_glow": (90.0, 340.0), "wc/ap_linear_light": (110.0, 420.0),
    "wc/ap_interior_closet_lights": (80.0, 300.0),
}


def glb_tris(path):
    import struct
    data = Path(path).read_bytes()
    jlen = struct.unpack_from("<I", data, 12)[0]
    doc = json.loads(data[20:20 + jlen])
    base = 20 + jlen + 8

    def acc(i, comps):
        a = doc["accessors"][i]
        bv = doc["bufferViews"][a["bufferView"]]
        dt = {5126: np.float32, 5125: np.uint32, 5123: np.uint16}[a["componentType"]]
        arr = np.frombuffer(data, dt, a["count"] * comps, base + bv.get("byteOffset", 0) + a.get("byteOffset", 0))
        return arr.reshape(-1, comps) if comps > 1 else arr

    out = []
    for mesh in doc["meshes"]:
        for prim in mesh["primitives"]:
            pos = acc(prim["attributes"]["POSITION"], 3).astype(np.float64)
            nrm = acc(prim["attributes"]["NORMAL"], 3).astype(np.float64)
            idx = acc(prim["indices"], 1).astype(np.int64).reshape(-1, 3)
            out.append((pos[idx].mean(axis=1), nrm[idx].mean(axis=1)))
    return out


def world_fixture_lights(manifest_dir, layer, near, start):
    """A point light 8 units in front of each panel of a lit world fixture surface;
    triangles within 24 units of each other are one panel, and a panel within 32 units
    of a light already placed (MW2's own or another surface's glow) is skipped."""
    man = json.loads((Path(manifest_dir) / "world_manifest.json").read_text())
    placed, out = [np.asarray(p, float) for p in near], []
    for m in man["meshes"]:
        if m["material"] not in WORLD_FIXTURES:
            continue
        rng, intensity = WORLD_FIXTURES[m["material"]]
        for cents, nrms in glb_tris(Path(manifest_dir) / m["glb"]):
            group = -np.ones(len(cents), int)
            for i in range(len(cents)):
                if group[i] >= 0:
                    continue
                group[i] = i
                todo = [i]
                while todo:
                    j = todo.pop()
                    close = np.where((group < 0) & (np.linalg.norm(cents - cents[j], axis=1) < 24.0))[0]
                    group[close] = i
                    todo.extend(close.tolist())
            for g in np.unique(group):
                sel = group == g
                n = nrms[sel].mean(axis=0)
                n = n / max(np.linalg.norm(n), 1e-6)
                pos = cents[sel].mean(axis=0) + n * 8.0
                if any(np.linalg.norm(pos - q) < 32.0 for q in placed):
                    continue
                placed.append(pos)
                out.append({"id": str(uuid.uuid4()), "name": f"iw4_fixture_{start + len(out)}", "layer": layer,
                            "parent": None,
                            "transform": {"position": pos.round(2).tolist(), "rotation": [0.0, 0.0, 0.0, 1.0],
                                          "scale": [1.0, 1.0, 1.0]},
                            "hidden": False, "locked": False,
                            "kind": {"type": "light", "kind": "point", "color": FIXTURE_COLOR,
                                     "intensity": intensity, "range": rng, "inner_angle": 0.0, "outer_angle": 0.0,
                                     "size": [0.0, 0.0], "cast_shadows": True, "baked": True, "realtime": False,
                                     "keyvalues": {}}})
    return out
FIXTURE_COLOR = [1.0, 0.96, 0.88]


def smd_bounds(smd):
    """A model's bounds on the placement axes: studiomdl's SMD holds (y, -x, z)."""
    pts = []
    lines = smd.read_text(errors="replace").splitlines()
    at = lines.index("triangles") + 1
    while at + 3 < len(lines) and lines[at] != "end":
        for k in (1, 2, 3):
            x, y, z = (float(v) for v in lines[at + k].split()[1:4])
            pts.append((-y, x, z))
        at += 4
    pts = np.array(pts)
    return pts.min(axis=0), pts.max(axis=0)


def rotate(q, v):
    x, y, z, w = q
    u = np.array([x, y, z])
    v = np.asarray(v, float)
    return v + 2.0 * np.cross(u, np.cross(u, v) + w * v)


def fixture_lights(doc, props_src, layer, near):
    """A point light under each lit fixture prop, clear of its own mesh (props block
    baked light), unless one of MW2's own lights is already within 96 units."""
    bounds, out = {}, []
    for o in doc["objects"]:
        k = o["kind"]
        if k["type"] != "prop":
            continue
        name = k["model"].split("/")[-1].rsplit(".", 1)[0]
        if name not in FIXTURES:
            continue
        if name not in bounds:
            smd = Path(props_src) / name / "ref.smd"
            bounds[name] = smd_bounds(smd) if smd.is_file() else None
        if bounds[name] is None:
            continue
        lo, hi = bounds[name]
        t = o["transform"]
        scale = float(t.get("scale", [1.0])[0])
        local = np.array([(lo[0] + hi[0]) / 2, (lo[1] + hi[1]) / 2, lo[2] - 6.0]) * scale
        pos = np.asarray(t["position"], float) + rotate(t["rotation"], local)
        if any(np.linalg.norm(pos - n) < 96.0 for n in near):
            continue
        rng, intensity = FIXTURES[name]
        out.append({"id": str(uuid.uuid4()), "name": f"iw4_fixture_{len(out)}", "layer": layer, "parent": None,
                    "transform": {"position": pos.round(2).tolist(), "rotation": [0.0, 0.0, 0.0, 1.0],
                                  "scale": [1.0, 1.0, 1.0]},
                    "hidden": False, "locked": False,
                    "kind": {"type": "light", "kind": "point", "color": FIXTURE_COLOR, "intensity": intensity,
                             "range": rng, "inner_angle": 0.0, "outer_angle": 0.0, "size": [0.0, 0.0],
                             "cast_shadows": True, "baked": True, "realtime": False, "keyvalues": {}}})
    return out


def fog_entity(fog, sun_angles, layer):
    rgb = " ".join(str(round(c * 255)) for c in fog["color"])
    kv = {
        "fogenable": "1",
        "fogcolor": rgb,
        "fogcolorstrength": "1",
        "fogdistoffset": str(round(fog["start"])),
        "foghalfdisttop": str(round(fog["halfway"])),
        "foghalfdistbottom": str(round(fog["halfway"])),
        "fogdensity": f"{fog['opacity']:g}",
        "fogztop": "65000",
        "fogzbottom": "-800",
        # MW2's exponential fog has no sun glow.
        "fogangles": sun_angles,
        "use_angles": "1",
        "fogdirhalfangle": "80",
        "fogdircolor": rgb,
        "fogdircolorstrength": "0",
        "enable_character_fog": "0",
        "disableWhenVfogEnabled": "0",
        "forceontosky": "0",
        "minfadetime": "3",
        "spawnflags": "0",
        "scale": "1",
        "targetname": "iw4_fog",
    }
    return {"id": str(uuid.uuid4()), "name": "iw4_fog", "layer": layer, "parent": None,
            "transform": {"position": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0]},
            "hidden": False, "locked": False,
            "kind": {"type": "entity", "classname": "env_fog_controller", "keyvalues": kv, "partition": None, "links": {}}}


def grade_entity(stem, layer):
    kv = {"spawnflags": "0", "minfalloff": "-1", "maxweight": "1", "maxfalloff": "-1",
          "fadeOutDuration": "0", "fadeInDuration": "0", "filename": f"materials/correction/{stem}.raw_hdr"}
    return {"id": str(uuid.uuid4()), "name": "iw4_grade", "layer": layer, "parent": None,
            "transform": {"position": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0]},
            "hidden": False, "locked": False,
            "kind": {"type": "entity", "classname": "color_correction", "keyvalues": kv, "partition": None, "links": {}}}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("project")
    ap.add_argument("dump")
    ap.add_argument("world")
    ap.add_argument("--grade-out", required=True)
    ap.add_argument("--neutral-tint", action="store_true", help="drop the film tints' colour, keep their brightness")
    ap.add_argument("--props-src", help="iw4props source dir (<model>/ref.smd): lit fixture props get baked lights")
    ap.add_argument("--world-glbs", help="iw4world output dir (world_manifest.json): lit fixture surfaces get baked lights")
    a = ap.parse_args()
    proj, dump, world = Path(a.project), Path(a.dump), Path(a.world)
    doc = json.loads((proj / "map.json").read_text())
    stem = doc["meta"]["stem"]
    base = dump.name
    layers = {l["name"]: l["id"] for l in doc["layers"]}
    lights_layer = layers.get("lights", doc["layers"][0]["id"])
    ents_layer = layers.get("entities", doc["layers"][0]["id"])

    doc["objects"] = [o for o in doc["objects"] if not o["name"].startswith("iw4_")]
    lights = json.loads((world / "comworld.json").read_text())["primaryLights"]
    made = [local_light(i, pl, lights_layer) for i, pl in enumerate(lights) if pl["type"] in (LIGHT_SPOT, LIGHT_OMNI)]
    made = [m for m in made if m]
    doc["objects"].extend(made)
    fixtures = []
    if a.props_src:
        fixtures = fixture_lights(doc, a.props_src, lights_layer, [np.asarray(m["transform"]["position"]) for m in made])
        doc["objects"].extend(fixtures)
    if a.world_glbs:
        near = [np.asarray(m["transform"]["position"]) for m in made + fixtures]
        panels = world_fixture_lights(a.world_glbs, lights_layer, near, len(fixtures))
        doc["objects"].extend(panels)
        fixtures += panels

    ents = dump / "maps" / "mp" / f"{base}.d3dbsp.ents"
    sun_angles = "0 90 0"
    if ents.exists():
        m = re.search(r'"sundirection"\s+"([^"]+)"', ents.read_text())
        if m:
            sun_angles = m.group(1)
    fog = exp_fog(dump / "maps" / "createart" / f"{base}_art.gsc")
    if fog:
        doc["objects"].append(fog_entity(fog, sun_angles, ents_layer))
        # Radiant's skycolor is a saturated blue that MW2's radiosity washes out; the
        # S21 bake keeps it, and the film grade then turns cool sunlit surfaces purple.
        peak = max(fog["color"])
        if peak > 0.0:
            doc["settings"]["bake"]["sky_color"] = [round(c / peak, 6) for c in fog["color"]]
    # A map whose zone ships no vision of its own leaves the film dvars at their
    # defaults, so an absent file is not an error: {} is that same default film.
    v = vision(dump / "vision" / f"{base}.vision") if (dump / "vision" / f"{base}.vision").exists() else {}
    size = bake_grade(v, a.grade_out, neutral_tint=a.neutral_tint)
    doc["objects"].append(grade_entity(stem, ents_layer))

    (proj / "map.json").write_text(json.dumps(doc))
    print(f"{len(made)} primary lights, {len(fixtures)} fixture lights, fog {fog}, grade {a.grade_out} ({size} bytes) "
          f"-> materials/correction/{stem}.raw_hdr")


if __name__ == "__main__":
    main()
