"""Build an r5ed project (map.json + meshes) from an IW4 world dump.

Inputs: the `.world/` directory from the OpenAssetTools-cafe dump, the entity
string, and the GLBs iw4world.py / iw4statics.py wrote. Materials go through
a map file (IW4 name -> S21 material path); unmapped ones fall back to the
dev material so the build never stops on a missing material.
"""

import argparse
import itertools
import json
import math
import re
import shutil
import uuid
from pathlib import Path

import numpy as np
from scipy.spatial import ConvexHull

from iw4world import glb_multi

DEV_MATERIAL = "world/dev/dev_gray_512"
NODRAW = "tools/toolsnodraw"

# IW4 contents bits.
IW_SOLID = 0x1
IW_GLASS = 0x10
IW_WATER = 0x20
IW_PLAYERCLIP = 0x10000

# Apex masks as shipped maps use them.
APEX_WORLD = 0
APEX_MOVE_CLIP = 0x00A30000
APEX_WATER = 0x20


def apex_contents(iw):
    if iw & (IW_SOLID | IW_GLASS):
        return APEX_WORLD
    if iw & IW_PLAYERCLIP:
        return APEX_MOVE_CLIP
    if iw & IW_WATER:
        return APEX_WATER
    return None


def new_id():
    return str(uuid.uuid4())


def quat_from_matrix(m):
    """Row-vector rotation (v' = v @ m) -> glam quaternion [x, y, z, w]."""
    r = np.asarray(m, dtype=np.float64).T
    t = np.trace(r)
    if t > 0:
        s = math.sqrt(t + 1.0) * 2
        w, x, y, z = 0.25 * s, (r[2, 1] - r[1, 2]) / s, (r[0, 2] - r[2, 0]) / s, (r[1, 0] - r[0, 1]) / s
    elif r[0, 0] > r[1, 1] and r[0, 0] > r[2, 2]:
        s = math.sqrt(1.0 + r[0, 0] - r[1, 1] - r[2, 2]) * 2
        w, x, y, z = (r[2, 1] - r[1, 2]) / s, 0.25 * s, (r[0, 1] + r[1, 0]) / s, (r[0, 2] + r[2, 0]) / s
    elif r[1, 1] > r[2, 2]:
        s = math.sqrt(1.0 + r[1, 1] - r[0, 0] - r[2, 2]) * 2
        w, x, y, z = (r[0, 2] - r[2, 0]) / s, (r[0, 1] + r[1, 0]) / s, 0.25 * s, (r[1, 2] + r[2, 1]) / s
    else:
        s = math.sqrt(1.0 + r[2, 2] - r[0, 0] - r[1, 1]) * 2
        w, x, y, z = (r[1, 0] - r[0, 1]) / s, (r[0, 2] + r[2, 0]) / s, (r[1, 2] + r[2, 1]) / s, 0.25 * s
    q = np.array([x, y, z, w])
    return (q / np.linalg.norm(q)).tolist()


def quat_between(a, b):
    a = np.asarray(a, float) / np.linalg.norm(a)
    b = np.asarray(b, float) / np.linalg.norm(b)
    c = np.cross(a, b)
    d = float(np.dot(a, b))
    if d < -0.999999:
        axis = np.cross(a, [1, 0, 0]) if abs(a[0]) < 0.9 else np.cross(a, [0, 1, 0])
        axis /= np.linalg.norm(axis)
        return [*axis.tolist(), 0.0]
    q = np.array([*c, 1.0 + d])
    return (q / np.linalg.norm(q)).tolist()


def angles_matrix(pitch, yaw, roll):
    """Source/IW angles -> row-vector rotation (rows = forward, left, up)."""
    p, y, r = (math.radians(v) for v in (pitch, yaw, roll))
    sp, cp, sy, cy, sr, cr = math.sin(p), math.cos(p), math.sin(y), math.cos(y), math.sin(r), math.cos(r)
    fwd = [cp * cy, cp * sy, -sp]
    left = [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp]
    up = [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp]
    return np.array([fwd, left, up])


