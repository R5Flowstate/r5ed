"""An Apex Legends Mobile level (cooked .umap sublevels + the asset backup) -> an r5ed project.

    py scripts/apexm/port_level.py <backup root> <level dir> <level prefix> <out dir> [--stem mp_rr_x]
    e.g. port_level.py backup backup/cyberpark_uefiles/scripts TDM_MP_CyberPark_ out/cyberpark

Writes into <out dir>:
  world.glb          every visible static mesh placement, baked to world space, one node per instance,
                     one primitive per material, base-colour textures embedded (atlas slices split out)
  airwall.glb        the air-wall shell, imported as invisible move-clip
  import_report.json what import-glb mapped (objmat.py --only takes it)
  project/           the r5ed project: meshes, 28 team spawns from ApexPlayerStart, lighting, cubemaps

Lighting (--lighting):
  night  the level's own: its directional light (a cool moon), sky light, every point/spot/rect light
         of the light sublevels (rect lights as wide spots), and its reflection captures as the cubemaps
  day    a clear-day preset (warm sun 50 deg up, blue sky, no local lights), same cubemaps

Units: Unreal centimetres, left-handed -> r5 inches, right-handed (Y negated, yaw negated).
Meshes wholly outside the air wall get a 128-unit lightmap texel so the charts fit 32 pages.
"""
import argparse, collections, json, math, os, pickle, shutil, struct, subprocess, sys, uuid
import numpy as np
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)

CM = 1 / 2.54
NODRAW = 'tools/toolsnodraw'
MOVE_CLIP = 0x00A30000

def compiler():
    exe = os.environ.get('R5ED_COMPILE') or os.path.join(HERE, '..', '..', 'target', 'release', 'r5ed-compile')
    if not os.path.exists(exe) and os.path.exists(exe + '.exe'): exe += '.exe'
    return exe

def run(*a):
    r = subprocess.run([compiler(), *a], capture_output=True, text=True)
    if r.returncode: sys.exit(f'r5ed-compile {a[0]} failed: {r.stderr[-800:]}')
    return r.stdout

def player_starts(level_dir, prefix):
    from upkg import Package, R
    from uprops import read_props
    path = os.path.join(level_dir, prefix + 'FixedPoint.umap')
    if not os.path.exists(path): return []
    p = Package(path)
    props = {i: {t['name']: t for t in read_props(p, R(p.export_bytes(e)))} for i, e in enumerate(p.exports)
             if e['class'] in ('ApexPlayerStart', 'CapsuleComponent', 'SceneComponent')}
    out = []
    for i, e in enumerate(p.exports):
        if e['class'] != 'ApexPlayerStart': continue
        pr = props[i]; root = pr.get('RootComponent')
        cp = props.get(R(root['raw']).i32() - 1, {}) if root else {}
        out.append(dict(loc=cp.get('RelativeLocation', {}).get('value', (0, 0, 0)), rot=cp.get('RelativeRotation', {}).get('value', (0, 0, 0)),
                        camp=pr.get('PlayerStartCamp', {}).get('value')))
    return out

def glb_bounds(path):
    b = open(path, 'rb').read(); n = struct.unpack_from('<I', b, 12)[0]; g = json.loads(b[20:20 + n])
    bl = struct.unpack_from('<I', b, 20 + n)[0]; bin_ = b[20 + n + 8:20 + n + 8 + bl]
    lo = np.full(3, np.inf); hi = np.full(3, -np.inf)
    for m in g['meshes']:
        for pr in m['primitives']:
            a = g['accessors'][pr['attributes']['POSITION']]; v = g['bufferViews'][a['bufferView']]
            P = np.frombuffer(bin_, '<f4', a['count'] * 3, v.get('byteOffset', 0) + a.get('byteOffset', 0)).reshape(-1, 3)
            if len(P): lo = np.minimum(lo, P.min(0)); hi = np.maximum(hi, P.max(0))
    return lo, hi

def object_bounds(proj, o):
    lo, hi = glb_bounds(os.path.join(proj, o['kind']['source']))
    s = np.array(o['transform']['scale']); t = np.array(o['transform']['position'])
    return lo * s + t, hi * s + t

