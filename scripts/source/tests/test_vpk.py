"""VPK v1/v2 reader tests: synthetic archives built in the test, plus fixture checks."""

import os
import struct
import sys
from pathlib import Path

import numpy as np
import pytest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import vmt  # noqa: E402
import vpk  # noqa: E402
import vtf  # noqa: E402

HL2_DIR = Path(os.environ.get("R5ED_TEST_MOMENTUM", "fixture-absent")) / "mount" / "hl2_dir.vpk"


def _files(entries):
    out = bytearray()
    for stem, e in entries:
        crc, pre, archive, eo, el = e
        out += stem.encode() + b"\0"
        out += struct.pack("<IHHIIH", crc, len(pre), archive, eo, el, 0xFFFF) + pre
    return bytes(out)


def _tree(groups):
    out = bytearray()
    for ext, paths in groups:
        out += ext.encode() + b"\0"
        for path, entries in paths:
            out += path.encode() + b"\0" + _files(entries)
            out += b"\0"
        out += b"\0"
    return bytes(out) + b"\0"


def _write_v1(tmp, stem, groups, archives):
    tree = _tree(groups)
    (tmp / f"{stem}_dir.vpk").write_bytes(struct.pack("<III", vpk.SIG, 1, len(tree)) + tree)
    for i, blob in archives.items():
        (tmp / f"{stem}_{i:03d}.vpk").write_bytes(blob)
    return tmp / f"{stem}_dir.vpk"


def _write_v2(tmp, stem, groups, archives, embedded=b""):
    tree = _tree(groups)
    head = struct.pack("<IIIIIII", vpk.SIG, 2, len(tree), len(embedded), 0, 0, 0)
    (tmp / f"{stem}_dir.vpk").write_bytes(head + tree + embedded)
    for i, blob in archives.items():
        (tmp / f"{stem}_{i:03d}.vpk").write_bytes(blob)
    return tmp / f"{stem}_dir.vpk"


def test_v1_lists_and_extracts_with_preload(tmp_path):
    blob = b"\0" * 16 + b"vmt-bytes" + b"pad" + b"vtf-bytes!!"
    root = _write_v1(tmp_path, "t", [
        ("vmt", [("materials/a", [("wall", (7, b"PRE", 0, 16, 9))])]),
        ("vtf", [("materials/a", [("wall", (9, b"", 0, 28, 11))])]),
    ], {0: blob})
    pack = vpk.Vpk(root)
    assert pack.paths() == ["materials/a/wall.vmt", "materials/a/wall.vtf"]
    assert pack.read("materials/a/wall.vmt") == b"PREvmt-bytes"
    assert pack.read("MATERIALS\\A\\WALL.VTF") == b"vtf-bytes!!"


def test_v2_reads_embedded_and_archive_files(tmp_path):
    arch = b"X" * 8 + b"arch-data"
    root = _write_v2(tmp_path, "t", [
        ("vmt", [(" ", [("root", (1, b"", 0x7FFF, 4, 7))])]),
        ("bin", [("d", [("a", (2, b"P", 1, 8, 9))])]),
    ], {1: arch}, embedded=b"....emb-six!")
    pack = vpk.Vpk(root)
    assert pack.read("root.vmt") == b"emb-six"
    assert pack.read("d/a.bin") == b"Parch-data"


def test_bad_files_raise_not_hang(tmp_path):
    root = tmp_path / "t_dir.vpk"
    root.write_bytes(b"nope")
    with pytest.raises(vpk.VpkError):
        vpk.Vpk(root)
    tree = _tree([("vmt", [("m", [("a", (1, b"", 0, 0, 4))])])])
    root.write_bytes(struct.pack("<III", vpk.SIG, 1, len(tree)) + tree[: len(tree) // 2])
    with pytest.raises(vpk.VpkError):
        vpk.Vpk(root)
    good = _write_v1(tmp_path, "g", [("vmt", [("m", [("a", (1, b"", 0, 90, 40))])])], {0: b"tiny"})
    with pytest.raises(vpk.VpkError):
        vpk.Vpk(good).read("m/a.vmt")
    with pytest.raises(vpk.VpkError):
        vpk.Vpk(good).read("m/missing.vmt")


def _vtf_rgba(w, h, px):
    head = bytearray(80)
    head[0:4] = b"VTF\0"
    struct.pack_into("<IIIHHIHH", head, 4, 7, 2, 80, w, h, 0, 1, 0)
    struct.pack_into("<i", head, 52, vtf.RGBA8888)
    head[56] = 1
    struct.pack_into("<i", head, 57, -1)
    struct.pack_into("<H", head, 63, 1)
    return bytes(head) + px.tobytes()


def test_content_root_serves_vmt_and_vtf_from_vpk(tmp_path):
    px = np.arange(2 * 2 * 4, dtype=np.uint8).reshape(2, 2, 4)
    body = _vtf_rgba(2, 2, px)
    text = b'"LightmappedGeneric" { "$basetexture" "test/wall" }'
    blob = text + b"\0" * 8 + body
    root = _write_v1(tmp_path, "c", [
        ("vmt", [("materials/test", [("wall", (3, b"", 0, 0, len(text)))])]),
        ("vtf", [("materials/test", [("wall", (5, b"", 0, len(text) + 8, len(body)))])]),
    ], {0: blob})
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


@pytest.mark.skipif(not HL2_DIR.is_file(), reason="hl2_dir.vpk fixture absent")
def test_hl2_dir_lists_tens_of_thousands_of_files():
    pack = vpk.Vpk(HL2_DIR)
    assert len(pack) > 30000
    assert "materials/brick/brickwall049c.vmt" in pack


@pytest.mark.skipif(not HL2_DIR.is_file(), reason="hl2_dir.vpk fixture absent")
def test_hl2_dir_vmt_parses_and_vtf_decodes():
    content = vmt.Content([HL2_DIR])
    shader, keys = content.material("models/weapons/w_annabelle/gun")
    assert shader == "vertexlitgeneric"
    found = content.texture(keys["$basetexture"])
    assert found is not None
    arr, hd = vtf.read(found)
    assert arr.shape == (hd["height"], hd["width"], 4)
    assert arr.dtype == np.uint8
