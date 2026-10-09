"""Level placements -> one world .glb (right-handed, Z up, cm) with embedded base-colour textures.

Texture2DArray materials (the merged atlases) are split per source texture: the colour stream's R
byte is its first array slice and G how it was cut into parts (0x11 2x2, 0x10 two along U), and the
parts are put back together before the triangles are split into one material each. Collision proxies (`*_Col*`) and the air-wall shell are left out of the
visual .glb; `airwall` selects the shell alone."""
import io, json, os, pickle, struct, sys, collections, numpy as np
from PIL import Image
from assemble import load
from umat import slot_paths, material_params, material_info, package_file
from utex import read_texture
from glbout import oct7

# Sublevels whose meshes are world geometry; decals, FX, light cards and the sky dome are left out.
KEEP_SUFFIXES = ('_Asset.umap', '_ZipLine.umap', '_Main_Random.umap', '_AirWall.umap')
SKIP_MESH = {'SM_TDM_canyonlands_night_car_02', 'AirWall_Mesh', 'Cylinder', 'Cube', 'Plane'}
DIFFUSE_KEYS = ('BaseColor', 'Diffuse', 'SplatColor', 'EmissiveMap', 'Param', 'CoverTexture', 'BaseColor_Lite', 'WPOTex')
MAX_TEX = 1024

def visual(name): return '_Col' not in name and name != 'AirWall_art'
def airwall(name): return name == 'AirWall_art'

def short(path): return path.rsplit('/', 1)[-1]

