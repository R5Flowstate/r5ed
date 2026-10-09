"""IW4 materials -> S21 matl v23 + txtr v10 assets and a Repak manifest.

Each MW2 material is rebuilt on a stock S21 material of the same class
(opaque, alpha-tested, translucent decal, glass, emissive screen; world or
model). The template supplies the shader set, uber buffer and render state;
the MW2 textures replace the slots they have an answer for:

    albedoTexture          <- colorMap (RGB, alpha kept)
    normalTexture          <- normalMap (DXT5nm: X in alpha, Y in green), at MW2's slope;
                              a material with none takes one built from its colorMap
    glossTexture           <- specularMap alpha, capped where MW2 barely reflected
    specTexture            <- reflectance at normal incidence from specularMap and envMapParms
    opacityMultiplyTexture <- colorMap alpha
    emissiveTexture        <- colorMap

Slots MW2 has nothing for keep the template's own texture.
"""

import argparse
import json
import math
import os
import shutil
import struct
import subprocess
import sys
from pathlib import Path

import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import r5edenv  # noqa: E402
from guid import string_to_guid  # noqa: E402
from synth_v10_tail import synth_v10_tail, total_mip_count  # noqa: E402

TEXCONV = r5edenv.get("R5ED_TEXCONV", required=False) or "texconv"

# S21 eTextureFormat and the DXGI name texconv takes.
FORMATS = {
    "BC1_UNORM_SRGB": 1,
    "BC4_UNORM": 6,
    "BC5_UNORM": 8,
    "BC7_UNORM": 12,
    "BC7_UNORM_SRGB": 13,
}
SLOT_FORMAT = {
    "albedoTexture": "BC7_UNORM_SRGB",
    "normalTexture": "BC5_UNORM",
    "glossTexture": "BC4_UNORM",
    "specTexture": "BC1_UNORM_SRGB",
    "opacityMultiplyTexture": "BC4_UNORM",
    "emissiveTexture": "BC1_UNORM_SRGB",
    "aoTexture": "BC4_UNORM",
    "detailTexture": "BC7_UNORM",
    "detailNormalTexture": "BC5_UNORM",
    "layerBlendTexture": "BC4_UNORM",
}
# What a slot gets when MW2 has no image for it: neutral, never a template's texture.
# 56 is a dielectric's 0.04 reflectance in sRGB.
FLAT = {"albedoTexture": (128, 128, 128, 255), "normalTexture": (128, 128, 0, 255),
        "glossTexture": (48, 48, 48, 255), "specTexture": (56, 56, 56, 255),
        "opacityMultiplyTexture": (255, 255, 255, 255), "emissiveTexture": (0, 0, 0, 255),
        "aoTexture": (255, 255, 255, 255), "detailTexture": (128, 128, 128, 255),
        "layerBlendTexture": (255, 255, 255, 255)}
# Short suffix names some templates use for the same slots.
SUFFIX_SLOT = {"_col": "albedoTexture", "_nml": "normalTexture", "_gls": "glossTexture",
               "_spc": "specTexture", "_opa": "opacityMultiplyTexture", "_msk": "opacityMultiplyTexture",
               "_ilm": "emissiveTexture", "_ao": "aoTexture", "_det": "detailTexture", "_bm": "layerBlendTexture"}

# class -> stock template material (exported with RSX) and its shader type.
TEMPLATES = {
    # Carries a tiling detail normal (kept from the template): close-up surface grain
    # MW2's single-layer world materials lack.
    "world_opaque": ("world/walls/wall_painted_dark_tan", "wldc"),
    "world_floor": ("world/floors/marble_tile", "wldc"),
    "world_blend": ("world/grass/divided_meadows_grass_green_01_to_divided_rocky_dirt_01", "wld_bm"),
    "world_test": ("world/metal/chain_link_fence", "wldc"),
    "world_decal": ("world/decal/sign_warning_yellow", "wldc"),
    "world_decal_multiply": ("world/decal/sign_warning_yellow", "wldc"),
    "world_glass": ("world/glass/glass_shiny", "wldc"),
    "world_emissive": ("world/signs/district_zone05_signs_01", "wldc"),
    "model_opaque": ("models/props/loot_tarp/construction_plastic_mat_white_01", "rgdp"),
    "model_test": ("models/foliage/english_ivy_green_01_optimized", "rgdp"),
    "model_test_2s": ("models/foliage/bush_cliff01", "rgdp"),
    "model_glass": ("models/imc_interior/imc_int_glass", "rgdp"),
    "model_fx": ("world/atmosphere/godray_500_fade_scroll", "rgdp"),
    "model_hidden": ("models/foliage/english_ivy_green_01_optimized", "rgdp"),
}
# Templates whose pixel shader reads only the detail normal through uv block 1
# (the base layers take the mesh UV): the block keeps the template's tiling.
DETAIL_UV_TEMPLATES = {"world/walls/wall_painted_dark_tan"}
# A template's depth and colour-pass materials; each one that samples textures
# gets a twin carrying ours, or it alpha-tests against the template's cutout.
TWIN_REFS = {"$depthShadowMaterial": "shadow", "$depthPrepassMaterial": "prepass", "$depthVSMMaterial": "vsm",
             "$depthShadowTightMaterial": "tightshadow", "$colpassMaterial": "colpass"}
