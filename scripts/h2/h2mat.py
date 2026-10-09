"""Halo 2 shaders -> S21 world materials, rebuilt from the original bitmaps.

Every Halo 2 material keeps its own layer stack and scales (the postprocess
constants h2dump writes), mapped onto a stock S21 material of the same shape:

  tex_bump_detail_blend_detail -> two-layer blend (world projected): macro colour
      in albedo, the two detail maps as detail layers, macro alpha as layer blend
  tex_bump / tex_bump_detail_overlay / tex_bump_env_* -> UV0 base + detail
  one_alpha_env_illum -> emissive sign template
  tex_alpha_test / *_alpha_test_* -> alpha tested template

Colour and detail bitmaps are upscaled 4x with Real-ESRGAN (wrap-padded so
tiling survives) and matched back to the source statistics; normals are
upscaled smoothly and gain the fine relief of the upscaled colour. Gloss and
cavity are derived where Halo 2 had none.
"""

import argparse
import json
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
# Detail layers fade to neutral over these mip levels (full strength below the start).
DETAIL_FADE_START, DETAIL_FADE_END = 1, 4
ESRGAN = os.environ.get("REALESRGAN", "realesrgan-ncnn-vulkan.exe")
PREFIX = "h2/bloodgulch"

FORMATS = {"BC1_UNORM": 0, "BC1_UNORM_SRGB": 1, "BC4_UNORM": 6, "BC5_UNORM": 8, "BC7_UNORM": 12, "BC7_UNORM_SRGB": 13}
SLOT_FORMAT = {
    "_col": "BC7_UNORM_SRGB", "albedoTexture": "BC7_UNORM_SRGB",
    "_nml": "BC5_UNORM", "normalTexture": "BC5_UNORM",
    "_gls": "BC4_UNORM", "glossTexture": "BC4_UNORM",
    "_spc": "BC1_UNORM_SRGB", "specTexture": "BC1_UNORM_SRGB",
    "_cav": "BC4_UNORM", "_ao": "BC4_UNORM", "_bm": "BC4_UNORM",
    "_det": "BC7_UNORM",
    "opacityMultiplyTexture": "BC4_UNORM", "emissiveTexture": "BC7_UNORM_SRGB",
}

# Stock S21 materials each family is rebuilt on, exported with RSX
# (`-export --exporttypes matl --texturenames guid --exportguids`) as <family>.json/.uber
# and <family>_colpass.json/.uber; the h2-map-port skill lists the guids.
TEMPLATE_DIR = Path(os.environ.get("H2_TEMPLATES", "templates"))
TEMPLATES = {f: TEMPLATE_DIR / f for f in ("blend", "detail", "test", "emissive")}
COLPASS = {f: TEMPLATE_DIR / f"{f}_colpass" for f in TEMPLATES}

# CBufUberStatic, from the decompiled world pixel shaders.
UV_BLOCK = {1: 0x00, 2: 0x18, 3: 0x30, 4: 0x48, 5: 0x60}
LAYER_BLEND_RAMP = 0x8C
FEATURE_FLAGS = 0x180
TINT_L0 = 0x160  # c_zUpBlendingVertexAlphaAndL0_albedoTint (c22)


