"""Halo 2 (H2V) map dump from h2dump.exe -> r5ed project.

Geometry is baked into world meshes (one GLB per source material), collision
comes from the render triangles, spawns from the scenario's player starting
locations. Halo world units are scaled into Apex units and the map is
recentred on the origin.
"""

import argparse
import hashlib
import json
import math
import sys
import uuid
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "iw4"))
from iw4world import glb_multi  # noqa: E402

NODRAW = "tools/toolsnodraw"
DEV_MATERIAL = "world/dev/dev_gray_512"


def new_id():
    return str(uuid.uuid4())


def transform(position=(0, 0, 0), rotation=(0, 0, 0, 1), scale=(1, 1, 1)):
    return {"position": [float(v) for v in position], "rotation": [float(v) for v in rotation],
            "scale": [float(v) for v in scale]}


def obj(name, layer, kind, xf=None):
    return {"id": new_id(), "name": name, "layer": layer, "parent": None, "transform": xf or transform(),
            "hidden": False, "locked": False, "kind": kind}


def yaw_quat(yaw_rad):
    return [0.0, 0.0, math.sin(yaw_rad / 2), math.cos(yaw_rad / 2)]


def quat_between(a, b):
    a = np.asarray(a, float) / np.linalg.norm(a)
    b = np.asarray(b, float) / np.linalg.norm(b)
    c = np.cross(a, b)
    d = float(np.dot(a, b))
    q = np.array([*c, 1.0 + d])
    return (q / np.linalg.norm(q)).tolist()


APEX_WATER = 0x20

# World units per lightmap texel: fine for thin trims and panels, coarse for open ground.
TEXEL_BY_KIND = (("ground", 32.0), ("cliffs_top", 32.0), ("cliff", 24.0), ("slide", 24.0), ("rock", 16.0),
                 ("cedar", 16.0), ("water", 32.0))


def lightmap_texel(mat, default):
    for key, texel in TEXEL_BY_KIND:
        if key in mat:
            return texel
    return 8.0 if default >= 16.0 else default


