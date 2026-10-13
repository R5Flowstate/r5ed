"""Halo 3 materials -> S21 world materials, rebuilt from the original bitmaps.

Reclaimer already resolves each H3 material into usage-tagged bitmaps
(materials.json from h3dump), so unlike h2mat no raw shader parsing is needed:

  blend (terrain macro + layer stack) -> composed to one albedo/normal at
      2048 (layers mixed by the blend-mask channels, detail baked in as
      base*detail*2 exactly like the H2 gamma overlay), strongest detail
      kept as the faded S21 detail layer. Flat terrain goes to the stock
      two-layer blend material, one per pair of layers (h3mesh splits the
      mesh by its two strongest layers); steep terrain cannot be world
      projected, so it keeps one material and the rock's detail.
  single diffuse (+detail/normal) -> detail template on UV0, detail scale
      relative to the diffuse tiling h3dump prescaled into the mesh UVs
  self_illum -> emissive sign template (uv1 scroll zeroed)
  clip / blended transparency -> alpha-tested template, opacity from the
      transparency bitmap

H3 has real normal maps, so normals are only resized and renormalised.
Gloss/cavity are derived unless the specular bitmap differs from diffuse.
"""

import argparse
import json
import os
import shutil
import struct
import subprocess
import sys
from io import BytesIO
from pathlib import Path

import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import r5edenv  # noqa: E402
from guid import string_to_guid  # noqa: E402
from synth_v10_tail import synth_v10_tail, total_mip_count  # noqa: E402

TEXCONV = r5edenv.get("R5ED_TEXCONV", required=False) or "texconv"
DETAIL_FADE_START, DETAIL_FADE_END = 1, 4
ESRGAN = os.environ.get("REALESRGAN", "realesrgan-ncnn-vulkan.exe")
PREFIX = "h3/valhalla"

# The S21 emissive shader draws the mask as it is stored, and an H3 self_illum
# bitmap tops out near 0.6 over roughly 5% of the texels: lamp glass reads as
# dark paint at 1:1. The lamp heads need to read as sources, not decals.
EMISSIVE_GAIN = 3.0

# H3 diffuse maps are dark by design (concrete ~0.02 linear): H3's brighter
# baked light and exposure carry the frame. Under Apex light they need a linear
# albedo gain to land in Apex's albedo range; each texture's gain stops where
# its brightest texels would clip.
ALBEDO_GAIN = 1.0
ALBEDO_CEILING = 0.9


def srgb_to_linear(c):
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def linear_to_srgb(c):
    return np.where(c <= 0.0031308, c * 12.92, 1.055 * np.power(np.maximum(c, 0), 1 / 2.4) - 0.055)


def albedo_gain(rgba):
    if ALBEDO_GAIN == 1.0:
        return rgba
    lin = srgb_to_linear(np.clip(rgba[..., :3], 0, 1))
    top = float(np.percentile(lin.max(axis=-1), 99.5))
    g = min(ALBEDO_GAIN, ALBEDO_CEILING / max(top, 1e-3))
    out = rgba.copy()
    out[..., :3] = linear_to_srgb(np.clip(lin * g, 0, 1))
    return out

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

TEMPLATE_DIR = Path(os.environ.get("H2_TEMPLATES", "templates"))
TEMPLATES = {f: TEMPLATE_DIR / f for f in ("blend", "detail", "test", "emissive")}
COLPASS = {f: TEMPLATE_DIR / f"{f}_colpass" for f in TEMPLATES}

UV_BLOCK = {1: 0x00, 2: 0x18, 3: 0x30, 4: 0x48, 5: 0x60}
FEATURE_FLAGS = 0x180
LAYER_BLEND_RAMP = 0x8C

CHANNEL = {"Red": 0, "Green": 1, "Blue": 2, "Alpha": 3, "Default": 0}
MACRO = 4096

GRAY = "gray_50_percent"
DEFAULT_DET = "default_detail"
DEFAULT_VEC = "default_vector"


def _bc4_signed(blk):
    e0 = blk[:, 0].view(np.int8).astype(np.float32)
    e1 = blk[:, 1].view(np.int8).astype(np.float32)
    bits = np.zeros(len(blk), np.uint64)
    for i in range(6):
        bits |= blk[:, 2 + i].astype(np.uint64) << np.uint64(8 * i)
    idx = np.stack([((bits >> np.uint64(3 * p)) & np.uint64(7)).astype(np.int64) for p in range(16)], 1)
    seven = np.zeros((len(blk), 8), np.float32)
    five = np.zeros((len(blk), 8), np.float32)
    seven[:, 0] = five[:, 0] = e0
    seven[:, 1] = five[:, 1] = e1
    for k in range(1, 7):
        seven[:, k + 1] = e0 + (e1 - e0) * k / 7.0
    for k in range(1, 5):
        five[:, k + 1] = e0 + (e1 - e0) * k / 5.0
    five[:, 6], five[:, 7] = -128.0, 127.0
    pal = np.where((e0 > e1)[:, None], seven, five)
    return (np.take_along_axis(pal, idx, 1) + 128.0) / 255.0


XBOX_DXN_SNORM = 6


