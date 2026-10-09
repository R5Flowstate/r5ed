"""Apex Legends Mobile cooked StaticMesh (.uasset/.uexp/.ubulk) -> .glb, or a corpus check.

    py scripts/apexm/mesh2glb.py <mesh.uasset> <out.glb> [--lod N]
    py scripts/apexm/mesh2glb.py --check <dir>     # parse every StaticMesh under <dir>, report failures

The .glb is right-handed, Z up, in centimetres; import it with
`r5ed-compile import-glb <project> <glb> --options '{"scale":0.3937,"up":"z"}'`.
The format is described in docs/APEXM_STATICMESH_FORMAT.md.
"""
import argparse, collections, glob, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from upkg import Package
from umesh import static_mesh
from glbout import write_glb, material_slots

def mesh_export(p):
    ex = [e for e in p.exports if e['class'] == 'StaticMesh']
    return ex[0] if ex else None

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('src'); ap.add_argument('out', nargs='?'); ap.add_argument('--lod', type=int, default=0)
    ap.add_argument('--check', action='store_true')
    a = ap.parse_args()
    if a.check:
        c = collections.Counter(); first = {}
        for f in glob.glob(os.path.join(a.src, '**', '*.uasset'), recursive=True):
            with open(f, 'rb') as fh:
                if b'StaticMesh' not in fh.read(4096): continue
            try:
                p = Package(f); e = mesh_export(p)
                if e is None: continue
                m = static_mesh(p, e); c['ok'] += 1; c['lods'] += len(m['lods'])
            except Exception as x:
                k = repr(x)[:80]; c['FAIL ' + k] += 1; first.setdefault(k, f)
        for k, v in c.most_common(): print(v, k)
        for k, f in first.items(): print(k, '<-', f)
        return 1 if any(k.startswith('FAIL') for k in c) else 0
    p = Package(a.src); e = mesh_export(p)
    if e is None: sys.exit(f'{a.src}: no StaticMesh export')
    m = static_mesh(p, e); lod = m['lods'][a.lod]
    write_glb(a.out, lod, material_slots(p, m))
    print(f"{e['name']}: LOD {a.lod}/{len(m['lods'])}, {lod['num_verts']} verts, {len(lod['indices'])//3} tris, "
          f"{len(lod['sections'])} sections -> {a.out}")

if __name__ == '__main__':
    sys.exit(main())
