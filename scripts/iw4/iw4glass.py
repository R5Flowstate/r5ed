"""MW2 breakable glass (FxWorld) panes as render meshes in an r5ed project.

The panes have no GfxWorld surface, so a port keeps whatever collision their brushes
carry and draws nothing. This writes the chosen panes as one double-sided GLB and adds
it to map.json as a see-through mesh. Each pane is a polygon in its own plane: FxWorld
stores the outline as shorts at 1/32 unit, placed by the piece's quat and origin.

    py iw4glass.py <world dir with fxworld.json> <project> --material <S21 material> [--flat-only]
"""

import argparse
import json
import sys
import uuid
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from iw4world import glb_bytes  # noqa: E402

NAME = "fxglass"


def rotate(q, v):
    x, y, z, w = q
    u = np.array([x, y, z])
    v = np.asarray(v, float)
    return v + 2.0 * np.cross(u, np.cross(u, v) + w * v)


def panes(fx, flat_only):
    geo, at = fx["initGeoData"], 0
    for p in fx["initPieceStates"]:
        pts = np.array(geo[at:at + p["vertCount"]], float) / 32.0
        at += p["vertCount"] + p["fanDataCount"]
        normal = rotate(p["quat"], [0.0, 0.0, 1.0])
        if flat_only and abs(normal[2]) < 0.7:
            continue
        world = np.array([np.asarray(p["origin"]) + rotate(p["quat"], [x, y, 0.0]) for x, y in pts])
        yield world, normal, pts


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("world")
    ap.add_argument("project")
    ap.add_argument("--material", required=True)
    ap.add_argument("--flat-only", action="store_true", help="only the horizontal panes (roof skylights)")
    a = ap.parse_args()
    fx = json.loads((Path(a.world) / "fxworld.json").read_text())
    pos, nrm, uv, tris = [], [], [], []
    count = 0
    for world, normal, local in panes(fx, a.flat_only):
        count += 1
        fan = [[0, i, i + 1] for i in range(1, len(world) - 1)]
        for t in (fan, [[a, c, b] for a, b, c in fan]):
            # Each copy's normal is its own counter-clockwise (glTF) front.
            n = np.cross(world[t[0][1]] - world[t[0][0]], world[t[0][2]] - world[t[0][0]])
            n = n / max(np.linalg.norm(n), 1e-9)
            base = len(pos)
            pos.extend(world.tolist())
            nrm.extend([n.tolist()] * len(world))
            uv.extend((local / 128.0).tolist())
            tris.extend([[base + a, base + b, base + c] for a, b, c in t])
    proj = Path(a.project)
    rel = f"meshes/world/{NAME}.glb"
    (proj / rel).parent.mkdir(parents=True, exist_ok=True)
    colors = np.full((len(pos), 4), 255, np.uint8)
    (proj / rel).write_bytes(glb_bytes(NAME, np.array(pos, np.float32), np.array(nrm, np.float32),
                                       np.array(uv, np.float32), colors, np.array(tris, np.uint32)))
    doc = json.loads((proj / "map.json").read_text())
    doc["objects"] = [o for o in doc["objects"] if o["name"] != NAME]
    layer = next((l["id"] for l in doc["layers"] if l["name"] == "world"), doc["layers"][0]["id"])
    doc["objects"].append({
        "id": str(uuid.uuid4()), "name": NAME, "layer": layer, "parent": None,
        "transform": {"position": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0]},
        "hidden": False, "locked": False,
        "kind": {"type": "mesh", "source": rel, "material_map": {NAME: a.material}, "collision": "none",
                 "contents": 0, "cast_shadows": False, "lightmap_scale": 0.0, "render": True}})
    (proj / "map.json").write_text(json.dumps(doc, indent=1))
    print(f"{count} glass panes, {len(tris)} triangles -> {rel} ({a.material})")


if __name__ == "__main__":
    main()