class Sources:
    """Decoded and upscaled Halo 2 bitmaps, cached on disk."""

    def __init__(self, dump, work):
        self.dump, self.work = Path(dump), Path(work)
        (self.work / "png").mkdir(parents=True, exist_ok=True)
        (self.work / "up").mkdir(parents=True, exist_ok=True)
        self.files = {}
        for sh in json.loads((self.dump / "shaders.json").read_text()).values():
            for b in sh["bitmaps"]:
                if b["bitmap"] and b["file"]:
                    self.files[b["bitmap"].split("\\")[-1]] = self.dump / b["file"]

    def rgba(self, name):
        src = self.files[name]
        png = self.work / "png" / (src.stem + ".png")
        if not png.is_file():
            r = subprocess.run([TEXCONV, "-nologo", "-y", "-ft", "png", "-f", "R8G8B8A8_UNORM", "-o", str(png.parent), str(src)],
                               capture_output=True)
            if r.returncode or not png.is_file():
                print(f"  cannot decode {src.name}")
                return None
        return np.asarray(Image.open(png).convert("RGBA")).astype(np.float32) / 255.0

    def upscaled(self, name, cap):
        """RGB upscaled 4x (tileable), statistics matched to the source; alpha resized smoothly."""
        src = self.rgba(name)
        h, w = src.shape[:2]
        out = self.work / "up" / f"{self.files[name].stem}_x4.png"
        if not out.is_file():
            tile = self.work / "up" / f"{self.files[name].stem}_3x3.png"
            Image.fromarray((np.tile(src[..., :3], (3, 3, 1)) * 255).round().astype(np.uint8)).save(tile)
            big = self.work / "up" / f"{self.files[name].stem}_3x3_x4.png"
            subprocess.run([ESRGAN, "-i", str(tile), "-o", str(big), "-n", "realesrgan-x4plus", "-s", "4"],
                           capture_output=True, check=True)
            u = np.asarray(Image.open(big).convert("RGB"))[h * 4:2 * h * 4, w * 4:2 * w * 4]
            Image.fromarray(u).save(out)
            tile.unlink()
            big.unlink()
        rgb = np.asarray(Image.open(out).convert("RGB")).astype(np.float32) / 255.0
        s_mean, s_std = src[..., :3].reshape(-1, 3).mean(0), src[..., :3].reshape(-1, 3).std(0) + 1e-4
        o_mean, o_std = rgb.reshape(-1, 3).mean(0), rgb.reshape(-1, 3).std(0) + 1e-4
        rgb = np.clip((rgb - o_mean) / o_std * s_std + s_mean, 0, 1)
        alpha = resize(src[..., 3], rgb.shape[1], rgb.shape[0])
        img = np.dstack([rgb, alpha])
        return fit(img, cap)

    def normal(self, name, size, relief=None, relief_strength=0.0):
        """A Halo 2 normal map resized to `size`, renormalised, with optional extra
        relief from a height field (the upscaled colour's luminance)."""
        px = self.rgba(name)
        if px is None:
            return None
        n = px[..., :3] * 2 - 1
        n = np.dstack([resize(n[..., i], size[0], size[1]) for i in range(3)])
        if relief is not None and relief_strength > 0:
            gy, gx = np.gradient(relief)
            n[..., 0] -= gx * relief_strength
            n[..., 1] -= gy * relief_strength
        n /= np.linalg.norm(n, axis=2, keepdims=True) + 1e-6
        return n


def fit_uvs(dump):
    """Halo UV0 of each world-projected material as a linear function of world XY."""
    parts = json.loads((Path(dump) / "parts.json").read_text())
    acc = {}
    for p in parts:
        raw = (Path(dump) / p["file"]).read_bytes()
        n = p["verts"]
        f = np.frombuffer(raw, "<f4", n * 8)
        acc.setdefault(p["material"], []).append((f[:n * 3].reshape(-1, 3), f[n * 6:n * 8].reshape(-1, 2)))
    out = {}
    for m, l in acc.items():
        P = np.concatenate([a for a, _ in l])
        U = np.concatenate([b for _, b in l])
        A = np.c_[P[:, 0], P[:, 1], np.ones(len(P))]
        cu = np.linalg.lstsq(A, U[:, 0], rcond=None)[0]
        cv = np.linalg.lstsq(A, U[:, 1], rcond=None)[0]
        out[m] = {"u": cu.tolist(), "v": cv.tolist()}
    return out


def resize(ch, w, h):
    if ch.shape[1] == w and ch.shape[0] == h:
        return ch
    return np.asarray(Image.fromarray(ch.astype(np.float32), mode="F").resize((w, h), Image.BICUBIC))


def fit(img, cap):
    h, w = img.shape[:2]
    s = min(1.0, cap / max(w, h))
    if s >= 1.0:
        return img
    nw, nh = max(4, int(w * s)), max(4, int(h * s))
    return np.dstack([resize(img[..., i], nw, nh) for i in range(img.shape[2])])


def luminance(rgb):
    return rgb[..., 0] * 0.2126 + rgb[..., 1] * 0.7152 + rgb[..., 2] * 0.0722


def highpass(x, radius):
    from scipy.ndimage import gaussian_filter
    return x - gaussian_filter(x, radius, mode="wrap")


