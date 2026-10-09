"""Material instances: texture, vector and scalar parameters and base-property overrides, following Parent chains."""
import glob, os, struct
from upkg import Package, R
from uprops import read_props, read_tag
ROOT = os.environ.get('APEXM_BACKUP', '.')
_files = None
def package_file(pkg_path):
    """'/Game/Art/.../Name' -> a .uasset in the backup (matched by file name)."""
    global _files
    if _files is None:
        _files = {}
        for sub in ('cyberpark_uefiles', 'Environment', 'new', 'AtlasMergeMetas'):
            for f in glob.glob(f'{ROOT}/{sub}/**/*.uasset', recursive=True):
                _files.setdefault(os.path.basename(f)[:-7].lower(), f)
    return _files.get(pkg_path.rsplit('/', 1)[-1].lower())

def import_path(p, idx):
    """Package path of an import (its outermost outer)."""
    imp = p.imports[-idx - 1]
    while imp['outer'] < 0: imp = p.imports[-imp['outer'] - 1]
    return imp['name']

def obj_ref(p, raw):
    i = R(raw).i32()
    if i < 0: return p.imports[-i - 1]['name'], import_path(p, i)
    return (p.exports[i - 1]['name'], None) if i > 0 else (None, None)

def _param_name(p, el):
    return [y['value'] for y in read_props(p, R(el['ParameterInfo']['raw'])) if y['name'] == 'Name'][0]

def material_info(pkg_path, depth=0):
    """Everything a port uses from a material instance, child values over its parents':
    textures (name -> package path), vectors (name -> rgba), scalars, the base-property
    overrides (BlendMode, ShadingModel, TwoSided) and the chain of parent names."""
    info = dict(textures={}, vectors={}, scalars={}, overrides={}, parents=[])
    f = package_file(pkg_path) if pkg_path else None
    if not f or depth > 6: return info
    p = Package(f); e = p.exports[0]
    pr = {t['name']: t for t in read_props(p, R(p.export_bytes(e)))}
    parent = pr.get('Parent')
    if parent is not None:
        nm, path = obj_ref(p, parent['raw'])
        info['parents'].append(nm)
        if path:
            up = material_info(path, depth + 1)
            for k in ('textures', 'vectors', 'scalars', 'overrides'): info[k].update(up[k])
            info['parents'] += up['parents']
    info['textures'].update(material_params_local(p, pr))
    for key, kind in (('VectorParameterValues', 'vectors'), ('ScalarParameterValues', 'scalars')):
        t = pr.get(key)
        if t is None: continue
        r = R(t['raw']); n = r.i32(); read_tag(p, r)
        for _ in range(n):
            el = {x['name']: x for x in read_props(p, r)}
            v = el.get('ParameterValue')
            if v is None: continue
            info[kind][_param_name(p, el)] = list(struct.unpack('<4f', v['raw'])) if kind == 'vectors' else v['value']
    if 'BasePropertyOverrides' in pr:
        for x in read_props(p, R(pr['BasePropertyOverrides']['raw'])): info['overrides'][x['name']] = x['value']
    return info

def material_params_local(p, pr):
    out = {}
    t = pr.get('TextureParameterValues')
    if t is not None:
        r = R(t['raw']); n = r.i32(); read_tag(p, r)
        for _ in range(n):
            el = {x['name']: x for x in read_props(p, r)}
            tex = el.get('ParameterValue')
            if tex is not None:
                tn, tp = obj_ref(p, tex['raw'])
                if tp: out[_param_name(p, el)] = tp
    return out

def material_params(pkg_path, depth=0):
    """Texture parameters only (kept for callers that need just those)."""
    info = material_info(pkg_path, depth)
    return info['textures'], package_file(pkg_path) if pkg_path else None

def slot_paths(p, mesh):
    """Mesh material slots -> (object name, package path)."""
    t = mesh['props'].get('StaticMaterials')
    if t is None: return []
    r = R(t['raw']); n = r.i32(); read_tag(p, r); out = []
    for _ in range(n):
        el = {x['name']: x for x in read_props(p, r)}
        mi = el.get('MaterialInterface')
        out.append(obj_ref(p, mi['raw']) if mi else (None, None))
    return out
