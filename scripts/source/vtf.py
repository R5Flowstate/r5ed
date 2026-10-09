"""Valve texture (.vtf 7.0-7.5) -> RGBA8 numpy array of the largest mip.

Block-compressed data is wrapped in a DDS header and decoded by texconv (the
same tool the S21 encoder side uses); raw formats are unpacked with numpy.
Only frame 0 / face 0 / slice 0 is read; cubemaps return their first face
unless a face index is asked for.
"""

import struct
import subprocess
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

import sys  # noqa: E402
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import r5edenv  # noqa: E402

TEXCONV = r5edenv.get("R5ED_TEXCONV", required=False) or "texconv"

RGBA8888, ABGR8888, RGB888, BGR888, RGB565, I8, IA88, P8, A8 = range(9)
RGB888_BLUESCREEN, BGR888_BLUESCREEN, ARGB8888, BGRA8888 = 9, 10, 11, 12
DXT1, DXT3, DXT5, BGRX8888, BGR565, BGRX5551, BGRA4444, DXT1_ONEBITALPHA, BGRA5551 = range(13, 22)
UV88, UVWQ8888, RGBA16161616F, RGBA16161616 = 22, 23, 24, 25

BLOCK = {DXT1: 8, DXT1_ONEBITALPHA: 8, DXT3: 16, DXT5: 16}
BPP = {RGBA8888: 4, ABGR8888: 4, RGB888: 3, BGR888: 3, RGB565: 2, I8: 1, IA88: 2, P8: 1, A8: 1,
       RGB888_BLUESCREEN: 3, BGR888_BLUESCREEN: 3, ARGB8888: 4, BGRA8888: 4, BGRX8888: 4, BGR565: 2,
       BGRX5551: 2, BGRA4444: 2, BGRA5551: 2, UV88: 2, UVWQ8888: 4, RGBA16161616F: 8, RGBA16161616: 8}
FOURCC = {DXT1: b"DXT1", DXT1_ONEBITALPHA: b"DXT1", DXT3: b"DXT3", DXT5: b"DXT5"}

ENVMAP = 0x4000
RES_HIGHRES = 0x30


class VtfError(Exception):
    pass


