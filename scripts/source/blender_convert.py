"""DCC models (.fbx/.dae/.stl/.ply/.3ds/.blend) -> .glb through Blender 4.3.

    py scripts/source/blender_convert.py <in> <out.glb> [--up y|z] [--scale S]

Everything but .3ds runs Blender in background mode with a generated script:
import by extension, scale the root objects, export GLB with materials and
embedded images (`export_apply` bakes the transforms). Blender 4.3 ships no
3DS importer, so .3ds is read here (verts 0x4110, faces 0x4120, groups 0x4130)
and written as GLB directly.

--up picks the GLB's up axis: y (the glTF standard) or z. Pass the same value
to `r5ed-compile import-glb --options {"up": ...}` and keep its scale at 1,
since --scale is already baked into the GLB.
"""

import argparse
import json
import math
import os
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np

DEFAULT_BLENDER = "blender"
SUPPORTED = "fbx dae stl ply 3ds blend"

IMPORT_OPS = {
    ".fbx": "bpy.ops.import_scene.fbx(filepath=IN)",
    ".dae": "bpy.ops.wm.collada_import(filepath=IN)",
    ".stl": "bpy.ops.wm.stl_import(filepath=IN)",
    ".ply": "bpy.ops.wm.ply_import(filepath=IN)",
    ".blend": None,
}

MAIN, EDITOR, OBJECT, TRIMESH = 0x4D4D, 0x3D3D, 0x4000, 0x4100
VERTS, FACES, MAT_GROUP, UV = 0x4110, 0x4120, 0x4130, 0x4140
MAX_3DS = 256 << 20
MAX_VERTS = 4_000_000


class ConvertError(Exception):
    pass


def build_script(src, dst, up, scale, mode, op):
    return f"""\
import bpy
import sys

IN = {json.dumps(str(src))}
OUT = {json.dumps(str(dst))}
UP = {json.dumps(up)}
SCALE = {scale!r}
MODE = {json.dumps(mode)}

def fail(msg):
    print("BLENDER_CONVERT_ERROR: " + msg, file=sys.stderr)
    sys.exit(1)

if MODE == "blend":
    try:
        bpy.ops.wm.open_mainfile(filepath=IN)
    except Exception as e:
        fail("open %s: %s" % (IN, e))
else:
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.object.delete(use_global=False)
    try:
        {op}
    except Exception as e:
        fail("import %s: %s" % (IN, e))

if abs(SCALE - 1.0) > 1e-12:
    for o in bpy.data.objects:
        if o.parent is None:
            o.scale[0] *= SCALE
            o.scale[1] *= SCALE
            o.scale[2] *= SCALE

try:
    bpy.ops.export_scene.gltf(
        filepath=OUT,
        export_format='GLB',
        export_yup=(UP == 'y'),
        export_apply=True,
        export_materials='EXPORT',
        export_image_format='AUTO',
        use_selection=False,
        export_cameras=False,
        export_lights=False,
        export_animations=False)
except Exception as e:
    fail("export %s: %s" % (OUT, e))

print("BLENDER_CONVERT_OK " + OUT)
"""


def convert_blender(src, dst, up, scale, blender, timeout):
    script = build_script(src, dst, up, scale,
                          "blend" if src.suffix.lower() == ".blend" else "import",
                          IMPORT_OPS[src.suffix.lower()])
    with tempfile.NamedTemporaryFile("w", suffix=".py", dir=dst.parent, delete=False) as f:
        f.write(script)
        gen = f.name
    try:
        r = subprocess.run([blender, "-b", "--factory-startup", "--python", gen],
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                           text=True, errors="replace", timeout=timeout)
    except subprocess.TimeoutExpired:
        raise ConvertError(f"{src}: blender took longer than {timeout}s")
    finally:
        Path(gen).unlink(missing_ok=True)
    if "BLENDER_CONVERT_OK" not in r.stdout or not dst.is_file():
        tail = "\n".join(r.stdout.splitlines()[-15:])
        raise ConvertError(f"{src}: blender conversion failed\n{tail}")


def _chunks(data, off, end, name):
    while off + 6 <= end:
        cid, ln = struct.unpack_from("<HI", data, off)
        if ln < 6 or off + ln > end:
            raise ConvertError(f"{name}: chunk 0x{cid:04x} runs past the end of the file")
        yield cid, off + 6, off + ln
        off += ln


def _asciiz(data, off, end, name):
    stop = data.find(b"\0", off, end)
    if stop < 0:
        raise ConvertError(f"{name}: object name runs past the end of the file")
    return data[off:stop].decode("ascii", "replace"), stop + 1


