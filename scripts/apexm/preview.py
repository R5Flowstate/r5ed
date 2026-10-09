"""Renders a ported level the way the bake will light it, before spending a bake on it.

    python3 scripts/apexm/preview.py <port_level.py out dir> <png prefix> [--light-scale 0.4]
        [--samples 16] [--size 1280x720] [--shots spawns,aerial] [--cam name:x,y,z:x,y,z ...]

Needs Blender's Python module (`import bpy`, Blender 4.x; Cycles on the CPU). Loads <out>/world.glb
and <out>/project/map.json and lights it with the project's own lights: the sun, the bake's sky
ambient as the world colour, and every point/spot light with the baker's falloff,
(brightness / 255) (1 - (d / 2 range)^4)^2 / (1 + d / range)^2, zero at twice the range (a light-path node per light;
Blender's own inverse square never reaches zero, so 1,400 lights would wash the scene out).
Emissive surfaces glow by their own .glb values and are not scaled by --light-scale.

Shots: `spawns` looks out from one spawn of each team; `aerial` and `plaza` frame the
middle of the world from outside and inside; --cam adds any (Unreal-free) r5 position in
inches looking at a point. Files come out as <prefix>_<shot>.png.

It is a preview, not the baker: no lightmap resolution, Cycles' own bounce, AgX tone mapping.
It answers "are the UVs right", "does the neon read", "is the sun where it should be".
"""
import argparse, json, math, os, sys

IN = 2.54  # the .glb is in centimetres; map.json in inches


def args():
    argv = sys.argv[sys.argv.index('--') + 1:] if '--' in sys.argv else sys.argv[1:]
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('out'); ap.add_argument('prefix')
    ap.add_argument('--light-scale', type=float, default=1.0)
    ap.add_argument('--samples', type=int, default=16)
    ap.add_argument('--size', default='1280x720')
    ap.add_argument('--shots', default='spawns,aerial,plaza')
    ap.add_argument('--cam', action='append', default=[], help='name:x,y,z:x,y,z in r5 inches (eye:target)')
    return ap.parse_args(argv)