def image_size(fmt, w, h):
    if fmt in BLOCK:
        return max(1, (w + 3) // 4) * max(1, (h + 3) // 4) * BLOCK[fmt]
    if fmt not in BPP:
        raise VtfError(f"unsupported vtf format {fmt}")
    return w * h * BPP[fmt]


def header(data):
    if data[:4] != b"VTF\0":
        raise VtfError("not a vtf")
    major, minor, header_size = struct.unpack_from("<III", data, 4)
    w, h, flags, frames, first_frame = struct.unpack_from("<HHIHH", data, 16)
    fmt, = struct.unpack_from("<i", data, 52)
    mips = data[56]
    lo_fmt, = struct.unpack_from("<i", data, 57)
    lo_w, lo_h = data[61], data[62]
    depth = struct.unpack_from("<H", data, 63)[0] if minor >= 2 else 1
    return {"major": major, "minor": minor, "header_size": header_size, "width": w, "height": h, "flags": flags,
            "frames": max(1, frames), "first_frame": first_frame, "format": fmt, "mips": max(1, mips),
            "lo_format": lo_fmt, "lo_w": lo_w, "lo_h": lo_h, "depth": max(1, depth)}


def faces_of(hd):
    if not hd["flags"] & ENVMAP:
        return 1
    # Before 7.5 an environment map carries a seventh, spheremap face.
    return 7 if hd["minor"] < 5 and hd["first_frame"] != 0xFFFF else 6


def high_res_offset(data, hd):
    if hd["minor"] >= 3:
        count, = struct.unpack_from("<I", data, 68)
        for i in range(count):
            tag, off = struct.unpack_from("<II", data, 80 + i * 8)
            if tag & 0xFFFFFF == RES_HIGHRES:
                return off
        raise VtfError("vtf has no high-res image resource")
    lo = image_size(hd["lo_format"], hd["lo_w"], hd["lo_h"]) if hd["lo_format"] >= 0 and hd["lo_w"] else 0
    return hd["header_size"] + lo


def top_mip_offset(data, hd, face=0):
    """Mips are stored smallest first; the largest one is the last block."""
    fmt, faces = hd["format"], faces_of(hd)
    off = high_res_offset(data, hd)
    for mip in range(hd["mips"] - 1, 0, -1):
        w, h, d = max(1, hd["width"] >> mip), max(1, hd["height"] >> mip), max(1, hd["depth"] >> mip)
        off += image_size(fmt, w, h) * d * faces * hd["frames"]
    return off + image_size(fmt, hd["width"], hd["height"]) * face


def _dds(fourcc, w, h, payload):
    flags = 0x1 | 0x2 | 0x4 | 0x1000 | 0x80000
    pf = struct.pack("<II4sIIIII", 32, 0x4, fourcc, 0, 0, 0, 0, 0)
    head = struct.pack("<4sIIIIIII44x", b"DDS ", 124, flags, h, w, len(payload), 0, 1) + pf
    head += struct.pack("<IIIII", 0x1000, 0, 0, 0, 0)
    return head + payload


def _decode_block(fmt, w, h, payload):
    with tempfile.TemporaryDirectory(dir=_scratch()) as tmp:
        src = Path(tmp) / "in.dds"
        src.write_bytes(_dds(FOURCC[fmt], w, h, payload))
        subprocess.run([TEXCONV, "-nologo", "-y", "-ft", "png", "-f", "R8G8B8A8_UNORM", "-o", tmp, str(src)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        return np.asarray(Image.open(Path(tmp) / "in.png").convert("RGBA")).copy()


_SCRATCH = None


def set_scratch(path):
    global _SCRATCH
    _SCRATCH = str(path)
    Path(path).mkdir(parents=True, exist_ok=True)


def _scratch():
    return _SCRATCH


def _decode_raw(fmt, w, h, raw):
    a = np.frombuffer(raw, np.uint8)
    full = np.full((h * w,), 255, np.uint8)
    if fmt in (RGBA8888, BGRA8888, ABGR8888, ARGB8888, BGRX8888, UVWQ8888):
        p = a.reshape(-1, 4)
        order = {RGBA8888: (0, 1, 2, 3), BGRA8888: (2, 1, 0, 3), ABGR8888: (3, 2, 1, 0), ARGB8888: (1, 2, 3, 0),
                 BGRX8888: (2, 1, 0, 3), UVWQ8888: (0, 1, 2, 3)}[fmt]
        out = p[:, order].copy()
        if fmt == BGRX8888:
            out[:, 3] = 255
    elif fmt in (RGB888, BGR888, RGB888_BLUESCREEN, BGR888_BLUESCREEN):
        p = a.reshape(-1, 3)
        rgb = p if fmt in (RGB888, RGB888_BLUESCREEN) else p[:, ::-1]
        out = np.column_stack([rgb, full])
    elif fmt in (I8, P8):
        out = np.column_stack([a, a, a, full])
    elif fmt == A8:
        z = np.zeros_like(a)
        out = np.column_stack([z, z, z, a])
    elif fmt == IA88:
        p = a.reshape(-1, 2)
        out = np.column_stack([p[:, 0], p[:, 0], p[:, 0], p[:, 1]])
    elif fmt == UV88:
        p = a.reshape(-1, 2)
        out = np.column_stack([p[:, 0], p[:, 1], np.zeros_like(p[:, 0]), full])
    elif fmt in (RGB565, BGR565, BGRX5551, BGRA5551, BGRA4444):
        v = np.frombuffer(raw, "<u2").astype(np.uint32)
        if fmt in (RGB565, BGR565):
            r, g, b = (v >> 11) & 31, (v >> 5) & 63, v & 31
            r, g, b = r * 255 // 31, g * 255 // 63, b * 255 // 31
            if fmt == RGB565:
                r, b = b, r
            al = np.full_like(r, 255)
        elif fmt == BGRA4444:
            b, g, r, al = v & 15, (v >> 4) & 15, (v >> 8) & 15, (v >> 12) & 15
            r, g, b, al = r * 17, g * 17, b * 17, al * 17
        else:
            b, g, r, al = v & 31, (v >> 5) & 31, (v >> 10) & 31, (v >> 15) & 1
            r, g, b, al = r * 255 // 31, g * 255 // 31, b * 255 // 31, al * 255
            if fmt == BGRX5551:
                al = np.full_like(r, 255)
        out = np.column_stack([r, g, b, al]).astype(np.uint8)
    elif fmt in (RGBA16161616F, RGBA16161616):
        p = np.frombuffer(raw, "<f2" if fmt == RGBA16161616F else "<u2").astype(np.float32).reshape(-1, 4)
        if fmt == RGBA16161616:
            p /= 65535.0
        rgb = p[:, :3] / (1.0 + p[:, :3])
        out = np.column_stack([np.clip(rgb ** (1 / 2.2), 0, 1) * 255, np.clip(p[:, 3], 0, 1) * 255]).astype(np.uint8)
    else:
        raise VtfError(f"unsupported vtf format {fmt}")
    return out.reshape(h, w, 4)


def read(path, face=0):
    """RGBA8 array (h, w, 4) and the header of `path` (a Path or a VpkFile)."""
    data = path.read_bytes() if hasattr(path, "read_bytes") else Path(path).read_bytes()
    hd = header(data)
    fmt, w, h = hd["format"], hd["width"], hd["height"]
    off = top_mip_offset(data, hd, face)
    size = image_size(fmt, w, h)
    payload = data[off:off + size]
    if len(payload) != size:
        raise VtfError(f"{path}: image data truncated ({len(payload)} of {size} bytes)")
    rgba = _decode_block(fmt, w, h, payload) if fmt in BLOCK else _decode_raw(fmt, w, h, payload)
    return rgba, hd


def size(path):
    blob = path.read_bytes() if hasattr(path, "read_bytes") else Path(path).read_bytes()
    hd = header(blob[:96])
    return hd["width"], hd["height"]