class Textures:
    def __init__(s): s.cache = {}; s.fail = collections.Counter()
    def normal(s, tex_path, slice_=0, atlas=False):
        """A tangent-space normal map as RGB (X, Y, rebuilt Z). Plain maps keep X,Y in R,G (B empty);
        the merged-atlas array keeps them in G,B (R and A hold other surface data). Unreal's green
        convention is kept: objmat passes it through to the material as it does Source's."""
        key = ('nm', tex_path, slice_)
        if key in s.cache: return s.cache[key]
        img = None
        f = package_file(tex_path)
        if f:
            try:
                src = s.rgba(tex_path, slice_)
                if src is not None:
                    a = src.astype(np.float32) / 255.0 * 2 - 1
                    x, y = (a[..., 1], a[..., 2]) if atlas else (a[..., 0], a[..., 1])
                    z = np.sqrt(np.clip(1 - x * x - y * y, 0, 1))
                    rgb = np.clip((np.stack([x, y, z], -1) + 1) * 127.5, 0, 255).astype(np.uint8)
                    im = Image.fromarray(rgb)
                    if max(im.size) > MAX_TEX: im.thumbnail((MAX_TEX, MAX_TEX))
                    buf = io.BytesIO(); im.save(buf, 'PNG'); img = buf.getvalue()
            except Exception as e: s.fail['normal ' + repr(e)[:70]] += 1
        s.cache[key] = img; return img

    def rgba(s, tex_path, slice_=0):
        """Decoded mip 0 of a texture (or array slice) as an RGBA uint8 array, or None. A merged-atlas
        key `base + 1000 * split` is the source texture reassembled from its parts (see atlas_texture)."""
        f = package_file(tex_path) if tex_path else None
        if not f: return None
        try:
            if (tex_path, 'all') not in s.cache: s.cache[(tex_path, 'all')] = read_texture(f)[1]
            sl = s.cache[(tex_path, 'all')]
            if slice_ >= 1000: return atlas_texture(sl, slice_ % 1000, slice_ // 1000)
            return sl[slice_] if slice_ < len(sl) else None
        except Exception as e:
            s.fail[repr(e)[:80]] += 1; return None

    def get(s, tex_path, slice_=0):
        key = (tex_path, slice_)
        if key in s.cache: return s.cache[key]
        img = None
        f = package_file(tex_path)
        if f:
            try:
                if (tex_path, 'all') not in s.cache:
                    s.cache[(tex_path, 'all')] = read_texture(f)[1]
                sl = s.cache[(tex_path, 'all')]
                if slice_ < len(sl):
                    im = Image.fromarray(sl[slice_]).convert('RGB')
                    if max(im.size) > MAX_TEX: im.thumbnail((MAX_TEX, MAX_TEX))
                    buf = io.BytesIO(); im.save(buf, 'PNG'); img = buf.getvalue()
            except Exception as e: s.fail[repr(e)[:80]] += 1
        else: s.fail['no file ' + short(tex_path)] += 1
        s.cache[key] = img; return img

def atlas_texture(slices, base, split):
    """A merged-atlas source texture from its parts. The atlas tool cut each source texture into
    256x256 parts stored in consecutive slices from `base`; the vertex colour's G says how
    (0x11: 2x2, row-major; 0x10: two parts side by side along U; 0: one part). The UVs address the
    whole source texture and the mobile shader picks the part per pixel, so the part alone is wrong."""
    nu, nv = 1 << ((split >> 4) & 0xF), 1 << (split & 0xF)
    if base + nu * nv > len(slices): nu = nv = 1
    rows = [np.concatenate([slices[base + y * nu + x] for x in range(nu)], 1) for y in range(nv)]
    return np.concatenate(rows, 0)

def png(arr):
    im = Image.fromarray(np.ascontiguousarray(arr[..., :3]).astype(np.uint8))
    if max(im.size) > MAX_TEX: im.thumbnail((MAX_TEX, MAX_TEX))
    buf = io.BytesIO(); im.save(buf, 'PNG'); return buf.getvalue()

def hue(c):
    """An HDR colour as (unit-peak colour, intensity)."""
    c = np.array(c[:3], dtype=np.float64); m = c.max()
    return (c / m if m > 0 else np.ones(3)), float(m)

def classify(info):
    """How a material instance ports: the template class objmat should build it on, and the
    layers that go into the .glb. Classes are iw4mat's: world_opaque, world_emissive, world_glass."""
    root = info['parents'][-1] if info['parents'] else ''
    ov, vec, sc = info['overrides'], info['vectors'], info['scalars']
    out = dict(klass='world_opaque', tint=None, emissive=None, emissive_from=None, opacity=None,
               two_sided=bool(ov.get('TwoSided')))
    if 'TintColor' in vec: out['tint'] = vec['TintColor'][:3]
    if ov.get('BlendMode') == 'BLEND_Translucent':
        out['klass'] = 'world_glass'
        out['opacity'] = float(sc.get('Opacity', sc.get('OpacityMin', 0.4)))
        if out['tint'] is None:
            out['tint'] = next((vec[k][:3] for k in ('BaseColor', 'Color') if k in vec), None)
        return out
    unlit = ov.get('ShadingModel') == 'MSM_Unlit'
    if root == 'M_BasePanner':
        # A scrolling LED ticker: an emissive texture tinted by Param. None of the templates scrolls a lit
        # surface, so it ports static; the speed rides along in the .glb.
        out.update(klass='world_emissive', emissive=vec.get('Param', [1, 1, 1, 1]), emissive_from='self',
                   scroll=[float(sc.get('PannerSpeed_U', 0)), float(sc.get('PannerSpeed_V', 0))])
    elif root in ('M_Emissive', 'M_videoscreen_Mat') or unlit:
        out.update(klass='world_emissive', emissive=vec.get('Emissive', [1, 1, 1, 1]), emissive_from='self')
    elif 'Emissive' in vec and any(v > 0 for v in vec['Emissive'][:3]):
        out.update(klass='world_emissive', emissive=vec['Emissive'], emissive_from='alpha')
    return out

def main(out_glb, out_json, select=None, pl=None):
    pl = pl if pl is not None else pickle.load(open('../placements.pkl', 'rb'))
    tex = Textures(); matparams = {}
    groups = collections.OrderedDict()   # (instance id, material key) -> lists
    materials = collections.OrderedDict()  # material key -> (name, png bytes or None)
    stats = collections.Counter()
    for inst_id, p in enumerate(pl):
        if not p['level'].endswith(KEEP_SUFFIXES) or p['hidden'] or p['mesh'] in SKIP_MESH: continue
        if not (select or visual)(p['mesh']): continue
        got = load(p['mesh'])
        if not got: stats['mesh missing'] += 1; continue
        pk, m, f = got; l = m['lods'][0]
        if 'positions' not in l: stats['no lod0'] += 1; continue
        slots = slot_paths(pk, m)
        P = l['positions']; T = l['tangents_raw'].astype(np.int32); N = oct7(T[:, 2], T[:, 3]); UV = l['uv'][:, 0, :].astype(np.float32)
        col = next((c for st, cn, c in l['colors'] if st == 4 and c is not None), None)
        I = l['indices'].astype(np.int64)
        for M in p['matrices']:
            V = np.c_[P, np.ones(len(P))] @ M
            W = V[:, :3].copy()
            R3 = M[:3, :3]; NN = N @ np.linalg.inv(R3).T; NN /= np.linalg.norm(NN, axis=1, keepdims=True) + 1e-12
            mirror = np.linalg.det(R3) < 0
            # Unreal (left-handed) -> right-handed: negate Y
            W[:, 1] *= -1; NN[:, 1] *= -1
            for s in l['sections']:
                tri = I[s['first_index']:s['first_index'] + 3 * s['num_tris']].reshape(-1, 3)
                # The stored order winds clockwise in Unreal's left-handed space; negating Y makes that
                # counter-clockwise in a right-handed one, so only a mirroring instance needs reversing.
                tri = tri[:, [0, 2, 1]] if mirror else tri
                mi = s['material']
                ovr = p['override'][mi] if mi < len(p['override']) and p['override'][mi] else None
                name, path = slots[mi] if mi < len(slots) else (None, None)
                if ovr: name = ovr  # override names an object; resolve by name
                if path not in matparams: matparams[path] = material_params(path)[0] if path else {}
                params = matparams[path]
                dk = next((k for k in DIFFUSE_KEYS if k in params), None)
                tex_path = params.get(dk) if dk else None
                is_array = tex_path and short(tex_path).startswith('T2A_')
                nrm = params.get('Normal'); mix = params.get('MIX')
                if is_array and col is not None:
                    # FColor BGRA: R is the first slice of the source texture, G how it was split into parts
                    sl = col[tri[:, 0], 2].astype(int) + 1000 * col[tri[:, 0], 1].astype(int)
                    keys = [(f'{short(tex_path)}_s{v % 1000}' + (f'x{v // 1000}' if v >= 1000 else ''), tex_path, int(v), nrm, path, mix) for v in sl]
                else:
                    k = (short(path) if path else (name or 'none'), tex_path, 0, nrm, path, mix); keys = [k] * len(tri)
                for t, k in zip(tri, keys):
                    g = groups.setdefault((inst_id, k[0]), [])
                    g.append(t)
                    if k[0] not in materials: materials[k[0]] = k
            stats['instances'] += 1
            # stash the instance's vertex arrays for this instance id
            groups.setdefault(('verts', inst_id, len([x for x in groups if x[0] == 'verts' and x[1] == inst_id])), (W.astype(np.float32), NN.astype(np.float32), UV))
    write(out_glb, out_json, groups, materials, tex, stats)

def write(out_glb, out_json, groups, materials, tex, stats):
    blobs = []; views = []; accs = []; images = []; textures = []; gmats = []; meshes = []; nodes = []
    def blob(data, target=None):
        off = sum(len(b) for b in blobs); pad = (-len(data)) % 4; blobs.append(data + b'\0' * pad)
        v = dict(buffer=0, byteOffset=off, byteLength=len(data))
        if target: v['target'] = target
        views.append(v); return len(views) - 1
    def acc(arr, target, comp, typ, mm=False):
        a = dict(bufferView=blob(arr.tobytes(), target), componentType=comp, count=len(arr), type=typ)
        if mm: a['min'] = arr.min(0).tolist(); a['max'] = arr.max(0).tolist()
        accs.append(a); return len(accs) - 1
    mat_index = {}
    def add_image(data, name):
        images.append(dict(bufferView=blob(data), mimeType='image/png', name=name)); textures.append(dict(source=len(images) - 1, sampler=0))
        return len(textures) - 1
    infos = {}
    for key, (name, tex_path, sl, nrm, mpath, mix) in materials.items():
        if mpath not in infos: infos[mpath] = classify(material_info(mpath)) if mpath else classify(dict(parents=[], overrides={}, vectors={}, scalars={}))
        c = infos[mpath]
        m = dict(name=name, pbrMetallicRoughness=dict(metallicFactor=0.0, roughnessFactor=1.0), extras=dict(r5ed_class=c['klass']))
        base = tex.rgba(tex_path, sl) if tex_path else None
        if base is not None:
            rgb = base[..., :3].astype(np.float64)
            if c['tint'] is not None: rgb = rgb * np.array(c['tint'])[None, None, :]; stats['tinted'] += 1
            m['pbrMetallicRoughness']['baseColorTexture'] = dict(index=add_image(png(np.clip(rgb, 0, 255)), name))
            if c['emissive'] is not None:
                h, intensity = hue(c['emissive'])
                glow = rgb * h[None, None, :]
                if c['emissive_from'] == 'alpha': glow = glow * (base[..., 3:4].astype(np.float64) / 255.0)
                m['emissiveTexture'] = dict(index=add_image(png(np.clip(glow, 0, 255)), name + '_e'))
                m['emissiveFactor'] = [1.0, 1.0, 1.0]; m['extras']['emissive_intensity'] = round(intensity, 3)
                stats['emissive'] += 1
        else:
            stats['material without texture'] += 1
            if c['tint'] is not None: m['pbrMetallicRoughness']['baseColorFactor'] = [float(min(1, v)) for v in c['tint']] + [1.0]
        if c['klass'] == 'world_glass':
            m['alphaMode'] = 'BLEND'; f = m['pbrMetallicRoughness'].setdefault('baseColorFactor', [1.0, 1.0, 1.0, 1.0])
            f[3] = float(max(0.05, min(1.0, c['opacity'] or 0.4))); stats['glass'] += 1
        if c['two_sided']: m['doubleSided'] = True; stats['two sided'] += 1
        if c.get('scroll'): m['extras']['uv_scroll'] = c['scroll']; stats['scrolling (static)'] += 1
        # Surface: MIX is R metallic, G roughness, B AO; a merged atlas keeps roughness in its normal
        # array's R and metallic in its A. Written as glTF metallicRoughness (G rough, B metal) and occlusion.
        surf = None
        if mix:
            a = tex.rgba(mix)
            if a is not None: surf = (a[..., 1], a[..., 0], a[..., 2])
        elif nrm and short(nrm).startswith('T2A_'):
            a = tex.rgba(nrm, sl)
            if a is not None: surf = (a[..., 0], a[..., 3], None)
        if surf is not None:
            rough, metal, ao = surf
            mr = np.stack([np.zeros_like(rough), rough, metal], -1)
            m['pbrMetallicRoughness'].update(metallicFactor=1.0, roughnessFactor=1.0, metallicRoughnessTexture=dict(index=add_image(png(mr), name + '_mr')))
            stats['metal/rough'] += 1
            if ao is not None:
                m['occlusionTexture'] = dict(index=add_image(png(np.stack([ao, ao, ao], -1)), name + '_ao')); stats['ao'] += 1
        npng = tex.normal(nrm, sl, atlas=short(nrm).startswith('T2A_')) if nrm else None
        if npng:
            m['normalTexture'] = dict(index=add_image(npng, name + '_n')); stats['materials with normal map'] += 1
        gmats.append(m); mat_index[key] = len(gmats) - 1
    # one node per placed instance, one primitive per material
    by_inst = collections.OrderedDict()
    for k, v in groups.items():
        if k[0] == 'verts': continue
        by_inst.setdefault(k[0], []).append((k[1], v))
    vert_sets = {k[1]: [] for k in groups if k[0] == 'verts'}
    for k, v in groups.items():
        if k[0] == 'verts': vert_sets[k[1]].append(v)
    # an instance id with several matrices has several vertex sets; split triangles per set in order of creation
    for inst, prims in by_inst.items():
        sets = vert_sets[inst]
        for si, (W, NN, UV) in enumerate(sets):
            prs = []
            for mkey, tris in prims:
                t = np.array(tris, dtype=np.uint32)
                per = len(t) // len(sets); t = t[si * per:(si + 1) * per]
                if not len(t): continue
                # each primitive carries only the vertices it uses
                used, remap = np.unique(t.reshape(-1), return_inverse=True)
                ip = acc(np.ascontiguousarray(W[used]), 34962, 5126, 'VEC3', True); inn = acc(np.ascontiguousarray(NN[used]), 34962, 5126, 'VEC3')
                iuv = acc(np.ascontiguousarray(UV[used]), 34962, 5126, 'VEC2')
                prs.append(dict(attributes=dict(POSITION=ip, NORMAL=inn, TEXCOORD_0=iuv), indices=acc(remap.astype(np.uint32), 34963, 5125, 'SCALAR'), material=mat_index[mkey]))
            if prs:
                meshes.append(dict(primitives=prs)); nodes.append(dict(mesh=len(meshes) - 1, name=f'inst{inst}_{si}'))
    bin_ = b''.join(blobs)
    g = dict(asset=dict(version='2.0', generator='r5ed apexm buildworld'), scene=0, scenes=[dict(nodes=list(range(len(nodes))))],
             nodes=nodes, meshes=meshes, materials=gmats, images=images, textures=textures, samplers=[dict(wrapS=10497, wrapT=10497)],
             accessors=accs, bufferViews=views, buffers=[dict(byteLength=len(bin_))])
    for t in textures: t['sampler'] = 0
    js = json.dumps(g).encode(); js += b' ' * ((-len(js)) % 4)
    open(out_glb, 'wb').write(struct.pack('<III', 0x46546C67, 2, 12 + 8 + len(js) + 8 + len(bin_)) + struct.pack('<II', len(js), 0x4E4F534A) + js + struct.pack('<II', len(bin_), 0x004E4942) + bin_)
    stats['nodes'] = len(nodes); stats['materials'] = len(gmats); stats['images'] = len(images)
    json.dump(dict(stats=stats, texture_failures=tex.fail), open(out_json, 'w'), indent=1)
    print(dict(stats)); print(dict(tex.fail))