def build_project(out, stem, display):
    P = os.path.join(out, 'project')
    if os.path.exists(P): shutil.rmtree(P)
    run('new', P, '--stem', stem)
    opts = os.path.join(out, 'import_options.json')
    json.dump({'scale': CM, 'up': 'z', 'material_prefix': f'world/obj/{stem}'}, open(opts, 'w'))
    rep = json.loads(run('import-glb', P, os.path.join(out, 'world.glb'), '--options', opts))
    json.dump(rep, open(os.path.join(out, 'import_report.json'), 'w'), indent=1)
    before = {o['id'] for o in json.load(open(f'{P}/map.json'))['objects']}
    if os.path.exists(os.path.join(out, 'airwall.glb')):
        run('import-glb', P, os.path.join(out, 'airwall.glb'), '--options', opts)
    d = json.load(open(f'{P}/map.json'))
    for o in d['objects']:
        if o['id'] not in before and o['kind']['type'] == 'mesh':
            k = o['kind']; k.update(render=False, cast_shadows=False, contents=MOVE_CLIP)
            k['material_map'] = {m: NODRAW for m in k['material_map']}; o['name'] = 'airwall_clip'
    json.dump(d, open(f'{P}/map.json', 'w'), indent=1)
    return P, rep

def add_gameplay(P, starts):
    common = ['--kv', 'model=mdl/dev/mp_spawn.rmdl', '--kv', 'scale=1', '--kv', 'gamemode_tdm=1', '--kv', 'gamemode_ffa=1',
              '--kv', 'gamemode_ctf=1', '--kv', 'control_teamnumber=-1', '--kv', 'alliance=-1']
    first = None
    for s in starts:
        x, y, z = s['loc']; yaw = -s['rot'][1]
        org = f'{x*CM:.2f},{-y*CM:.2f},{(z - 90)*CM:.2f}'  # the spawn capsule's centre sits ~90 cm above the floor
        team = 2 if s['camp'] == 1 else 3
        run('entity', P, '--class', 'info_spawnpoint_human', '--origin', org, '--angles', f'0,{yaw:.2f},0', '--kv', f'teamnumber={team}', *common)
        first = first or (org, yaw)
    if first: run('entity', P, '--class', 'info_player_start', '--origin', first[0], '--angles', f'0,{first[1]:.2f},0')

# The night sky: a stock night vista (the compiler gives a map without one eden_se, a day sky).
NIGHT_SKY = 'mdl/vistas/party_crashers_night_se.rmdl'

# The clear-day preset: kept as first shipped, the look to fall back on.
DAY = dict(sun_elevation=50.0, sun_azimuth=35.0, sun_color=[1.0, 0.95, 0.86], sun_intensity=2.5,
           sky_color=[0.55, 0.68, 1.0], sky_intensity=0.7)

# The night environment. Without these the compiler adds canyonlands_staging_mu1's, a clear day: sky-blue
# fog (127 208 255), sun-warm directional fog and the desertlands grade, which make any night read as day.
# Every value is one a shipped map uses (schema/entities.json observed values): the night grade, a night
# map's deep-blue fog, a violet glow toward the moon and faint cool volumetrics.
NIGHT_ENV = {
    'color_correction': {'filename': 'materials/correction/mp_canyonlands_night_hdr.raw_hdr', 'spawnflags': '0',
                         'minfalloff': '-1', 'maxfalloff': '-1', 'maxweight': '1', 'fadeInDuration': '0',
                         'fadeOutDuration': '0'},
    'env_fog_controller': {'fogenable': '1', 'fogcolor': '11 35 53', 'fogcolorstrength': '.5',
                           'fogdircolor': '129 103 214', 'fogdircolorstrength': '.25', 'fogdirhalfangle': '80',
                           'fogdensity': '0.3', 'fogdistoffset': '1500', 'foghalfdisttop': '6000',
                           'foghalfdistbottom': '10000', 'fogztop': '65000', 'fogzbottom': '-800',
                           'fogangles': '0 90 0', 'use_angles': '1', 'forceontosky': '0', 'minfadetime': '3',
                           'spawnflags': '0', 'disableWhenVfogEnabled': '0', 'vl_color': '0.5 0.75 1',
                           'vl_intensity': '0.05', 'vl_scatter': '0.8', 'vl_in_shadow_scatter': '0.0',
                           'vl_distFalloff': '1500'},
}

def add_environment(P, env):
    for cls, kv in env.items():
        run('entity', P, '--class', cls, '--origin', '0,0,0', *[f'--kv={k}={v}' for k, v in kv.items()],
            '--kv', f'targetname={cls}_night')

def facing(forward):
    """The rotation [x, y, z, w] that turns a light's -Z (where r5ed lights shine) onto `forward`."""
    a = np.array([0, 0, -1.0]); b = np.asarray(forward, float); b = b / np.linalg.norm(b)
    if a @ b < -0.999999: return [1.0, 0.0, 0.0, 0.0]
    q = np.append(np.cross(a, b), 1 + a @ b); return [float(v) for v in q / np.linalg.norm(q)]

