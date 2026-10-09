"""IW4 static models -> S21 client rmdl v17 and S3 dedi rmdl v10.

Rigid Z-up GLBs (iw4statics.py) become SMD + QC, Portal 2's studiomdl
compiles them to MDL v49, and rmdlconv converts v49 -> v17 (client) and
v17 -> v10 (dedi). A model the clipMap lists as colliding gets a concave
collision model from its render surfaces, foliage and light beams left out.
"""

import argparse
import json
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np

from iw4statics import accessor, read_glb

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import r5edenv  # noqa: E402

STUDIOMDL = r5edenv.path("R5ED_STUDIOMDL", required=False) or Path("studiomdl")
P2 = STUDIOMDL.parent.parent
RMDLCONV = r5edenv.path("R5ED_RMDLCONV", required=False) or Path("rmdlconv")
MODEL_DIR = "mw2/terminal"
MATERIAL_DIR = "models/mw2/terminal"


def material_path(iw_name):
    return f"{MATERIAL_DIR}/{iw_name.split('/')[-1]}_rgdp"


def glb_triangles(path):
    doc, blob = read_glb(path)
    for mesh in doc["meshes"]:
        for prim in mesh["primitives"]:
            a = prim["attributes"]
            pos = accessor(doc, blob, a["POSITION"]).astype(np.float64)
            nrm = accessor(doc, blob, a["NORMAL"]).astype(np.float64)
            uv = accessor(doc, blob, a["TEXCOORD_0"]).astype(np.float64)
            idx = accessor(doc, blob, prim["indices"]).reshape(-1, 3)
            mat = doc["materials"][prim["material"]]["name"]
            yield mat, pos, nrm, uv, idx


# rmdlconv packs positions only inside +-1023 (x, y) and +-2047 (z); past that a mesh
# keeps float positions, which no rgdp shader set reads. Chunks stay inside with room
# for triangles that cross a cell edge.
PACKED_HALF = np.array([1023.0, 1023.0, 2047.0])
CHUNK_HALF = np.array([900.0, 900.0, 1900.0])


def subdivide(mat, pos, nrm, uv, idx, limit=CHUNK_HALF * 0.5):
    """Splits every triangle at the midpoint of its longest edge until no edge
    spans more than `limit` on any axis, so each one fits inside a chunk."""
    pos, nrm, uv = list(pos), list(nrm), list(uv)
    todo, done = [tuple(t) for t in idx], []
    while todo:
        t = todo.pop()
        edges = [(t[0], t[1], t[2]), (t[1], t[2], t[0]), (t[2], t[0], t[1])]
        span = [np.abs(pos[a] - pos[b]) / limit for a, b, _ in edges]
        i = int(np.argmax([s.max() for s in span]))
        if span[i].max() <= 1.0:
            done.append(t)
            continue
        a, b, c = edges[i]
        m = len(pos)
        pos.append((pos[a] + pos[b]) * 0.5)
        n = nrm[a] + nrm[b]
        nrm.append(n / max(np.linalg.norm(n), 1e-12))
        uv.append((uv[a] + uv[b]) * 0.5)
        todo += [(a, m, c), (m, b, c)]
    return mat, np.array(pos), np.array(nrm), np.array(uv), np.array(done, dtype=np.int64).reshape(-1, 3)


def chunks_of(glb):
    """None when the model fits the packed range, else its triangles split into
    [(offset, [(material, pos, nrm, uv, idx), ...]), ...] with each chunk's
    vertices about its own centre `offset` (model space)."""
    prims = list(glb_triangles(glb))
    allpos = np.concatenate([p[1] for p in prims])
    lo, hi = allpos.min(axis=0), allpos.max(axis=0)
    if np.all(lo >= -PACKED_HALF) and np.all(hi <= PACKED_HALF):
        return None
    prims = [subdivide(*p) for p in prims]
    cells = {}
    for mat, pos, nrm, uv, idx in prims:
        cent = pos[idx].mean(axis=1)
        key = np.floor((cent - lo) / (2 * CHUNK_HALF)).astype(int)
        for k in {tuple(r) for r in key}:
            sel = idx[np.all(key == k, axis=1)]
            cells.setdefault(k, []).append((mat, pos, nrm, uv, sel))
    out = []
    for k in sorted(cells):
        parts = cells[k]
        used = np.concatenate([pos[sel.ravel()] for _, pos, _, _, sel in parts])
        c = (used.min(axis=0) + used.max(axis=0)) * 0.5
        if np.any(used.max(axis=0) - c > PACKED_HALF):
            raise SystemExit(f"{glb}: a chunk still spans past the packed range; a single triangle is too large")
        out.append((c, [(m, pos - c, nrm, uv, sel) for m, pos, nrm, uv, sel in parts]))
    return out


def write_smd(path, glb, material_map=None, prims=None):
    lines = ["version 1", "nodes", '0 "root" -1', "end", "skeleton", "time 0", "0 0 0 0 0 0 0", "end", "triangles"]
    for mat, pos, nrm, uv, idx in prims if prims is not None else glb_triangles(glb):
        name = material_map(mat) if material_map else "phys"
        for t in idx:
            lines.append(name)
            for v in t:
                p, n, u = pos[v], nrm[v], uv[v]
                # studiomdl turns a $staticprop +90 degrees about Z; write it -90 so it lands unturned.
                lines.append(f"0 {p[1]:.6f} {-p[0]:.6f} {p[2]:.6f} {n[1]:.6f} {-n[0]:.6f} {n[2]:.6f} {u[0]:.6f} {1.0 - u[1]:.6f}")
    lines.append("end")
    Path(path).write_text("\n".join(lines) + "\n")


