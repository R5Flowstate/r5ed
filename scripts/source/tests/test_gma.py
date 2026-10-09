"""GMA reader tests: archives built in the test, plus content-root checks."""

import struct
import sys
import zlib
from pathlib import Path

import numpy as np
import pytest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import gma  # noqa: E402
import vmt  # noqa: E402
import vtf  # noqa: E402


def _cstr(s):
    return s.encode("utf-8") + b"\0"


def _build(version=3, name="Test", desc="desc", author="author", addon_version=1,
           required=(), table=(), blob=b"", trailer=True):
    out = bytearray()
    out += b"GMAD" + bytes([version])
    out += struct.pack("<QQ", 0, 1234567890)
    if version > 1:
        for r in required:
            out += _cstr(r)
        out += b"\0"
    out += _cstr(name) + _cstr(desc) + _cstr(author)
    out += struct.pack("<i", addon_version)
    num = 0
    for rel, size, crc in table:
        num += 1
        out += struct.pack("<I", num) + _cstr(rel) + struct.pack("<qI", size, crc)
    out += struct.pack("<I", 0)
    out += blob
    if trailer:
        out += struct.pack("<I", zlib.crc32(bytes(out)) & 0xFFFFFFFF)
    return bytes(out)


def _write(path, files, **kw):
    table, blob = [], bytearray()
    for rel, data in files:
        table.append((rel, len(data), zlib.crc32(data) & 0xFFFFFFFF))
        blob += data
    Path(path).write_bytes(_build(table=table, blob=bytes(blob), **kw))
    return Path(path)


def test_v3_lists_reads_and_extracts(tmp_path):
    vmt_bytes = b'"LightmappedGeneric" { "$basetexture" "test/wall" }'
    vtf_bytes = bytes(range(256)) * 4
    root = _write(tmp_path / "t.gma", [
        ("materials/test/wall.vmt", vmt_bytes),
        ("materials/test/wall.vtf", vtf_bytes),
    ], name="Walls", desc='{"description": "walls"}', author="me", addon_version=7)
    pack = gma.Gma(root)
    assert (pack.name, pack.author, pack.addon_version) == ("Walls", "me", 7)
    assert (pack.format_version, pack.steamid) == (3, 0)
    assert pack.paths() == ["materials/test/wall.vmt", "materials/test/wall.vtf"]
    assert pack.entries()[0][:2] == ("materials/test/wall.vmt", len(vmt_bytes))
    assert pack.entries()[0][2] == zlib.crc32(vmt_bytes) & 0xFFFFFFFF
    assert pack.info("materials/test/wall.vtf") == (len(vtf_bytes), zlib.crc32(vtf_bytes) & 0xFFFFFFFF)
    assert pack.read("MATERIALS\\TEST\\WALL.VMT") == vmt_bytes
    assert len(pack) == 2
    assert "materials/test/wall.vtf" in pack
    out = pack.extract(tmp_path / "root")
    assert out == pack.paths()
    assert (tmp_path / "root" / "materials" / "test" / "wall.vtf").read_bytes() == vtf_bytes
    assert gma.GmaFile(pack, "materials/test/wall.vmt").read_text() == vmt_bytes.decode()


def test_v1_skips_required_section_and_v2_keeps_it(tmp_path):
    one = _write(tmp_path / "one.gma", [("a.txt", b"1")], version=1)
    assert gma.Gma(one).required == []
    assert gma.Gma(one).read("a.txt") == b"1"
    two = _write(tmp_path / "two.gma", [("a.txt", b"2")], version=2, required=["servercontent"])
    pack = gma.Gma(two)
    assert pack.required == ["servercontent"]
    assert pack.read("a.txt") == b"2"