def main():
    import bpy, mathutils
    a = args()
    sc = bpy.context.scene
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    bpy.ops.import_scene.gltf(filepath=os.path.join(a.out, 'world.glb'))
    for o in sc.objects:
        # The importer turns glTF's Y-up into Blender's Z-up; the port wrote Z-up already.
        if o.parent is None: o.rotation_mode = 'XYZ'; o.rotation_euler = (-math.pi / 2, 0, 0)
    sc.render.engine = 'CYCLES'; sc.cycles.device = 'CPU'; sc.cycles.samples = a.samples; sc.cycles.use_denoising = True
    w, h = map(int, a.size.split('x')); sc.render.resolution_x, sc.render.resolution_y = w, h
    sc.view_settings.view_transform = 'AgX'
    d = json.load(open(os.path.join(a.out, 'project', 'map.json')))
    F = a.light_scale; B = d['settings']['bake']
    world = bpy.data.worlds.new('sky'); sc.world = world; world.use_nodes = True
    bg = world.node_tree.nodes['Background']
    bg.inputs[0].default_value = (*B['sky_color'], 1); bg.inputs[1].default_value = B['sky_intensity'] * F

    def math_node(nt, op, x, y):
        n = nt.nodes.new('ShaderNodeMath'); n.operation = op
        for i, v in enumerate((x, y)):
            if isinstance(v, (int, float)): n.inputs[i].default_value = v
            else: nt.links.new(v, n.inputs[i])
        return n.outputs[0]

    lights = 0
    for o in d['objects']:
        k = o['kind']
        if k['type'] != 'light' or k['kind'] not in ('sun', 'point', 'spot'): continue
        q = o['transform']['rotation']
        if k['kind'] == 'sun':
            L = bpy.data.lights.new('sun', 'SUN'); L.energy = k['intensity'] * F; L.color = k['color']; L.angle = math.radians(1)
        else:
            L = bpy.data.lights.new('l', 'SPOT' if k['kind'] == 'spot' else 'POINT')
            r = k['range'] * IN
            L.energy = 1.0; L.use_nodes = True; nt = L.node_tree; em = nt.nodes['Emission']
            dist = nt.nodes.new('ShaderNodeLightPath').outputs['Ray Length']
            m = lambda op, x, y: math_node(nt, op, x, y)
            # The baker's falloff sees at least half the light's size (a panel, not a point).
            soft = 0.5 * max(k.get('size', [0, 0])) * IN
            ds = m('POWER', m('ADD', m('MULTIPLY', dist, dist), soft * soft), 0.5)
            s = m('MULTIPLY', m('ADD', m('DIVIDE', ds, r), 1.0), m('ADD', m('DIVIDE', ds, r), 1.0))
            # Cycles divides a point light's power by 4 pi d^2; multiply it back out.
            # The baker's window (1 - x^4)^2, x = distance / (2 * range), fades it to zero.
            x2 = m('POWER', m('DIVIDE', ds, 2 * r), 2.0); w = m('SUBTRACT', 1.0, m('MULTIPLY', x2, x2))
            v = m('MULTIPLY', m('DIVIDE', m('MULTIPLY', dist, dist), s), 4 * math.pi * k['intensity'] * F / 255)
            v = m('MULTIPLY', v, m('MULTIPLY', w, w))
            nt.links.new(m('MULTIPLY', v, m('LESS_THAN', ds, 2 * r)), em.inputs['Strength'])
            L.color = [c ** 2.2 for c in k['color']]  # local light colours are gamma, as the baker reads them
            L.shadow_soft_size = 20; L.use_shadow = k['cast_shadows']
            if k['kind'] == 'spot':
                L.spot_size = min(math.radians(2 * k['outer_angle']), math.pi)
                L.spot_blend = max(0.0, min(1.0, 1 - k['inner_angle'] / max(k['outer_angle'], 1e-3)))
        ob = bpy.data.objects.new('light', L); sc.collection.objects.link(ob)
        ob.location = [p * IN for p in o['transform']['position']]
        # r5ed lights shine along their local -Z, as Blender's do.
        ob.rotation_mode = 'QUATERNION'; ob.rotation_quaternion = mathutils.Quaternion((q[3], q[0], q[1], q[2]))
        lights += 1
    print(f'{lights} lights at scale {F}')

    def shot(name, eye, look, lens=18):
        cam = bpy.data.cameras.new(name); cam.lens = lens; cam.clip_start = 10; cam.clip_end = 400000
        co = bpy.data.objects.new(name, cam); sc.collection.objects.link(co); co.location = eye
        co.rotation_euler = (mathutils.Vector(look) - mathutils.Vector(eye)).to_track_quat('-Z', 'Y').to_euler()
        sc.camera = co; sc.render.filepath = f'{a.prefix}_{name}.png'
        bpy.ops.render.render(write_still=True); print('wrote', sc.render.filepath)

    wanted = a.shots.split(',')
    if 'spawns' in wanted:
        for team in ('2', '3'):
            sp = next((o for o in d['objects'] if o['kind']['type'] == 'entity'
                       and o['kind'].get('classname') == 'info_spawnpoint_human'
                       and o['kind']['keyvalues'].get('teamnumber') == team), None)
            if not sp: continue
            x, y, z = [p * IN for p in sp['transform']['position']]
            qx, qy, qz, qw = sp['transform']['rotation']
            yaw = math.atan2(2 * (qw * qz + qx * qy), 1 - 2 * (qy * qy + qz * qz))
            shot(f'spawn_team{team}', (x, y, z + 160), (x + math.cos(yaw) * 1000, y + math.sin(yaw) * 1000, z + 120), lens=16)
    if 'aerial' in wanted or 'plaza' in wanted:
        pts = [ob.matrix_world @ mathutils.Vector(c) for ob in sc.objects if ob.type == 'MESH' for c in ob.bound_box]
        lo = mathutils.Vector([min(p[i] for p in pts) for i in range(3)])
        hi = mathutils.Vector([max(p[i] for p in pts) for i in range(3)])
        mid, size = (lo + hi) / 2, (hi - lo)
        if 'aerial' in wanted:
            shot('aerial', (mid.x + 0.3 * size.x, mid.y - 0.4 * size.y, hi.z * 0.5 + 0.2 * size.z), (mid.x, mid.y, mid.z), lens=22)
        if 'plaza' in wanted:
            shot('plaza', (mid.x, mid.y - 0.06 * size.y, 900), (mid.x, mid.y + 0.2 * size.y, 300), lens=16)
    for c in a.cam:
        name, eye, look = c.split(':')
        shot(name, [float(v) * IN for v in eye.split(',')], [float(v) * IN for v in look.split(',')])
    print('PREVIEW_OK')


if __name__ == '__main__':
    main()