def parse_3ds(data, name):
    """(object name, material, positions, normals, uvs, indices) per material group."""
    if len(data) > MAX_3DS:
        raise ConvertError(f"{name}: file too large for a 3ds ({len(data)} bytes)")
    top = [c for c in _chunks(data, 0, len(data), name) if c[0] == MAIN]
    if not top:
        raise ConvertError(f"{name}: not a 3ds (no 0x4D4D chunk)")
    out = []
    for cid, off, end in _chunks(data, top[0][1], top[0][2], name):
        if cid != EDITOR:
            continue
        for ocid, ooff, oend in _chunks(data, off, end, name):
            if ocid != OBJECT:
                continue
            obj, at = _asciiz(data, ooff, oend, name)
            for tcid, toff, tend in _chunks(data, at, oend, name):
                if tcid != TRIMESH:
                    continue
                verts, faces, uvs, groups = None, None, None, []
                for fcid, foff, fend in _chunks(data, toff, tend, name):
                    if fcid == VERTS:
                        (n,) = struct.unpack_from("<H", data, foff)
                        if n > MAX_VERTS or 2 + n * 12 > fend - foff:
                            raise ConvertError(f"{name}: {n} verts run past the end of the file")
                        verts = np.frombuffer(data, dtype="<f4", count=n * 3, offset=foff + 2).reshape(-1, 3).copy()
                    elif fcid == FACES:
                        (n,) = struct.unpack_from("<H", data, foff)
                        if n > MAX_VERTS or 2 + n * 8 > fend - foff:
                            raise ConvertError(f"{name}: {n} faces run past the end of the file")
                        raw = np.frombuffer(data, dtype="<u2", count=n * 4, offset=foff + 2).reshape(-1, 4)
                        if (raw[:, :3] >= (len(verts) if verts is not None else 0)).any():
                            raise ConvertError(f"{name}: a face names a vertex that does not exist")
                        faces = raw[:, :3].copy()
                        for gcid, goff, gend in _chunks(data, foff + 2 + n * 8, fend, name):
                            if gcid != MAT_GROUP:
                                continue
                            gname, members = _asciiz(data, goff, gend, name)
                            (gn,) = struct.unpack_from("<H", data, members)
                            if 2 + gn * 2 > gend - members:
                                raise ConvertError(f"{name}: material group runs past the end of the file")
                            sel = np.frombuffer(data, dtype="<u2", count=gn, offset=members + 2).copy()
                            if (sel >= n).any():
                                raise ConvertError(f"{name}: a material group names a face that does not exist")
                            groups.append((gname, sel))
                    elif fcid == UV:
                        (n,) = struct.unpack_from("<H", data, foff)
                        if n > MAX_VERTS or 2 + n * 8 > fend - foff:
                            raise ConvertError(f"{name}: uvs run past the end of the file")
                        uvs = np.frombuffer(data, dtype="<f4", count=n * 2, offset=foff + 2).reshape(-1, 2).copy()
                if verts is None or faces is None or not len(faces):
                    continue
                if uvs is None or len(uvs) != len(verts):
                    uvs = np.zeros((len(verts), 2), np.float32)
                fn = verts[faces[:, 1]] - verts[faces[:, 0]]
                fn = np.cross(fn, verts[faces[:, 2]] - verts[faces[:, 0]])
                normals = np.zeros_like(verts)
                np.add.at(normals, faces.ravel(), np.repeat(fn, 3, axis=0))
                zero = (normals == 0).all(-1)
                normals[zero] = (0, 0, 1)
                normals /= np.linalg.norm(normals, axis=1, keepdims=True)
                if not groups:
                    groups = [("default", np.arange(len(faces)))]
                for gname, sel in groups:
                    remap = np.full(len(verts), -1, np.int64)
                    order = faces[sel].ravel()
                    ids = np.unique(order, return_index=False)
                    remap[ids] = np.arange(len(ids))
                    out.append((obj or "object", gname or "default",
                                verts[ids], normals[ids], uvs[ids], remap[order].reshape(-1, 3)))
    return out


