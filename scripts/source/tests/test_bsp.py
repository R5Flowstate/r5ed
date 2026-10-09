"""BSP reader and converter tests: synthetic BSPs built in the test, plus
fixture maps that skip when absent."""

import tempfile
import io
import json
import lzma
import os
import re
import shutil
import struct
import subprocess
import sys
import zipfile
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
REPO = HERE.parents[1]

import bsp  # noqa: E402
import bsp2vmf  # noqa: E402
from bsp import BspError  # noqa: E402

MOMENTUM = Path(os.environ.get("R5ED_TEST_MOMENTUM", "fixture-absent")) / "momentum" / "maps"
PORTAL2 = Path(os.environ.get("R5ED_TEST_PORTAL2", "fixture-absent")) / "portal2" / "maps"
SCRATCH = Path(tempfile.gettempdir()) / "r5ed-tests"

_EXE = None


def _r5ed_compile():
    global _EXE
    if _EXE is not None:
        return _EXE
    base = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
    exe = base / "debug" / ("r5ed-compile.exe" if sys.platform == "win32" else "r5ed-compile")
    if not exe.is_file():
        import shutil as _sh
        if _sh.which("cargo") is None:
            pytest.skip("cargo is not on PATH")
        subprocess.run(["cargo", "build", "-q", "-p", "r5ed-compile"], cwd=REPO, check=False)
    if not exe.is_file():
        pytest.skip("r5ed-compile binary unavailable")
    _EXE = str(exe)
    return _EXE


def _workdir(name):
    d = SCRATCH / "pytest" / name
    shutil.rmtree(d, ignore_errors=True)
    d.mkdir(parents=True, exist_ok=True)
    return d


def _import_vmf(exe, work, stem, vmf):
    proj = work / "proj"
    r = subprocess.run([exe, "new", str(proj), "--stem", stem],
                       capture_output=True, text=True, cwd=REPO)
    assert r.returncode == 0, r.stderr
    r = subprocess.run([exe, "import-vmf", str(proj), str(vmf)],
                       capture_output=True, text=True, cwd=REPO)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)["report"]


def _cube_planes(ox, oy, oz, s):
    return [(-1.0, 0.0, 0.0, -ox), (1.0, 0.0, 0.0, ox + s),
            (0.0, -1.0, 0.0, -oy), (0.0, 1.0, 0.0, oy + s),
            (0.0, 0.0, -1.0, -oz), (0.0, 0.0, 1.0, oz + s)]


def _texinfo_row(u=(1.0, 0.0, 0.0, 0.0), v=(0.0, -1.0, 0.0, 0.0)):
    return struct.pack("<16f2i", *(u + v + (0.0,) * 8), 0, 0)


def _build(lumps, order="std", version=20):
    blob = {i: b"" for i in range(64)}
    blob.update(lumps)
    off = bsp.HEADER_SIZE
    table = []
    for i in range(64):
        table.append((off, len(blob[i]), 0))
        off += len(blob[i])
    if blob[35] and len(blob[35]) >= 4 and 0 <= struct.unpack_from("<i", blob[35], 0)[0] <= 256:
        raw = bytearray(blob[35])
        count = struct.unpack_from("<i", raw, 0)[0]
        if 4 + 16 * count <= len(raw):
            for i in range(count):
                p = 4 + 16 * i + 8
                struct.pack_into("<i", raw, p, struct.unpack_from("<i", raw, p)[0] + table[35][0])
            blob[35] = bytes(raw)
    head = struct.pack("<4si", b"VBSP", version)
    for o, s, v in table:
        head += struct.pack("<iii4s", o, s, v, b"\0\0\0\0") if order == "std" \
            else struct.pack("<iii4s", v, o, s, b"\0\0\0\0")
    return head + b"".join(blob[i] for i in range(64))


def _entities_lump():
    return (b'{\n"classname" "worldspawn"\n"skyname" "test_sky"\n}\n'
            b'{\n"classname" "func_detail"\n"model" "*1"\n}\n'
            b'{\n"classname" "info_player_start"\n"origin" "0 0 64"\n}\n')


