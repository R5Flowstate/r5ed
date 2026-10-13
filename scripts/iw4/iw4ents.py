"""Bring an IW4 map's entity props in as static props.

The GfxWorld holds only the static models. Vehicles, cases, TVs and animated
trees are script_model entities in the map's entity string; the luggage piles,
food, cones and shop clutter are the clipMap's dynamic entities (physics props).

  iw4ents.py export <dump> <ents> <world_dir> <glb_dir> <used_materials.json>
      writes each entity model's GLB into <glb_dir>/models, adds it to
      static_models.json and its materials to the used-materials list.
  iw4ents.py place <ents> <world_dir> <glb_dir> <project>
      appends one static prop per entity to <project>/map.json.
  iw4ents.py solid <ents> <world_dir> <glb_dir>
      prints the entity models that should collide (for iw4props --solid).
"""
import json
import sys
from pathlib import Path

import numpy as np

from iw4import import parse_ents, vec, angles_matrix, quat_from_matrix, transform, obj, NEVER_CULLED_FADE
from iw4props import is_air, glb_triangles
from iw4statics import lod0_path, load_rigid, draw_distance
from iw4world import glb_multi

MODEL_NS = "mw2/terminal"
# Game-mode objects the S&D / flag modes spawn on demand.
SKIP_MODELS = {"com_bomb_objective", "com_bomb_objective_d", "mil_tntbomb_mp", "prop_flag_neutral"}
# Hand-held clutter that MW2 never collided with.
LOOSE_WORDS = ("soda", "cellphone", "laptop", "flatscreen", "lightfluo", "wallfan", "windsock", "roofvent")
# A dynamic entity smaller than this on every axis is clutter a player walks through.
SOLID_EXTENT = 20.0
# IW4 DynEntityDef contents bit for solid.
CONTENTS_SOLID = 0x1


def placements(ents, clip):
    """(label, model, origin, quat xyzw, scale, kind) for every entity prop."""
    out = []
    for i, e in enumerate(ents):
        name = e.get("model", "")
        if e.get("classname") != "script_model" or not name or name in SKIP_MODELS:
            continue
        rot = quat_from_matrix(angles_matrix(*vec(e.get("angles", "0 0 0"))))
        out.append((f"ent_{name}_{i}", name, vec(e.get("origin", "0 0 0")), rot, float(e.get("modelscale", "1")), "ent"))
    for i, d in enumerate(clip.get("dynEnts", [])):
        name = d.get("model", "")
        if not name or name in SKIP_MODELS:
            continue
        q = np.asarray(d["quat"], dtype=np.float64)
        q = q / max(np.linalg.norm(q), 1e-9)
        out.append((f"dyn_{name}_{i}", name, d["origin"], q.tolist(), 1.0, "dyn" if d.get("contents", 0) & CONTENTS_SOLID else "dyn_loose"))
    return out


def load(args_ents, args_world):
    ents = parse_ents(Path(args_ents).read_text(encoding="latin-1"))
    clip = json.loads((Path(args_world) / "clipmap.json").read_text())
    return placements(ents, clip)


def export(dump, places, glb_dir, used_path):
    statics_path = glb_dir / "static_models.json"
    statics = json.loads(statics_path.read_text())
    # iw4world's meshes carry the world's materials; without them iw4mat converts
    # nothing but the entity models' own. One entry per material: a mesh per class
    # and zone would list the same material many times.
    if used_path.exists():
        used = json.loads(used_path.read_text())
    else:
        manifest = json.loads((glb_dir / "world_manifest.json").read_text())
        used = [{"material": m, "model": False} for m in dict.fromkeys(
            x["material"] for x in manifest["meshes"])]
    have_mat = {u["material"] for u in used}
    added, missing = [], []
    for name in sorted({p[1] for p in places}):
        if name in statics["models"]:
            continue
        src = lod0_path(dump, name)
        parts = load_rigid(src) if src and src.is_file() else None
        if not parts:
            missing.append(name)
            continue
        rel = f"models/{name}.glb"
        (glb_dir / rel).write_bytes(glb_multi(name, parts))
        mats = [p[0] for p in parts]
        statics["models"][name] = {"glb": rel, "materials": mats, "tris": int(sum(len(p[4]) for p in parts)),
                                   "draw_distance": draw_distance(dump, name)}
        for m in mats:
            if m not in have_mat:
                used.append({"material": m, "model": True})
                have_mat.add(m)
        added.append(name)
    statics_path.write_text(json.dumps(statics, indent=1))
    used_path.write_text(json.dumps(used, indent=1))
    print(f"{len(added)} entity models exported, {len(missing)} missing {missing}")
    print(" ".join(added))


def extent(glb):
    pts = [pos for _, pos, *_ in glb_triangles(glb)]
    if not pts:
        return 0.0
    p = np.concatenate(pts)
    return float((p.max(0) - p.min(0)).max())


def solid_models(places, glb_dir):
    statics = json.loads((glb_dir / "static_models.json").read_text())["models"]
    out = set()
    for _, name, _, _, _, kind in places:
        if name in out or name not in statics or is_air(name) or any(w in name for w in LOOSE_WORDS):
            continue
        if kind == "dyn_loose":
            continue
        if kind == "dyn" and extent(glb_dir / statics[name]["glb"]) < SOLID_EXTENT:
            continue
        out.add(name)
    return sorted(out)


def place(places, glb_dir, project):
    statics = json.loads((glb_dir / "static_models.json").read_text())["models"]
    doc_path = project / "map.json"
    doc = json.loads(doc_path.read_text())
    layer = next(o["layer"] for o in doc["objects"] if o["kind"].get("type") == "prop")
    names = {o["name"] for o in doc["objects"]}
    solid = set(solid_models(places, glb_dir))
    added = 0
    for label, name, origin, rot, scale, _ in places:
        if name not in statics or label in names:
            continue
        dist = statics[name].get("draw_distance", 0.0)
        doc["objects"].append(obj(label, layer,
                                  {"type": "prop", "model": f"mdl/{MODEL_NS}/{name}.rmdl", "skin": 0,
                                   "static_prop": True, "solid": name in solid, "cast_shadows": True,
                                   "fade_distance": float(dist) if 0 < dist < NEVER_CULLED_FADE else NEVER_CULLED_FADE,
                                   "keyvalues": {}},
                                  transform(origin, rot, [scale] * 3)))
        added += 1
    doc_path.write_text(json.dumps(doc, indent=1))
    print(f"{added} entity props placed")


def main():
    global MODEL_NS
    argv = sys.argv[1:]
    if "--model-ns" in argv:
        i = argv.index("--model-ns")
        MODEL_NS = argv[i + 1]
        del argv[i:i + 2]
    cmd, a = argv[0], argv[1:]
    if cmd == "export":
        export(Path(a[0]), load(a[1], a[2]), Path(a[3]), Path(a[4]))
    elif cmd == "place":
        place(load(a[0], a[1]), Path(a[2]), Path(a[3]))
    elif cmd == "solid":
        print(" ".join(solid_models(load(a[0], a[1]), Path(a[2]))))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