def transform(position=(0, 0, 0), rotation=(0, 0, 0, 1), scale=(1, 1, 1)):
    return {"position": list(map(float, position)), "rotation": list(map(float, rotation)), "scale": list(map(float, scale))}


def obj(name, layer, kind, xf=None):
    return {"id": new_id(), "name": name, "layer": layer, "parent": None, "transform": xf or transform(),
            "hidden": False, "locked": False, "kind": kind}


def mesh_kind(source, materials, material_map, collision, contents=0, render=True, shadows=True, texel=0.0):
    return {"type": "mesh", "source": source,
            "material_map": {m: material_map.get(m, DEV_MATERIAL) for m in materials},
            "collision": collision, "contents": contents, "cast_shadows": shadows,
            "lightmap_scale": texel, "render": render}


def parse_ents(text):
    out = []
    for block in re.findall(r"\{[^{}]*\}", text):
        out.append(dict(re.findall(r'"([^"]+)" "([^"]*)"', block)))
    return out


def vec(s):
    return [float(v) for v in s.split()]


# ---------------------------------------------------------------- collision

def brush_sets(clip):
    """Brush indices owned by each brush submodel; the rest are world."""
    nodes, leaf_brushes = clip["leafBrushNodes"], clip["leafBrushes"]

    def walk(i, out, depth=0):
        if i < 0 or i >= len(nodes) or depth > 256:
            return
        n = nodes[i]
        if n["count"] > 0:
            first = n["firstLeafBrush"]
            out.update(leaf_brushes[first:first + n["count"]])
        elif n["count"] == 0 and "childOffset" in n and any(n["childOffset"]):
            for off in n["childOffset"]:
                if off:
                    walk(i + off, out, depth + 1)

    per_model = {}
    for m, cm in enumerate(clip["cmodels"]):
        if m == 0:
            continue
        s = set()
        walk(cm["leafBrushNode"], s)
        per_model[m] = s
    return per_model


def brush_faces(brush):
    faces = []
    for side in brush["sides"]:
        n = np.array(side["n"][:3], float)
        length = float(np.linalg.norm(n))
        faces.append({"normal": (n / length).tolist(), "dist": side["n"][3] / length})
    mid, half = np.array(brush["bounds"]["mid"]), np.array(brush["bounds"]["half"])
    for k in range(3):
        e = [0.0, 0.0, 0.0]
        e[k] = 1.0
        faces.append({"normal": e, "dist": float(mid[k] + half[k])})
        e = [0.0, 0.0, 0.0]
        e[k] = -1.0
        faces.append({"normal": e, "dist": float(-(mid[k] - half[k]))})
    unique = []
    for f in faces:
        n = np.array(f["normal"], float)
        if not any(np.abs(n - np.array(u["normal"])).max() < 1e-4 and abs(f["dist"] - u["dist"]) < 1e-3 for u in unique):
            unique.append(f)
    faces = unique
    for f in faces:
        f.update({"material": NODRAW,
                  "uv": {"u_axis": [1.0, 0.0, 0.0], "v_axis": [0.0, -1.0, 0.0], "offset": [0.0, 0.0],
                         "scale": [1.0, 1.0], "rotation": 0.0},
                  "lightmap_scale": 0.0})
    return faces