FX_MATERIAL = "world/atmosphere/godray_500_fade_scroll"
FX_TECHS = ("effect", "zfeather", "falloff", "unlit_add", "unlit_blend", "unlit_screen", "distfalloff")
SKIP_TECHS = ("shadowcaster", "tools", "sky")

SURFACE_WORDS = [
    ("glass", "glass"), ("window", "glass"), ("metal", "metal"), ("chrome", "metal"), ("steel", "metal"),
    ("alumin", "metal"), ("wood", "wood"), ("carpet", "carpet"), ("concrete", "concrete"), ("asphalt", "asphalt"),
    ("brick", "brick"), ("tile", "tile"), ("marble", "tile"), ("grass", "grass"), ("dirt", "dirt"),
    ("plaster", "plaster"), ("drywall", "plaster"), ("wall", "plaster"), ("rubber", "rubber"),
    ("plastic", "plastic"), ("paper", "paper"), ("cloth", "carpet"), ("fabric", "upholstery"),
    ("seat", "upholstery"), ("foliage", "foliage"), ("leaf", "foliage"), ("rock", "rock"), ("sand", "sand"),
]


def surface_prop(name):
    low = name.lower()
    for word, prop in SURFACE_WORDS:
        if word in low:
            return prop
    return "concrete"


# Opaque world materials that take the floor template's detail grain.
FLOOR_WORDS = ("floor", "tile", "marble")
# Classes whose surfaces block baked light and so bounce it.
BOUNCE_CLASSES = ("world_opaque", "world_floor", "world_test", "world_emissive",
                  "model_opaque", "model_test", "model_test_2s")
# Opaque classes that take relief built from the colour map when MW2 gave no normal map,
# minus printed surfaces, whose lettering is not relief.
RELIEF_CLASSES = ("world_opaque", "world_floor", "model_opaque")
NO_RELIEF_WORDS = ("sign", "poster", "screen", "monitor", "logo", "text", "board", "display", "flag",
                   "paper", "map", "billboard", "label", "decal", "glass", "window", "light", "painting",
                   "picture", "frame", "photo", "tape", "book", "magazine", "box", "stripe", "receipt", "chrome",
                   "film", "_lit", "granite", "marble", "polished")


def classify(mat, is_model):
    tech = mat.get("techniqueSet", "")
    if any(t in tech for t in SKIP_TECHS):
        return None
    if any(t in tech for t in FX_TECHS):
        return "fx"
    sb = (mat.get("stateBits") or [{}])[0]
    blended = sb.get("dstBlendRgb") not in (None, "zero")
    tested = sb.get("alphaTest") not in (None, "always", "disabled")
    name = mat.get("_name", "").lower()
    side = "model" if is_model else "world"
    if "unlit_replace" in tech:
        return "model_opaque" if is_model else ("world_test" if tested else "world_emissive")
    # dst * src: the colour map darkens what is under it and its alpha is unused.
    if not is_model and sb.get("srcBlendRgb") == "zero" and sb.get("dstBlendRgb") == "srccolor":
        return "world_decal_multiply"
    if blended:
        if "glass" in name or "window" in name:
            return f"{side}_glass"
        return "model_glass" if is_model else "world_decal"
    if tested:
        lit = next((s for s in mat.get("stateBits", []) if s.get("colorWriteRgb")), sb)
        if is_model and lit.get("cullFace") == "none":
            return "model_test_2s"
        return f"{side}_test"
    if not is_model and any(w in name.split("/")[-1] for w in FLOOR_WORDS):
        return "world_floor"
    return f"{side}_opaque"