def write_glb(groups, up, scale):
    views, accessors, meshes, nodes, materials = [], [], [], [], []
    mat_index = {}
    blob = bytearray()
    for obj, mat, pos, nrm, uv, idx in groups:
        pos = np.asarray(pos, np.float32) * scale
        if up == "y":
            pos = pos[:, [0, 2, 1]] * (1, 1, -1)
            nrm = np.asarray(nrm, np.float32)[:, [0, 2, 1]] * (1, 1, -1)
        else:
            nrm = np.asarray(nrm, np.float32)
        aids = []
        # 3DS counts V from the bottom of the image; glTF (and import-glb) from the top.
        uv = np.asarray(uv, np.float32) * (1, -1) + (0, 1)
        for arr, tp in ((pos, "VEC3"), (nrm, "VEC3"), (uv, "VEC2")):
            raw = np.ascontiguousarray(arr, dtype=np.float32).tobytes()
            views.append({"buffer": 0, "byteOffset": len(blob), "byteLength": len(raw)})
            accessors.append({"bufferView": len(views) - 1, "componentType": 5126,
                              "count": len(arr), "type": tp})
            blob += raw
            aids.append(len(accessors) - 1)
        raw = np.ascontiguousarray(idx, dtype=np.uint32).tobytes()
        views.append({"buffer": 0, "byteOffset": len(blob), "byteLength": len(raw)})
        accessors.append({"bufferView": len(views) - 1, "componentType": 5125,
                          "count": int(idx.size), "type": "SCALAR"})
        blob += raw
        aids.append(len(accessors) - 1)
        lo, hi = pos.min(0).tolist(), pos.max(0).tolist()
        accessors[aids[0]]["min"], accessors[aids[0]]["max"] = [lo], [hi]
        if mat not in mat_index:
            mat_index[mat] = len(materials)
            materials.append({"name": mat})
        meshes.append({"name": obj, "primitives": [
            {"attributes": {"POSITION": aids[0], "NORMAL": aids[1], "TEXCOORD_0": aids[2]},
             "indices": aids[3], "material": mat_index[mat]}]})
        nodes.append({"name": obj, "mesh": len(meshes) - 1})
    doc = {"asset": {"version": "2.0", "generator": "r5ed-blender-convert"},
           "scene": 0, "scenes": [{"nodes": list(range(len(nodes)))}],
           "nodes": nodes, "meshes": meshes, "materials": materials,
           "accessors": accessors, "bufferViews": views, "buffers": [{"byteLength": len(blob)}]}
    js = json.dumps(doc, separators=(",", ":")).encode()
    js += b" " * (-len(js) % 4)
    blob += b"\0" * (-len(blob) % 4)
    out = bytearray()
    out += struct.pack("<III", 0x46546C67, 2, 12 + 8 + len(js) + 8 + len(blob))
    out += struct.pack("<II", len(js), 0x4E4F534A) + js
    out += struct.pack("<II", len(blob), 0x004E4942) + blob
    return bytes(out)


def convert_3ds(data, name, up, scale):
    groups = parse_3ds(data, name)
    if not groups:
        raise ConvertError(f"{name}: the file holds no triangle mesh")
    return write_glb(groups, up, scale)


def convert(src, dst, up="y", scale=1.0, blender=None, timeout=600.0):
    src, dst = Path(src), Path(dst)
    if up not in ("y", "z"):
        raise ConvertError(f"{src}: up must be y or z, not {up!r}")
    if not isinstance(scale, (int, float)) or not math.isfinite(scale) or scale <= 0:
        raise ConvertError(f"{src}: scale must be positive, not {scale!r}")
    if not src.is_file():
        raise ConvertError(f"{src}: no such input file")
    suf = src.suffix.lower()
    if suf == ".3ds":
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_bytes(convert_3ds(src.read_bytes(), str(src), up, float(scale)))
        return dst
    if suf not in IMPORT_OPS:
        raise ConvertError(f"{src}: {suf or 'no suffix'} is not convertible ({SUPPORTED})")
    exe = blender or os.environ.get("R5ED_BLENDER") or os.environ.get("BLENDER") or DEFAULT_BLENDER
    if not Path(exe).is_file():
        raise ConvertError(f"{src}: blender not found at {exe}")
    dst.parent.mkdir(parents=True, exist_ok=True)
    convert_blender(src, dst, up, float(scale), exe, timeout)
    return dst


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("src")
    ap.add_argument("dst", help=".glb to write")
    ap.add_argument("--up", default="y", choices=("y", "z"))
    ap.add_argument("--scale", type=float, default=1.0)
    ap.add_argument("--blender", default=None)
    ap.add_argument("--timeout", type=float, default=600.0)
    args = ap.parse_args()
    dst = convert(args.src, args.dst, args.up, args.scale, args.blender, args.timeout)
    print(f"{Path(args.src).name} -> {dst.name} ({dst.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
