"""IW4 world dump (OpenAssetTools-cafe `.world/`) -> per-material GLB meshes.

Reads gfxworld.json + vertices.bin + indices.bin and writes one GLB per world
material, plus a JSON manifest the r5ed importer consumes.
"""

import argparse
import json
import struct
from pathlib import Path

import numpy as np

VERTEX = np.dtype([
    ("xyz", "<f4", 3),
    ("binormal_sign", "<f4"),
    ("color", "u1", 4),
    ("uv", "<f4", 2),
    ("lmap_uv", "<f4", 2),
    ("normal", "u1", 4),
    ("tangent", "u1", 4),
])
assert VERTEX.itemsize == 44


def unpack_unit_vec(packed):
    scale = (packed[:, 3].astype(np.float32) + 192.0) / 32385.0
    return (packed[:, :3].astype(np.float32) - 127.0) * scale[:, None]


def load_world(world_dir):
    world_dir = Path(world_dir)
    gfx = json.loads((world_dir / "gfxworld.json").read_text())
    verts = np.fromfile(world_dir / "vertices.bin", dtype=VERTEX)
    indices = np.fromfile(world_dir / "indices.bin", dtype="<u2")
    if len(verts) != gfx["vertexCount"] or len(indices) != gfx["indexCount"]:
        raise SystemExit("vertex/index blob sizes do not match gfxworld.json")
    return gfx, verts, indices


def surface_triangles(surf, indices):
    base = surf["baseIndex"]
    tri = indices[base:base + 3 * surf["triCount"]].astype(np.int64)
    return tri.reshape(-1, 3) + surf["firstVertex"]


def winding_sign(gfx, verts, indices, sample=4000):
    """+1 when (b-a)x(c-a) agrees with the vertex normal, i.e. CCW front faces."""
    pos = verts["xyz"].astype(np.float64)
    nrm = unpack_unit_vec(verts["normal"])
    agree = total = 0
    for surf in gfx["surfaces"][: gfx["ranges"]["litOpaque"][1]]:
        tris = surface_triangles(surf, indices)
        a, b, c = pos[tris[:, 0]], pos[tris[:, 1]], pos[tris[:, 2]]
        fn = np.cross(b - a, c - a)
        d = np.einsum("ij,ij->i", fn, nrm[tris[:, 0]])
        agree += int((d > 0).sum())
        total += int((d != 0).sum())
        if total >= sample:
            break
    return agree, total


def glb_bytes(name, positions, normals, uvs, colors, tris):
    """One mesh, one primitive. Colors are normalized u8 RGBA."""
    return glb_multi(name, [(name, positions, normals, uvs, tris)], colors=[colors])


def glb_multi(name, parts, colors=None):
    """One mesh node with a primitive per (material, positions, normals, uvs, tris)."""
    blobs = []
    views = []
    accessors = []

    def add(arr, target, comp, kind, normalized=False, minmax=False):
        data = np.ascontiguousarray(arr).tobytes()
        offset = sum(len(b) for b in blobs)
        blobs.append(data + b"\0" * (-len(data) % 4))
        views.append({"buffer": 0, "byteOffset": offset, "byteLength": len(data), "target": target})
        acc = {"bufferView": len(views) - 1, "componentType": comp, "count": len(arr), "type": kind}
        if normalized:
            acc["normalized"] = True
        if minmax:
            acc["min"] = arr.min(axis=0).tolist()
            acc["max"] = arr.max(axis=0).tolist()
        accessors.append(acc)
        return len(accessors) - 1

    materials, prims = [], []
    for i, (material, positions, normals, uvs, tris) in enumerate(parts):
        attrs = {
            "POSITION": add(positions.astype("<f4"), 34962, 5126, "VEC3", minmax=True),
            "NORMAL": add(normals.astype("<f4"), 34962, 5126, "VEC3"),
            "TEXCOORD_0": add(uvs.astype("<f4"), 34962, 5126, "VEC2"),
        }
        if colors is not None:
            attrs["COLOR_0"] = add(colors[i].astype("u1"), 34962, 5121, "VEC4", normalized=True)
        idx = add(tris.reshape(-1).astype("<u4"), 34963, 5125, "SCALAR")
        materials.append({"name": material})
        prims.append({"attributes": attrs, "indices": idx, "material": i, "mode": 4})
    doc = {
        "asset": {"version": "2.0", "generator": "r5ed iw4world"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0, "name": name}],
        "materials": materials,
        "meshes": [{"name": name, "primitives": prims}],
        "buffers": [{"byteLength": sum(len(b) for b in blobs)}],
        "bufferViews": views,
        "accessors": accessors,
    }
    js = json.dumps(doc, separators=(",", ":")).encode()
    js += b" " * (-len(js) % 4)
    bin_ = b"".join(blobs)
    total = 12 + 8 + len(js) + 8 + len(bin_)
    return (struct.pack("<III", 0x46546C67, 2, total)
            + struct.pack("<II", len(js), 0x4E4F534A) + js
            + struct.pack("<II", len(bin_), 0x004E4942) + bin_)


