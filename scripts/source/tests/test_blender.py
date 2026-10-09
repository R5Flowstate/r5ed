"""Phase F7: DCC models through Blender into GLB, then import-glb + objmat."""

import json
import os
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
import pytest
from PIL import Image

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
REPO = HERE.parents[1]

import blender_convert  # noqa: E402
import objmat  # noqa: E402

BLENDER = os.environ.get("R5ED_BLENDER", "blender")
SCRATCH = Path(tempfile.gettempdir()) / "r5ed-tests" / "pytest"
needs_blender = pytest.mark.skipif(not Path(BLENDER).is_file(), reason="blender 4.3 absent")
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
EXE = TARGET / "debug" / "r5ed-compile.exe"
needs_exe = pytest.mark.skipif(not EXE.is_file(), reason="r5ed-compile binary absent")


def scratch(name):
    d = SCRATCH / name
    d.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(dir=d))


def png(path, alpha):
    arr = np.zeros((4, 4, 4), np.uint8)
    arr[..., 0] = 200
    arr[..., 3] = alpha
    Image.fromarray(arr).save(path)


def chunk(cid, payload):
    return struct.pack("<HI", cid, 6 + len(payload)) + payload


def quad_3ds():
    verts = struct.pack("<H", 4) + struct.pack("<12f", 0, 0, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0)
    faces = struct.pack("<H", 2) + struct.pack("<8H", 0, 1, 2, 7, 0, 2, 3, 7)
    group = chunk(0x4130, b"Mat\0" + struct.pack("<H", 2) + struct.pack("<2H", 0, 1))
    faces += group
    uvs = struct.pack("<H", 4) + struct.pack("<8f", 0, 0, 1, 0, 1, 1, 0, 1)
    mesh = chunk(0x4110, verts) + chunk(0x4120, faces) + chunk(0x4140, uvs)
    return chunk(0x4D4D, chunk(0x3D3D, chunk(0x4000, b"Box\0" + chunk(0x4100, mesh))))


def glb(doc, blob):
    js = json.dumps(doc, separators=(",", ":")).encode()
    js += b" " * (-len(js) % 4)
    blob = bytes(blob) + b"\0" * (-len(blob) % 4)
    out = struct.pack("<III", 0x46546C67, 2, 12 + 8 + len(js) + 8 + len(blob))
    return out + struct.pack("<II", len(js), 0x4E4F534A) + js + struct.pack("<II", len(blob), 0x004E4942) + blob


def materials_glb(tmp):
    png(tmp / "brick.png", 255)
    png(tmp / "glass.png", 255)
    arr = np.asarray(Image.open(tmp / "glass.png")).copy()
    arr[:2, :, 3] = 0
    Image.fromarray(arr).save(tmp / "glass.png")
    brick, glass = (tmp / "brick.png").read_bytes(), (tmp / "glass.png").read_bytes()
    views = [{"buffer": 0, "byteOffset": 0, "byteLength": len(brick)},
             {"buffer": 0, "byteOffset": len(brick), "byteLength": len(glass)}]
    pos = np.zeros((3, 3), np.float32).tobytes()
    idx = np.arange(3, dtype=np.uint32).tobytes()
    views += [{"buffer": 0, "byteOffset": len(brick) + len(glass), "byteLength": len(pos)},
              {"buffer": 0, "byteOffset": len(brick) + len(glass) + len(pos), "byteLength": len(idx)}]
    doc = {"asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": [0]}],
           "nodes": [{"name": "Cube", "mesh": 0}],
           "meshes": [{"name": "Cube", "primitives": [
               {"attributes": {"POSITION": 0}, "indices": 1, "material": 0},
               {"attributes": {"POSITION": 0}, "indices": 1, "material": 1},
               {"attributes": {"POSITION": 0}, "indices": 1, "material": 2}]}],
           "materials": [
               {"name": "Brick", "doubleSided": True, "pbrMetallicRoughness": {
                   "baseColorFactor": [1.0, 0.5, 0.25, 1.0], "baseColorTexture": {"index": 0}}},
               {"name": "Glass", "alphaMode": "BLEND", "pbrMetallicRoughness": {
                   "baseColorFactor": [0.5, 0.7, 1.0, 0.6], "baseColorTexture": {"index": 1}}},
               {"name": "Grate", "alphaMode": "MASK", "pbrMetallicRoughness": {
                   "baseColorTexture": {"index": 0}}}],
           "textures": [{"source": 0}, {"source": 1}],
           "images": [{"mimeType": "image/png", "bufferView": 0},
                      {"mimeType": "image/png", "bufferView": 1}],
           "bufferViews": views,
           "accessors": [{"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC3"},
                         {"bufferView": 3, "componentType": 5125, "count": 3, "type": "SCALAR"}],
           "buffers": [{"byteLength": len(brick) + len(glass) + len(pos) + len(idx)}]}
    return glb(doc, brick + glass + pos + idx)


