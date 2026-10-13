"""Replicates rbake's lightmap chart packing offline.

One chart per mesh map entry: the mesh's projected AABB divided by the texel,
padded, skyline-packed into 1024-texel pages, tallest first. Validated against
the tool's own page count, so a policy can be tuned without a bake per try.

usage: chart_sim.py <project> [texel]
"""

import json
import math
import struct
import sys
from pathlib import Path

import numpy as np

PAGE = 1024
MARGIN = 1
PADDING = 2
MAX_CHART = 16384
ROOM = PAGE - 2 * MARGIN - 2 * PADDING


def read_glb(path):
    b = path.read_bytes()
    jlen = struct.unpack_from("<I", b, 12)[0]
    j = json.loads(b[20:20 + jlen])
    bin_off = 20 + jlen + 8
    out = {}
    for mesh in j["meshes"]:
        for prim in mesh["primitives"]:
            ai = prim["attributes"]["POSITION"]
            acc = j["accessors"][ai]
            bv = j["bufferViews"][acc["bufferView"]]
            start = bin_off + bv.get("byteOffset", 0) + acc.get("byteOffset", 0)
            count = acc["count"]
            pos = np.frombuffer(b, dtype="<f4", count=count * 3, offset=start).reshape(count, 3)
            ni = prim["attributes"].get("NORMAL")
            nrm = None
            if ni is not None:
                acc = j["accessors"][ni]
                bv = j["bufferViews"][acc["bufferView"]]
                start = bin_off + bv.get("byteOffset", 0) + acc.get("byteOffset", 0)
                nrm = np.frombuffer(b, dtype="<f4", count=acc["count"] * 3,
                                    offset=start).reshape(acc["count"], 3)
            ii = prim.get("indices")
            idx = None
            if ii is not None:
                acc = j["accessors"][ii]
                bv = j["bufferViews"][acc["bufferView"]]
                start = bin_off + bv.get("byteOffset", 0) + acc.get("byteOffset", 0)
                comp = j["accessors"][ii]["componentType"]
                dt = {5121: "<u1", 5123: "<u2", 5125: "<u4"}[comp]
                idx = np.frombuffer(b, dtype=dt, count=acc["count"], offset=start).astype(np.int64)
                idx = idx[: len(idx) - len(idx) % 3].reshape(-1, 3)
            out[mesh.get("name", str(len(out)))] = (pos, idx, nrm)
    return out


def chart_size(pos, idx, texel, first_normal=None):
    a = pos[idx[:, 0]]
    b = pos[idx[:, 1]]
    c = pos[idx[:, 2]]
    n = np.cross(b - a, c - a).sum(axis=0)
    # rbake falls back to the surface's own first vertex normal when the
    # summed normals of a closed mesh cancel.
    if np.linalg.norm(n) < 1e-6 and first_normal is not None:
        n = np.asarray(first_normal, dtype=np.float64)
    ln = np.linalg.norm(n)
    if ln < 1e-9:
        return None
    n = n / ln
    ax = np.abs(n)
    if ax[0] >= ax[1] and ax[0] >= ax[2]:
        tangent = np.array([0.0, 1.0, 0.0])
    elif ax[1] >= ax[2]:
        tangent = np.array([0.0, 0.0, 1.0])
    else:
        tangent = np.array([1.0, 0.0, 0.0])
    e1 = tangent - n * float(np.dot(tangent, n))
    e1 /= np.linalg.norm(e1)
    e2 = np.cross(n, e1)
    u = pos @ e1
    v = pos @ e2
    lo_u, hi_u = u.min(), u.max()
    lo_v, hi_v = v.min(), v.max()
    span = max(hi_u - lo_u, hi_v - lo_v)
    if span / texel > ROOM:
        texel = span / ROOM * 1.001
    w = int(min(max(math.ceil((hi_u - lo_u) / texel), 1), MAX_CHART))
    h = int(min(max(math.ceil((hi_v - lo_v) / texel), 1), MAX_CHART))
    return w, h


def pack(charts):
    order = sorted(range(len(charts)), key=lambda i: (charts[i][1], charts[i][0], i), reverse=True)
    pages = []
    tops = np.zeros(PAGE, dtype=np.int64)
    for i in order:
        w, h = charts[i][0] + 2 * MARGIN + 2 * PADDING, charts[i][1] + 2 * MARGIN + 2 * PADDING
        if w > PAGE or h > PAGE:
            raise SystemExit(f"chart {i} {w}x{h} does not fit a page")
        placed = False
        for x in range(0, PAGE - w + 1):
            top = int(tops[x:x + w].max())
            if top + h > PAGE:
                continue
            if not placed or top < best:
                best, bx = top, x
                placed = True
        if placed:
            tops[bx:bx + w] = best + h
            continue
        pages.append(tops)
        tops = np.zeros(PAGE, dtype=np.int64)
        top = 0
        for x in range(0, PAGE - w + 1):
            t = int(tops[x:x + w].max())
            if t < top:
                top, bx = t, x
        tops[bx:bx + w] = top + h
    pages.append(tops)
    return len(pages)


def main():
    proj = Path(sys.argv[1])
    texel = float(sys.argv[2]) if len(sys.argv) > 2 else None
    doc = json.loads((proj / "map.json").read_text())
    global_texel = doc["settings"]["bake"].get("texel_size", 16.0)

    charts = []
    for o in doc["objects"]:
        if o["kind"].get("type") != "mesh":
            continue
        rel = o["kind"]["source"]
        path = proj / rel
        if not path.is_file():
            continue
        mesh = list(read_glb(path).values())[0]
        pos, idx, first_normal = mesh
        if idx is None or len(idx) == 0:
            continue
        t = texel if texel is not None else (
            global_texel if global_texel and global_texel > 0 and math.isfinite(global_texel)
            else o["kind"].get("lightmap_scale", 16.0))
        size = chart_size(pos.astype(np.float64), idx, float(t),
                          None if first_normal is None or len(first_normal) == 0 else first_normal[0])
        if size is None:
            continue
        charts.append((size[0] + 2 * PADDING, size[1] + 2 * PADDING, o["name"]))
    n = pack(charts)
    big = sorted(charts, key=lambda c: -c[0] * c[1])[:12]
    print(f"meshes {len(charts)}  global texel {global_texel}  -> {n} pages of 32")
    for w, h, name in big:
        print(f"  {w:5d}x{h:<5d} {name}")


if __name__ == "__main__":
    main()