def gloss_map(rgb, base, spread):
    lum = luminance(rgb)
    return np.clip(base + (lum - lum.mean()) * spread, 0.02, 0.95)


def cavity_map(rgb, depth=2.2):
    """Soft occlusion from the colour's local darkness: crevices in rock and dirt."""
    lum = luminance(rgb)
    hp = highpass(lum, max(2.0, rgb.shape[0] / 128))
    return np.clip(1.0 + np.minimum(hp, 0) * depth, 0.55, 1.0)


def tile_to(img, n, cap):
    """Repeat an image n x n (a layer Halo 2 scaled n times finer than its base) at the cap."""
    t = np.tile(img, (n, n, 1))
    return fit(t, cap)


class TextureBank:
    def __init__(self, assets, scratch):
        self.assets, self.scratch = Path(assets), Path(scratch)
        (self.assets / "texture").mkdir(parents=True, exist_ok=True)
        self.scratch.mkdir(parents=True, exist_ok=True)
        self.entries = {}

    def write(self, key, rgba, slot, slot_name, fade_to=None):
        if key in self.entries:
            return self.entries[key]["$guid"]
        fmt_name = SLOT_FORMAT[slot]
        guid = string_to_guid(f"texture/{PREFIX}/{key}.rpak")
        tag = f"0x{guid:016X}"
        h, w = rgba.shape[:2]
        w2, h2 = 1 << int(np.ceil(np.log2(max(4, w)))), 1 << int(np.ceil(np.log2(max(4, h))))
        if (w2, h2) != (w, h):
            rgba = np.dstack([resize(rgba[..., i], w2, h2) for i in range(rgba.shape[2])])
            h, w = h2, w2
        png = self.scratch / f"{tag}.png"
        Image.fromarray((np.clip(rgba, 0, 1) * 255).round().astype(np.uint8)).save(png)
        srgb = ["-srgbi"] if fmt_name.endswith("SRGB") else []
        conv = [TEXCONV, "-nologo", "-y", "-dx10", "-f", fmt_name, *srgb, "-bc", "q"]
        subprocess.run([*conv, "-m", "0", "-o", str(self.assets / "texture"), str(png)], capture_output=True, check=True)
        mips = total_mip_count(w, h)
        if fade_to is not None:
            self._fade_mips(tag, rgba, conv, mips, np.asarray(fade_to, np.float32))
        tail = synth_v10_tail(FORMATS[fmt_name], w, h, 1, mips, 0, 0, slot_name).hex()
        (self.assets / "texture" / f"{tag}.json").write_text(json.dumps(
            {"streamLayout": ["permanent"] * mips, "hdrTail": tail, "resourceFlags": "0x0", "usageFlags": "0x0"}, indent="\t"))
        self.entries[key] = {"_type": "txtr", "_path": f"texture/{tag}", "$guid": guid, "$hdrTail": tail,
                             "$strmMips": 0, "$optMips": 0}
        return guid

    def _fade_mips(self, tag, rgba, conv, mips, neutral):
        """Rebuild the mip chain fading to `neutral`, as Halo 2 detail bitmaps do: the layer
        is full strength up close and gone by the distance it would read as a repeat."""
        dds = self.assets / "texture" / f"{tag}.dds"
        head = dds.read_bytes()[:148]
        level, body = rgba.astype(np.float32), b""
        for k in range(mips):
            if k:
                h, w = max(1, level.shape[0] // 2), max(1, level.shape[1] // 2)
                level = np.dstack([resize(level[..., i], w, h) for i in range(level.shape[2])])
            f = min(1.0, max(0.0, (k - DETAIL_FADE_START) / (DETAIL_FADE_END - DETAIL_FADE_START)))
            out = level * (1.0 - f) + neutral * f
            lp = self.scratch / f"{tag}_m{k}.png"
            Image.fromarray((np.clip(out, 0, 1) * 255).round().astype(np.uint8)).save(lp)
            subprocess.run([*conv, "-m", "1", "-o", str(self.scratch), str(lp)], capture_output=True, check=True)
            body += (self.scratch / f"{tag}_m{k}.dds").read_bytes()[148:]
        dds.write_bytes(head + body)

    def flat(self, slot, slot_name, value):
        rgba = np.zeros((4, 4, 4), np.float32)
        rgba[...] = value
        return self.write(f"flat_{slot}_{'_'.join(str(round(v, 3)) for v in value)}", rgba, slot, slot_name)


def rgba_of(rgb, a=None):
    return np.dstack([rgb, np.ones(rgb.shape[:2]) if a is None else a])


def normal_rgba(n):
    return np.dstack([n[..., 0] * 0.5 + 0.5, n[..., 1] * 0.5 + 0.5, np.zeros(n.shape[:2]), np.ones(n.shape[:2])])


def grey(x):
    return np.dstack([x, x, x, np.ones_like(x)])


def set_uv(uber, block, sx, sy, tx=0.0, ty=0.0):
    o = UV_BLOCK[block]
    struct.pack_into("<6f", uber, o, sx, 0.0, 0.0, sy, tx, ty)


# PS register order of each template, index-aligned with its $textures.
PS_SLOTS = {
    "blend": ["albedoTexture", "albedo2Texture", "normalTexture", "normal2Texture", "glossTexture", "gloss2Texture",
              "aoTexture", "ao2Texture", "detailTexture", "detail2Texture", "detailNormalTexture",
              "detailNormal2Texture", "layerBlendTexture"],
    "detail": ["albedoTexture", "normalTexture", "glossTexture", "cavityTexture", "detailTexture", "detailNormalTexture"],
    "test": ["albedoTexture", "normalTexture", "glossTexture", "specTexture", "opacityMultiplyTexture"],
    "emissive": ["albedoTexture", "normalTexture", "glossTexture", "specTexture", "emissiveTexture"],
}

SURFACE = {"grass": "grass", "dirt": "dirt", "rock": "rock", "metal": "metal_solid", "wood": "wood"}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("dump", help="h2dump output with shaders.json")
    ap.add_argument("work", help="scratch + repak assets root")
    ap.add_argument("--project", required=True, help="r5ed project h2import.py wrote (h2_gameplay.json: centre, scale)")
    args = ap.parse_args()

    dump, work = Path(args.dump), Path(args.work).resolve()
    shaders = json.loads((dump / "shaders.json").read_text())
    gp = json.loads((Path(args.project) / "h2_gameplay.json").read_text())
    centre, S = np.array(gp["centre"]), gp["scale"]
    uv_fit = fit_uvs(dump)
    src = Sources(dump, work)
    assets = work / "assets"
    bank = TextureBank(assets, work / "texscratch")
    files, name_map = [], {}

    def bm(sh, slot):
        b = sh["bitmaps"][slot]
        return b["bitmap"].split("\\")[-1] if b["bitmap"] and b["local"] else None

    def const(sh, i):
        c = sh["constants"]
        return c[i] if i < len(c) else [1, 1, 0, 0]

    def emit(h2name, family, textures, uber_edit=None, surface="concrete"):
        def one(tpl_path, s21, colpass_guid):
            tpl = json.loads(tpl_path.with_suffix(".json").read_text())
            uber = bytearray(tpl_path.with_suffix(".uber").read_bytes())
            if uber_edit:
                uber_edit(uber)
            tex = dict(tpl["$textures"])
            for i, slot_name in enumerate(PS_SLOTS[family]):
                if textures.get(slot_name) is not None:
                    tex[str(i)] = f"0x{textures[slot_name]:016X}"
            doc = dict(tpl)
            doc["name"] = s21
            doc["$textures"] = tex
            doc["surfaceProp"] = surface
            if colpass_guid is not None:
                doc["$colpassMaterial"] = f"0x{colpass_guid:016X}"
            rel = f"material/{s21}_{tpl['shaderType']}"
            (assets / rel).parent.mkdir(parents=True, exist_ok=True)
            (assets / f"{rel}.json").write_text(json.dumps(doc, indent=1))
            (assets / f"{rel}.uber").write_bytes(bytes(uber))
            guid = string_to_guid(f"{rel}.rpak")
            files.append({"_type": "matl", "_path": f"{rel}.rpak", "$guid": guid})
            return guid

        # Every stock world material has a colour-pass twin drawing the same textures;
        # the template's own twin lives only in another map, so each gets its own.
        s21 = f"world/{PREFIX}/{h2name}"
        colpass = one(Path(COLPASS[family]), f"{s21}_colpass", None)
        one(Path(TEMPLATES[family]), s21, colpass)
        name_map[h2name] = s21
        print(f"{h2name:26s} -> {family:8s} {s21}")

    def T(key, img, slot, fade_to=None):
        family_slot = slot
        fmt_slot = {"albedoTexture": "_col", "albedo2Texture": "_col", "normalTexture": "_nml", "normal2Texture": "_nml",
                    "detailNormalTexture": "_nml", "detailNormal2Texture": "_nml", "glossTexture": "_gls",
                    "gloss2Texture": "_gls", "aoTexture": "_ao", "ao2Texture": "_ao", "detailTexture": "_det",
                    "detail2Texture": "_det", "layerBlendTexture": "_bm", "cavityTexture": "_cav",
                    "specTexture": "_spc", "opacityMultiplyTexture": "opacityMultiplyTexture",
                    "emissiveTexture": "emissiveTexture"}[slot]
        return bank.write(key, img, fmt_slot, family_slot, fade_to)

    flat_nml = lambda slot: bank.flat("_nml", slot, (0.5, 0.5, 0.0, 1.0))
    flat_det = lambda slot: bank.flat("_det", slot, (0.5, 0.5, 0.5, 1.0))
    white = lambda fmt, slot: bank.flat(fmt, slot, (1.0, 1.0, 1.0, 1.0))

    for h2name, sh in shaders.items():
        tmpl = (sh["template"] or "").split("\\")[-1]

        if tmpl == "tex_bump_detail_blend_detail":
            # Slots: 0 bump, 1 base (macro, alpha = blend), 2 detail A, 3 detail B.
            macro_name, det_a, det_b, bump = bm(sh, 1), bm(sh, 2), bm(sh, 3), bm(sh, 0)
            bump_s, base_s, da_s, db_s = const(sh, 0)[0], const(sh, 1)[0], const(sh, 2)[0], const(sh, 3)[0]
            macro = src.upscaled(macro_name, 2048)
            a = src.upscaled(det_a, 2048)
            b = src.upscaled(det_b, 2048)
            relief_a, relief_b = luminance(a[..., :3]), luminance(b[..., :3])
            na = src.normal(bump, (a.shape[1], a.shape[0]), highpass(relief_a, 3), 2.5) if bump else None
            nb = src.normal(bump, (b.shape[1], b.shape[0]), highpass(relief_b, 3), 2.5) if bump else None
            tex = {
                "albedoTexture": T(f"{h2name}_macro", rgba_of(macro[..., :3]), "albedoTexture"),
                "albedo2Texture": T(f"{h2name}_macro", rgba_of(macro[..., :3]), "albedo2Texture"),
                "normalTexture": flat_nml("normalTexture"),
                "normal2Texture": flat_nml("normal2Texture"),
                "glossTexture": T(f"{h2name}_gls_a", grey(gloss_map(a[..., :3], 0.16, 0.6)), "glossTexture"),
                "gloss2Texture": T(f"{h2name}_gls_b", grey(gloss_map(b[..., :3], 0.22, 0.6)), "gloss2Texture"),
                "aoTexture": T(f"{h2name}_ao_a", grey(cavity_map(a[..., :3])), "aoTexture"),
                "ao2Texture": T(f"{h2name}_ao_b", grey(cavity_map(b[..., :3])), "ao2Texture"),
                "detailTexture": T(f"{h2name}_det_a", rgba_of(a[..., :3]), "detailTexture"),
                "detail2Texture": T(f"{h2name}_det_b", rgba_of(b[..., :3]), "detail2Texture"),
                "detailNormalTexture": T(f"{h2name}_dnml_a", normal_rgba(na), "detailNormalTexture") if na is not None else flat_nml("detailNormalTexture"),
                "detailNormal2Texture": T(f"{h2name}_dnml_b", normal_rgba(nb), "detailNormal2Texture") if nb is not None else flat_nml("detailNormal2Texture"),
                "layerBlendTexture": T(f"{h2name}_blend", grey(macro[..., 3]), "layerBlendTexture"),
            }
            fitj = uv_fit[h2name]
            au, bu = fitj["u"][0], fitj["u"][2]
            av, bv = fitj["v"][1], fitj["v"][2]

            def world_uv(uber, block, k, au=au, bu=bu, av=av, bv=bv):
                # Halo UV0 is linear in world XY; the shader projects apex (x, -y).
                sx = au / S * k
                cu = (au * centre[0] + bu) * k
                sy = -av / S * k
                cv = (av * centre[1] + bv) * k
                set_uv(uber, block, sx, sy, cu / sx, cv / sy)

            def edit(uber, bs=base_s, da=da_s, db=db_s):
                world_uv(uber, 1, bs)
                world_uv(uber, 2, bs)
                # Macro alpha 0 is detail A (grass), 1 is detail B (dirt); layer 2 is t = 1.
                world_uv(uber, 3, da)
                world_uv(uber, 4, db)
                struct.pack_into("<f", uber, LAYER_BLEND_RAMP, 1.0)
                flags = struct.unpack_from("<I", uber, FEATURE_FLAGS)[0]
                struct.pack_into("<I", uber, FEATURE_FLAGS, flags | 4)
            emit(h2name, "blend", tex, edit, "dirt" if "water" in h2name else "grass")

        elif tmpl in ("tex_bump", "tex_bump_detail_overlay", "tex_bump_env_combined", "tex_bump_env",
                      "two_alpha_env_detail"):
            overlay = tmpl == "tex_bump_detail_overlay"
            if overlay:
                bump, base, det = bm(sh, 0), bm(sh, 1), bm(sh, 2)
                bump_s, base_s, det_s = const(sh, 0)[0], const(sh, 1)[0], const(sh, 2)[0]
            elif tmpl == "two_alpha_env_detail":
                bump, base, det = None, bm(sh, 1), None
                bump_s, base_s, det_s = 1, 1, 1
            else:
                bump, base, det = bm(sh, 0), bm(sh, 2), bm(sh, 3)
                bump_s, base_s, det_s = const(sh, 0)[0], const(sh, 1)[0], const(sh, 2)[0]
            if base is None:
                print(f"{h2name}: no local base map, skipped")
                continue
            metal = "env" in tmpl or any(k in h2name for k in ("panel", "plate", "metal", "graytech", "scratchy", "floor", "lights"))
            rockish = any(k in h2name for k in ("cliff", "rock", "slide", "washere"))
            col = src.upscaled(base, 2048 if rockish else 1024)
            rep = max(1, int(round(bump_s / max(base_s, 1e-6))))
            relief = highpass(luminance(col[..., :3]), 2)
            if bump:
                if rep > 1:
                    nb = src.normal(bump, (src.rgba(bump).shape[1], src.rgba(bump).shape[0]))
                    n = tile_to(nb, rep, 2048)
                    n /= np.linalg.norm(n, axis=2, keepdims=True) + 1e-6
                else:
                    n = src.normal(bump, (col.shape[1], col.shape[0]), relief, 1.5 if rockish else 0.8)
                nml = T(f"{h2name}_nml", normal_rgba(n), "normalTexture")
            else:
                nml = flat_nml("normalTexture")
            if det:
                d = src.upscaled(det, 1024)
                # Fade to the map's own average: a detail darker than 0.5 would otherwise brighten with distance.
                avg = d[..., :3].reshape(-1, 3).mean(0)
                det_tex = T(f"{det}_detf", rgba_of(d[..., :3]), "detailTexture", (*avg, 1.0))
                rel = highpass(luminance(d[..., :3]), 2)
                gy, gx = np.gradient(rel)
                dnm = np.dstack([-gx * 3.0, -gy * 3.0, np.ones_like(rel)])
                dnm /= np.linalg.norm(dnm, axis=2, keepdims=True)
                det_nml = T(f"{det}_dnmlf", normal_rgba(dnm), "detailNormalTexture", (0.5, 0.5, 0.0, 1.0))
                ds = const(sh, 2 if not overlay else 2)
                scale = (ds[0] / max(base_s, 1e-6), ds[1] / max(const(sh, 1)[1], 1e-6))
            else:
                det_tex, det_nml, scale = flat_det("detailTexture"), flat_nml("detailNormalTexture"), (1.0, 1.0)
            gloss_base = 0.55 if "env_combined" in tmpl else 0.4 if metal else 0.26
            tex = {
                "albedoTexture": T(f"{h2name}_col", rgba_of(col[..., :3]), "albedoTexture"),
                "normalTexture": nml,
                "glossTexture": T(f"{h2name}_gls", grey(gloss_map(col[..., :3], gloss_base, 0.8)), "glossTexture"),
                "cavityTexture": T(f"{h2name}_cav", grey(cavity_map(col[..., :3], 2.8 if rockish else 1.2)), "cavityTexture"),
                "detailTexture": det_tex,
                "detailNormalTexture": det_nml,
            }

            def edit(uber, k=scale):
                set_uv(uber, 1, k[0], k[1])
                flags = struct.unpack_from("<I", uber, FEATURE_FLAGS)[0]
                struct.pack_into("<I", uber, FEATURE_FLAGS, flags | 2)
            surface = ("rock" if rockish else "wood" if "bark" in h2name
                       else "solidmetal" if any(k in h2name for k in ("plate", "floor")) else "metalpanel" if metal else "concrete")
            emit(h2name, "detail", tex, edit, surface)

        elif tmpl in ("tex_alpha_test", "tex_bump_alpha_test_single_pass"):
            base = bm(sh, 0) if tmpl == "tex_alpha_test" else bm(sh, 2)
            bump = None if tmpl == "tex_alpha_test" else bm(sh, 0)
            col = src.upscaled(base, 1024)
            n = src.normal(bump, (col.shape[1], col.shape[0])) if bump else None
            tex = {  # alpha-tested
                "albedoTexture": T(f"{h2name}_col", rgba_of(col[..., :3]), "albedoTexture"),
                "normalTexture": T(f"{h2name}_nml", normal_rgba(n), "normalTexture") if n is not None else flat_nml("normalTexture"),
                "glossTexture": T(f"{h2name}_gls", grey(gloss_map(col[..., :3], 0.2, 0.5)), "glossTexture"),
                "specTexture": bank.flat("_spc", "specTexture", (0.16, 0.16, 0.16, 1.0)),
                "opacityMultiplyTexture": T(f"{h2name}_opa", grey((col[..., 3] > 0.5).astype(np.float32)), "opacityMultiplyTexture"),
            }
            emit(h2name, "test", tex, None, "foliage" if "bough" in h2name else "upholstery")

        elif tmpl == "one_alpha_env_illum":
            base = bm(sh, 3) or bm(sh, 2)
            if base is None:
                continue
            tint = np.array(const(sh, 0)[:3], np.float32)
            col = src.upscaled(base, 1024)
            lit = np.clip(col[..., :3] * tint, 0, 1)
            tex = {
                "albedoTexture": T(f"{h2name}_col", rgba_of(lit * 0.5), "albedoTexture"),
                "normalTexture": flat_nml("normalTexture"),
                "glossTexture": bank.flat("_gls", "glossTexture", (0.5, 0.5, 0.5, 1.0)),
                "specTexture": bank.flat("_spc", "specTexture", (0.1, 0.1, 0.1, 1.0)),
                "emissiveTexture": T(f"{h2name}_ilm", rgba_of(lit), "emissiveTexture"),
            }
            def edit(uber):
                # The sign template scrolls its emissive UV by c_uv1Translate * time.
                struct.pack_into("<2f", uber, UV_BLOCK[1] + 16, 0.0, 0.0)
            emit(h2name, "emissive", tex, edit, "metalpanel")
        else:
            print(f"{h2name}: template {tmpl} has no rebuild; left to the fallback map")

    files = list(bank.entries.values()) + files
    manifest = {"version": 8, "name": "mp_rr_bloodgulch_src", "assetsDir": str(assets).replace("\\", "/") + "/",
                "outputDir": str(work / "out").replace("\\", "/") + "/", "headerFlags": 32, "files": files}
    (work / "mp_rr_bloodgulch_src.json").write_text(json.dumps(manifest, indent=1))
    (work / "material_map.json").write_text(json.dumps(name_map, indent=1))
    print(f"{sum(1 for f in files if f['_type'] == 'matl')} materials, {len(bank.entries)} textures")


if __name__ == "__main__":
    main()
