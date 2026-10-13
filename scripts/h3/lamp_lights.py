"""Gives an H3 map's self-illum lamp fixtures real baked light.

A self-illum material only makes the lamp glass glow; the bake receives no
light from it. Each lamp (the parts of one fixture, grouped by distance) gets a
spot light just in front of its glowing faces, aimed along their mean normal,
weighted by how bright the self-illum mask is on each face. Colour is the
colour the glass draws in. Local lights take Source `_light` units: gamma
colour, brightness / 255.

usage: lamp_lights.py <project> <dump> [--work <h3mat work dir>] [--spec mat=brightness,range,...]
"""

import argparse
import base64
import json
import sys
import uuid
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "h2"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from h2import import load_part  # noqa: E402
from h3mat import EMISSIVE_GAIN, Sources  # noqa: E402

# One fixture's parts sit within a few units of each other; fixtures are
# metres apart.
JOIN = 64.0
# A fixture's glass is a few units across: the light sits just off it so the
# housing does not shadow it.
STANDOFF = 6.0
INNER, OUTER = 50.0, 85.0

DEFAULT = {
    "metal_doodad_a_illum_cool": (420.0, 250.0),
    "metal_doodad_a_illum_warm": (480.0, 320.0),
}


def bulb_rgb(src, m):
    """The colour the lamp glass draws in: its self-illum bitmap times tint
    times the gain h3mat stores it with, averaged over the lit texels. The
    tint alone is far more saturated than the clipped glass the player sees."""
    tint = np.ones(3)
    for t in m.get("tints", []):
        if t["usage"] == "self_illum":
            tint = np.frombuffer(base64.b64decode(t["rgba"]), np.uint8)[:3].astype(np.float64) / 255.0
    ill = next((t for t in m["textures"] if t["usage"] == "self_illum" and t.get("file")), None)
    img = src.rgba(ill["file"]) if ill else None
    if img is None:
        return tint / max(tint.max(), 1e-6)
    lit = np.clip(img[..., :3] * tint * EMISSIVE_GAIN, 0, 1).reshape(-1, 3)
    on = lit @ np.array([0.2126, 0.7152, 0.0722]) > 0.3
    c = lit[on].mean(axis=0) if on.any() else tint
    return c / max(c.max(), 1e-6)


def illum_weights(src, m, uv, idx):
    """Per triangle, the self-illum mask brightness at its UV centroid."""
    ill = next((t for t in m["textures"] if t["usage"] == "self_illum" and t.get("file")), None)
    img = src.rgba(ill["file"]) if ill else None
    if img is None:
        return np.ones(len(idx))
    lum = img[..., :3] @ np.array([0.2126, 0.7152, 0.0722])
    tiling = ill.get("tiling") or [1, 1]
    c = uv[idx].mean(axis=1) * np.asarray(tiling, float)
    h, w = lum.shape
    x = ((c[:, 0] % 1.0) * w).astype(int).clip(0, w - 1)
    y = ((c[:, 1] % 1.0) * h).astype(int).clip(0, h - 1)
    return lum[y, x]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("project")
    ap.add_argument("dump")
    ap.add_argument("--work", help="h3mat work dir (decoded bitmap cache)")
    ap.add_argument("--spec", action="append", default=[],
                    help="material=brightness,range (Source _light brightness, range in units)")
    a = ap.parse_args()
    proj, dump = Path(a.project), Path(a.dump)
    spec = dict(DEFAULT)
    for s in a.spec:
        k, v = s.split("=")
        b, r = v.split(",")
        spec[k] = (float(b), float(r))

    meta = json.loads((proj / "h2_gameplay.json").read_text())
    centre, scale = np.asarray(meta["centre"], float), float(meta["scale"])
    mats = json.loads((dump / "materials.json").read_text())
    src = Sources(dump, Path(a.work) if a.work else dump.parent / "mat")
    parts = json.loads((dump / "parts.json").read_text())

    emit = []  # (material, weighted centre, weighted normal sum, weight)
    for p in parts:
        if p["material"] not in spec:
            continue
        pos, nrm, uv, idx = load_part(dump, p)
        P = (pos - centre) * scale
        t = P[idx]
        area = 0.5 * np.linalg.norm(np.cross(t[:, 1] - t[:, 0], t[:, 2] - t[:, 0]), axis=1)
        w = area * illum_weights(src, mats[p["material"]], uv, idx)
        if w.sum() <= 0:
            continue
        n = nrm[idx].mean(axis=1)
        n /= np.linalg.norm(n, axis=1, keepdims=True) + 1e-9
        emit.append((p["material"], (t.mean(axis=1) * w[:, None]).sum(0), (n * w[:, None]).sum(0), w.sum()))

    lamps = []
    for mat, cw, nw, w in emit:
        c = cw / w
        for lamp in lamps:
            if lamp["mat"] == mat and np.linalg.norm(lamp["cw"] / lamp["w"] - c) < JOIN:
                lamp["cw"] += cw
                lamp["nw"] += nw
                lamp["w"] += w
                break
        else:
            lamps.append({"mat": mat, "cw": cw.copy(), "nw": nw.copy(), "w": w})

    doc = json.loads((proj / "map.json").read_text())
    layers = {l["name"]: l["id"] for l in doc["layers"]}
    doc["objects"] = [o for o in doc["objects"] if not o["name"].startswith("lamp_light_")]
    lamps.sort(key=lambda l: (l["mat"], *np.round(l["cw"] / l["w"]).tolist()))
    for i, lamp in enumerate(lamps):
        c = lamp["cw"] / lamp["w"]
        n = lamp["nw"] / (np.linalg.norm(lamp["nw"]) + 1e-9)
        bright, rng = spec[lamp["mat"]]
        # Lights shine along local -Z.
        axis = np.cross([0.0, 0.0, -1.0], n)
        q = np.array([*axis, 1.0 + float(np.dot([0.0, 0.0, -1.0], n))])
        q = q / np.linalg.norm(q) if np.linalg.norm(q) > 1e-6 else np.array([1.0, 0.0, 0.0, 0.0])
        tag = "cool" if "cool" in lamp["mat"] else "warm"
        doc["objects"].append({
            "id": str(uuid.uuid4()), "name": f"lamp_light_{tag}_{i}", "layer": layers["lights"], "parent": None,
            "transform": {"position": [float(v) for v in c + n * STANDOFF], "rotation": [float(v) for v in q],
                          "scale": [1.0, 1.0, 1.0]},
            "hidden": False, "locked": False,
            "kind": {"type": "light", "kind": "spot", "color": [round(float(v), 4) for v in bulb_rgb(src, mats[lamp["mat"]])],
                     "intensity": bright, "range": rng, "inner_angle": INNER, "outer_angle": OUTER,
                     "size": [0.0, 0.0], "cast_shadows": True, "baked": True, "realtime": False, "keyvalues": {}}})
    (proj / "map.json").write_text(json.dumps(doc, indent=1))
    down = sum(1 for l in lamps if (l["nw"] / np.linalg.norm(l["nw"]))[2] < -0.5)
    print(f"{len(lamps)} lamp spots ({down} aimed down), from {len(emit)} glowing parts")


if __name__ == "__main__":
    main()