def r5(v): return [v[0] * CM, -v[1] * CM, v[2] * CM]
def r5dir(v): return [v[0], -v[1], v[2]]
def linear(c): return [float(x / 12.92 if x <= 0.04045 else ((x + 0.055) / 1.055) ** 2.4) for x in c]

def candelas(l):
    """A local light's luminous intensity. Unreal's legacy unitless is 1/625 cd; lumens spread over the
    sphere, the cone (half angle) or, for a rect light, the cosine-weighted hemisphere."""
    I, u = l['intensity'], l['units']
    if u.endswith('Candelas'): return I
    if u.endswith('Lumens'):
        if l['cls'] == 'SpotLight': return I / (2 * math.pi * (1 - math.cos(math.radians(l['outer']))))
        return I / (math.pi if l['cls'] == 'RectLight' else 4 * math.pi)
    return I / 625

def brightness(l, scale=1.0):
    """The Source `_light` brightness that matches the Unreal light's illuminance halfway to its
    attenuation radius R. The baker reads brightness / 255 at the light, falling off as
    1 / (1 + d / range)^2 under Unreal's window (1 - (d / 2 range)^4)^2, reaching zero smoothly at
    2 * range; with range = R / 2 (local_light) the light ends where Unreal's does, and at
    d = R / 2 = range it gives 0.22 of its own value (the window's 0.88 is left uncompensated). Unreal
    illuminance and the sun's irradiance share a scale (the level's exposure sits at bias 0)."""
    if l['inverse_squared']: E = candelas(l) / max((l['radius'] / 200) ** 2, 0.25)
    else: E = l['intensity'] * 0.75 ** l['exponent']  # legacy exponent falloff: (1 - (d/R)^2)^exp at R/2
    return min(255 * 4 * E * scale, 4000.0)

def local_light(l, layer, scale):
    b = brightness(l, scale)
    if b < 4: return None
    spot = l['cls'] in ('SpotLight', 'RectLight')
    # A rect light lights its front hemisphere: a wide spot along its normal.
    inner, outer = (40.0, 85.0) if l['cls'] == 'RectLight' else (min(l['inner'], l['outer']), l['outer'])
    return {'id': str(uuid.uuid4()), 'name': 'ue_' + l['cls'].lower(), 'layer': layer, 'parent': None,
            'transform': {'position': r5(l['pos']), 'rotation': facing(r5dir(l['forward'])) if spot else [0.0, 0.0, 0.0, 1.0],
                          'scale': [1.0, 1.0, 1.0]}, 'hidden': False, 'locked': False,
            'kind': {'type': 'light', 'kind': 'spot' if spot else 'point', 'color': [float(c) for c in l['color']],
                     'intensity': round(b, 2), 'range': round(l['radius'] / 2 * CM, 1), 'inner_angle': float(inner),
                     'outer_angle': float(max(outer, inner)), 'size': [l['width'] * CM, l['height'] * CM],
                     'cast_shadows': l['shadows'], 'baked': True, 'realtime': False, 'keyvalues': {}}}

def lighting(lights, preset):
    """The sun, the sky and the local lights of a preset, plus the cubemap points (the level's own
    reflection captures under either preset)."""
    sun = next((l for l in lights if l['cls'] == 'DirectionalLight'), None)
    sky = next((l for l in lights if l['cls'] == 'SkyLight'), None)
    cubemaps = [r5(l['pos']) for l in lights if 'ReflectionCapture' in l['cls']]
    if preset == 'day' or not sun:
        el, az = math.radians(DAY['sun_elevation']), math.radians(DAY['sun_azimuth'])
        to_sun = [math.cos(el) * math.cos(az), math.cos(el) * math.sin(az), math.sin(el)]
        return dict(sun_dir=[-v for v in to_sun], sun_color=DAY['sun_color'], sun_intensity=DAY['sun_intensity'],
                    sky_color=DAY['sky_color'], sky_intensity=DAY['sky_intensity'], local=[], cubemaps=cubemaps)
    # The sky light's colour times its cooked cubemap's average brightness.
    sky_color = linear(sky['color']) if sky else DAY['sky_color']
    sky_int = sky['intensity'] * (sky['average'] or 1.0) if sky else 0.3
    return dict(sun_dir=r5dir(sun['forward']), sun_color=linear(sun['color']), sun_intensity=sun['intensity'],
                sky_color=sky_color, sky_intensity=round(sky_int, 3),
                local=[l for l in lights if l['cls'] in ('PointLight', 'SpotLight', 'RectLight')], cubemaps=cubemaps)