# MW2's world pixel shaders decode a normal texel as slope = a * 4.08 - 2.08 (x, from
# alpha) and g * 4.0645 - 2.0645 (y) with z = 1; S21's decode t * 2.86766 - 1.43945.
# Both put +x along +U and +y along +V.
MW2_NORMAL_X = (4.07999992, -2.07999992)
MW2_NORMAL_Y = (4.06451607, -2.06451607)
S21_NORMAL = (2.86766052, -1.43945312)


def s21_normal_texels(slope_x, slope_y):
    """RGBA8 for BC5 that S21 decodes back to these tangent-space slopes."""
    tx = np.clip((slope_x - S21_NORMAL[1]) / S21_NORMAL[0], 0.0, 1.0)
    ty = np.clip((slope_y - S21_NORMAL[1]) / S21_NORMAL[0], 0.0, 1.0)
    return np.stack([np.round(tx * 255), np.round(ty * 255), np.zeros_like(tx), np.full_like(tx, 255)],
                    -1).astype(np.uint8)


def mw2_slopes(arr):
    a = arr[..., 3].astype(np.float32) / 255.0
    g = arr[..., 1].astype(np.float32) / 255.0
    return a * MW2_NORMAL_X[0] + MW2_NORMAL_X[1], g * MW2_NORMAL_Y[0] + MW2_NORMAL_Y[1]


def relief_slopes(arr):
    """Slopes of a height field read from a colour map's luminance: dark grout, cracks and
    pores sit low. Shading broader than 1/24 of the texture is left out and the steepest 5%
    of texels come out at a 0.25 slope, never amplified past 8x, so a near-flat colour (its
    block noise) stays flat."""
    from scipy.ndimage import gaussian_filter
    lum = arr[..., :3].astype(np.float32) @ np.array([0.299, 0.587, 0.114], np.float32) / 255.0
    h = lum - gaussian_filter(lum, max(lum.shape) / 24.0, mode="wrap")
    h = gaussian_filter(h, 0.7, mode="wrap")
    if float(h.std()) < 0.008:
        return np.zeros_like(h), np.zeros_like(h)
    gx = (np.roll(h, -1, axis=1) - np.roll(h, 1, axis=1)) * 0.5
    gy = (np.roll(h, -1, axis=0) - np.roll(h, 1, axis=0)) * 0.5
    k = min(0.25 / max(float(np.percentile(np.hypot(gx, gy), 95)), 1e-4), 8.0)
    return np.clip(-gx * k, -1.0, 1.0), np.clip(-gy * k, -1.0, 1.0)


def srgb_encode(lin):
    lin = np.clip(lin, 0.0, 1.0)
    return np.where(lin <= 0.0031308, lin * 12.92, 1.055 * np.power(lin, 1 / 2.4) - 0.055)


def srgb_decode(c):
    return np.where(c <= 0.04045, c / 12.92, np.power((c + 0.055) / 1.055, 2.4))


DIELECTRIC_F0 = 0.04
# What a material with no envMapParms reflects: MW2 drew no probe reflection on it.
ENV_NONE = (0.07, 0.4, 4.0)


def reflectance(mat, spec_arr):
    """How a material's MW2 reflection maps onto S21's: (metal, envMin, gloss cap).

    MW2 reflected specularMap^2 * lerp(envMin, envMax, (1 - N.V)^exp). A material it
    already lit at normal incidence (spec^2 * envMin of 0.04 or more) keeps that
    reflectance per texel, tinted; anything else is a 0.04 dielectric, which S21's
    Fresnel takes to a full reflection at grazing. MW2's gloss is the S21 gloss, except
    where MW2's strongest reflection (spec^2 * envMax) stayed under 10%: there its gloss
    meant nothing (carpet and grass carry 0.95 under a near-black specular map) and is
    capped, from 0.97 at 10% down to 0.1 at none."""
    consts = {c["name"]: c.get("literal") for c in mat.get("constants", [])}
    env = tuple(consts["envMapParms"][:3]) if consts.get("envMapParms") else ENV_NONE
    lin = (spec_arr[..., :3].astype(np.float32) / 255.0) ** 2
    mean = float((lin @ np.array([0.2126, 0.7152, 0.0722], np.float32)).mean())
    metal = mean * env[0] >= DIELECTRIC_F0
    peak = mean * env[1]
    if peak >= 0.1:
        cap = 0.97
    elif peak >= 0.03:
        cap = 0.3 + 0.67 * (peak - 0.03) / 0.07
    else:
        cap = 0.1 + 0.2 * peak / 0.03
    return metal, env[0], round(cap, 2)