def brush_points(brush, inside_eps=0.01, merge_eps=0.05):
    """Corner points of a brush from its planes, tolerant of float32 planes."""
    planes = []
    for side in brush["sides"]:
        n = np.array(side["n"][:3], float)
        length = np.linalg.norm(n)
        planes.append((*(n / length), side["n"][3] / length))
    mid, half = np.array(brush["bounds"]["mid"]), np.array(brush["bounds"]["half"])
    for k in range(3):
        e = np.zeros(3)
        e[k] = 1.0
        planes.append((*e, mid[k] + half[k]))
        planes.append((*(-e), -(mid[k] - half[k])))
    P = np.array(planes)
    N, D = P[:, :3], P[:, 3]
    idx = np.array(list(itertools.combinations(range(len(P)), 3)))
    M = N[idx]
    det = np.linalg.det(M)
    ok = np.abs(det) > 1e-9
    if not ok.any():
        return None
    pts = np.linalg.solve(M[ok], D[idx[ok]][..., None])[..., 0]
    inside = (pts @ N.T - D <= inside_eps).all(axis=1)
    pts = pts[inside]
    if len(pts) < 4:
        return None
    uniq = []
    for p in pts:
        if not any(np.abs(p - q).max() <= merge_eps for q in uniq):
            uniq.append(p)
    pts = np.array(uniq)
    if len(pts) < 4:
        return None
    # A 1/8 grid is exact in f32, so the compiler re-hulls the same points.
    pts = np.unique(np.round(pts * 8.0) / 8.0, axis=0)
    if len(pts) < 4:
        return None
    try:
        hull = ConvexHull(pts)
        pts = pts[hull.vertices]
        hull = ConvexHull(pts)
    except Exception:
        return None
    # Width across the thinnest facet; slimmer hulls collapse once the
    # compiler takes them to f32 and rebuilds their planes.
    width = min(float((pts @ eq[:3] + eq[3]).min()) for eq in hull.equations)
    if hull.volume < 1.0 or -width < 1.0:
        return None
    return pts


def hull_part(pts):
    hull = ConvexHull(pts)
    tris = hull.simplices.astype(np.uint32)
    return ("clip", pts.astype(np.float32), np.zeros_like(pts, dtype=np.float32), np.zeros((len(pts), 2), np.float32), tris)


def collision_tris(world, clip, out_dir, flip):
    """Every collision partition in the aabb trees, one GLB per Apex mask."""
    verts = np.fromfile(world / "clip_verts.bin", dtype="<f4").reshape(-1, 3)
    tris = np.fromfile(world / "clip_tris.bin", dtype="<u2").reshape(-1, 3).astype(np.int64)
    by_mask = {}
    seen = set()
    for node in clip["aabbTrees"]:
        child_count, u, material = node[7], node[8], node[6]
        if child_count != 0 or u in seen:
            continue
        seen.add(u)
        first, count = clip["partitions"][u]
        mask = apex_contents(clip["materials"][material]["contents"])
        if mask is None:
            continue
        by_mask.setdefault(mask, []).append(tris[first:first + count])
    written = []
    for mask, parts in by_mask.items():
        t = np.concatenate(parts)
        used, remap = np.unique(t, return_inverse=True)
        local = remap.reshape(-1, 3).astype(np.uint32)
        if flip:
            local = local[:, [0, 2, 1]]
        pos = verts[used]
        rel = f"meshes/collision/clip_{mask:08x}.glb"
        (out_dir / rel).parent.mkdir(parents=True, exist_ok=True)
        (out_dir / rel).write_bytes(glb_multi(f"clip_{mask:08x}", [("clip", pos, np.zeros_like(pos), np.zeros((len(pos), 2), np.float32), local)]))
        written.append((rel, mask, len(local)))
    return written


# ---------------------------------------------------------------- main