def add_lighting(P, L, local_scale=1.0):
    run('light', P, '--kind', 'sun', '--origin', '0,0,8000', '--color', ','.join(f'{c:.4f}' for c in L['sun_color']),
        '--intensity', str(L['sun_intensity']))
    # No `sky` light: that kind compiles to a sky_camera, and the compiler seats its own at the vista.
    # The sky's ambient is the bake settings' sky colour below.
    d = json.load(open(f'{P}/map.json'))
    layer = d['objects'][0]['layer']
    for o in d['objects']:
        if o['kind']['type'] == 'light' and o['kind']['kind'] == 'sun': o['transform']['rotation'] = facing(L['sun_dir'])
    added = [o for o in (local_light(l, layer, local_scale) for l in L['local']) if o]
    d['objects'] += added
    d['settings']['bake'].update(sky_color=[float(c) for c in L['sky_color']], sky_intensity=float(L['sky_intensity']),
                                 cubemap_points=[[float(c) for c in p] for p in L['cubemaps']])
    json.dump(d, open(f'{P}/map.json', 'w'), indent=1)
    return len(added)

def finish(P, display, description):
    d = json.load(open(f'{P}/map.json'))
    d['meta'].update(display_name=display, description=description,
                     minimap={'enabled': True, 'origin': [-9500.0, -9500.0], 'size': 19000.0, 'resolution': 2048})
    d['settings']['bake'].update(texel_size=32.0, probe_spacing=512.0)
    clip = [o for o in d['objects'] if o.get('name') == 'airwall_clip']
    far = 0
    if clip:
        blo, bhi = object_bounds(P, clip[0]); pad = 0.05 * (bhi - blo); blo, bhi = blo - pad, bhi + pad
        for o in d['objects']:
            k = o['kind']
            if k['type'] != 'mesh' or not k.get('render', True): continue
            lo, hi = object_bounds(P, o)
            if (hi < blo).any() or (lo > bhi).any() or lo[2] > bhi[2] * 0.6:
                k['lightmap_scale'] = 128.0; far += 1
    json.dump(d, open(f'{P}/map.json', 'w'), indent=1)
    return far

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('backup'); ap.add_argument('level_dir'); ap.add_argument('prefix'); ap.add_argument('out')
    ap.add_argument('--stem', default='mp_rr_cyberpark'); ap.add_argument('--display', default='CyberPark')
    ap.add_argument('--lighting', choices=('night', 'day'), default='night')
    ap.add_argument('--sky', help='sky vista model (default: party_crashers_night_se for night, '
                                  'the compiler\'s eden_se for day)')
    ap.add_argument('--light-scale', type=float, default=1.0,
                    help='multiplies all baked light (sun, sky ambient, local lights), not emissive surfaces: '
                         'below 1 makes the neon stand out')
    ap.add_argument('--local-light-scale', type=float, default=1.0, help='multiplies the ported local lights on top')
    a = ap.parse_args()
    os.environ['APEXM_BACKUP'] = os.path.abspath(a.backup)
    import assemble, umat, world, uplace, ulight
    assemble.ROOT = umat.ROOT = os.path.abspath(a.backup)
    os.makedirs(a.out, exist_ok=True)
    pl = uplace.all_placements(a.level_dir, a.prefix)
    print(f'{len(pl)} components, {sum(len(p["matrices"]) for p in pl)} placed instances')
    world.main(os.path.join(a.out, 'world.glb'), os.path.join(a.out, 'world_report.json'), world.visual, pl)
    world.main(os.path.join(a.out, 'airwall.glb'), os.path.join(a.out, 'airwall_report.json'), world.airwall, pl)
    P, rep = build_project(a.out, a.stem, a.display)
    starts = player_starts(a.level_dir, a.prefix)
    add_gameplay(P, starts)
    L = lighting(ulight.all_lights(a.level_dir, a.prefix), a.lighting)
    L['sun_intensity'] *= a.light_scale; L['sky_intensity'] *= a.light_scale
    sky = a.sky or (NIGHT_SKY if a.lighting == 'night' else None)
    if sky:
        d = json.load(open(f'{P}/map.json')); d['meta']['sky'] = sky; json.dump(d, open(f'{P}/map.json', 'w'), indent=1)
    nl = add_lighting(P, L, a.light_scale * a.local_light_scale)
    if a.lighting == 'night': add_environment(P, NIGHT_ENV)
    far = finish(P, a.display, f'{a.display}, ported from the Apex Legends Mobile backup')
    r = rep['report']
    print(f"project {P}: {r['triangles']} triangles, {len(r['materials'])} materials, {len(starts)} spawns, "
          f"{far} meshes on a coarse lightmap; {a.lighting} lighting, {nl} local lights, {len(L['cubemaps'])} cubemaps; validate: {run('validate', P).strip() or 'ok'}")

if __name__ == '__main__':
    main()