def test_bad_files_raise_not_hang(tmp_path):
    root = tmp_path / "t.gma"
    root.write_bytes(b"nope")
    with pytest.raises(gma.GmaError):
        gma.Gma(root)
    good = _build(table=[("a.txt", 1, 0)], blob=b"1")
    root.write_bytes(good[:30])
    with pytest.raises(gma.GmaError):
        gma.Gma(root)
    root.write_bytes(good[:-6])
    with pytest.raises(gma.GmaError):
        gma.Gma(root)
    bad = bytearray(good)
    bad[4] = 9
    root.write_bytes(bytes(bad))
    with pytest.raises(gma.GmaError, match="version 9"):
        gma.Gma(root)
    bad = bytearray(good)
    bad[4] = 0
    root.write_bytes(bytes(bad))
    with pytest.raises(gma.GmaError, match="version 0"):
        gma.Gma(root)
    root.write_bytes(_build(table=[("a.txt", -5, 0)], blob=b"1"))
    with pytest.raises(gma.GmaError, match="negative size"):
        gma.Gma(root)
    root.write_bytes(_build(table=[("a.txt", 40, 0)], blob=b"tiny"))
    with pytest.raises(gma.GmaError, match="past end of file"):
        gma.Gma(root)
    root.write_bytes(_build(table=[("a.txt", 1, 0)], blob=b"1", trailer=False))
    with pytest.raises(gma.GmaError, match="past end of file"):
        gma.Gma(root)
    root.write_bytes(b"GMAD\x03" + b"A" * 64)
    with pytest.raises(gma.GmaError, match="runs past end of file"):
        gma.Gma(root)
    pack = gma.Gma(_write(tmp_path / "ok.gma", [("a.txt", b"1")]))
    with pytest.raises(gma.GmaError):
        pack.read("missing.txt")
    with pytest.raises(gma.GmaError):
        pack.info("missing.txt")


def test_extract_refuses_paths_outside_the_root(tmp_path):
    root = tmp_path / "evil.gma"
    root.write_bytes(_build(table=[("../evil.txt", 4, 0)], blob=b"evil"))
    with pytest.raises(gma.GmaError, match="outside"):
        gma.Gma(root).extract(tmp_path / "root")
    assert not (tmp_path / "evil.txt").exists()


def _vtf_rgba(w, h, px):
    head = bytearray(80)
    head[0:4] = b"VTF\0"
    struct.pack_into("<IIIHHIHH", head, 4, 7, 2, 80, w, h, 0, 1, 0)
    struct.pack_into("<i", head, 52, vtf.RGBA8888)
    head[56] = 1
    struct.pack_into("<i", head, 57, -1)
    struct.pack_into("<H", head, 63, 1)
    return bytes(head) + px.tobytes()


def test_content_root_serves_vmt_and_vtf_from_gma(tmp_path):
    px = np.arange(2 * 2 * 4, dtype=np.uint8).reshape(2, 2, 4)
    root = _write(tmp_path / "c.gma", [
        ("materials/test/wall.vmt", b'"LightmappedGeneric" { "$basetexture" "test/wall" }'),
        ("materials/test/wall.vtf", _vtf_rgba(2, 2, px)),
    ])
    content = vmt.Content([root])
    shader, keys = content.material("test/wall")
    assert shader == "lightmappedgeneric"
    assert keys["$basetexture"] == "test/wall"
    found = content.texture(keys["$basetexture"])
    assert not isinstance(found, Path)
    arr, hd = vtf.read(found)
    assert (hd["width"], hd["height"]) == (2, 2)
    assert (arr == px).all()
    assert vtf.size(found) == (2, 2)


def test_earlier_roots_win_over_gma(tmp_path):
    plain = tmp_path / "plain"
    (plain / "materials" / "test").mkdir(parents=True)
    (plain / "materials" / "test" / "wall.vmt").write_text('"VertexLitGeneric" {}')
    root = _write(tmp_path / "c.gma", [("materials/test/wall.vmt", b'"LightmappedGeneric" {}')])
    content = vmt.Content([plain, root])
    assert content.material("test/wall")[0] == "vertexlitgeneric"
    assert len(content.gmas) == 1