def dds_xbox_normal(src):
    """Top mip of a Reclaimer 'XBOX' DDS in DXN_SNorm (format 6 of its Xbox
    enum): signed BC5 in linear block order after a 36-byte extension.
    texconv reads these files without error and returns noise, so they are
    decoded here, z rebuilt. None for anything else."""
    data = open(src, "rb").read()
    if data[:4] != b"DDS " or data[84:88] != b"XBOX" or struct.unpack_from("<I", data, 128)[0] != XBOX_DXN_SNORM:
        return None
    h, w = struct.unpack_from("<2I", data, 12)
    bw, bh = max(1, w // 4), max(1, h // 4)
    body = np.frombuffer(data, np.uint8, bw * bh * 16, 164).reshape(-1, 16)

    def img(v):
        return v.reshape(bh, bw, 4, 4).transpose(0, 2, 1, 3).reshape(bh * 4, bw * 4)[:h, :w]
    r, g = img(_bc4_signed(body[:, :8])), img(_bc4_signed(body[:, 8:]))
    b = (np.sqrt(np.clip(1 - (r * 2 - 1) ** 2 - (g * 2 - 1) ** 2, 0, 1)) + 1) / 2
    return np.dstack([r, g, b, np.ones_like(r)]).astype(np.float32)


class Sources:
    """Decoded and upscaled H3 bitmaps from the h3dump textures dir."""

    def __init__(self, dump, work):
        self.dump, self.work = Path(dump), Path(work)
        (self.work / "png").mkdir(parents=True, exist_ok=True)
        (self.work / "up").mkdir(parents=True, exist_ok=True)

    def _key(self, rel):
        return Path(rel).stem

    def rgba(self, rel):
        if rel is None:
            return None
        src = self.dump / rel
        png = self.work / "png" / (self._key(rel) + ".png")
        if not png.is_file():
            nb = dds_xbox_normal(src)
            if nb is not None:
                Image.fromarray((nb * 255).round().astype(np.uint8)).save(png)
        if not png.is_file():
            # Reclaimer writes only the top level but leaves the header's mip
            # count at the full chain; texconv then reads past the data. Convert
            # a copy whose header claims the one level that is there.
            one = self.work / "png" / (self._key(rel) + ".top.dds")
            data = bytearray(src.read_bytes())
            if data[:4] == b"DDS ":
                struct.pack_into("<I", data, 28, 1)
            one.write_bytes(bytes(data))
            r = subprocess.run([TEXCONV, "-nologo", "-y", "-ft", "png", "-f", "R8G8B8A8_UNORM",
                                "-o", str(png.parent), str(one)], capture_output=True)
            made = self.work / "png" / (self._key(rel) + ".top.png")
            if made.is_file():
                made.replace(png)
            one.unlink()
            if r.returncode or not png.is_file():
                err = (r.stderr or b"").decode(errors="replace")[-160:]
                try:
                    img = Image.open(src).convert("RGBA")
                    img.save(png)
                    print(f"  {src.name}: texconv failed ({err.strip()}), PIL fallback")
                except Exception:
                    print(f"  cannot decode {src.name}: texconv failed ({err.strip()})")
                    return None
        try:
            return np.asarray(Image.open(png).convert("RGBA")).astype(np.float32) / 255.0
        except Exception as e:
            print(f"  cannot read {png.name}: {e}")
            return None

    def upscaled(self, rel, cap):
        """RGB upscaled 4x (tileable), statistics matched to the source."""
        src = self.rgba(rel)
        if src is None:
            return None
        h, w = src.shape[:2]
        out = self.work / "up" / f"{self._key(rel)}_x4.png"
        if not out.is_file():
            tile = self.work / "up" / f"{self._key(rel)}_3x3.png"
            Image.fromarray((np.tile(src[..., :3], (3, 3, 1)) * 255).round().astype(np.uint8)).save(tile)
            # ESRGAN time explodes past 2K input; the result is fit to cap
            # afterwards anyway, so pre-fit the tile (3x3 of a 1K map = 3K).
            try:
                _t = np.asarray(Image.open(tile).convert("RGB")).astype(np.float32) / 255.0
                if max(_t.shape[:2]) > 2048:
                    s = 2048 / max(_t.shape[:2])
                    _t = np.dstack([resize(_t[..., i], int(_t.shape[1] * s), int(_t.shape[0] * s))
                                    for i in range(3)])
                    Image.fromarray((_t * 255).round().astype(np.uint8)).save(tile)
            except Exception as e:
                print(f"  tile pre-fit failed: {e}")
            big = self.work / "up" / f"{self._key(rel)}_3x3_x4.png"
            ok = False
            for attempt in range(3):
                try:
                    subprocess.run([ESRGAN, "-i", str(tile), "-o", str(big), "-n", "realesrgan-x4plus", "-s", "4"],
                                   capture_output=True, check=True)
                    ok = True
                    break
                except subprocess.CalledProcessError as e:
                    print(f"  ESRGAN attempt {attempt + 1}/3 failed ({e.returncode}), retrying")
            if not ok:
                # GPU contention (a running game starves Vulkan); Lanczos keeps
                # the pipeline moving, stat-matched to the source below.
                print(f"  ESRGAN failed 3x, Lanczos fallback for {tile.name}")
                w4, h4 = src.shape[1] * 4, src.shape[0] * 4
                Image.fromarray((np.clip(src[..., :3], 0, 1) * 255).round().astype(np.uint8)).resize(
                    (w4, h4), Image.Lanczos).save(out)
                tile.unlink(missing_ok=True)
                big.unlink(missing_ok=True)
            else:
                b = np.asarray(Image.open(big).convert("RGB"))
                # Centre third of whatever the tile actually was (it may have
                # been pre-fit below 3x source before upscaling).
                bh, bw = b.shape[:2]
                u = b[bh // 3:2 * bh // 3, bw // 3:2 * bw // 3]
                Image.fromarray(u).save(out)
                tile.unlink()
                big.unlink()
        rgb = np.asarray(Image.open(out).convert("RGB")).astype(np.float32) / 255.0
        s_mean, s_std = src[..., :3].reshape(-1, 3).mean(0), src[..., :3].reshape(-1, 3).std(0) + 1e-4
        o_mean, o_std = rgb.reshape(-1, 3).mean(0), rgb.reshape(-1, 3).std(0) + 1e-4
        rgb = np.clip((rgb - o_mean) / o_std * s_std + s_mean, 0, 1)
        alpha = resize(src[..., 3], rgb.shape[1], rgb.shape[0])
        return fit(np.dstack([rgb, alpha]), cap)

    def normal(self, rel, size):
        """A real H3 normal map resized to `size` (w, h) and renormalised."""
        px = self.rgba(rel)
        if px is None:
            return None
        n = px[..., :3] * 2 - 1
        n = np.dstack([resize(n[..., i], size[0], size[1]) for i in range(3)])
        n /= np.linalg.norm(n, axis=2, keepdims=True) + 1e-6
        return n


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


def area_resize(img, w, h):
    """Box-filtered resize, so a downsized tile averages its texels instead of
    picking every Nth one."""
    out = []
    for i in range(img.shape[2]):
        im = Image.fromarray(img[..., i].astype(np.float32), mode="F")
        out.append(np.asarray(im.resize((max(1, w), max(1, h)), Image.BOX if w < img.shape[1] else Image.BICUBIC)))
    return np.dstack(out)


def sample_tiled(img, tx, ty, out_w, out_h):
    """img repeated (tx, ty) times over an out_w x out_h 0-1 UV grid, each
    repeat filtered down to the pixels it gets."""
    rw, rh = max(1, round(out_w / tx)), max(1, round(out_h / ty))
    tile = area_resize(img, rw, rh)
    nx, ny = -(-out_w // rw), -(-out_h // rh)
    return np.tile(tile, (ny, nx, 1))[:out_h, :out_w]


def luminance(rgb):
    return rgb[..., 0] * 0.2126 + rgb[..., 1] * 0.7152 + rgb[..., 2] * 0.0722


def highpass(x, radius):
    from scipy.ndimage import gaussian_filter
    return x - gaussian_filter(x, radius, mode="wrap")


def gloss_map(rgb, base, spread):
    lum = luminance(rgb)
    return np.clip(base + (lum - lum.mean()) * spread, 0.02, 0.95)


def neutral_detail(rgb):
    """A grey detail map with mean 0.5: luminance only, so whichever way the
    shader combines it (gamma multiply, overlay, linear multiply) it cannot
    shift the hue the macro carries."""
    lum = luminance(rgb)
    return np.clip(lum * (0.5 / max(float(lum.mean()), 1e-3)), 0, 1)


# Gloss per terrain surface; H3 terrain is matte apart from wet rock and ice.
LAYER_GLOSS = (("snow", 0.32), ("grass", 0.14), ("sand", 0.2), ("dirt", 0.18), ("rock", 0.28), ("cliff", 0.28))


def layer_gloss(name):
    kl = (name or "").lower()
    return next((g for k, g in LAYER_GLOSS if k in kl), 0.22)


# Which layer's detail a non-projected terrain draws at runtime: the rock, so the
# snow stays a colour in the macro rather than a tiled texture.
ROCK_WORDS = ("rock", "cliff", "stone", "gravel", "dirt", "snow")


def rockiest(layers, per_layer, dets):
    best, rank = next(iter(dets)), -1
    for i in dets:
        entry = per_layer[i] or layers[i]["dif"]
        name = (short(entry) or "").lower()
        score = sum(1 for k in ROCK_WORDS if k in name) - (2 if "grass" in name or "leaf" in name else 0)
        if score > rank:
            best, rank = i, score
    return best


def tile_to(img, n, cap):
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
        rgba = np.asarray(rgba, np.float32)
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


PS_SLOTS = {
    "blend": ["albedoTexture", "albedo2Texture", "normalTexture", "normal2Texture", "glossTexture", "gloss2Texture",
              "aoTexture", "ao2Texture", "detailTexture", "detail2Texture", "detailNormalTexture",
              "detailNormal2Texture", "layerBlendTexture"],
    "detail": ["albedoTexture", "normalTexture", "glossTexture", "cavityTexture", "detailTexture", "detailNormalTexture"],
    "test": ["albedoTexture", "normalTexture", "glossTexture", "specTexture", "opacityMultiplyTexture"],
    "emissive": ["albedoTexture", "normalTexture", "glossTexture", "specTexture", "emissiveTexture"],
}


def surface_for(name):
    kl = name.lower()
    for key, prop in (("snow", "snow"), ("grass", "grass"), ("riversand", "sand"), ("sand", "sand")):
        if key in kl:
            return prop
    if any(k in kl for k in ("rock", "cliff", "distant")):
        return "rock"
    if any(k in kl for k in ("ground", "grass", "dirt", "sand", "lake")):
        return "dirt"
    if "glass" in kl:
        return "glass"
    if any(k in kl for k in ("bark", "leaf", "tree", "netting")):
        return "wood"
    if any(k in kl for k in ("plate", "floor")):
        return "solidmetal"
    if any(k in kl for k in ("panel", "metal", "trim", "grill", "equipment", "radio", "turbine",
                             "pelican", "longsword", "mast", "turret", "generator", "stool")):
        return "metalpanel"
    return "concrete"


def world_projected(dump, max_err=0.05, max_steep=0.1):
    """Materials whose UV0 is a linear, unrotated function of world XY on
    mostly flat geometry: the ones the world-projected blend material can draw.
    Value: Halo-unit fit {u: [x, y, c], v: [x, y, c]}."""
    parts = json.loads((Path(dump) / "parts.json").read_text())
    acc = {}
    for p in parts:
        raw = (Path(dump) / p["file"]).read_bytes()
        n = p["verts"]
        f = np.frombuffer(raw, "<f4", n * 8)
        acc.setdefault(p["material"], []).append((f[:n * 3].reshape(-1, 3), f[n * 3:n * 6].reshape(-1, 3), f[n * 6:n * 8].reshape(-1, 2)))
    out = {}
    for m, l in acc.items():
        P = np.concatenate([a for a, _, _ in l])
        N = np.concatenate([b for _, b, _ in l])
        U = np.concatenate([c for _, _, c in l])
        A = np.c_[P[:, 0], P[:, 1], np.ones(len(P))]
        cu = np.linalg.lstsq(A, U[:, 0], rcond=None)[0]
        cv = np.linalg.lstsq(A, U[:, 1], rcond=None)[0]
        err = max(np.abs(U[:, 0] - A @ cu).max(), np.abs(U[:, 1] - A @ cv).max())
        rotated = abs(cu[1]) > abs(cu[0]) * 0.01 or abs(cv[0]) > abs(cv[1]) * 0.01
        steep = float(np.mean(np.abs(N[:, 2]) < 0.5))
        if err < max_err and steep < max_steep and not rotated:
            out[m] = {"u": cu.tolist(), "v": cv.tolist()}
    return out


def tint(m, usage):
    """H3 colour multiplier for a usage (albedo, self_illum), 0..1 RGB."""
    import base64
    for t in m.get("tints", []):
        if t["usage"] == usage:
            return np.frombuffer(base64.b64decode(t["rgba"]), np.uint8)[:3].astype(np.float32) / 255.0
    return np.ones(3, np.float32)


def short(tex):
    return (tex["name"] or "").split("\\")[-1] if tex else None


def main():
    global PREFIX, ALBEDO_GAIN
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("dump", help="h3dump output with materials.json")
    ap.add_argument("work", help="scratch + repak assets root")
    ap.add_argument("--project", required=True, help="the h2import project (h2_gameplay.json: centre, scale)")
    ap.add_argument("--skip", default="",
                    help="comma-separated H3 material names to leave to the fallback map (water, drops)")
    ap.add_argument("--prefix", default=PREFIX,
                    help="asset namespace under world/ (per-map, so two H3 ports never share guids)")
    ap.add_argument("--stem", default="mp_rr_valhalla_src",
                    help="repak manifest name for the materials source pak")
    ap.add_argument("--albedo-gain", type=float, default=ALBEDO_GAIN,
                    help="linear albedo gain (H3 maps are dark by design; The Pit uses 4)")
    args = ap.parse_args()
    ALBEDO_GAIN = args.albedo_gain

    dump, work = Path(args.dump), Path(args.work).resolve()
    PREFIX = args.prefix
    meta = json.loads((Path(args.project) / "h2_gameplay.json").read_text())
    centre, S = meta["centre"], float(meta["scale"])
    projected = world_projected(dump)
    skip = set(filter(None, args.skip.split(",")))
    mats = json.loads((dump / "materials.json").read_text())
    src = Sources(dump, work)
    assets = work / "assets"
    bank = TextureBank(assets, work / "texscratch")
    files, name_map = [], {}

    def T(key, img, slot, fade_to=None):
        fmt_slot = {"albedoTexture": "_col", "normalTexture": "_nml", "detailNormalTexture": "_nml",
                    "glossTexture": "_gls", "aoTexture": "_ao", "detailTexture": "_det",
                    "cavityTexture": "_cav", "specTexture": "_spc",
                    "opacityMultiplyTexture": "opacityMultiplyTexture",
                    "emissiveTexture": "emissiveTexture", "layerBlendTexture": "_bm"}[slot]
        if slot == "albedoTexture":
            img = albedo_gain(img)
        return bank.write(key, img, fmt_slot, slot, fade_to)

    flat_nml = lambda slot: bank.flat("_nml", slot, (0.5, 0.5, 0.0, 1.0))
    flat_det = lambda slot: bank.flat("_det", slot, (0.5, 0.5, 0.5, 1.0))

    def emit(h3name, family, textures, uber_edit=None, surface="concrete"):
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

        s21 = f"world/{PREFIX}/{h3name}"
        colpass = one(Path(COLPASS[family]), f"{s21}_colpass", None)
        one(Path(TEMPLATES[family]), s21, colpass)
        name_map[h3name] = s21
        print(f"{h3name:30s} -> {family:8s} {s21}", flush=True)

    layer_split, uv_factor = {}, {}

    def usable(t, neutral):
        return t is not None and t.get("file") is not None and short(t) != neutral

    def detail_pair(h3name, det, ndet, base, cap=1024, neutral=False):
        """Faded S21 detail + detailNormal layer at uv1, tiling relative to the
        `base` tiling the mesh UV0 carries. A real normal_detail map is used
        when it repeats with the detail; otherwise one is built from it."""
        if usable(det, DEFAULT_DET):
            d = src.upscaled(det["file"], cap)
        else:
            d = None
        if d is None and usable(ndet, DEFAULT_VEC):
            n = src.normal(ndet["file"], (1024, 1024))
            if n is not None:
                t = ndet["tiling"]
                return (flat_det("detailTexture"),
                        T(f"{short(ndet)}_dnmlr", normal_rgba(n), "detailNormalTexture", (0.5, 0.5, 0.0, 1.0)),
                        (t[0] / base[0], t[1] / base[1]))
        if d is None:
            return flat_det("detailTexture"), flat_nml("detailNormalTexture"), (1.0, 1.0)
        avg = d[..., :3].reshape(-1, 3).mean(0)
        if neutral:
            # Colour lives in the macro; the detail only adds its pattern.
            det_tex = T(f"{short(det)}_detg", grey(neutral_detail(d[..., :3])), "detailTexture", (0.5, 0.5, 0.5, 1.0))
        else:
            det_tex = T(f"{short(det)}_detf", rgba_of(d[..., :3]), "detailTexture", (*avg, 1.0))
        n = None
        if usable(ndet, DEFAULT_VEC) and list(ndet["tiling"]) == list(det["tiling"]):
            n = src.normal(ndet["file"], (d.shape[1], d.shape[0]))
        if n is not None:
            det_nml = T(f"{short(ndet)}_dnmlr", normal_rgba(n), "detailNormalTexture", (0.5, 0.5, 0.0, 1.0))
        else:
            rel = highpass(luminance(d[..., :3]), 2)
            gy, gx = np.gradient(rel)
            dnm = np.dstack([-gx * 3.0, -gy * 3.0, np.ones_like(rel)])
            dnm /= np.linalg.norm(dnm, axis=2, keepdims=True)
            det_nml = T(f"{short(det)}_dnmlf", normal_rgba(dnm), "detailNormalTexture", (0.5, 0.5, 0.0, 1.0))
        t = det["tiling"]
        return det_tex, det_nml, (t[0] / base[0], t[1] / base[1])

    def texs_by_usage(m):
        d = {}
        for t in m["textures"]:
            d.setdefault(t["usage"], []).append(t)
        return d

    def detail_edit(k):
        def edit(uber):
            set_uv(uber, 1, k[0], k[1])
            flags = struct.unpack_from("<I", uber, FEATURE_FLAGS)[0]
            struct.pack_into("<I", uber, FEATURE_FLAGS, flags | 2)
        return edit

    def pow2(x):
        return 1 << max(2, int(round(np.log2(max(4, x)))))

    def terrain(h3name, m, by, rockish, projected=None):
        """H3 terrain: a macro albedo/normal from the layer stack mixed by the
        blend mask's own channels, plus one material per layer whose runtime
        detail is that layer's full-resolution texture. The mesh is split by
        dominant layer (h3mesh), so the ground close to the player shows the
        real tiled grass/dirt/rock/snow instead of the macro."""
        mask_rel = by["blend"][0]["file"]
        mask = src.rgba(mask_rel)
        layers, cur = [], None
        for t in m["textures"]:
            if t["usage"] == "diffuse":
                cur = {"dif": t, "det": None, "nml": None, "ch": CHANNEL.get(t["channel"], 0)}
                layers.append(cur)
            elif cur is not None and t["usage"] in ("diffuse_detail", "normal") and t["channel"] == cur["dif"]["channel"]:
                key = "det" if t["usage"] == "diffuse_detail" else "nml"
                if cur[key] is None:
                    cur[key] = t
        ratio = float(np.median([l["dif"]["tiling"][1] / max(l["dif"]["tiling"][0], 1e-6) for l in layers]))
        W, H = (pow2(MACRO / ratio), MACRO) if ratio >= 1 else (MACRO, pow2(MACRO * ratio))
        if mask is not None:
            ch = np.dstack([resize(mask[..., l["ch"]], W, H) for l in layers])
            s = ch.sum(-1, keepdims=True)
            w = np.where(s > 1e-4, ch / np.maximum(s, 1e-4), 0.0).astype(np.float32)
            w[..., 0] += (s[..., 0] <= 1e-4)
            del ch, s
        else:
            print(f"{h3name}: no blend mask, equal weights")
            w = np.full((H, W, len(layers)), 1.0 / len(layers), np.float32)
        alb = np.zeros((H, W, 3), np.float32)
        nrm = np.zeros((H, W, 3), np.float32)
        gls = np.zeros((H, W), np.float32)
        per_layer, ok = [], []
        for i, lay in enumerate(layers):
            dt = lay["dif"]["tiling"]
            det_src = lay["det"] if usable(lay["det"], DEFAULT_DET) else None
            if short(lay["dif"]) == GRAY:
                base = np.full((H, W, 3), 0.5, np.float32)
            else:
                di = src.rgba(lay["dif"]["file"])
                if di is None:
                    per_layer.append(None)
                    ok.append(False)
                    continue
                if det_src is None and min(dt) >= 4:
                    # A densely tiled diffuse with no detail map is itself the
                    # runtime detail; the macro keeps only its mean colour.
                    mean = di[..., :3].reshape(-1, 3).mean(0)
                    base = np.broadcast_to(mean, (H, W, 3)).astype(np.float32)
                    det_src = lay["dif"]
                else:
                    base = sample_tiled(di[..., :3], dt[0], dt[1], W, H)
            if det_src is not None and det_src is not lay["dif"]:
                # H3 layer colour is base * detail * 2; the macro carries the
                # detail's mean so every layer pair agrees on colour.
                dd = src.rgba(det_src["file"])
                if dd is not None:
                    base = np.clip(base * 2.0 * dd[..., :3].reshape(-1, 3).mean(0), 0, 1)
            nn = np.zeros((H, W, 3), np.float32)
            nn[..., 2] = 1.0
            if usable(lay["nml"], DEFAULT_VEC):
                ni = src.rgba(lay["nml"]["file"])
                if ni is not None:
                    nt = lay["nml"]["tiling"]
                    nn = sample_tiled(ni[..., :3], nt[0], nt[1], W, H) * 2 - 1
            wi = w[..., i:i + 1]
            alb += wi * base
            nrm += wi * nn
            gls += wi[..., 0] * layer_gloss(short(det_src) or short(lay["dif"]))
            per_layer.append(det_src)
            ok.append(True)
        nrm /= np.linalg.norm(nrm, axis=2, keepdims=True) + 1e-6
        macro = {
            "albedoTexture": T(f"{h3name}_col", rgba_of(np.clip(alb, 0, 1)), "albedoTexture"),
            "normalTexture": T(f"{h3name}_nml", normal_rgba(nrm), "normalTexture"),
            "glossTexture": T(f"{h3name}_gls", grey(np.clip(fit(gls[..., None], 2048)[..., 0], 0.02, 0.6)), "glossTexture"),
            # H3 materials carry no occlusion map; one built from albedo darkens
            # every dark texel a second time.
            "cavityTexture": bank.flat("_cav", "cavityTexture", (1.0, 1.0, 1.0, 1.0)),
        }
        dets = {}
        for i, lay in enumerate(layers):
            if not ok[i]:
                continue
            det_src = per_layer[i]
            if det_src is not None and det_src is lay["dif"]:
                d = src.upscaled(det_src["file"], 1024)
                det_tex = T(f"{short(det_src)}_detg", grey(neutral_detail(d[..., :3])), "detailTexture", (0.5, 0.5, 0.5, 1.0))
                rel = highpass(luminance(d[..., :3]), 2)
                gy, gx = np.gradient(rel)
                dnm = np.dstack([-gx * 3.0, -gy * 3.0, np.ones_like(rel)])
                dnm /= np.linalg.norm(dnm, axis=2, keepdims=True)
                det_nml = T(f"{short(det_src)}_dnmlf", normal_rgba(dnm), "detailNormalTexture", (0.5, 0.5, 0.0, 1.0))
                if usable(lay["nml"], DEFAULT_VEC) and list(lay["nml"]["tiling"]) == list(det_src["tiling"]):
                    n = src.normal(lay["nml"]["file"], (d.shape[1], d.shape[0]))
                    if n is not None:
                        det_nml = T(f"{short(lay['nml'])}_dnmlr", normal_rgba(n), "detailNormalTexture", (0.5, 0.5, 0.0, 1.0))
                t = det_src["tiling"]
                scale = (t[0], t[1])
            else:
                det_tex, det_nml, scale = detail_pair(h3name, det_src, lay["nml"], [1.0, 1.0], neutral=True)
            dets[i] = (det_tex, det_nml, scale, surface_for(short(det_src) or short(lay["dif"]) or h3name))
        mask_png = str(src.work / "png" / (src._key(mask_rel) + ".png")) if mask is not None else None
        if projected is not None and mask is not None and len(dets) >= 2:
            blend_pairs(h3name, layers, dets, mask, mask_png, macro, projected)
            return
        # Steep terrain: the blend material cannot draw it (it world-projects), and
        # splitting it by dominant layer lands snow as hard whole triangles. One
        # material instead -- the macro already carries the layer mix, so the snow
        # line stays soft and only the rock's detail tiles at full resolution.
        det_tex, det_nml, scale, surf = dets[rockiest(layers, per_layer, dets)]
        emit(h3name, "detail", {**macro, "detailTexture": det_tex, "detailNormalTexture": det_nml},
             detail_edit(scale), surf)

    def blend_pairs(h3name, layers, dets, mask, mask_png, macro, fitj):
        """The Blood Gulch ground route: the stock two-layer blend material,
        world projected, one material per pair of layers. Each pair draws its two
        layers' details tiled at full resolution over the macro colour, mixed by
        that pair's share of the H3 blend mask; h3mesh sends each triangle to
        the pair of its two strongest layers."""
        au, bu = fitj["u"][0], fitj["u"][2]
        av, bv = fitj["v"][1], fitj["v"][2]

        def world_uv(uber, block, ku, kv):
            # H3 terrain UV is linear in world XY; the shader projects apex (x, -y).
            sx, sy = au / S * ku, -av / S * kv
            cu, cv = (au * centre[0] + bu) * ku, (av * centre[1] + bv) * kv
            set_uv(uber, block, sx, sy, cu / sx, cv / sy)

        h, w = mask.shape[:2]
        MW, MH = pow2(w * 2), pow2(h * 2)
        idx = sorted(dets)
        names, pairs = [], []
        for a in range(len(idx)):
            for b in range(a + 1, len(idx)):
                i, j = idx[a], idx[b]
                wi = resize(mask[..., layers[i]["ch"]], MW, MH)
                wj = resize(mask[..., layers[j]["ch"]], MW, MH)
                t = np.clip(wj / np.maximum(wi + wj, 1e-4), 0, 1)
                t = np.where(wi + wj > 1e-4, t, 0.0)
                ci, cj = "rgba"[layers[i]["ch"]], "rgba"[layers[j]["ch"]]
                ti, tj = dets[i], dets[j]
                tex = {
                    "albedoTexture": macro["albedoTexture"], "albedo2Texture": macro["albedoTexture"],
                    "normalTexture": macro["normalTexture"], "normal2Texture": macro["normalTexture"],
                    "glossTexture": macro["glossTexture"], "gloss2Texture": macro["glossTexture"],
                    "aoTexture": macro["cavityTexture"], "ao2Texture": macro["cavityTexture"],
                    "detailTexture": ti[0], "detail2Texture": tj[0],
                    "detailNormalTexture": ti[1], "detailNormal2Texture": tj[1],
                    "layerBlendTexture": T(f"{h3name}_mask_{ci}{cj}", grey(t), "layerBlendTexture"),
                }

                def edit(uber, si=ti[2], sj=tj[2]):
                    world_uv(uber, 1, 1.0, 1.0)
                    world_uv(uber, 2, 1.0, 1.0)
                    world_uv(uber, 3, si[0], si[1])
                    world_uv(uber, 4, sj[0], sj[1])
                    struct.pack_into("<f", uber, LAYER_BLEND_RAMP, 1.0)
                    flags = struct.unpack_from("<I", uber, FEATURE_FLAGS)[0]
                    struct.pack_into("<I", uber, FEATURE_FLAGS, flags | 4)
                name = f"{h3name}__{ci}{cj}"
                emit(name, "blend", tex, edit, ti[3])
                names.append(name_map[name])
                pairs.append([layers[i]["ch"], layers[j]["ch"]])
        layer_split[h3name] = {"mask": mask_png, "pairs": pairs, "materials": names}
        name_map[h3name] = names[0]

    for h3name in sorted(mats):
        if h3name in skip:
            continue
        m = mats[h3name]
        by = texs_by_usage(m)
        alpha = m.get("alpha") or "opaque"
        rockish = any(k in h3name for k in ("rock", "cliff", "distant", "ground"))
        cap = 2048 if rockish else 1024

        if "blend" in by:
            terrain(h3name, m, by, rockish, projected.get(h3name))

        elif "self_illum" in by:
            ill_entry = by["self_illum"][0]
            illum = src.upscaled(ill_entry["file"], cap)
            if illum is None:
                print(f"{h3name}: illum undecodable, skipped")
                continue
            dif = (by.get("diffuse") or [None])[0]
            nml_entry = (by.get("normal") or [None])[0]
            col = src.upscaled(dif["file"], cap) if usable(dif, GRAY) else None
            if col is None:
                # Illum-only (additive glow): the illum map is the whole look.
                lit = np.clip(illum[..., :3] * tint(m, "self_illum") * EMISSIVE_GAIN, 0, 1)
                alb, ntex, gls = np.clip(lit * 0.5, 0, 1), flat_nml("normalTexture"), None
            else:
                # Every map shares UV0, which carries the diffuse tiling; the
                # coarsest repeat becomes UV0 and the others tile inside it.
                D = [max(float(v), 1e-6) for v in dif["tiling"]]
                maps = [dif, ill_entry] + ([nml_entry] if usable(nml_entry, DEFAULT_VEC) else [])
                B = [min(float(e["tiling"][i]) for e in maps) for i in range(2)]
                if B != D:
                    uv_factor[h3name] = [B[0] / D[0], B[1] / D[1]]
                rep = lambda e: [max(1, round(float(e["tiling"][i]) / B[i])) for i in range(2)]
                kd = rep(dif)
                W = pow2(min(2048, col.shape[1] * kd[0]))
                H = pow2(min(2048, col.shape[0] * kd[1]))
                col = sample_tiled(col, kd[0], kd[1], W, H)
                alb = np.clip(col[..., :3] * tint(m, "albedo"), 0, 1)
                ki = rep(ill_entry)
                illum = sample_tiled(illum, ki[0], ki[1], W, H)
                lit = np.clip(illum[..., :3] * tint(m, "self_illum") * EMISSIVE_GAIN, 0, 1)
                ntex = flat_nml("normalTexture")
                if usable(nml_entry, DEFAULT_VEC):
                    kn = rep(nml_entry)
                    n = src.normal(nml_entry["file"], (max(4, W // kn[0]), max(4, H // kn[1])))
                    if n is not None:
                        n = np.tile(n, (kn[1], kn[0], 1))
                        n = np.dstack([resize(n[..., i], W, H) for i in range(3)])
                        n /= np.linalg.norm(n, axis=2, keepdims=True) + 1e-6
                        ntex = T(f"{h3name}_nml", normal_rgba(n), "normalTexture")
                gls = T(f"{h3name}_gls", grey(gloss_map(alb, 0.4, 0.8)), "glossTexture")
            tex = {
                "albedoTexture": T(f"{h3name}_col", rgba_of(alb), "albedoTexture"),
                "normalTexture": ntex,
                "glossTexture": gls or bank.flat("_gls", "glossTexture", (0.5, 0.5, 0.5, 1.0)),
                "specTexture": bank.flat("_spc", "specTexture", (0.1, 0.1, 0.1, 1.0)),
                "emissiveTexture": T(f"{h3name}_ilm", rgba_of(lit), "emissiveTexture"),
            }

            def edit(uber):
                struct.pack_into("<2f", uber, UV_BLOCK[1] + 16, 0.0, 0.0)
            emit(h3name, "emissive", tex, edit, "metalpanel")

        elif alpha in ("clip", "blend", "pre_multiplied"):
            dif = (by.get("diffuse") or [None])[0]
            if dif is None or dif["file"] is None:
                print(f"{h3name}: no diffuse, skipped")
                continue
            col = src.upscaled(dif["file"], cap)
            if col is None:
                print(f"{h3name}: diffuse undecodable, skipped")
                continue
            nml_entry = (by.get("normal") or [None])[0]
            if usable(nml_entry, DEFAULT_VEC) and list(nml_entry["tiling"]) == list(dif["tiling"]):
                n = src.normal(nml_entry["file"], (col.shape[1], col.shape[0]))
                ntex = T(f"{h3name}_nml", normal_rgba(n), "normalTexture") if n is not None else flat_nml("normalTexture")
            else:
                ntex = flat_nml("normalTexture")
            tra = (by.get("transparency") or [None])[0]
            opa = None
            if tra is not None and tra["file"] is not None:
                ti = src.rgba(tra["file"])
                if ti is not None:
                    a = resize(ti[..., 3], col.shape[1], col.shape[0])
                    if a.std() < 0.02:
                        a = (luminance(fit(ti[..., :3], 64)) > 0.5).astype(np.float32)
                        a = resize(a, col.shape[1], col.shape[0])
                    opa = a
            if opa is None:
                opa = (col[..., 3] > 0.5).astype(np.float32)
            tex = {
                "albedoTexture": T(f"{h3name}_col", rgba_of(np.clip(col[..., :3] * tint(m, "albedo"), 0, 1)), "albedoTexture"),
                "normalTexture": ntex,
                "glossTexture": T(f"{h3name}_gls", grey(gloss_map(col[..., :3], 0.2, 0.5)), "glossTexture"),
                "specTexture": bank.flat("_spc", "specTexture", (0.16, 0.16, 0.16, 1.0)),
                "opacityMultiplyTexture": T(f"{h3name}_opa", grey(opa), "opacityMultiplyTexture"),
            }
            surf = "wood" if "leaf" in h3name else ("glass" if "glass" in h3name else surface_for(h3name))
            emit(h3name, "test", tex, None, surf)

        else:
            dif = (by.get("diffuse") or [None])[0]
            nml_entry = (by.get("normal") or [None])[0]
            if dif is None or dif["file"] is None:
                flat_tint = next((t for t in m.get("tints", []) if t["usage"] == "albedo"), None)
                if flat_tint is None:
                    print(f"{h3name}: no diffuse, skipped")
                    continue
                # The albedo tint is applied with the rest below.
                rgb = np.ones(3, np.float32)
                col = np.dstack([np.broadcast_to(rgb, (64, 64, 3)), np.ones((64, 64))]).astype(np.float32)
                dif = {"tiling": nml_entry["tiling"] if usable(nml_entry, DEFAULT_VEC) else [1, 1], "file": None}
                if usable(nml_entry, DEFAULT_VEC):
                    col = np.dstack([np.broadcast_to(rgb, (1024, 1024, 3)), np.ones((1024, 1024))]).astype(np.float32)
            else:
                col = src.upscaled(dif["file"], cap)
                if col is None:
                    print(f"{h3name}: diffuse undecodable, skipped")
                    continue
            # UV0 carries the diffuse tiling (h3dump prescale). A normal map that
            # repeats less often than the diffuse needs UV0 at its own, coarser
            # tiling (h3mesh rescales the mesh UVs), the diffuse repeating
            # inside the texture; one that repeats more often is tiled into it.
            D = [max(float(v), 1e-6) for v in dif["tiling"]]
            Nt = [float(v) for v in nml_entry["tiling"]] if usable(nml_entry, DEFAULT_VEC) else None
            B = list(D)
            if Nt is not None and (Nt[0] < D[0] * 0.99 or Nt[1] < D[1] * 0.99):
                k = [max(1, round(D[i] / Nt[i])) for i in range(2)]
                B = [D[i] / k[i] for i in range(2)]
                size = [min(4096 if rockish else 2048, col.shape[1] * k[0]), min(4096 if rockish else 2048, col.shape[0] * k[1])]
                size = [pow2(s) for s in size]
                col = sample_tiled(col, k[0], k[1], size[0], size[1])
                uv_factor[h3name] = [B[0] / D[0], B[1] / D[1]]
            ntex = flat_nml("normalTexture")
            if Nt is not None:
                r = [max(1, round(Nt[i] / B[i])) for i in range(2)]
                n = src.normal(nml_entry["file"], (max(4, col.shape[1] // r[0]), max(4, col.shape[0] // r[1])))
                if n is not None:
                    n = np.tile(n, (r[1], r[0], 1))
                    n = np.dstack([resize(n[..., i], col.shape[1], col.shape[0]) for i in range(3)])
                    n /= np.linalg.norm(n, axis=2, keepdims=True) + 1e-6
                    ntex = T(f"{h3name}_nml", normal_rgba(n), "normalTexture")
            spec_entry = (by.get("specular") or [None])[0]
            if spec_entry is not None and spec_entry["file"] is not None and dif.get("file") and short(spec_entry) != short(dif):
                si = src.rgba(spec_entry["file"])
                gloss_in = luminance(fit(si[..., :3], 256)) if si is not None else luminance(col[..., :3])
            else:
                gloss_in = col[..., :3]
            metal = any(k in h3name for k in ("panel", "metal", "trim", "equipment", "radio", "grill",
                                             "turbine", "pelican", "mast", "turret", "generator"))
            det_entry = (by.get("diffuse_detail") or [None])[0]
            ndet_entry = (by.get("normal_detail") or [None])[0]
            det_tex, det_nml, scale = detail_pair(h3name, det_entry, ndet_entry, B)
            tex = {
                "albedoTexture": T(f"{h3name}_col", rgba_of(np.clip(col[..., :3] * tint(m, "albedo"), 0, 1)), "albedoTexture"),
                "normalTexture": ntex,
                "glossTexture": T(f"{h3name}_gls", grey(gloss_map(gloss_in, 0.4 if metal else 0.26, 0.8)), "glossTexture"),
                "cavityTexture": bank.flat("_cav", "cavityTexture", (1.0, 1.0, 1.0, 1.0)),
                "detailTexture": det_tex,
                "detailNormalTexture": det_nml,
            }
            emit(h3name, "detail", tex, detail_edit(scale), surface_for(h3name))

    (work / "layer_split.json").write_text(json.dumps(layer_split, indent=1))
    (work / "uv_factor.json").write_text(json.dumps(uv_factor, indent=1))

    files = list(bank.entries.values()) + files
    manifest = {"version": 8, "name": args.stem, "assetsDir": str(assets).replace("\\", "/") + "/",
                "outputDir": str(work / "out").replace("\\", "/") + "/", "headerFlags": 32, "files": files}
    (work / f"{args.stem}.json").write_text(json.dumps(manifest, indent=1))
    (work / "material_map.json").write_text(json.dumps(name_map, indent=1))
    print(f"{sum(1 for f in files if f['_type'] == 'matl')} materials, {len(bank.entries)} textures")


if __name__ == "__main__":
    main()