# MW2 writes 1e6 as the last LOD distance of a model it never culls.
NEVER_CULLED = 1.0e5
NEVER_CULLED_FADE = 30000.0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("world_dir")
    ap.add_argument("ents")
    ap.add_argument("glb_dir", help="output of iw4world.py + iw4statics.py")
    ap.add_argument("project")
    ap.add_argument("--stem", default="mp_rr_terminal")
    ap.add_argument("--name", default="Terminal")
    ap.add_argument("--materials", help="JSON map IW4 material -> S21 material path")
    ap.add_argument("--far-texel", type=float, default=128.0, help="lightmap texel size for backdrop meshes")
    ap.add_argument("--loadscreen", help="loading screen image, copied into the project")
    ap.add_argument("--converted", help="iw4mat.py material map; an entry of null drops the surface")
    ap.add_argument("--no-trans", action="store_true", help="leave out translucent world surfaces")
    ap.add_argument("--model-ns", default="mw2/terminal", help="prop model folder under mdl/")
    ap.add_argument("--map-id", default="fdm_terminal")
    ap.add_argument("--description", default=None)
    ap.add_argument("--minimap-origin", nargs=2, type=float, default=[-1440.0, 2368.0])
    ap.add_argument("--minimap-size", type=float, default=5248.0)
    ap.add_argument("--spawn-classes", nargs="*", default=["mp_dm_spawn", "mp_tdm_spawn"])
    args = ap.parse_args()
    description = args.description or f"Modern Warfare 2 {args.name}"

    world, glb_dir, proj = Path(args.world_dir), Path(args.glb_dir), Path(args.project)
    gfx = json.loads((world / "gfxworld.json").read_text())
    clip = json.loads((world / "clipmap.json").read_text())
    ents = parse_ents(Path(args.ents).read_text())
    manifest = json.loads((glb_dir / "world_manifest.json").read_text())
    statics = json.loads((glb_dir / "static_models.json").read_text())
    converted = json.loads(Path(args.converted).read_text()) if args.converted else {}
    material_map = {k: v for k, v in converted.items() if v}
    material_map.update(json.loads(Path(args.materials).read_text()) if args.materials else {})
    dropped_materials = {k for k, v in converted.items() if v is None} - set(material_map)
    flip = manifest["flipped_winding"]

    proj.mkdir(parents=True, exist_ok=True)
    for sub in ("world", "submodels", "models"):
        src = glb_dir / sub
        if src.is_dir():
            shutil.copytree(src, proj / "meshes" / sub, dirs_exist_ok=True)

    layers = {name: {"id": new_id(), "name": name, "visible": True, "locked": False}
              for name in ("world", "props", "collision", "entities", "lights")}
    L = {k: v["id"] for k, v in layers.items()}
    objects = []

    for m in manifest["meshes"]:
        if m["class"] == "shadow_caster" or (args.no_trans and m["class"] == "trans") or m["material"] in dropped_materials:
            continue
        # The engine holds 32 lightmap pages; the backdrop bakes coarse so the
        # playable space keeps the fine texels.
        texel = args.far_texel if m.get("zone") == "far" else 0.0
        objects.append(obj(Path(m["glb"]).stem, L["world"],
                           mesh_kind("meshes/" + m["glb"], [m["material"]], material_map, "none",
                                     shadows=m["class"] != "trans", texel=texel)))

    # Brush submodels sit at their entity's origin/angles.
    placed = {}
    for e in ents:
        mdl = e.get("model", "")
        if mdl.startswith("*"):
            placed[int(mdl[1:])] = e
    sub_brushes = brush_sets(clip)
    for sm in manifest["submodels"]:
        e = placed.get(sm["index"])
        if e is None:
            continue
        origin = vec(e.get("origin", "0 0 0"))
        rot = quat_from_matrix(angles_matrix(*vec(e.get("angles", "0 0 0")))) if "angles" in e else [0, 0, 0, 1]
        for part in sm["meshes"]:
            if part["material"] in dropped_materials:
                continue
            objects.append(obj(f"bmodel{sm['index']}_{Path(part['glb']).stem}", L["world"],
                               mesh_kind("meshes/" + part["glb"], [part["material"]], material_map, "none"),
                               transform(origin, rot)))

    # A prop with no fade of its own falls back to the engine's radius * 40, which
    # dissolves small props within a few metres; MW2's own draw distance applies.
    def prop_fade(inst, model):
        dist = inst.get("cullDist") or model.get("draw_distance", 0.0)
        return float(dist) if 0 < dist < NEVER_CULLED else NEVER_CULLED_FADE

    # Static models are static props of the converted models; the clipMap
    # lists the instances MW2 collided with.
    colliding = {(s["model"], tuple(round(v, 1) for v in s["origin"])) for s in clip["staticModels"]}
    models = statics["models"]
    for i, inst in enumerate(gfx["staticModels"]):
        if inst["model"] not in models:
            continue
        axis = np.array(inst["axis"])
        key = (inst["model"], tuple(round(v, 1) for v in inst["origin"]))
        objects.append(obj(f"{inst['model']}_{i}", L["props"],
                           {"type": "prop", "model": f"mdl/{args.model_ns}/{inst['model']}.rmdl", "skin": 0,
                            "static_prop": True, "solid": key in colliding, "cast_shadows": True,
                            "fade_distance": prop_fade(inst, models[inst["model"]]), "keyvalues": {}},
                           transform(inst["origin"], quat_from_matrix(axis), [inst["scale"]] * 3)))

    # World collision: every brush becomes a convex hull part, grouped by mask.
    owned = set().union(*sub_brushes.values()) if sub_brushes else set()
    hulls = {}
    kept = dropped = 0

    def add_brush(b, xf=None):
        nonlocal kept, dropped
        mask = apex_contents(b.get("contents", 0))
        pts = brush_points(b) if mask is not None and "bounds" in b else None
        if pts is None:
            dropped += 1
            return
        if xf is not None:
            pts = pts @ xf[1] + xf[0]
        hulls.setdefault(mask, []).append(pts)
        kept += 1

    # Breakable glass panes (FxWorld) ship already shattered: their brushes would be
    # invisible walls, since the panes are not GfxWorld surfaces.
    panes = 0
    for i, b in enumerate(clip["brushes"]):
        if b.get("glassPiece"):
            panes += 1
            continue
        if i not in owned:
            add_brush(b)
    for m, brushes in sub_brushes.items():
        e = placed.get(m)
        if e is None:
            continue
        xf = (np.array(vec(e.get("origin", "0 0 0"))), angles_matrix(*vec(e.get("angles", "0 0 0"))))
        for i in brushes:
            add_brush(clip["brushes"][i], xf)
    for mask, parts in hulls.items():
        rel = f"meshes/collision/hulls_{mask:08x}.glb"
        (proj / rel).parent.mkdir(parents=True, exist_ok=True)
        (proj / rel).write_bytes(glb_multi(f"hulls_{mask:08x}", [hull_part(p) for p in parts]))
        objects.append(obj(Path(rel).stem, L["collision"],
                           mesh_kind(rel, ["clip"], {"clip": NODRAW}, "convex_hulls", contents=mask, render=False, shadows=False)))

    tri_meshes = collision_tris(world, clip, proj, flip)
    for rel, mask, n in tri_meshes:
        objects.append(obj(Path(rel).stem, L["collision"],
                           mesh_kind(rel, ["clip"], {"clip": NODRAW}, "triangles", contents=mask, render=False, shadows=False)))

    # Spawns: MW2's deathmatch and team deathmatch spawns become one FreeDM
    # location ("fdm_terminal"): human spawn points linked from a map node, as
    # the stock arenas author them, so FFA and TDM playlists can force it.
    import hashlib

    def link_guid(text):
        return hashlib.sha1(text.encode()).hexdigest()[:16]

    spawn_keys = {"teamnumber": "0", "model": "mdl/dev/mp_spawn.rmdl", "scale": "1", "gamemode_tdm": "1",
                  "gamemode_ffa": "1", "gamemode_ctf": "1", "gamemode_cp": "1", "gamemode_at": "1", "gamemode_fd": "1",
                  "gamemode_fw": "0", "gamemode_freelance": "0", "control_teamnumber": "-1", "alliance": "-1"}
    links = []
    spawn_count = 0
    first_spawn = None
    for e in ents:
        cls = e.get("classname", "")
        if cls not in args.spawn_classes or "origin" not in e:
            continue
        origin = vec(e["origin"])
        yaw = vec(e.get("angles", "0 0 0"))[1]
        rot = quat_from_matrix(angles_matrix(0, yaw, 0))
        guid = link_guid(f"{cls}_{e['origin']}")
        links.append(guid)
        objects.append(obj(f"{cls}_{spawn_count}", L["entities"],
                           {"type": "entity", "classname": "info_spawnpoint_human",
                            "keyvalues": {**spawn_keys, "link_guid": guid}, "partition": None, "links": {}},
                           transform(origin, rot)))
        if first_spawn is None:
            first_spawn = (origin, rot)
        spawn_count += 1
    inter = next((e for e in ents if e.get("classname") == "mp_global_intermission"), None)
    if inter:
        guid = link_guid("intro_camera")
        links.append(guid)
        a = vec(inter.get("angles", "0 0 0"))
        objects.append(obj("freedm_intro_camera", L["entities"],
                           {"type": "entity", "classname": "script_ref",
                            "keyvalues": {"editorclass": "info_freedm_intro_camera", "spawnflags": "0",
                                          "model": "mdl/dev/editor_cube_w_arrow.rmdl", "scale": "1", "link_guid": guid},
                            "partition": None, "links": {}},
                           transform(vec(inter["origin"]), quat_from_matrix(angles_matrix(*a)))))
    node_keys = {"editorclass": "info_freedm_map_location", "spawnflags": "0", "map_id": args.map_id}
    for i, g in enumerate(links):
        node_keys[f"link_to_guid_{i}"] = g
    centre = first_spawn[0] if first_spawn else [0, 0, 0]
    objects.append(obj("freedm_map_location", L["entities"],
                       {"type": "entity", "classname": "script_ref", "keyvalues": node_keys, "partition": None, "links": {}},
                       transform(centre)))
    if first_spawn:
        objects.append(obj("info_player_start", L["entities"],
                           {"type": "entity", "classname": "info_player_start", "keyvalues": {},
                            "partition": None, "links": {}},
                           transform(first_spawn[0], first_spawn[1])))

    # Sun from worldspawn: sundirection points at the sun.
    ws = next(e for e in ents if e.get("classname") == "worldspawn")
    pitch, yaw = vec(ws.get("sundirection", "-45 0 0"))[:2]
    to_sun = np.array([math.cos(math.radians(pitch)) * math.cos(math.radians(yaw)),
                       math.cos(math.radians(pitch)) * math.sin(math.radians(yaw)),
                       -math.sin(math.radians(pitch))])
    sun_color = vec(ws.get("suncolor", "1 1 1"))
    objects.append(obj("sun", L["lights"],
                       {"type": "light", "kind": "sun", "color": sun_color, "intensity": float(ws.get("sunlight", 1.0)),
                        "range": 0.0, "inner_angle": 0.0, "outer_angle": 0.0, "size": [0.0, 0.0],
                        "cast_shadows": True, "baked": True, "realtime": False, "keyvalues": {}},
                       transform([0, 0, 4096], quat_between([0, 0, -1], -to_sun))))

    loadscreen = None
    if args.loadscreen:
        (proj / "ui").mkdir(exist_ok=True)
        shutil.copyfile(args.loadscreen, proj / "ui" / Path(args.loadscreen).name)
        loadscreen = f"ui/{Path(args.loadscreen).name}"
    # Slot 0 is the engine's default probe, not a placed one.
    cubemap_points = [p["origin"] for p in gfx.get("reflectionProbes", [])[1:]]
    doc = {
        "format": 1,
        "meta": {"stem": args.stem, "display_name": args.name, "description": description,
                 "playlists": [], "loadscreen": loadscreen,
                 "minimap": {"enabled": True, "origin": list(args.minimap_origin), "size": args.minimap_size, "resolution": 2048},
                 "sky": "mdl/vistas/eden_se.rmdl", "resident_paks": []},
        "layers": list(layers.values()),
        "objects": objects,
        "settings": {"bake": {"texel_size": 16.0, "bounces": 2, "samples": 256,
                              "sky_color": vec(ws.get("skycolor", "0.35 0.55 1")), "sky_intensity": float(ws.get("skylight", 0.65)),
                              "ambient_occlusion": True, "probe_spacing": 256.0, "cubemap_spacing": 2048.0,
                              "cubemap_points": cubemap_points},
                     "navmesh": {"enabled": True, "hulls": ["small", "med_short", "medium", "large", "extra_large"], "bounds": None},
                     "vis": {"portals": False}},
    }
    (proj / "map.json").write_text(json.dumps(doc))
    kinds = {}
    for o in objects:
        kinds[o["kind"]["type"]] = kinds.get(o["kind"]["type"], 0) + 1
    print(f"objects {len(objects)} {kinds}; brushes kept {kept} dropped {dropped} glass panes {panes}; "
          f"collision tri meshes {[(r, hex(m), n) for r, m, n in tri_meshes]}; spawns {spawn_count}; "
          f"unmapped materials fall back to {DEV_MATERIAL}")


if __name__ == "__main__":
    main()