def load_part(dump, p):
    raw = (dump / p["file"]).read_bytes()
    n, k = p["verts"], p["indices"]
    f = np.frombuffer(raw, "<f4", n * 8)
    pos = f[: n * 3].reshape(-1, 3)
    nrm = f[n * 3: n * 6].reshape(-1, 3)
    uv = f[n * 6: n * 8].reshape(-1, 2)
    idx = np.frombuffer(raw, "<u4", k, offset=n * 32).reshape(-1, 3)
    return pos, nrm, uv, idx


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("dump")
    ap.add_argument("project")
    ap.add_argument("--materials", required=True, help="JSON: H2 shader name -> S21 material, null drops it")
    ap.add_argument("--stem", default="mp_rr_bloodgulch")
    ap.add_argument("--name", default="Blood Gulch")
    ap.add_argument("--scale", type=float, default=100.0, help="Apex units per Halo world unit")
    ap.add_argument("--lightmap-scale", type=float, default=32.0, help="world units per lightmap texel")
    ap.add_argument("--overrides", help="JSON: H2 shader name -> rebuilt S21 material (h2mat.py material_map.json)")
    ap.add_argument("--blend", default="coag_ground,coag_ground_water",
                    help="materials drawn with a two-layer blend: their vertex alpha 0.5 lets the blend mask decide")
    ap.add_argument("--water", default="coag_water_pool", help="materials whose mesh is a water volume")
    ap.add_argument("--split", default="",
                    help="mat|mask.png|second_mat -- collide a blend material's triangles as its own or the second "
                         "material's surface by the mask alpha under them")
    ap.add_argument("--sun", default="40,200",
                    help="pitch,yaw in degrees of the direction TO the sun (Halo convention: yaw from +x toward +y)")
    ap.add_argument("--index", help="r5ed-cache index/assets.json to check material names against")
    args = ap.parse_args()

    dump, proj = Path(args.dump), Path(args.project)
    parts = json.loads((dump / "parts.json").read_text())
    gameplay = json.loads((dump / "gameplay.json").read_text())
    mat_map = json.loads(Path(args.materials).read_text())
    if args.overrides:
        mat_map.update(json.loads(Path(args.overrides).read_text()))
    blend = set(filter(None, args.blend.split(",")))
    water_mats = set(filter(None, args.water.split(",")))
    split_masks = {}
    for spec in filter(None, args.split.split(",")):
        mat_a, mask_png, mat_b = spec.split("|")
        from PIL import Image
        split_masks[mat_a] = (np.asarray(Image.open(mask_png).convert("RGBA"))[..., 3].astype(np.float32) / 255.0, mat_b)
    S = args.scale

    if args.index:
        rows = json.loads(Path(args.index).read_text())
        rows = rows["assets"] if isinstance(rows, dict) else rows
        have = {r["path"].removeprefix("material/").removesuffix(".rpak") for r in rows if r["kind"] == "matl"}
        for k, v in mat_map.items():
            if v and f"{v}_wldc" not in have and not v.startswith("world/h2/"):
                raise SystemExit(f"{k}: {v} is not an installed world material")

    world_parts = [p for p in parts if "skies" not in p["group"]]
    unknown = sorted({p["material"] for p in world_parts} - set(mat_map))
    if unknown:
        raise SystemExit(f"materials missing from the map file: {unknown}")

    loaded = [(p, *load_part(dump, p)) for p in world_parts]
    lo = np.min([x[1].min(0) for x in loaded], axis=0)
    hi = np.max([x[1].max(0) for x in loaded], axis=0)
    centre = np.array([(lo[0] + hi[0]) / 2, (lo[1] + hi[1]) / 2, 0.0])

    def to_apex(v):
        return (np.asarray(v, np.float64) - centre) * S

    # Front-face winding from the stored normals.
    agree = total = 0
    for p, pos, nrm, uv, idx in loaded:
        a, b, c = pos[idx[:, 0]], pos[idx[:, 1]], pos[idx[:, 2]]
        d = np.einsum("ij,ij->i", np.cross(b - a, c - a), nrm[idx[:, 0]])
        agree += int((d > 0).sum())
        total += int((d != 0).sum())
    flip = agree < total / 2
    print(f"winding {agree}/{total} CCW-front -> {'flip' if flip else 'keep'}")

    groups = {}
    for p, pos, nrm, uv, idx in loaded:
        target = mat_map[p["material"]]
        if target is None:
            continue
        groups.setdefault(p["material"], []).append((pos, nrm, uv, idx))

    layers = {n: {"id": new_id(), "name": n, "visible": True, "locked": False}
              for n in ("world", "collision", "entities", "lights")}
    L = {k: v["id"] for k, v in layers.items()}
    objects = []
    (proj / "meshes" / "world").mkdir(parents=True, exist_ok=True)
    tri_total = 0
    for mat, chunks in sorted(groups.items()):
        P, N, U, I, base = [], [], [], [], 0
        for pos, nrm, uv, idx in chunks:
            P.append(to_apex(pos))
            N.append(nrm)
            U.append(uv)
            I.append(idx.astype(np.uint32) + base)
            base += len(pos)
        P, N, U, I = np.concatenate(P), np.concatenate(N), np.concatenate(U), np.concatenate(I)
        if flip:
            I = I[:, [0, 2, 1]]
        rel = f"meshes/world/{mat}.glb"
        colors = None
        if mat in blend:
            c = np.full((len(P), 4), 255, np.uint8)
            c[:, 3] = 128
            colors = [c]
        (proj / rel).write_bytes(glb_multi(mat, [(mat, P.astype(np.float32), N.astype(np.float32), U.astype(np.float32), I)], colors=colors))
        texel = lightmap_texel(mat, args.lightmap_scale)
        water = mat in water_mats
        split = split_masks.get(mat)
        objects.append(obj(mat, L["world"], {
            "type": "mesh", "source": rel, "material_map": {mat: mat_map[mat]},
            "collision": "none" if split is not None else "triangles",
            "contents": APEX_WATER if water else 0, "cast_shadows": not water,
            "lightmap_scale": texel, "render": True}))
        if split is not None:
            # One collision per surface under the blend mask, so impacts and
            # footsteps follow the paths the texture draws.
            mask_img, second_mat = split
            uvc = np.concatenate([u for _, _, u, _ in chunks])
            tri_uv = uvc[I].mean(axis=1)
            h, w = mask_img.shape
            px = np.clip((tri_uv[:, 0] % 1.0) * w, 0, w - 1).astype(int)
            py = np.clip((tri_uv[:, 1] % 1.0) * h, 0, h - 1).astype(int)
            second = mask_img[py, px] > 0.5
            for tag, sel, material in (("a", ~second, mat_map[mat]), ("b", second, mat_map[second_mat])):
                if not sel.any():
                    continue
                crel = f"meshes/collision/{mat}_{tag}.glb"
                (proj / crel).parent.mkdir(parents=True, exist_ok=True)
                (proj / crel).write_bytes(glb_multi(f"{mat}_{tag}", [(material, P.astype(np.float32), N.astype(np.float32),
                                                                      U.astype(np.float32), I[sel])]))
                objects.append(obj(f"{mat}_{tag}_collision", L["collision"], {
                    "type": "mesh", "source": crel, "material_map": {material: material},
                    "collision": "triangles", "contents": 0, "cast_shadows": False,
                    "lightmap_scale": texel, "render": False}))
        tri_total += len(I)

    def link_guid(text):
        return hashlib.sha1(text.encode()).hexdigest()[:16]

    spawn_keys = {"teamnumber": "0", "model": "mdl/dev/mp_spawn.rmdl", "scale": "1", "gamemode_tdm": "1",
                  "gamemode_ffa": "1", "gamemode_ctf": "1", "gamemode_cp": "1", "gamemode_at": "1", "gamemode_fd": "1",
                  "gamemode_fw": "0", "gamemode_freelance": "0", "control_teamnumber": "-1", "alliance": "-1",
                  **{f"phase_{i}": "0" for i in range(1, 10)}}
    seen, links, first = set(), [], None
    for i, s in enumerate(gameplay["player_starts"]):
        key = tuple(round(v, 2) for v in s["position"])
        if key in seen:
            continue
        seen.add(key)
        origin = to_apex(s["position"])
        origin[2] += 8.0
        rot = yaw_quat(s["facing"])
        guid = link_guid(f"h2_start_{key}")
        links.append(guid)
        objects.append(obj(f"spawn_{len(links)}", L["entities"], {
            "type": "entity", "classname": "info_spawnpoint_human",
            "keyvalues": {**spawn_keys, "link_guid": guid}, "partition": None, "links": {}},
            transform(origin, rot)))
        if first is None:
            first = (origin, rot)

    node_keys = {"editorclass": "info_freedm_map_location", "spawnflags": "0", "scale": "1", "map_id": "fdm_bloodgulch"}
    for i, g in enumerate(links):
        node_keys[f"link_to_guid_{i}"] = g
    objects.append(obj("freedm_map_location", L["entities"], {
        "type": "entity", "classname": "script_ref", "keyvalues": node_keys, "partition": None, "links": {}},
        transform(first[0])))
    objects.append(obj("info_player_start", L["entities"], {
        "type": "entity", "classname": "info_player_start", "keyvalues": {"spawnflags": "0", "scale": "1"}, "partition": None, "links": {}},
        transform(first[0], first[1])))

    # Default: Blood Gulch's late-afternoon sun from the west-south-west.
    pitch, yaw = (math.radians(float(v)) for v in args.sun.split(","))
    to_sun = np.array([math.cos(pitch) * math.cos(yaw), math.cos(pitch) * math.sin(yaw), math.sin(pitch)])
    objects.append(obj("sun", L["lights"], {
        "type": "light", "kind": "sun", "color": [1.0, 0.93, 0.8], "intensity": 1.0,
        "range": 0.0, "inner_angle": 0.0, "outer_angle": 0.0, "size": [0.0, 0.0],
        "cast_shadows": True, "baked": True, "realtime": False, "keyvalues": {}},
        transform([0, 0, (hi[2] - centre[2]) * S + 1024], quat_between([0, 0, -1], -to_sun))))

    ext = (hi - lo) * S
    size = float(max(ext[0], ext[1]) * 1.05)
    doc = {
        "format": 1,
        "meta": {"stem": args.stem, "display_name": args.name, "description": "Halo 2 Coagulation (Blood Gulch)",
                 "playlists": [], "loadscreen": None,
                 "minimap": {"enabled": True, "origin": [-size / 2, size / 2], "size": size, "resolution": 2048},
                 "sky": "mdl/vistas/eden_se.rmdl", "resident_paks": []},
        "layers": list(layers.values()),
        "objects": objects,
        "settings": {"bake": {"texel_size": 32.0, "bounces": 2, "samples": 256,
                              "sky_color": [0.45, 0.6, 0.9], "sky_intensity": 0.6,
                              "ambient_occlusion": True, "probe_spacing": 512.0, "cubemap_spacing": 4096.0},
                     "navmesh": {"enabled": True, "hulls": ["small", "med_short", "medium", "large", "extra_large"], "bounds": None},
                     "vis": {"portals": False}},
    }
    (proj / "map.json").write_text(json.dumps(doc, indent=1))
    (proj / "h2_gameplay.json").write_text(json.dumps({"centre": centre.tolist(), "scale": S, **gameplay}, indent=1))
    print(f"{len(groups)} world meshes, {tri_total} tris, {len(links)} spawns, extent {ext.round(0).tolist()} units")


if __name__ == "__main__":
    main()
