"""Per-mesh lightmap texel policy for The Pit.

The client binds at most 32 lightmap pages; a blanket fine texel pushed the
chart packer to 36. Cost is world surface area in texels, so the scale is
chosen per mesh: walkable surfaces the player stands on and small props keep
a fine texel, large walls/roofs/foliage go coarse.

usage: lightmap_policy.py <project> <dump>
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "h2"))
from h2import import load_part  # noqa: E402

import numpy as np  # noqa: E402

SCALE = 100.0
# Large surfaces seen from a distance: one texel there costs the most and
# shows the least. Everything else keeps a stock-scale 16.
COARSE_WORDS = ("leaf", "canopy", "tree", "roof", "wall_concrete_d",
                "wall_metal_a", "pelican", "wall_concrete_sections",
                "modular_piece_trim", "metal_out_fence")
# Small props the player stands next to keep the fine texel: cheap either way.
TINY_AREA = 50e3


def surface_areas(dump: Path):
    parts = json.loads((dump / "parts.json").read_text())
    area = {}
    for p in parts:
        if "skies" in p["group"]:
            continue
        pos, nrm, uv, idx = load_part(dump, p)
        if len(idx) == 0:
            continue
        a, b, c = pos[idx[:, 0]], pos[idx[:, 1]], pos[idx[:, 2]]
        tri = 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1).sum()
        area[p["material"]] = area.get(p["material"], 0.0) + tri
    return {k: v * SCALE * SCALE for k, v in area.items()}


def choose(name, area):
    low = name.lower()
    if area < TINY_AREA:
        return 8.0
    for word in COARSE_WORDS:
        if word in low:
            return 24.0
    return 16.0


def main():
    proj, dump = Path(sys.argv[1]), Path(sys.argv[2])
    area = surface_areas(dump)

    doc = json.loads((proj / "map.json").read_text())
    meshes = [o for o in doc["objects"] if o["kind"].get("type") == "mesh"]
    total = 0.0
    for o in meshes:
        for name in o["kind"]["material_map"]:
            scale = choose(name, area.get(name, 0.0))
            o["kind"]["lightmap_scale"] = scale
            total += area.get(name, 0.0) / (scale * scale)

    baseline = sum(v / 64.0 for v in area.values())
    pages = total / (baseline / 36.0)
    (proj / "map.json").write_text(json.dumps(doc, indent=1))
    print(f"{len(meshes)} meshes, {total / 1e6:.2f}M texels -> ~{pages:.0f} of 32 pages")


if __name__ == "__main__":
    main()