def _full_lumps():
    planes = _cube_planes(0, 0, 0, 64) + _cube_planes(128, 0, 0, 64)
    sides = b"".join(struct.pack("<Hhhh", pn, ti, 0, bv) for pn, ti, bv in [
        (0, 0, 0), (1, 0, 0), (2, 0, 0), (3, 0, 0), (4, 0, 0), (5, 0, 0),
        (0, 0, 1), (1, -1, 0),
        (6, 0, 0), (7, 0, 0), (8, 0, 0), (9, 0, 0), (10, 0, 0), (11, 0, 0)])
    leaf_fmt = "<ihh6h4Hh2x"
    lumps = {
        0: _entities_lump(),
        1: b"".join(struct.pack("<4fi", *p, 0) for p in planes),
        2: struct.pack("<3f5i", 0.0, 0.0, 0.0, 0, 512, 512, 512, 512),
        5: struct.pack("<i2i6h2H2h", 0, -1, -2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
        6: _texinfo_row(),
        10: struct.pack(leaf_fmt, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, -1)
        + struct.pack(leaf_fmt, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, -1),
        14: struct.pack("<9f3i", 0, 0, 0, 192, 64, 64, 0, 0, 0, 0, 0, 0)
        + struct.pack("<9f3i", 0, 0, 0, 192, 64, 64, 0, 0, 0, 0, 0, 0),
        17: struct.pack("<2H", 0, 1),
        18: struct.pack("<3i", 0, 8, 1) + struct.pack("<3i", 8, 6, 1),
        19: sides,
        43: b"BRICK/WALL\0",
        44: struct.pack("<i", 0),
    }
    pak = io.BytesIO()
    with zipfile.ZipFile(pak, "w") as z:
        z.writestr("materials/brick/wall.vmt", b'"LightmappedGeneric" {}', compress_type=zipfile.ZIP_STORED)
        z.writestr("nested/deep/file.txt", b"hi")
    lumps[40] = pak.getvalue()
    name = b"models/props/test.mdl" + b"\0" * (128 - len("models/props/test.mdl"))
    prop = struct.pack("<6f3HBBi2f3ff", 10, 20, 30, 0, 90, 0, 0, 0, 1, 1, 0, 2, 0.0, 0.0,
                       0, 0, 0, 1.0)
    body = struct.pack("<i", 1) + name + struct.pack("<i", 0) + struct.pack("<i", 1) + prop
    lumps[35] = struct.pack("<i", 1) + struct.pack("<4sHHii", b"prps", 0, 5, 20, len(body)) + body
    return lumps


def test_synthetic_bsp_converts():
    work = _workdir("synthetic")
    src = work / "mini.bsp"
    src.write_bytes(_build(_full_lumps()))
    out = work / "out"
    stats = bsp2vmf.convert(src, out)
    assert (stats["brushes"], stats["dropped"], stats["props"]) == (2, 0, 1)
    assert (stats["files"], stats["disps"]) == (2, 0)
    vmf = (out / "mini.vmf").read_text(encoding="latin1")
    assert vmf.count("\tsolid\n") == 4
    assert vmf.count("\t\tside\n") == 24
    assert '"material" "BRICK/WALL"' in vmf
    assert '"classname" "prop_static"' in vmf
    assert '"skin" "2"' in vmf and '"origin" "10 20 30"' in vmf
    assert (out / "content" / "materials" / "brick" / "wall.vmt").is_file()
    assert (out / "content" / "nested" / "deep" / "file.txt").read_bytes() == b"hi"


def test_bad_files_raise_not_hang():
    work = _workdir("bad")
    good = _build(_full_lumps())

    def write(data):
        p = work / "bad.bsp"
        p.write_bytes(data)
        return str(p)

    with pytest.raises(BspError, match="bad.bsp"):
        bsp.Bsp(write(good[:100]))
    with pytest.raises(BspError, match="bad.bsp"):
        bsp.Bsp(write(b"NOPE" + good[4:]))
    with pytest.raises(BspError, match="bad.bsp"):
        bsp.Bsp(write(good[:4] + struct.pack("<i", 18) + good[8:]))
    lumps = _full_lumps()
    lumps[2] = b"x" * 31
    with pytest.raises(BspError, match="bad.bsp"):
        bsp.texdata_of(bsp.Bsp(write(_build(lumps))))
    lumps = _full_lumps()
    lumps[44] = struct.pack("<i", 9999)
    with pytest.raises(BspError, match="bad.bsp"):
        bsp.texnames(bsp.Bsp(write(_build(lumps))))
    lumps = _full_lumps()
    lumps[18] = struct.pack("<3i", 9999, 6, 1) + struct.pack("<3i", 8, 6, 1)
    with pytest.raises(BspError, match="bad.bsp"):
        bsp.brushes(bsp.Bsp(write(_build(lumps))))
    lumps = _full_lumps()
    lumps[1] = b"LZMA" + b"\0" * 100
    with pytest.raises(BspError, match="bad.bsp"):
        bsp.planes(bsp.Bsp(write(_build(lumps))))
    lumps = {35: struct.pack("<i", 9999)}
    with pytest.raises(BspError, match="bad.bsp"):
        bsp.static_props(bsp.Bsp(write(_build(lumps))))
    with pytest.raises(SystemExit):
        bsp2vmf.convert(write(good[:100]), work / "oops")


def _lzma_lump(data):
    comp = lzma.LZMACompressor(format=lzma.FORMAT_RAW, filters=[
        {"id": lzma.FILTER_LZMA1, "dict_size": 1 << 24, "lc": 3, "lp": 0, "pb": 2}])
    blob = comp.compress(data) + comp.flush()
    return b"LZMA" + struct.pack("<II", len(data), len(blob)) + bytes([0x5D]) \
        + struct.pack("<I", 1 << 24) + blob


def test_l4d2_header_order_and_compressed_lump():
    work = _workdir("l4d2")
    lumps = _full_lumps()
    lumps[0] = _lzma_lump(_entities_lump())
    src = work / "l4d2style.bsp"
    src.write_bytes(_build(lumps, order="l4d2", version=21))
    assert len(bsp.entities(bsp.Bsp(src))) == 3
    stats = bsp2vmf.convert(src, work / "out")
    assert (stats["brushes"], stats["props"]) == (2, 1)


def _prps(ver, stride, records):
    body = struct.pack("<i", 1) + b"models/p.mdl" + b"\0" * (128 - len("models/p.mdl"))
    body += struct.pack("<i", 0) + struct.pack("<i", len(records)) + b"".join(records)
    assert all(len(r) == stride for r in records)
    return struct.pack("<i", 1) + struct.pack("<4sHHii", b"prps", 0, ver, 20, len(body)) + body


def test_sprp_version_strides():
    work = _workdir("sprp")

    def props(ver, stride, records):
        p = work / "s.bsp"
        p.write_bytes(_build({35: _prps(ver, stride, records)}))
        return bsp.static_props(bsp.Bsp(p))

    base = struct.pack("<6f3HBBi2f3f", 1, 2, 3, 0, 0, 0, 0, 0, 0, 1, 0, 5, 0.0, 10.0, 0, 0, 0)
    assert props(4, 56, [base])[0]["skin"] == 5
    assert props(5, 60, [base + struct.pack("<f", 1.0)])[0]["skin"] == 5
    assert props(9, 72, [base + struct.pack("<f", 1.0) + b"\0" * 12])[0]["skin"] == 5
    got = props(11, 76, [base + struct.pack("<f", 1.0) + b"\0" * 12 + struct.pack("<f", 2.5)])
    assert got[0]["scale"] == 2.5
    with pytest.raises(BspError, match="s.bsp"):
        props(13, 72, [base + struct.pack("<f", 1.0) + b"\0" * 12])


def _disp_lumps():
    corners = [(0.0, 0.0, 64.0), (0.0, 64.0, 64.0), (64.0, 64.0, 64.0), (64.0, 0.0, 64.0)]
    faces = bytearray(56)
    struct.pack_into("<HBB", faces, 0, 5, 0, 0)
    struct.pack_into("<i", faces, 4, 0)
    struct.pack_into("<3h", faces, 8, 4, 0, 0)
    dispinfo = bytearray(176)
    struct.pack_into("<3f", dispinfo, 0, *corners[0])
    struct.pack_into("<3i", dispinfo, 12, 0, 0, 2)
    struct.pack_into("<H", dispinfo, 36, 0)
    return {
        1: b"".join(struct.pack("<4fi", *p, 0) for p in _cube_planes(0, 0, 0, 64)),
        3: b"".join(struct.pack("<3f", *c) for c in corners),
        5: struct.pack("<i2i6h2H2h", 0, -1, -1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
        6: _texinfo_row(),
        7: bytes(faces),
        10: struct.pack("<ihh6h4Hh2x", 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, -1),
        12: b"".join(struct.pack("<2H", *e) for e in [(0, 1), (1, 2), (2, 3), (3, 0)]),
        13: struct.pack("<4i", 0, 1, 2, 3),
        14: struct.pack("<9f3i", 0, 0, 0, 64, 64, 64, 0, 0, 0, 0, 0, 0),
        17: struct.pack("<H", 0),
        18: struct.pack("<3i", 0, 6, 1),
        19: b"".join(struct.pack("<Hhhh", pn, 0, -1, 0) for pn in range(6)),
        26: bytes(dispinfo),
        33: b"".join(struct.pack("<5f", 0, 0, 0, 0, 0) for _ in range(25)),
        43: b"BRICK/WALL\0",
        44: struct.pack("<i", 0),
        2: struct.pack("<3f5i", 0.0, 0.0, 0.0, 0, 64, 64, 64, 64),
        0: b'{\n"classname" "worldspawn"\n}\n',
    }


def test_dispinfo_block_on_flat_quad():
    work = _workdir("disp")
    src = work / "disp.bsp"
    src.write_bytes(_build(_disp_lumps()))
    stats = bsp2vmf.convert(src, work / "out")
    assert (stats["attached"], stats["unattached"], stats["disps"]) == (1, 0, 1)
    vmf = (work / "out" / "disp.vmf").read_text(encoding="latin1")
    assert vmf.count("dispinfo") == 1
    assert vmf.count("\tsolid\n") == 1
    assert '"power" "2"' in vmf and '"startposition" "[0 0 64]"' in vmf
    rows = re.findall(r'"row\d" "([^"]*)"', vmf)
    assert len(rows) == 29
    for row in rows[:5]:
        vals = [float(x) for x in row.split()]
        assert len(vals) == 15 and all(vals[k + 2] > 0.99 for k in range(0, 15, 3))


SURF = MOMENTUM / "surf_mom_training.bsp"
E1912 = PORTAL2 / "e1912.bsp"
TRIG = MOMENTUM / "triggertests.bsp"
BHOP = MOMENTUM / "bhop_mom_training_beta.bsp"


def _convert_fixture(path, work):
    out = work / "out"
    return bsp2vmf.convert(path, out), out / (Path(path).stem + ".vmf")


@pytest.mark.skipif(not SURF.is_file(), reason="surf_mom_training.bsp fixture absent")
def test_surf_mom_training_converts_and_imports():
    work = _workdir("surf")
    stats, vmf = _convert_fixture(SURF, work)
    assert stats["brushes"] > 0 and stats["dropped"] == 0
    report = _import_vmf(_r5ed_compile(), work, "surf_mom_training", vmf)
    assert report["brushes"] > 0
    assert report["bad_brushes"] / report["brushes"] <= 0.01


@pytest.mark.skipif(not E1912.is_file(), reason="e1912.bsp fixture absent")
def test_portal2_e1912_converts_and_imports():
    work = _workdir("e1912")
    stats, vmf = _convert_fixture(E1912, work)
    assert stats["props"] == 328
    assert (stats["attached"], stats["unattached"]) == (77, 0)
    report = _import_vmf(_r5ed_compile(), work, "e1912", vmf)
    assert report["brushes"] > 0
    assert report["bad_brushes"] / report["brushes"] <= 0.01
    assert report["props"] > 0


@pytest.mark.skipif(not TRIG.is_file(), reason="triggertests.bsp fixture absent")
def test_triggertests_disps_convert_and_import():
    work = _workdir("trig")
    stats, vmf = _convert_fixture(TRIG, work)
    assert (stats["attached"], stats["unattached"]) == (9, 0)
    report = _import_vmf(_r5ed_compile(), work, "triggertests", vmf)
    assert report["brushes"] > 0
    assert report["bad_brushes"] / report["brushes"] <= 0.01


@pytest.mark.skipif(not BHOP.is_file(), reason="bhop_mom_training_beta.bsp fixture absent")
def test_bhop_lzma_lumps_convert_and_import():
    work = _workdir("bhop")
    stats, vmf = _convert_fixture(BHOP, work)
    assert stats["brushes"] > 0 and stats["dropped"] == 0
    report = _import_vmf(_r5ed_compile(), work, "bhop", vmf)
    assert report["brushes"] > 0
    assert report["bad_brushes"] / report["brushes"] <= 0.01
