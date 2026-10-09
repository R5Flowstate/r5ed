"""Fixture-free checks of the Source readers, plus fixture checks that skip when absent."""

import os
import struct
import sys
from pathlib import Path

import numpy as np
import pytest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import objmat  # noqa: E402
import srcprops  # noqa: E402
import vmt  # noqa: E402
import vtf  # noqa: E402

SURF = Path(os.environ.get("R5ED_TEST_SURF_CASTLE", "fixture-absent")) / "csgo"


def test_vmt_parses_quoted_unquoted_comments_and_blocks():
    shader, keys = vmt.parse('LightmappedGeneric // c\n{\n "$basetexture" "a/b"\n $surfaceprop metal\n'
                             ' Proxies { AnimatedTexture { animatedtextureframerate 5 } }\n}')
    assert shader == "lightmappedgeneric"
    assert keys["$basetexture"] == "a/b"
    assert keys["$surfaceprop"] == "metal"
    assert isinstance(keys["proxies"], dict)


def _vtf_7_2_rgba(w, h, pixels):
    head = bytearray(80)
    head[0:4] = b"VTF\0"
    struct.pack_into("<IIIHHIHH", head, 4, 7, 2, 80, w, h, 0, 1, 0)
    struct.pack_into("<i", head, 52, vtf.RGBA8888)
    head[56] = 1
    struct.pack_into("<i", head, 57, -1)
    struct.pack_into("<H", head, 63, 1)
    return bytes(head) + pixels.tobytes()


def test_vtf_raw_rgba_round_trips(tmp_path):
    px = np.arange(4 * 2 * 4, dtype=np.uint8).reshape(2, 4, 4)
    p = tmp_path / "t.vtf"
    p.write_bytes(_vtf_7_2_rgba(4, 2, px))
    arr, hd = vtf.read(p)
    assert (hd["width"], hd["height"]) == (4, 2)
    assert (arr == px).all()


def test_mdl_large_rule_matches_rmdlconv():
    data = bytearray(0x100)
    struct.pack_into("<3f", data, 0x68, -10, -10, -10)
    struct.pack_into("<3f", data, 0x74, 10, 10, 10)
    assert not srcprops.is_large(bytes(data))
    struct.pack_into("<3f", data, 0x74, 1500, 10, 10)
    assert srcprops.is_large(bytes(data))


def test_mtl_last_token_is_the_image():
    m = objmat.parse_mtl("newmtl A\nmap_Kd -s 1 1 1 a.png\nd 0.5\n")
    assert m["A"]["diffuse"] == "a.png"
    assert m["A"]["dissolve"] == 0.5


@pytest.mark.skipif(not SURF.is_dir(), reason="surf_castle fixture absent")
def test_surf_castle_materials_resolve():
    content = vmt.Content([SURF])
    shader, keys = content.material("models/surf/castle_wall")
    assert shader == "vertexlitgeneric"
    assert content.texture(keys["$basetexture"]) is not None


def test_s21_name_has_no_empty_segment():
    import srcmat
    assert srcmat.s21_name("/ALTFLAG", False) == "world/source/altflag"
    assert srcmat.s21_name("CONCRETE/Brick", False) == "world/source/concrete/brick"


def test_translucent_foliage_is_cut_out_and_glass_blends():
    import srcmat
    leaves = {"$basetexture": "models/surf/list", "$translucent": "1", "$nocull": "1"}
    assert srcmat.classify("vertexlitgeneric", leaves, True) == "model_test_2s"
    assert srcmat.classify("vertexlitgeneric", {"$basetexture": "glass/window01", "$translucent": "1"}, True) == "model_glass"
    assert srcmat.classify("lightmappedgeneric", {"$basetexture": "decals/x", "$translucent": "1"}, False) == "world_decal"