# Surfaces that are air in MW2's own collision: foliage cards, shadow cards, light
# beams and flares, and blended decals.
AIR_WORDS = ("tree_", "ivy", "leaf", "leaves", "foliage", "shadow", "gfx_", "godray", "flare", "_dec_")


def is_air(iw_name):
    leaf = iw_name.split("/")[-1].lower()
    return any(w in leaf for w in AIR_WORDS) and "trunk" not in leaf


def compile_model(name, glb, work, game, colliding, prims=None):
    src = work / "src" / name
    src.mkdir(parents=True, exist_ok=True)
    write_smd(src / "ref.smd", glb, material_path, prims)
    qc = [f'$modelname "{MODEL_DIR}/{name}.mdl"', '$cdmaterials ""', "$staticprop",
          '$body body "ref.smd"', '$sequence idle "ref.smd"']
    if colliding:
        solid = [p for p in (prims if prims is not None else glb_triangles(glb)) if not is_air(p[0])]
        if solid:
            write_smd(src / "phys.smd", glb, None, solid)
            qc.append('$collisionmodel "phys.smd" { $concave $maxconvexpieces 256 }')
    (src / f"{name}.qc").write_text("\n".join(qc) + "\n")
    r = subprocess.run([str(STUDIOMDL), "-game", str(game), "-nop4", "-nox360", "-quiet", str(src / f"{name}.qc")],
                       cwd=src, capture_output=True, text=True, errors="replace")
    ok = (game / "models" / MODEL_DIR / f"{name}.mdl").is_file()
    return name, ok, "" if ok else (r.stdout[-600:] + r.stderr[-300:])


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("glb_dir", help="iw4statics.py output (models/, static_models.json)")
    ap.add_argument("world_dir", help="<asset>.world, for the clipMap's colliding models")
    ap.add_argument("work")
    ap.add_argument("--only", nargs="*")
    ap.add_argument("--solid", nargs="*", default=[], help="models that collide although the clipMap has no instance (entity props)")
    ap.add_argument("--jobs", type=int, default=6)
    ap.add_argument("--model-dir", default="mw2/terminal")
    ap.add_argument("--material-dir", default="models/mw2/terminal")
    args = ap.parse_args()
    global MODEL_DIR, MATERIAL_DIR
    MODEL_DIR = args.model_dir
    MATERIAL_DIR = args.material_dir
    glb_dir, work = Path(args.glb_dir), Path(args.work)
    game = work / "game"
    game.mkdir(parents=True, exist_ok=True)
    (game / "gameinfo.txt").write_text((P2 / "portal2" / "gameinfo.txt").read_text())

    models = json.loads((glb_dir / "static_models.json").read_text())["models"]
    clip = json.loads((Path(args.world_dir) / "clipmap.json").read_text())
    colliding = {s["model"] for s in clip["staticModels"]} | set(args.solid)
    names = sorted(args.only or models)

    # An oversized model is compiled as chunks <name>_cN; iw4chunks.py places them.
    units, split = [], {}
    for n in names:
        parts = chunks_of(glb_dir / models[n]["glb"])
        if parts is None:
            units.append((n, n, None))
            continue
        split[n] = []
        for i, (offset, prims) in enumerate(parts):
            units.append((f"{n}_c{i}", n, prims))
            split[n].append({"model": f"{n}_c{i}", "offset": offset.tolist()})
        for d in (game / "models" / MODEL_DIR, work / "client" / MODEL_DIR, work / "dedi" / MODEL_DIR):
            for f in d.glob(f"{n}.*"):
                f.unlink()
        print(f"{n}: past the packed range, {len(parts)} chunks")
    chunk_file = work / "chunks.json"
    known = json.loads(chunk_file.read_text()) if chunk_file.is_file() else {}
    known.update(split)
    chunk_file.write_text(json.dumps(known, indent=1))

    failed = []
    with ThreadPoolExecutor(args.jobs) as pool:
        jobs = [pool.submit(compile_model, u, glb_dir / models[n]["glb"], work, game, n in colliding, prims)
                for u, n, prims in units]
        for j in jobs:
            name, ok, err = j.result()
            if not ok:
                failed.append(name)
                print(f"studiomdl failed: {name}\n{err}")
    print(f"studiomdl: {len(units) - len(failed)}/{len(units)} compiled")

    mdl_src = game / "models"
    client = work / "client"
    dedi = work / "dedi"
    for d in (client, dedi):
        d.mkdir(parents=True, exist_ok=True)
    subprocess.run([str(RMDLCONV), "-v49", str(mdl_src), str(client), "-targetversion", "17", "-nopause", "-vertexcolor"],
                   stdout=open(work / "rmdlconv_client.log", "w"), stderr=subprocess.STDOUT)
    subprocess.run([str(RMDLCONV), "-v17", str(client), str(dedi), "-nopause"],
                   stdout=open(work / "rmdlconv_dedi.log", "w"), stderr=subprocess.STDOUT)
    n_client = len(list(client.rglob("*.rmdl")))
    n_dedi = len(list(dedi.rglob("*.rmdl")))
    print(f"rmdlconv: client {n_client}, dedi {n_dedi}")
    (work / "props_report.json").write_text(json.dumps({"failed": failed, "client": n_client, "dedi": n_dedi}, indent=1))


if __name__ == "__main__":
    main()
