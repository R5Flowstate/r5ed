"""Rebuilds an h2import-made project's world meshes from the h3dump parts with
what h3mat decided per material:

  layer_split.json  terrain triangles go to the material of the layer that
                    dominates the blend mask under them
  uv_factor.json    UV0 rescaled where a material's normal map repeats less
                    often than its diffuse
  --water           the only materials that are water volumes; everything
                    else (river and lake beds included) is solid ground

Objects keep their ids, layers and lightmap scale; spawns and other entities
are untouched.
"""

import argparse
import json
import sys
from pathlib import Path

import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "iw4"))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "h2"))
from iw4world import glb_multi  # noqa: E402
from h2import import APEX_WATER, load_part  # noqa: E402


def mask_at(split, uv):
    mask = np.asarray(Image.open(split["mask"]).convert("RGBA")).astype(np.float32) / 255.0
    h, w = mask.shape[:2]
    px = np.clip((uv[:, 0] % 1.0) * w, 0, w - 1).astype(int)
    py = np.clip((uv[:, 1] % 1.0) * h, 0, h - 1).astype(int)
    return mask[py, px]


def choose(split, uv):
    """Per triangle, the index into split['materials']: the strongest layer
    for a per-layer split, the pair of the two strongest layers for a blend
    split (split['pairs'])."""
    if split["mask"] is None:
        return np.zeros(len(uv), np.int64)
    m = mask_at(split, uv)
    if "pairs" not in split:
        return np.stack([m[:, c] for c in split["channels"]], axis=1).argmax(axis=1)
    chans = sorted({c for pr in split["pairs"] for c in pr})
    w = np.stack([m[:, c] for c in chans], axis=1)
    order = np.argsort(-w, axis=1)
    a, b = np.array(chans)[order[:, 0]], np.array(chans)[order[:, 1]]
    lo, hi = np.minimum(a, b), np.maximum(a, b)
    lookup = {tuple(sorted(pr)): i for i, pr in enumerate(split["pairs"])}
    return np.array([lookup[(int(x), int(y))] for x, y in zip(lo, hi)], np.int64)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("dump")
    ap.add_argument("project")
    ap.add_argument("mat_work", help="h3mat work dir (material_map.json, layer_split.json, uv_factor.json)")
    ap.add_argument("--water", default="riverworld_water_rough,vahalla_waterfall")
    args = ap.parse_args()

    dump, proj, work = Path(args.dump), Path(args.project), Path(args.mat_work)
    doc = json.loads((proj / "map.json").read_text())
    meta = json.loads((proj / "h2_gameplay.json").read_text())
    centre, S = np.asarray(meta["centre"], np.float64), float(meta["scale"])
    names = json.loads((work / "material_map.json").read_text())
    split = json.loads((work / "layer_split.json").read_text())
    factor = json.loads((work / "uv_factor.json").read_text())
    water = set(filter(None, args.water.split(",")))

    parts = json.loads((dump / "parts.json").read_text())
    by_mat = {}
    for p in parts:
        if "skies" in p["group"]:
            continue
        by_mat.setdefault(p["material"], []).append(p)

    objects = {o["name"]: o for o in doc["objects"] if o["kind"]["type"] == "mesh"}
    for name, o in sorted(objects.items()):
        chunks = by_mat.get(name)
        if not chunks:
            print(f"{name}: no dump parts, kept as is")
            continue
        P, N, U, I, base = [], [], [], [], 0
        for p in chunks:
            pos, nrm, uv, idx = load_part(dump, p)
            P.append((pos.astype(np.float64) - centre) * S)
            N.append(nrm)
            U.append(uv)
            I.append(idx.astype(np.uint32) + base)
            base += len(pos)
        P = np.concatenate(P).astype(np.float32)
        N = np.concatenate(N).astype(np.float32)
        U = np.concatenate(U).astype(np.float32)
        I = np.concatenate(I)
        if name in factor:
            U = U * np.asarray(factor[name], np.float32)

        kind = o["kind"]
        if name in split:
            sp = split[name]
            raw = U
            layer = choose(sp, raw[I].mean(axis=1))
            prims, mmap = [], {}
            for li, mat in enumerate(sp["materials"]):
                sel = layer == li
                if not sel.any():
                    continue
                gname = f"{name}__{li}"
                prims.append((gname, P, N, U, I[sel]))
                mmap[gname] = mat
            counts = {sp["materials"][li].split("/")[-1]: int((layer == li).sum()) for li in range(len(sp["materials"]))}
            print(f"{name}: split {counts}")
        else:
            target = names.get(name, next(iter(kind["material_map"].values())))
            prims, mmap = [(name, P, N, U, I)], {name: target}
        rel = kind["source"]
        colors = None
        if name in split and "pairs" in split[name]:
            # The blend material reads vertex alpha; 0.5 leaves the mix to the mask.
            c = np.full((len(P), 4), 255, np.uint8)
            c[:, 3] = 128
            colors = [c] * len(prims)
        (proj / rel).write_bytes(glb_multi(name, prims, colors=colors))
        is_water = name in water
        kind.update({"material_map": mmap, "contents": APEX_WATER if is_water else 0, "cast_shadows": not is_water})

    (proj / "map.json").write_text(json.dumps(doc, indent=1))


if __name__ == "__main__":
    main()