def bank(tmp, *roots):
    assets = tmp / "assets"
    return objmat.ImageBank(list(roots), assets, tmp / "tex", "f7/test")


def test_glb_material_table_from_built_bytes():
    tmp = scratch("table")
    p = tmp / "mats.glb"
    p.write_bytes(materials_glb(tmp))
    table, skipped = objmat.parse_glb(p, tmp / "img")
    assert skipped == []
    assert table["Brick"]["color"] == [1.0, 0.5, 0.25]
    assert table["Brick"]["dissolve"] == 1.0
    assert table["Glass"]["dissolve"] == 0.6
    assert table["Glass"]["alpha"] == table["Glass"]["diffuse"]
    assert (tmp / "img" / table["Brick"]["diffuse"]).is_file()
    b = bank(tmp, tmp / "img")
    klass, _, _, _, _, two_sided, missing = objmat.plan("Brick", table["Brick"], b)
    assert (klass, two_sided, missing) == ("world_opaque", True, [])
    klass, _, _, _, flat, _, _ = objmat.plan("Glass", table["Glass"], b)
    assert klass == "world_test"
    assert flat[:3] == (127, 178, 255)
    klass, _, _, _, _, _, _ = objmat.plan("Grate", table["Grate"], b)
    assert klass == "world_test"


def test_glb_cutout_needs_real_alpha():
    tmp = scratch("cuts")
    png(tmp / "solid.png", 255)
    m = {"diffuse": "solid.png", "alpha": "solid.png", "color": [1, 1, 1], "dissolve": 1.0}
    klass, _, _, _, _, _, _ = objmat.plan("Wall", m, bank(tmp, tmp))
    assert klass == "world_opaque"


def test_mtl_path_still_plans_and_reports_missing():
    tmp = scratch("mtl")
    m = objmat.parse_mtl("newmtl A\nmap_Kd a.png\nd 0.5\n")["A"]
    klass, albedo, _, _, _, _, missing = objmat.plan("A", m, bank(tmp, tmp))
    assert klass == "world_glass"
    assert albedo is None
    assert missing == ["A a.png"]


def test_3ds_quad_converts_with_transform():
    tmp = scratch("3ds")
    out = blender_convert.convert_3ds(quad_3ds(), "q.3ds", "y", 2.0)
    p = tmp / "q.glb"
    p.write_bytes(out)
    doc, blob = objmat.read_glb(p)
    assert doc["materials"][0]["name"] == "Mat"
    assert doc["accessors"][3]["count"] == 6
    views = doc["bufferViews"]
    raw = blob[views[0]["byteOffset"]:views[0]["byteOffset"] + views[0]["byteLength"]]
    pts = np.frombuffer(raw, "<f4").reshape(-1, 3)
    assert pts.tolist() == [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 0.0, -2.0], [0.0, 0.0, -2.0]]


def test_3ds_truncated_names_the_file():
    with pytest.raises(blender_convert.ConvertError, match=r"q\.3ds"):
        blender_convert.convert_3ds(quad_3ds()[:40], "q.3ds", "y", 1.0)
    with pytest.raises(blender_convert.ConvertError, match="triangle mesh"):
        blender_convert.convert_3ds(chunk(0x4D4D, chunk(0x3D3D, b"")), "empty.3ds", "y", 1.0)


