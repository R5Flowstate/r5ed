"""matbuild: no template texture survives into a ported material, and glass takes a flat opacity."""

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import iw4mat  # noqa: E402
import matbuild  # noqa: E402

STOCK = "0x00000000DEADBEEF"


class FakeBank:
    """Hands out a distinct guid per key, the way TextureBank does, without texconv."""

    def __init__(self):
        self.keys = {}

    def _guid(self, key):
        return self.keys.setdefault(key, 0x1000 + len(self.keys))

    def derive(self, image, slot):
        return self._guid(f"{image}|{slot}")

    def flat(self, slot, value):
        return self._guid(f"flat_{slot}_{'_'.join(map(str, value))}")


def template(tmp_path, types):
    tdir = tmp_path / "templates" / "material" / "world" / "signs"
    tdir.mkdir(parents=True)
    doc = {"name": "world/signs/district_zone05_signs_01", "$textures": {k: STOCK for k in types},
           "$textureTypes": types, "rasterizerFlags": "0x6"}
    (tdir / "district_zone05_signs_01_wldc.json").write_text(json.dumps(doc))
    return tmp_path / "templates"


def build(tmp_path, types, **kw):
    templates = template(tmp_path, types)
    bank, files = FakeBank(), []
    matbuild.build(tmp_path / "assets", templates, {}, bank, "world_emissive", "world/obj/x/screen",
                   albedo="screen.png", files=files, **kw)
    out = json.loads((tmp_path / "assets" / "material" / "world/obj/x/screen_wldc.json").read_text())
    return out, bank


def test_an_unknown_template_slot_does_not_keep_the_stock_texture(tmp_path, capsys):
    types = {"0": "_col", "1": "_ilm", "2": "_scrollmask", "3": "_glowscroll"}
    out, bank = build(tmp_path, types)
    assert STOCK not in out["$textures"].values(), "a stock texture would draw over the port's own"
    white = bank.keys["flat_detailTexture_255_255_255_255"]
    black = bank.keys["flat_detailTexture_0_0_0_255"]
    assert out["$textures"]["2"] == f"0x{white:016X}"
    assert out["$textures"]["3"] == f"0x{black:016X}", "an additive-sounding slot gets black"
    assert "_scrollmask" in capsys.readouterr().out, "the substitution is reported"


def test_glass_takes_a_flat_opacity_over_the_albedo_alpha(tmp_path):
    types = {"0": "_col", "1": "_opa"}
    out, bank = build(tmp_path, types, opacity_value=0.4)
    assert out["$textures"]["1"] == f"0x{bank.keys['flat_opacityMultiplyTexture_102_102_102_255']:016X}"
    assert "screen.png|opacityMultiplyTexture" not in bank.keys


def test_known_slots_are_untouched_by_the_neutral_fallback():
    assert all(iw4mat.slot_of(s) in iw4mat.SLOT_FORMAT for s in iw4mat.SUFFIX_SLOT)


def cull(template_flags, cull_face):
    doc = {"rasterizerFlags": template_flags}
    iw4mat.cull_state({"stateBits": [{"colorWriteRgb": 1, "cullFace": cull_face}]}, doc)
    return int(doc["rasterizerFlags"], 16)


def test_a_one_sided_material_culls_back_faces_whatever_the_template_had():
    # RSX material.h: cull bits 0x2 NONE, 0x4 FRONT, 0x6 BACK; 0x8 makes CCW the front.
    for template in ("0x0", "0x2", "0x4", "0x6", "0x8", "0xE"):
        assert cull(template, "back") & 0xE == 0x6, f"template {template} must end as BACK with CW fronts"
    # The old OR made 0x4 (FRONT) from a template with no cull bits.
    assert cull("0x0", "back") != 0x4


def test_a_two_sided_material_culls_nothing():
    for template in ("0x0", "0x6", "0xE"):
        assert cull(template, "none") & 0xE == 0x2


def test_depth_bias_and_scissor_bits_survive():
    assert cull("0x16", "back") == 0x16, "decal preset 0x10 with BACK"
    assert cull("0x96", "none") == 0x92, "scissor 0x80 + decal 0x10 kept, cull none"


def test_a_glass_albedo_carries_its_opacity_in_alpha(tmp_path):
    import numpy as np
    from PIL import Image
    import objmat
    rgba = np.zeros((4, 4, 4), np.uint8); rgba[...] = (10, 20, 30, 255)
    Image.fromarray(rgba).save(tmp_path / "glass.png")
    bank = objmat.ImageBank([tmp_path], tmp_path / "assets", tmp_path / "scratch", "t")
    out = bank.rgba("glass.png@a102")
    assert out[..., 3].max() == 102 and out[..., 3].min() == 102
    assert tuple(out[0, 0, :3]) == (10, 20, 30), "colour kept"
    assert bank.rgba("glass.png")[..., 3].min() == 255, "the plain key is untouched"


def test_a_world_template_uv_transform_is_reset_and_other_data_kept(tmp_path, capsys):
    import struct
    uber = bytearray(0x200)
    struct.pack_into("<6f", uber, 0x00, 4.0, 0.0, 0.0, 4.0, 0.25, 0.0)   # tiled x4: the template's own
    struct.pack_into("<6f", uber, 0x18, 1.0, 0.5, 0.5, 1.0, 0.0, 0.0)    # not a pure scale: left alone
    struct.pack_into("<f", uber, 0x180, 7.0)                              # unrelated data
    src = tmp_path / "t.uber"; src.write_bytes(bytes(uber))
    iw4mat.copy_uber(src, tmp_path / "w.uber", "wldc", "world/walls/wall_paint_grey")
    out = (tmp_path / "w.uber").read_bytes()
    assert struct.unpack_from("<6f", out, 0x00) == (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    assert struct.unpack_from("<6f", out, 0x18) == (1.0, 0.5, 0.5, 1.0, 0.0, 0.0)
    assert struct.unpack_from("<f", out, 0x180)[0] == 7.0
    assert "uv block 1 (4, 4, 0.25, 0)" in capsys.readouterr().out
    iw4mat.copy_uber(src, tmp_path / "m.uber", "rgdp", "models/x")
    assert (tmp_path / "m.uber").read_bytes() == bytes(uber), "a model shader's buffer is copied as is"
