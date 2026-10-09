"""OAT xmodel GLBs (skinned, Y-up) -> rigid Z-up GLBs for the static models a
world places, plus a check of every placement against the clipMap bounds."""

import argparse
import json
import struct
from pathlib import Path

import numpy as np

from iw4world import glb_multi

COMPONENT = {5120: "i1", 5121: "u1", 5122: "<i2", 5123: "<u2", 5125: "<u4", 5126: "<f4"}
WIDTH = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}


def read_glb(path):
    b = Path(path).read_bytes()
    jlen = struct.unpack_from("<I", b, 12)[0]
    doc = json.loads(b[20:20 + jlen])
    blob = b[20 + jlen + 8:]
    return doc, blob


def accessor(doc, blob, index):
    acc = doc["accessors"][index]
    view = doc["bufferViews"][acc["bufferView"]]
    dt = np.dtype(COMPONENT[acc["componentType"]])
    n = WIDTH[acc["type"]]
    start = view.get("byteOffset", 0) + acc.get("byteOffset", 0)
    stride = view.get("byteStride", 0)
    if stride and stride != dt.itemsize * n:
        rows = np.frombuffer(blob, np.uint8, stride * acc["count"], start).reshape(acc["count"], stride)
        arr = rows[:, : dt.itemsize * n].copy().view(dt)
    else:
        arr = np.frombuffer(blob, dt, acc["count"] * n, start)
    return arr.reshape(acc["count"], n) if n > 1 else arr


def y_up_to_z_up(v):
    return np.stack([v[:, 0], -v[:, 2], v[:, 1]], axis=1)


def load_rigid(path):
    """Every primitive of every mesh node, in Z-up model space."""
    doc, blob = read_glb(path)
    parts = {}
    for node in doc.get("nodes", []):
        if "mesh" not in node:
            continue
        for prim in doc["meshes"][node["mesh"]]["primitives"]:
            a = prim["attributes"]
            pos = y_up_to_z_up(accessor(doc, blob, a["POSITION"]).astype(np.float32))
            nrm = y_up_to_z_up(accessor(doc, blob, a["NORMAL"]).astype(np.float32)) if "NORMAL" in a else np.zeros_like(pos)
            uv = accessor(doc, blob, a["TEXCOORD_0"]).astype(np.float32) if "TEXCOORD_0" in a else np.zeros((len(pos), 2), np.float32)
            idx = accessor(doc, blob, prim["indices"]).astype(np.uint32).reshape(-1, 3)
            mat = doc["materials"][prim["material"]]["name"] if "material" in prim else "default"
            parts.setdefault(mat, []).append((pos, nrm, uv, idx))
    merged = []
    for mat, chunks in parts.items():
        base = 0
        P, N, U, I = [], [], [], []
        for pos, nrm, uv, idx in chunks:
            P.append(pos); N.append(nrm); U.append(uv); I.append(idx + base)
            base += len(pos)
        merged.append((mat, np.concatenate(P), np.concatenate(N), np.concatenate(U), np.concatenate(I)))
    return merged


def draw_distance(dump, model):
    """The distance MW2 stops drawing the model at: its last LOD's switch-out."""
    meta = dump / "xmodel" / f"{model}.json"
    lods = json.loads(meta.read_text()).get("lods", []) if meta.is_file() else []
    return float(lods[-1]["distance"]) if lods else 0.0


def lod0_path(dump, model):
    meta = dump / "xmodel" / f"{model}.json"
    if not meta.is_file():
        return None
    lods = json.loads(meta.read_text()).get("lods", [])
    return dump / lods[0]["file"] if lods else None


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("dump", help="OAT zone dump root (has xmodel/ and model_export/)")
    ap.add_argument("world_dir", help="<asset>.world directory")
    ap.add_argument("out_dir")
    args = ap.parse_args()
    dump, world, out = Path(args.dump), Path(args.world_dir), Path(args.out_dir)

    gfx = json.loads((world / "gfxworld.json").read_text())
    clip = json.loads((world / "clipmap.json").read_text())
    models = sorted({s["model"] for s in gfx["staticModels"] if s["model"]})

    written, missing, bounds = {}, [], {}
    (out / "models").mkdir(parents=True, exist_ok=True)
    for name in models:
        src = lod0_path(dump, name)
        if not src or not src.is_file():
            missing.append(name)
            continue
        parts = load_rigid(src)
        if not parts:
            missing.append(name)
            continue
        rel = f"models/{name}.glb"
        (out / rel).write_bytes(glb_multi(name, parts))
        pts = np.concatenate([p[1] for p in parts])
        bounds[name] = (pts.min(0), pts.max(0))
        written[name] = {"glb": rel, "materials": [p[0] for p in parts], "tris": int(sum(len(p[4]) for p in parts)),
                         "draw_distance": draw_distance(dump, name)}

    # A placement is right when the model's box, placed, lands on the clipMap's
    # world-space box for the same instance.
    # Two instances can share an origin (back-to-back lamps), so any clip entry
    # at the same origin may be the match.
    by_origin = {}
    for s in clip["staticModels"]:
        by_origin.setdefault((s["model"], tuple(round(v, 1) for v in s["origin"])), []).append(s["absBounds"])
    checked = off = 0
    for inst in gfx["staticModels"]:
        key = (inst["model"], tuple(round(v, 1) for v in inst["origin"]))
        if key not in by_origin or inst["model"] not in bounds:
            continue
        lo, hi = bounds[inst["model"]]
        corners = np.array([[x, y, z] for x in (lo[0], hi[0]) for y in (lo[1], hi[1]) for z in (lo[2], hi[2])])
        axis = np.array(inst["axis"]) * inst["scale"]
        world_pts = corners @ axis + np.array(inst["origin"])
        got_mid = (world_pts.min(0) + world_pts.max(0)) / 2
        checked += 1
        if not any(np.abs(got_mid - np.array(r["mid"])).max() <= max(8.0, 0.1 * max(r["half"])) for r in by_origin[key]):
            off += 1
    print(f"{len(written)} models written, {len(missing)} missing; placements checked {checked}, off {off}")
    (out / "static_models.json").write_text(json.dumps({"models": written, "missing": missing}, indent=1))


if __name__ == "__main__":
    main()
