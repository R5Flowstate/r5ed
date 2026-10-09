"""One decoded Apex Mobile StaticMesh LOD -> a .glb (one primitive per section, material named by slot).

Unreal is left-handed, Z up, centimetres. The .glb is written right-handed and Z up (Y negated, which also
turns the stored clockwise winding counter-clockwise), still in centimetres; import it with up=z and scale
1/2.54 for inches."""
import json, struct, numpy as np

def oct7(a, b):
    u = (a & 0x7f) / 63.5 - 1; v = (b & 0x7f) / 63.5 - 1
    z = 1 - np.abs(u) - np.abs(v); t = np.clip(-z, 0, None)
    u = u + np.where(u >= 0, -t, t); v = v + np.where(v >= 0, -t, t)
    n = np.stack([u, v, z], 1); return n / np.linalg.norm(n, axis=1, keepdims=True)

def write_glb(path, lod, material_names):
    P = lod['positions'].astype(np.float32).copy(); P[:, 1] *= -1
    T = lod['tangents_raw'].astype(np.int32)
    N = oct7(T[:, 2], T[:, 3]).astype(np.float32); N[:, 1] *= -1
    UV = lod['uv'][:, 0, :].astype(np.float32)
    idx = lod['indices'].astype(np.uint32)
    blobs = []; views = []; accs = []
    def add(arr, target, comp, typ, minmax=False):
        off = sum(len(b) for b in blobs); data = arr.tobytes(); pad = (-len(data)) % 4
        blobs.append(data + b'\0' * pad)
        views.append(dict(buffer=0, byteOffset=off, byteLength=len(data), **({'target': target} if target else {})))
        a = dict(bufferView=len(views) - 1, componentType=comp, count=len(arr), type=typ)
        if minmax: a['min'] = arr.min(0).tolist(); a['max'] = arr.max(0).tolist()
        accs.append(a); return len(accs) - 1
    ip = add(P, 34962, 5126, 'VEC3', True); inn = add(N, 34962, 5126, 'VEC3'); iuv = add(UV, 34962, 5126, 'VEC2')
    prims = []; mats = []
    for s in lod['sections']:
        tri = idx[s['first_index']:s['first_index'] + 3 * s['num_tris']].reshape(-1, 3)
        ii = add(np.ascontiguousarray(tri).reshape(-1), 34963, 5125, 'SCALAR')
        name = material_names[s['material']] if 0 <= s['material'] < len(material_names) else f"material_{s['material']}"
        if name not in mats: mats.append(name)
        prims.append(dict(attributes=dict(POSITION=ip, NORMAL=inn, TEXCOORD_0=iuv), indices=ii, material=mats.index(name)))
    bin_ = b''.join(blobs)
    g = dict(asset=dict(version='2.0'), scene=0, scenes=[dict(nodes=[0])], nodes=[dict(mesh=0)],
             meshes=[dict(primitives=prims)], materials=[dict(name=m) for m in mats],
             accessors=accs, bufferViews=views, buffers=[dict(byteLength=len(bin_))])
    js = json.dumps(g).encode(); js += b' ' * ((-len(js)) % 4)
    out = struct.pack('<III', 0x46546C67, 2, 12 + 8 + len(js) + 8 + len(bin_)) + struct.pack('<II', len(js), 0x4E4F534A) + js + struct.pack('<II', len(bin_), 0x004E4942) + bin_
    open(path, 'wb').write(out)

def material_slots(p, mesh):
    """Slot index -> material asset name, from the StaticMaterials property."""
    from upkg import R
    from uprops import read_props, read_tag
    t = mesh['props'].get('StaticMaterials')
    if t is None: return []
    r = R(t['raw']); n = r.i32(); read_tag(p, r); out = []
    for _ in range(n):
        el = {x['name']: x for x in read_props(p, r)}
        mi = el.get('MaterialInterface'); out.append(mi['value'][1] if mi and isinstance(mi['value'], tuple) else 'none')
    return out
