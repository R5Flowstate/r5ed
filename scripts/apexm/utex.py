"""Apex Mobile cooked Texture2D / Texture2DArray -> RGBA images (mip 0)."""
import os, struct, numpy as np, texture2ddecoder
from upkg import Package, R
from uprops import read_props

BLOCK = {'PF_ASTC_4x4': (4, 4), 'PF_ASTC_6x6': (6, 6), 'PF_ASTC_8x8': (8, 8), 'PF_ASTC_10x10': (10, 10), 'PF_ASTC_12x12': (12, 12)}

def decode_rgba(fmt, data, w, h):
    if fmt in BLOCK:
        bw, bh = BLOCK[fmt]; out = texture2ddecoder.decode_astc(data, w, h, bw, bh)
    elif fmt == 'PF_ETC2_RGB': out = texture2ddecoder.decode_etc2(data, w, h)
    elif fmt == 'PF_ETC2_RGBA': out = texture2ddecoder.decode_etc2a8(data, w, h)
    elif fmt == 'PF_ETC1': out = texture2ddecoder.decode_etc1(data, w, h)
    elif fmt == 'PF_B8G8R8A8': out = bytes(data)
    elif fmt == 'PF_G8':
        g = np.frombuffer(data, np.uint8)[:w*h]; return np.stack([g, g, g, np.full_like(g, 255)], 1).reshape(h, w, 4)
    else: raise ValueError('pixel format ' + fmt)
    a = np.frombuffer(out, np.uint8).reshape(h, w, 4)
    return a[:, :, [2, 1, 0, 3]]  # BGRA -> RGBA

def block_bytes(fmt, w, h):
    if fmt in BLOCK:
        bw, bh = BLOCK[fmt]; return -(-w // bw) * -(-h // bh) * 16
    if fmt in ('PF_ETC2_RGB', 'PF_ETC1'): return -(-w // 4) * -(-h // 4) * 8
    if fmt == 'PF_ETC2_RGBA': return -(-w // 4) * -(-h // 4) * 16
    if fmt == 'PF_B8G8R8A8': return w * h * 4
    if fmt == 'PF_G8': return w * h
    raise ValueError(fmt)

def read_texture(path):
    """-> (format, [RGBA slice arrays]) for mip 0."""
    p = Package(path)
    e = [x for x in p.exports if x['class'] in ('Texture2D', 'Texture2DArray', 'TextureCube')][0]
    b = p.export_bytes(e); r = R(b); read_props(p, r, False)
    if r.i32(): r.raw(16)
    r.u8(); r.u8()                      # UTexture strip flags
    r.u8(); r.u8()                      # subclass strip flags
    r.u32()                             # bCooked
    fmt_name = p.fname(r)               # pixel format FName, then skip offset
    r.i64()
    w = r.i32(); h = r.i32(); packed = r.u32(); fmt = r.fstr()
    slices = packed & 0x3fffffff if e['class'] != 'Texture2D' else 1
    if e['class'] == 'Texture2D': slices = 1
    r.i32()                             # FirstMipToSerialize
    nmips = r.i32()
    # mip 0
    r.u32()                             # bCooked
    flags = r.u32(); count = r.i32(); disk = r.i32(); off = r.i64()
    if flags & 0x100 or (count and disk and flags & 0x1) :  # payload elsewhere
        pass
    if r.o + disk <= len(b) and not (flags & 0x100):
        data = b[r.o:r.o + count]
    else:
        with open(p.ubulk_path, 'rb') as fh:
            fh.seek(off + p.bulk_start if off < 0 else off); data = fh.read(count)
    mw, mh = w, h
    per = block_bytes(fmt, mw, mh)
    if per * slices != len(data):
        # the first mip may live in the .ubulk while the header says inline; fall back on size
        raise ValueError((path, fmt, w, h, slices, per, len(data), hex(flags)))
    return fmt, [decode_rgba(fmt, data[i*per:(i+1)*per], mw, mh) for i in range(slices)]
