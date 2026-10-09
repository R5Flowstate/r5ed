"""Static mesh placements of an Apex Mobile level (all its sublevels): every StaticMeshComponent and
InstancedStaticMeshComponent, resolved to world matrices (Unreal space, cm)."""
import glob, os, struct, numpy as np
from upkg import Package, R
from uprops import read_props

def rotator_matrix(pitch, yaw, roll):
    """FRotationMatrix: rows are the X, Y, Z axes; row vectors (v * M)."""
    p, y, r = np.radians([pitch, yaw, roll])
    SP, CP, SY, CY, SR, CR = np.sin(p), np.cos(p), np.sin(y), np.cos(y), np.sin(r), np.cos(r)
    return np.array([
        [CP * CY, CP * SY, SP],
        [SR * SP * CY - CR * SY, SR * SP * SY + CR * CY, -SR * CP],
        [-(CR * SP * CY + SR * SY), CY * SR - CR * SP * SY, CR * CP]])

def trs(loc, rot, scale):
    M = np.eye(4); M[:3, :3] = np.diag(scale) @ rotator_matrix(*rot); M[3, :3] = loc; return M

def level_placements(path):
    p = Package(path); exp = p.exports
    props = {}
    for i, e in enumerate(exp):
        if e['class'] in ('StaticMeshComponent', 'InstancedStaticMeshComponent', 'HierarchicalInstancedStaticMeshComponent',
                          'SceneComponent', 'StaticMeshActor', 'Actor') or e['class'].endswith('Component'):
            b = p.export_bytes(e); r = R(b)
            try: pr = {t['name']: t for t in read_props(p, r)}
            except Exception: continue
            props[i] = (pr, b, r.o)
    def val(pr, k, d=None):
        t = pr.get(k); return d if t is None else t['value']
    def local(i):
        pr = props[i][0]
        return trs(val(pr, 'RelativeLocation', (0, 0, 0)), val(pr, 'RelativeRotation', (0, 0, 0)), val(pr, 'RelativeScale3D', (1, 1, 1)))
    def world(i, depth=0):
        M = local(i); pr = props[i][0]; par = pr.get('AttachParent')
        if par is not None and depth < 32:
            raw = R(par['raw']).i32()
            if raw > 0 and raw - 1 in props: M = M @ world(raw - 1, depth + 1)
        return M
    out = []
    for i, e in enumerate(exp):
        if i not in props or 'StaticMeshComponent' not in e['class']: continue
        pr, b, o = props[i]
        mesh = val(pr, 'StaticMesh')
        if not mesh or mesh[1] is None: continue
        hidden = val(pr, 'bHiddenInGame', False) or (val(pr, 'bVisible', True) is False)
        ovr = [x[1] for x in (val(pr, 'OverrideMaterials') or []) if isinstance(x, tuple)] if isinstance(val(pr, 'OverrideMaterials'), list) else []
        W = world(i)
        rec = dict(level=os.path.basename(path), name=e['name'], outer=p.ref(e['outer']), mesh=mesh[1], hidden=bool(hidden),
                   override=ovr, cls=e['class'])
        if 'Instanced' in e['class']:
            k = b.find(struct.pack('<ii', 112, 0)[:4], o)
            inst = []
            while k >= 0:
                n = struct.unpack_from('<i', b, k + 4)[0]
                if 0 < n < 100000 and k + 8 + 112 * n <= len(b):
                    for j in range(n):
                        f = struct.unpack_from('<28f', b, k + 8 + 112 * j)
                        IM = np.array(f[:16]).reshape(4, 4); IM[3, 3] = 1
                        inst.append(IM @ W)
                    break
                k = b.find(struct.pack('<i', 112), k + 1)
            rec['matrices'] = inst
        else:
            rec['matrices'] = [W]
        out.append(rec)
    return out

def all_placements(level_dir, prefix):
    res = []
    for f in sorted(glob.glob(os.path.join(level_dir, prefix + '*.umap'))):
        if f.endswith('_BuiltData.umap'): continue
        res += level_placements(f)
    return res