def surface_class(i, ranges):
    if ranges["litOpaque"][0] <= i < ranges["litOpaque"][1]:
        return "opaque"
    if ranges["litTrans"][0] <= i < ranges["litTrans"][1]:
        return "trans"
    if ranges["shadowCaster"][0] <= i < ranges["shadowCaster"][1]:
        return "shadow_caster"
    if ranges["emissive"][0] <= i < ranges["emissive"][1]:
        return "emissive"
    return "other"


def is_sky(material):
    """The sky shell: drawn by the engine's sky pass in IW4, and far outside
    the extents an Apex map may have."""
    base = material.split("/")[-1]
    return base.startswith("sky_") or base == "sky"


def safe_name(material):
    return material.replace("/", "_").replace("\\", "_").replace("~", "").replace("&", "_").replace("#", "_")


def export(world_dir, out_dir, flip, play_box=None):
    gfx, verts, indices = load_world(world_dir)
    out_dir = Path(out_dir)
    (out_dir / "world").mkdir(parents=True, exist_ok=True)

    sky_surfs = {s for sky in gfx["skies"] for s in sky["surfaces"]}
    submodel_surfs = set()
    for bm in gfx["brushModels"][1:]:
        submodel_surfs.update(range(bm["startSurfIndex"], bm["startSurfIndex"] + bm["surfaceCount"]))

    pos = verts["xyz"]
    nrm = unpack_unit_vec(verts["normal"])
    uv = verts["uv"]
    # D3DCOLOR is stored B, G, R, A. Only alpha carries over: MW2 fades its blended layers
    # by vertex alpha, while S21 world shaders read vertex red as an id, not a tint.
    col = np.full((len(verts), 4), 255, np.uint8)
    col[:, 3] = verts["color"][:, 3]

    groups = {}
    for i, surf in enumerate(gfx["surfaces"]):
        if i in sky_surfs or i in submodel_surfs or surf["triCount"] == 0 or is_sky(surf["material"]):
            continue
        tris = surface_triangles(surf, indices)
        centre = pos[tris.reshape(-1)].mean(axis=0)
        zone = "play" if play_box is None or (play_box[0] <= centre[0] <= play_box[2] and play_box[1] <= centre[1] <= play_box[3]) else "far"
        key = (surf["material"], surface_class(i, gfx["ranges"]), zone)
        groups.setdefault(key, []).append(tris)

    manifest = {"source": str(world_dir), "flipped_winding": flip, "meshes": [], "sky": gfx["skies"],
                "submodels": [], "bounds": gfx["bounds"]}

    def write_mesh(rel, material, tris, klass):
        used, remap = np.unique(tris, return_inverse=True)
        local = remap.reshape(-1, 3).astype(np.uint32)
        if flip:
            local = local[:, [0, 2, 1]]
        (out_dir / rel).parent.mkdir(parents=True, exist_ok=True)
        colors = col[used] if klass == "trans" else np.full((len(used), 4), 255, np.uint8)
        (out_dir / rel).write_bytes(glb_bytes(material, pos[used], nrm[used], uv[used], colors, local))
        return {"glb": rel, "material": material, "class": klass, "tris": int(len(local)), "verts": int(len(used))}

    for (material, klass, zone), tri_lists in sorted(groups.items()):
        rel = f"world/{safe_name(material)}__{klass}__{zone}.glb"
        entry = write_mesh(rel, material, np.concatenate(tri_lists), klass)
        entry["zone"] = zone
        manifest["meshes"].append(entry)

    for n, bm in enumerate(gfx["brushModels"][1:], start=1):
        parts = []
        for s in range(bm["startSurfIndex"], bm["startSurfIndex"] + bm["surfaceCount"]):
            surf = gfx["surfaces"][s]
            if surf["triCount"]:
                rel = f"submodels/{n}/{s}_{safe_name(surf['material'])}.glb"
                parts.append(write_mesh(rel, surf["material"], surface_triangles(surf, indices), surface_class(s, gfx["ranges"])))
        manifest["submodels"].append({"index": n, "bounds": bm["bounds"], "meshes": parts})

    (out_dir / "world_manifest.json").write_text(json.dumps(manifest, indent=1))
    return manifest


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("world_dir", help="<asset>.world directory from the OAT world dump")
    ap.add_argument("out_dir")
    ap.add_argument("--play-box", nargs=4, type=float, metavar=("X0", "Y0", "X1", "Y1"),
                    help="surfaces centred outside this XY box are backdrop (zone 'far')")
    args = ap.parse_args()

    gfx, verts, indices = load_world(args.world_dir)
    agree, total = winding_sign(gfx, verts, indices)
    flip = agree < total / 2
    print(f"winding: {agree}/{total} triangles CCW-front -> {'flip to CCW' if flip else 'keep'}")
    m = export(args.world_dir, args.out_dir, flip, args.play_box)
    print(f"{len(m['meshes'])} world meshes, {sum(x['tris'] for x in m['meshes'])} tris, "
          f"{len(m['submodels'])} brush submodels")


if __name__ == "__main__":
    main()