def test_bad_inputs_name_the_file():
    tmp = scratch("bad")
    with pytest.raises(blender_convert.ConvertError, match="no such input"):
        blender_convert.convert(tmp / "gone.fbx", tmp / "o.glb")
    src = tmp / "a.xyz"
    src.write_bytes(b"nope")
    with pytest.raises(blender_convert.ConvertError, match=r"a\.xyz"):
        blender_convert.convert(src, tmp / "o.glb")
    with pytest.raises(blender_convert.ConvertError, match="scale"):
        blender_convert.convert(src, tmp / "o.glb", scale=0.0)
    fbx = tmp / "b.fbx"
    fbx.write_bytes(b"nope")
    with pytest.raises(blender_convert.ConvertError, match="blender not found"):
        blender_convert.convert(fbx, tmp / "o.glb", blender=str(tmp / "nope.exe"))
    with pytest.raises(blender_convert.ConvertError, match="up must be"):
        blender_convert.convert(fbx, tmp / "o.glb", up="x")


def test_import_table_covers_phase_extensions():
    assert set(blender_convert.IMPORT_OPS) == {".fbx", ".dae", ".stl", ".ply", ".blend"}


def glb_triangles(doc):
    acc = doc["accessors"]
    return sum(acc[p["indices"]]["count"] // 3 if isinstance(p.get("indices"), int)
               else acc[p["attributes"]["POSITION"]]["count"] // 3
               for mesh in doc.get("meshes", []) for p in mesh.get("primitives", []))


CUBE_MAKER = """\
import bpy
bpy.ops.object.select_all(action='SELECT')
bpy.ops.object.delete(use_global=False)
bpy.ops.mesh.primitive_cube_add(size=2.0)
o = bpy.context.active_object
o.name = 'Cube'
mat = bpy.data.materials.new('Stone')
o.data.materials.append(mat)
bpy.ops.wm.stl_export(filepath={stl})
bpy.ops.wm.ply_export(filepath={ply})
bpy.ops.export_scene.fbx(filepath={fbx})
"""


@needs_blender
def test_blender_cube_fbx_stl_ply_convert():
    tmp = scratch("cube")
    maker = tmp / "maker.py"
    srcs = {k: tmp / f"cube.{k}" for k in ("stl", "ply", "fbx")}
    maker.write_text(CUBE_MAKER.format(**{k: json.dumps(str(v)) for k, v in srcs.items()}))
    r = subprocess.run([BLENDER, "-b", "--factory-startup", "--python", str(maker)],
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                       text=True, errors="replace", timeout=300)
    assert all(v.is_file() for v in srcs.values()), r.stdout[-2000:]
    outs = {}
    for k, v in srcs.items():
        out = tmp / f"cube.{k}.glb"
        blender_convert.convert(v, out)
        doc, _ = objmat.read_glb(out)
        outs[k] = (glb_triangles(doc), [m.get("name") for m in doc.get("materials", [])])
    assert outs["stl"][0] == outs["ply"][0] == outs["fbx"][0] == 12
    assert "Stone" in outs["fbx"][1]


@needs_blender
@needs_exe
def test_converted_glb_imports():
    tmp = scratch("import")
    maker = tmp / "maker.py"
    stl = tmp / "cube.stl"
    maker.write_text(CUBE_MAKER.format(stl=json.dumps(str(stl)),
                                       ply=json.dumps(str(tmp / "x.ply")),
                                       fbx=json.dumps(str(tmp / "x.fbx"))))
    subprocess.run([BLENDER, "-b", "--factory-startup", "--python", str(maker)],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True, timeout=300)
    glb_path = tmp / "cube.glb"
    blender_convert.convert(stl, glb_path)
    (tmp / "opts.json").write_text(json.dumps({"scale": 1.0, "up": "y", "material_prefix": "world/f7/test"}))
    probe = tmp / "probe"
    subprocess.run([str(EXE), "new", str(probe), "--stem", "f7"], check=True,
                   stdout=subprocess.DEVNULL, timeout=120)
    r = subprocess.run([str(EXE), "import-glb", str(probe), str(glb_path),
                        "--options", str(tmp / "opts.json")],
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                       text=True, errors="replace", timeout=120)
    assert r.returncode == 0, r.stdout[-2000:]
    rep = json.loads(r.stdout)
    assert rep["objects"] == 1
    assert rep["report"]["triangles"] == 12
