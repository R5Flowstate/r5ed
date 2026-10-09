"""Lights and reflection captures of an Apex Mobile level (all its sublevels), in Unreal space (cm).

    py scripts/apexm/ulight.py <level dir> <level prefix>      prints a summary

Every light component is resolved through its AttachParent chain the way uplace.py resolves meshes.
Unreal lights shine along their local +X; cone angles are half angles in degrees.
"""
import glob, os, sys, numpy as np
from upkg import Package, R
from uprops import read_props
from uplace import trs

LOCAL = ('PointLightComponent', 'SpotLightComponent', 'RectLightComponent')
CAPTURES = ('BoxReflectionCaptureComponent', 'SphereReflectionCaptureComponent', 'PlaneReflectionCaptureComponent')

def fcolor(t, default=(1.0, 1.0, 1.0)):
    """An FColor property (BGRA bytes, sRGB) as an RGB 0..1 tuple."""
    if not t: return default
    v = t.get('value')
    raw = v[1] if isinstance(v, tuple) and len(v) == 2 and isinstance(v[1], (bytes, bytearray)) else t.get('raw')
    if not isinstance(raw, (bytes, bytearray)) or len(raw) != 4: return default
    return (raw[2] / 255, raw[1] / 255, raw[0] / 255)

def level_lights(path):
    p = Package(path); exp = p.exports; props = {}
    for i, e in enumerate(exp):
        if not e['class'].endswith('Component'): continue
        try: props[i] = {t['name']: t for t in read_props(p, R(p.export_bytes(e)))}
        except Exception: continue
    def val(pr, k, d=None):
        t = pr.get(k); return d if t is None else t['value']
    def world(i, depth=0):
        pr = props[i]
        M = trs(val(pr, 'RelativeLocation', (0, 0, 0)), val(pr, 'RelativeRotation', (0, 0, 0)), val(pr, 'RelativeScale3D', (1, 1, 1)))
        par = pr.get('AttachParent')
        if par is not None and depth < 32:
            raw = R(par['raw']).i32()
            if raw > 0 and raw - 1 in props: M = M @ world(raw - 1, depth + 1)
        return M
    level = os.path.basename(path); out = []
    for i, e in enumerate(exp):
        if i not in props: continue
        c = e['class']; pr = props[i]
        if c in LOCAL or c in ('DirectionalLightComponent', 'SkyLightComponent'):
            if val(pr, 'bVisible', True) is False or val(pr, 'bAffectsWorld', True) is False: continue
            M = world(i); X = M[0, :3] / (np.linalg.norm(M[0, :3]) or 1)
            out.append(dict(level=level, cls=c.replace('Component', ''), pos=tuple(M[3, :3]), forward=tuple(X),
                            intensity=float(val(pr, 'Intensity', 5000.0 if c in LOCAL else 3.14159)),
                            color=fcolor(pr.get('LightColor')), units=val(pr, 'IntensityUnits', 'ELightUnits::Unitless'),
                            inverse_squared=bool(val(pr, 'bUseInverseSquaredFalloff', True)),
                            exponent=float(val(pr, 'LightFalloffExponent', 8.0)),
                            radius=float(val(pr, 'AttenuationRadius', 1000.0)),
                            inner=float(val(pr, 'InnerConeAngle', 0.0)), outer=float(val(pr, 'OuterConeAngle', 44.0)),
                            width=float(val(pr, 'SourceWidth', 64.0)), height=float(val(pr, 'SourceHeight', 64.0)),
                            shadows=bool(val(pr, 'CastShadows', True)), temperature=val(pr, 'Temperature'),
                            use_temperature=bool(val(pr, 'bUseTemperature', False)),
                            sh_color=fcolor(pr.get('TempSkySHColor'), None), average=val(pr, 'CookedAverageBrightness')))
        elif c in CAPTURES:
            M = world(i)
            out.append(dict(level=level, cls=c.replace('Component', ''), pos=tuple(M[3, :3]),
                            extent=float(val(pr, 'InfluenceRadius', val(pr, 'BoxTransitionDistance', 0.0)) or 0.0),
                            scale=tuple(np.linalg.norm(M[:3, :3], axis=1))))
    return out

def all_lights(level_dir, prefix):
    res = []
    for f in sorted(glob.glob(os.path.join(level_dir, prefix + '*.umap'))):
        if not f.endswith('_BuiltData.umap'): res += level_lights(f)
    return res

if __name__ == '__main__':
    import collections
    L = all_lights(sys.argv[1], sys.argv[2])
    print(collections.Counter((l['level'], l['cls']) for l in L))
    for l in L:
        if l['cls'] in ('DirectionalLight', 'SkyLight') or 'Capture' in l['cls']: print(l)
    print(collections.Counter((l['cls'], l['units'], l['inverse_squared']) for l in L if 'Capture' not in l['cls']))