class TextureBank:
    """Decodes MW2 images once and writes each derived S21 texture once."""

    def __init__(self, images, assets, scratch, namespace="mw2/terminal"):
        self.namespace = namespace
        self.image_dirs = [Path(d) for d in images]
        self.assets, self.scratch = Path(assets), Path(scratch)
        self.decoded = {}
        self.entries = {}
        (self.assets / "texture").mkdir(parents=True, exist_ok=True)
        self.scratch.mkdir(parents=True, exist_ok=True)

    def find(self, image):
        return next((d / f"{image}.dds" for d in self.image_dirs if (d / f"{image}.dds").is_file()), None)

    def rgba(self, image):
        if image in self.decoded:
            return self.decoded[image]
        src = self.find(image)
        if src is None:
            self.decoded[image] = None
            return None
        safe = "".join(c if c.isalnum() or c in "-_" else "_" for c in image)
        tmp = self.scratch / f"{safe}.dds"
        shutil.copyfile(src, tmp)
        subprocess.run([TEXCONV, "-nologo", "-y", "-ft", "png", "-f", "R8G8B8A8_UNORM", "-o", str(self.scratch), str(tmp)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        png = self.scratch / f"{safe}.png"
        arr = np.asarray(Image.open(png).convert("RGBA")) if png.is_file() else None
        self.decoded[image] = arr
        return arr

    def blank_normal(self, image):
        """A normal map MW2 shipped as one flat colour."""
        arr = self.rgba(image)
        if arr is None:
            return True
        sx, sy = mw2_slopes(arr)
        return float(np.percentile(np.hypot(sx - sx.mean(), sy - sy.mean()), 95)) < 0.05

    def derive(self, image, slot, multiply=False, refl=None, relief=False):
        # A multiply decal becomes black over the surface at 1 - luminance, which
        # is dst * colour for a grey stain.
        if multiply:
            key = f"{image}|{slot}|multiply"
        elif slot == "normalTexture":
            key = f"{image}|{slot}|relief" if relief else f"{image}|{slot}|s21slope"
        elif refl is not None and slot in ("glossTexture", "specTexture"):
            key = f"{image}|{slot}|m{int(refl[0])}_{refl[1]:.3f}_{refl[2]:.2f}"
        else:
            key = f"{image}|{slot}"
        tag = f"0x{string_to_guid(f'texture/{self.namespace}/{key}.rpak'):016X}"
        if key not in self.entries and (self.assets / "texture" / f"{tag}.dds").is_file() and self.find(image) is not None:
            h, w = self.dims(self.assets / "texture" / f"{tag}.dds")
            return self.write(key, np.zeros((h, w, 4), np.uint8), slot)
        arr = self.rgba(image)
        if arr is None:
            return None
        if multiply and slot in ("albedoTexture", "opacityMultiplyTexture"):
            lum = arr[..., 0].astype(np.float32) * 0.2126 + arr[..., 1] * 0.7152 + arr[..., 2] * 0.0722
            cover = (255 - np.clip(lum, 0, 255)).astype(np.uint8)
            zero = np.zeros_like(cover)
            if slot == "albedoTexture":
                out = np.stack([zero, zero, zero, cover], -1)
            else:
                out = np.stack([cover, cover, cover, np.full_like(cover, 255)], -1)
        elif slot == "albedoTexture" or slot == "emissiveTexture":
            out = arr
        elif slot == "normalTexture":
            out = s21_normal_texels(*(relief_slopes(arr) if relief else mw2_slopes(arr)))
        elif slot == "glossTexture":
            a = arr[..., 3].astype(np.float32)
            if refl is not None:
                a = np.minimum(a, refl[2] * 255.0)
            a = np.round(a).astype(np.uint8)
            out = np.stack([a, a, a, np.full_like(a, 255)], -1)
        elif slot == "specTexture":
            if refl is None:
                out = arr.copy()
                out[..., 3] = 255
            else:
                metal, env_min, _ = refl
                lin = (arr[..., :3].astype(np.float32) / 255.0) ** 2
                if metal:
                    f0 = np.clip(lin * env_min, DIELECTRIC_F0, 0.95)
                else:
                    # A dielectric keeps the specular map's variation around 0.04.
                    lum = lin @ np.array([0.2126, 0.7152, 0.0722], np.float32)
                    scale = np.clip(np.sqrt(lum / max(float(lum.mean()), 1e-6)), 0.5, 1.5)
                    f0 = np.repeat((DIELECTRIC_F0 * scale)[..., None], 3, -1)
                rgb = np.round(srgb_encode(f0) * 255).astype(np.uint8)
                out = np.concatenate([rgb, np.full(rgb.shape[:2] + (1,), 255, np.uint8)], -1)
        elif slot in ("opacityMultiplyTexture", "aoTexture"):
            a = arr[..., 3]
            out = np.stack([a, a, a, np.full_like(a, 255)], -1)
        else:
            return None
        return self.write(key, out, slot)

    @staticmethod
    def dims(dds):
        h, w = struct.unpack_from("<II", Path(dds).read_bytes()[:20], 12)
        return h, w

    def write(self, key, rgba, slot):
        if key in self.entries:
            return self.entries[key]["$guid"]
        fmt_name = SLOT_FORMAT[slot]
        guid = string_to_guid(f"texture/{self.namespace}/{key}.rpak")
        tag = f"0x{guid:016X}"
        h, w = rgba.shape[:2]
        # BC blocks need 4-aligned sizes; MW2 textures are powers of two >= 4 except a few.
        if w % 4 or h % 4:
            w2, h2 = max(4, (w + 3) // 4 * 4), max(4, (h + 3) // 4 * 4)
            rgba = np.asarray(Image.fromarray(rgba).resize((w2, h2), Image.LANCZOS))
            h, w = h2, w2
        out = self.assets / "texture" / f"{tag}.dds"
        if not out.is_file():
            png = self.scratch / f"{tag}.png"
            Image.fromarray(rgba).save(png)
            srgb = ["-srgbi"] if fmt_name.endswith("SRGB") else []
            subprocess.run([TEXCONV, "-nologo", "-y", "-dx10", "-m", "0", "-f", fmt_name, *srgb, "-bc", "q",
                            "-o", str(self.assets / "texture"), str(png)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        mips = total_mip_count(w, h)
        tail = synth_v10_tail(FORMATS[fmt_name], w, h, 1, mips, 0, 0, slot).hex()
        (self.assets / "texture" / f"{tag}.json").write_text(json.dumps(
            {"streamLayout": ["permanent"] * mips, "hdrTail": tail, "resourceFlags": "0x0", "usageFlags": "0x0"}, indent="\t"))
        self.entries[key] = {"_type": "txtr", "_path": f"texture/{tag}", "$guid": guid, "$hdrTail": tail,
                             "$strmMips": 0, "$optMips": 0}
        assert out.is_file(), out
        return guid

    def mean_albedo(self, image, cutout=False):
        """Mean linear colour of a colour map (texels an alpha test keeps, for a cutout);
        mid grey with none."""
        arr = self.rgba(image) if image else None
        if arr is None:
            return [0.216, 0.216, 0.216]
        rgb = srgb_decode(arr[..., :3].astype(np.float32) / 255.0).reshape(-1, 3)
        if cutout:
            keep = arr[..., 3].reshape(-1) >= 128
            rgb = rgb[keep] if keep.any() else rgb
        return [round(float(v), 4) for v in rgb.mean(axis=0)]

    def flat(self, slot, value):
        rgba = np.zeros((4, 4, 4), np.uint8)
        rgba[...] = value
        return self.write(f"flat_{slot}_{'_'.join(map(str, value))}", rgba, slot)


def load_template(templates, name, stype):
    matches = list(Path(templates).rglob(f"{Path(name).name}_{stype}.json"))
    matches = [m for m in matches if "depth" not in m.parts]
    if not matches:
        raise SystemExit(f"template {name}_{stype} is not exported under {templates}")
    j = json.loads(matches[0].read_text())
    uber = matches[0].with_suffix(".uber")
    return j, uber if uber.is_file() else None


def slot_of(type_name):
    return SUFFIX_SLOT.get(type_name, type_name)


def template_index(templates):
    """Every exported template material by its asset guid."""
    out = {}
    root = Path(templates) / "material"
    for f in root.rglob("*.json"):
        rel = f.relative_to(root).with_suffix("").as_posix()
        out[string_to_guid(f"material/{rel}.rpak")] = f
    return out


# Rasterizer flags, the D3D11_RASTERIZER_DESC bits a stock material carries (RSX's
# material.h): bits 1-2 are the cull mode, 0x2 NONE, 0x4 FRONT, 0x6 BACK; 0x8 makes
# counter-clockwise triangles the front; 0x10..0x60 pick the depth-bias preset (decal,
# shadow map, ...); 0x80 enables the scissor test.
RS_CULL_MASK = 0x6
RS_CULL_NONE = 0x2
RS_CULL_BACK = 0x6
RS_FRONT_CCW = 0x8


def cull_state(mat, doc):
    """Gives a material the cull mode `mat` asks for (back, or none for two-sided).

    The cull bits are set outright, not OR-ed onto the template's: OR-ing 0x4 into a
    template with no cull bits made 0x4, FRONT culling, and a template with 0x8 culled
    back faces of the other winding. Either way the faces you look at were dropped and
    closed meshes vanished by view angle. Every compiled mesh fronts clockwise
    triangles, so 0x8 is cleared too. The template's depth-bias and scissor bits stay."""
    lit = next((s for s in mat.get("stateBits", []) if s.get("colorWriteRgb")), None)
    if lit is None:
        return
    flags = int(doc.get("rasterizerFlags", "0x0"), 16)
    mode = RS_CULL_BACK if lit.get("cullFace") == "back" else RS_CULL_NONE
    flags = (flags & ~(RS_CULL_MASK | RS_FRONT_CCW)) | mode
    doc["rasterizerFlags"] = f"0x{flags:X}"


# CBufUberStatic of the world pixel shaders (scripts/h2/h2mat.py): texture-coordinate
# transforms, 2x3 floats (sx, 0, 0, sy, tx, ty) per block. A template's own tiling sits
# there, and copied verbatim it rescales every ported surface's UVs: a uniquely unwrapped
# prop (CyberPark's trucks) shows the wrong part of its texture.
WORLD_SHADER_TYPES = ("wldc", "wld_bm")
UV_BLOCKS = (0x00, 0x18, 0x30, 0x48, 0x60)
_UV_REPORTED = set()


def copy_uber(src, dst, stype, label="", keep=()):
    """Copies a template's uber buffer, with its UV transforms set to identity on world
    shaders. Only a block that reads as a 2x3 transform (zero shear, finite non-zero scale)
    is touched, and none numbered in `keep`; what it held is printed once per template."""
    data = bytearray(Path(src).read_bytes())
    if stype in WORLD_SHADER_TYPES:
        for i, o in enumerate(UV_BLOCKS, 1):
            if o + 24 > len(data):
                break
            if i in keep:
                continue
            sx, b, c, sy, tx, ty = struct.unpack_from("<6f", data, o)
            vals = (sx, b, c, sy, tx, ty)
            looks = all(math.isfinite(v) and abs(v) < 1e4 for v in vals) and b == 0 and c == 0 and sx != 0 and sy != 0
            if not looks or vals == (1.0, 0.0, 0.0, 1.0, 0.0, 0.0):
                continue
            if (label, i) not in _UV_REPORTED:
                _UV_REPORTED.add((label, i))
                print(f"  template {label}: uv block {i} ({sx:g}, {sy:g}, {tx:g}, {ty:g}) -> identity")
            struct.pack_into("<6f", data, o, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    Path(dst).write_bytes(bytes(data))


def twin_materials(doc, textures_by_slot, s21_name, stype, index, assets, files, mat):
    """Points each depth/colpass ref of `doc` at a twin that samples our textures."""
    for key, kind in TWIN_REFS.items():
        ref = int(doc.get(key, "0x0"), 16)
        src = index.get(ref)
        if not ref or src is None:
            continue
        twin = json.loads(src.read_text())
        textures = dict(twin.get("$textures", {}))
        changed = False
        for k, type_name in twin.get("$textureTypes", {}).items():
            guid = textures_by_slot.get(slot_of(type_name))
            if guid is not None:
                textures[k] = f"0x{guid:016X}"
                changed = True
        if not changed:
            continue
        twin_stype = src.stem.rsplit("_", 1)[1]
        folder, base = s21_name.rsplit("/", 1)
        name = f"{folder}/{base}_colpass" if kind == "colpass" else f"depth/{folder}/{base}_{kind}"
        twin["name"] = name
        twin["$textures"] = textures
        cull_state(mat, twin)
        rel = f"material/{name}_{twin_stype}"
        (assets / rel).parent.mkdir(parents=True, exist_ok=True)
        (assets / f"{rel}.json").write_text(json.dumps(twin, indent=1))
        uber = src.with_suffix(".uber")
        if uber.is_file():
            copy_uber(uber, assets / f"{rel}.uber", twin_stype, src.stem)
        guid = string_to_guid(f"{rel}.rpak")
        files.append({"_type": "matl", "_path": f"{rel}.rpak", "$guid": guid})
        doc[key] = f"0x{guid:016X}"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("dump", help="OAT zone dump root (materials/, images/)")
    ap.add_argument("templates", help="RSX export of the template materials")
    ap.add_argument("used", help="JSON list of {material, model: bool} the map draws")
    ap.add_argument("assets", help="Repak assets dir to write")
    ap.add_argument("--pak-name", default="mw2_terminal_src")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--map-out", required=True, help="IW4 material -> S21 material path map to write")
    ap.add_argument("--images", nargs="*", default=[], help="more image dirs, e.g. a common_mp dump")
    ap.add_argument("--albedo-out", help="S21 material path -> mean linear albedo, for the bake's bounce light")
    ap.add_argument("--namespace", default="mw2/terminal")
    args = ap.parse_args()

    dump, assets = Path(args.dump), Path(args.assets)
    bank = TextureBank([dump / "images", *args.images], assets, assets.parent / "texscratch", namespace=args.namespace)
    index = template_index(args.templates)
    used = json.loads(Path(args.used).read_text())
    files, name_map, report, albedo = [], {}, {}, {}

    for item in used:
        iw_name, is_model = item["material"], item["model"]
        src = dump / "materials" / f"{iw_name}.json"
        if src.is_file():
            mat = json.loads(src.read_text())
            mat["_name"] = iw_name
            klass = classify(mat, is_model)
        else:
            mat = {"_name": iw_name, "textures": []}
            klass = "model_opaque" if is_model else None
            report.setdefault("missing_source", []).append(iw_name)
        if is_model and klass == "fx":
            # MW2's beam, flare and glow images are wavelet IWIs nothing here decodes, so on
            # the godray card they draw its own scrolling texture; they go.
            klass = "model_hidden"
        elif is_model and (klass is None or "shadowcaster" in iw_name):
            # A model names every surface it has; one MW2 never drew still needs a material.
            klass = "model_hidden"
        report.setdefault(str(klass), []).append(iw_name)
        if klass is None or klass == "fx":
            name_map[iw_name] = None
            continue
        tname, stype = TEMPLATES[klass]
        tpl, uber = load_template(args.templates, tname, stype)

        by_slot = {t["name"]: t["image"] for t in mat.get("textures", [])}
        color, normal, spec = by_slot.get("colorMap"), by_slot.get("normalMap"), by_slot.get("specularMap")
        # `$identitynormalmap` and friends are engine built-ins with no image.
        if normal and normal.startswith("$"):
            normal = None
        base = iw_name.split("/")[-1]
        folder = f"models/{args.namespace}" if is_model else f"world/{args.namespace}"
        s21_name = f"{folder}/{base}"
        textures = dict(tpl.get("$textures", {}))
        spec_arr = bank.rgba(spec) if spec else None
        refl = reflectance(mat, spec_arr) if spec_arr is not None else None
        glossy = refl is not None and (refl[0] or min(float(spec_arr[..., 3].mean()) / 255.0, refl[2]) >= 0.75)
        if normal and bank.blank_normal(normal):
            normal = None
        # Polished metal and glossy surfaces take no relief: their colour maps carry painted reflections.
        relief = (not normal and color is not None and klass in RELIEF_CLASSES and not glossy
                  and not any(w in base.lower() for w in NO_RELIEF_WORDS))
        by_guid = {}
        for key, type_name in tpl.get("$textureTypes", {}).items():
            slot = slot_of(type_name)
            guid = None
            if klass == "model_hidden" and slot in ("albedoTexture", "opacityMultiplyTexture"):
                guid = bank.flat(slot, (0, 0, 0, 0) if slot == "opacityMultiplyTexture" else (128, 128, 128, 0))
            elif slot in ("albedoTexture", "emissiveTexture", "opacityMultiplyTexture") and color:
                guid = bank.derive(color, slot, multiply=klass == "world_decal_multiply")
            elif slot == "albedoTexture":
                guid = bank.flat(slot, (128, 128, 128, 255))
            elif slot == "normalTexture":
                if normal:
                    guid = bank.derive(normal, slot)
                elif relief:
                    guid = bank.derive(color, slot, relief=True)
                    report.setdefault("relief_normal", []).append(iw_name)
                else:
                    guid = bank.flat(slot, (128, 128, 0, 255))
            elif slot in ("glossTexture", "specTexture"):
                if spec_arr is not None:
                    guid = bank.derive(spec, slot, refl=refl)
                else:
                    guid = bank.flat(slot, FLAT[slot])
            elif slot == "detailNormalTexture" and glossy:
                # Chrome and polished metal take no plaster grain.
                guid = bank.flat(slot, (128, 128, 0, 255))
            # The template's tiling detail normal stays: MW2 has no such layer.
            if guid is None and slot in SLOT_FORMAT and slot != "detailNormalTexture":
                if klass == "model_fx" and slot == "albedoTexture":
                    # The beam images are IW4 wavelet IWIs nothing here decodes; the godray keeps its own.
                    report.setdefault("template_texture", []).append(f"{iw_name} {slot}")
                    continue
                guid = bank.flat(slot, FLAT[slot])
                report.setdefault("unsourced_flat", []).append(f"{iw_name} {slot}")
            elif guid is None and type_name != "unavailable":
                report.setdefault("template_texture", []).append(f"{iw_name} {type_name}")
            if guid is not None:
                textures[key] = f"0x{guid:016X}"
                by_guid[slot] = guid
        doc = dict(tpl)
        doc["name"] = s21_name
        doc["$textures"] = textures
        doc["surfaceProp"] = surface_prop(base)
        cull_state(mat, doc)
        if klass in ("world_glass", "model_glass"):
            # Glass shows from both sides: a pane seen from inside a room is its back face.
            flags = int(doc.get("rasterizerFlags", "0x0"), 16)
            doc["rasterizerFlags"] = f"0x{(flags & ~(RS_CULL_MASK | RS_FRONT_CCW)) | RS_CULL_NONE:X}"
        twin_materials(doc, by_guid, s21_name, stype, index, assets, files, mat)
        rel = f"material/{s21_name}_{stype}"
        (assets / rel).parent.mkdir(parents=True, exist_ok=True)
        (assets / f"{rel}.json").write_text(json.dumps(doc, indent=1))
        if uber:
            copy_uber(uber, assets / f"{rel}.uber", stype, tname, keep=(1,) if tname in DETAIL_UV_TEMPLATES else ())
        files.append({"_type": "matl", "_path": f"{rel}.rpak", "$guid": string_to_guid(f"{rel}.rpak")})
        name_map[iw_name] = s21_name
        if klass in BOUNCE_CLASSES:
            albedo[s21_name] = bank.mean_albedo(color, cutout=klass.endswith(("_test", "_test_2s")))

    files = list(bank.entries.values()) + files
    manifest = {"version": 8, "name": args.pak_name, "assetsDir": str(assets).replace("\\", "/") + "/",
                "outputDir": args.out_dir.replace("\\", "/") + "/", "headerFlags": 32, "files": files}
    manifest_path = assets.parent / f"{args.pak_name}.json"
    manifest_path.write_text(json.dumps(manifest, indent=1))
    Path(args.map_out).write_text(json.dumps(name_map, indent=1))
    if args.albedo_out:
        Path(args.albedo_out).write_text(json.dumps(albedo, indent=1, sort_keys=True))
    print(f"{sum(1 for f in files if f['_type'] == 'matl')} materials, {len(bank.entries)} textures -> {manifest_path}")
    print({k: len(v) for k, v in report.items()})
    for k in ("unsourced_flat", "template_texture", "relief_normal"):
        for line in report.get(k, []):
            print(f"  {k}: {line}")


if __name__ == "__main__":
    main()
