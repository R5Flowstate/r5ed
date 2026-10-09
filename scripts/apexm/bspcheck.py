"""Checks a compiled r5 client map's world-mesh visibility data against the meshes' own vertices.

    python3 scripts/apexm/bspcheck.py <map>.bsp [x y z]

The map must be written as separate lumps (<map>.bsp.XXXX.bsp_lump, as r5ed-compile writes them).
For every render mesh (0x50) it decodes the vertices the engine draws (slot =
material_sort.first_vertex + index, FINDINGS 6d) and checks them against the mesh's bounds (0x51),
its object reference and reference box (0x78/0x79), and the cell AABB tree (0x77) above it. With a
point (the `mat_staticprop` hit position), it also lists the meshes whose triangles contain it.
A clean report means the engine is handed correct culling data; a mesh that still vanishes in game
has another cause.
"""
import collections, os, sys
import numpy as np

MESH = np.dtype([('fi', '<u4'), ('tc', '<u2'), ('fv', '<u2'), ('lvc', '<u2'), ('vt', 'u1'), ('cube', 'u1'),
                 ('u0', '4u1'), ('ls', '<u2', 2), ('fade', '<u2'), ('ms', '<u2'), ('flags', '<u4')])
SORT = np.dtype([('td', '<u2'), ('lm', '<i2'), ('u0', '<u2'), ('lvc', '<u2'), ('fv', '<u4')])
NODE = np.dtype([('mins', '<f4', 3), ('ch', '<u4'), ('maxs', '<f4', 3), ('refs', '<u4')])
# Vertex lump and record width (u32 words) per mesh vertex type.
VERTS = {0: (0x47, 5), 1: (0x48, 7), 2: (0x49, 8), 3: (0x4A, 6)}
TOL = 0.5
# MESH_FLAG_SKY: a mesh the skybox shows through.
SKY = 0x4


def lump(base, i, dt):
    f = f'{base}.{i:04x}.bsp_lump'
    return np.fromfile(f, dt) if os.path.exists(f) else np.zeros(0, dt)


def main():
    base = sys.argv[1]
    point = np.array([float(v) for v in sys.argv[2:5]]) if len(sys.argv) >= 5 else None
    pos = lump(base, 0x03, '<f4').reshape(-1, 3)
    meshes, bounds = lump(base, 0x50, MESH), lump(base, 0x51, '<f4').reshape(-1, 8)
    sorts, idx = lump(base, 0x52, SORT), lump(base, 0x4F, '<u2')
    verts = {t: lump(base, i, '<u4').reshape(-1, w) for t, (i, w) in VERTS.items()}
    nodes, refs, ref_bounds = lump(base, 0x77, NODE), lump(base, 0x78, '<u4'), lump(base, 0x79, '<f4').reshape(-1, 8)
    print(f'{len(meshes)} meshes, {len(nodes)} tree nodes, {len(refs)} references')

    lo, hi, tris, problems = {}, {}, {}, collections.Counter()
    for i, m in enumerate(meshes):
        vl = verts[int(m['vt'])]
        slots = int(sorts[m['ms']]['fv']) + idx[m['fi']:m['fi'] + 3 * m['tc']].astype(np.int64)
        if len(slots) == 0 or slots.max() >= len(vl):
            problems['mesh indexes past its vertex lump'] += 1
            continue
        P = pos[vl[slots, 0]]
        lo[i], hi[i], tris[i] = P.min(0), P.max(0), P.reshape(-1, 3, 3)
        o, r, e = bounds[i, :3], bounds[i, 3], bounds[i, 4:7]
        if (lo[i] < o - e - TOL).any() or (hi[i] > o + e + TOL).any() or (np.linalg.norm(P - o, axis=1) > r + TOL).any():
            problems['vertices outside the 0x51 bounds'] += 1
    count = collections.Counter(int(r) for r in refs)
    # Sky meshes (flag 0x4) are drawn through the sky path; stock references none of its 5 either.
    problems['meshes no reference names'] += sum(1 for i in range(len(meshes)) if count[i] == 0 and not meshes[i]['flags'] & SKY)
    for k, r in enumerate(refs):
        r = int(r)
        if r in lo and ((lo[r] < ref_bounds[k, :3] - TOL).any() or (hi[r] > ref_bounds[k, 4:7] + TOL).any()):
            problems['reference boxes that miss their mesh'] += 1
    reached = set()

    def walk(n, depth=0):
        nd = nodes[n]
        cc, fc = int(nd['ch'] & 0xFF), int((nd['ch'] >> 8) & 0xFFFF)
        rc, fr = int(nd['refs'] & 0xFF), int((nd['refs'] >> 8) & 0xFFFF)
        for k in range(fr, fr + rc):
            reached.add(k)
            if (ref_bounds[k, :3] < nd['mins'] - TOL).any() or (ref_bounds[k, 4:7] > nd['maxs'] + TOL).any():
                problems['tree nodes that miss a reference'] += 1
        for c in range(fc, fc + cc):
            if (nodes[c]['mins'] < nd['mins'] - TOL).any() or (nodes[c]['maxs'] > nd['maxs'] + TOL).any():
                problems['tree nodes that miss a child'] += 1
            if depth < 64:
                walk(c, depth + 1)
    if len(nodes):
        walk(0)
    problems['references the world tree never reaches'] += sum(1 for k, r in enumerate(refs) if k not in reached and int(r) < len(meshes))
    for k, v in problems.items():
        print(f'  {k}: {v}')
    print('clean' if not any(problems.values()) else 'PROBLEMS FOUND')

    if point is not None:
        print(f'meshes with a triangle containing {point.tolist()}:')
        for i, T in tris.items():
            n = np.cross(T[:, 1] - T[:, 0], T[:, 2] - T[:, 0]); ln = np.linalg.norm(n, axis=1)
            ok = ln > 1e-9
            if not ok.any():
                continue
            T, nn = T[ok], n[ok] / ln[ok, None]
            d = ((point - T[:, 0]) * nn).sum(1); q = point - d[:, None] * nn
            side = lambda a, b: (np.cross(b - a, q - a) * nn).sum(1)
            s = np.stack([side(T[:, 0], T[:, 1]), side(T[:, 1], T[:, 2]), side(T[:, 2], T[:, 0])], 1)
            hit = (((s >= -TOL).all(1)) | ((s <= TOL).all(1))) & (np.abs(d) < TOL)
            if hit.any():
                k = int(np.where(hit)[0][0])
                print(f'  mesh {i}: sort {int(meshes[i]["ms"])}, {int(meshes[i]["tc"])} triangles, '
                      f'offset {d[k]:+.3f}, normal {nn[k].round(3).tolist()}, bounds {bounds[i, :3].round(1).tolist()} '
                      f'+- {bounds[i, 4:7].round(1).tolist()}')


if __name__ == '__main__':
    main()
