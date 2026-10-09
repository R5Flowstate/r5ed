"""The v10 texture header tail [0x14, 0x38) for a texture Repak packs from a DDS.

Streaming counts come from the texture's mip layout, the semantic (texSemantic) from
the material slot that binds it, and packedStreamSize from the raw mip sizes.
"""
import glob
import json
import os
import struct


# (slot_name, eTextureFormat) -> eTextureType
# Measured over the stock common.rpak materials and their v10 texture headers.
SLOT_TYPE_RULE = {
    ('albedoTexture',                1):  4,  ('albedoTexture',                6): 45,
    ('albedoTexture',               13): 45,  ('albedoTexture',               32): 45,
    ('albedoTexture',               53): 45,
    ('albedo2Texture',               1):  4,
    ('albedoMultiplyTexture',        1): 45,  ('albedoMultiplyTexture',       13): 45,
    ('albedoMultiplyTexture',       32): 45,  ('albedoMultiplyTexture',       53): 45,
    ('normalTexture',                0): 12,  ('normalTexture',                8): 12,
    ('normalTexture',               44): 12,
    ('normal2Texture',               8): 12,
    ('detailNormalTexture',          8): 12,
    ('glossTexture',                 6): 17,
    ('gloss2Texture',                6): 17,
    ('specTexture',                  1): 13,
    ('spec2Texture',                 1): 13,
    ('aoTexture',                    6): 25,
    ('ao2Texture',                   6): 25,
    ('cavityTexture',                6): 26,
    ('emissiveTexture',              1): 19,  ('emissiveTexture',             12): 23,
    ('emissiveTexture',             13): 45,  ('emissiveTexture',             32): 19,
    ('emissive2Texture',             1): 19,
    ('emissiveMultiplyTexture',      1): 19,  ('emissiveMultiplyTexture',      6): 22,
    ('emissiveMultiplyTexture',     12): 24,  ('emissiveMultiplyTexture',     13): 19,
    ('emissiveMultiplyTexture',     32): 19,
    ('opacityMultiplyTexture',       0):  6,  ('opacityMultiplyTexture',       1): 19,
    ('opacityMultiplyTexture',       6):  5,  ('opacityMultiplyTexture',       8):  8,
    ('opacityMultiplyTexture',      13): 45,  ('opacityMultiplyTexture',      53):  5,
    ('detailTexture',                0):  6,  ('detailTexture',                1):  4,
    ('colorRampTexture',             1): 45,  ('colorRampTexture',            13): 45,
    ('colorRampTexture',            32): 45,
    ('iridescenceRampTexture',       1):  4,
    ('layerBlendTexture',            6): 37,
    ('anisoSpecDirTexture',          6): 14,  ('anisoSpecDirTexture',          8): 16,
    ('scatterThicknessTexture',      0): 28,  ('scatterThicknessTexture',      6): 30,
    ('scatterThicknessTexture',     12): 31,
    ('transmittanceTintTexture',     1): 27,
    ('uvDistortionTexture',          0): 12,  ('uvDistortionTexture',          8): 12,
    ('uvDistortionTexture',         44): 12,
    ('uvDistortion2Texture',         8): 12,
    # some materials name slots by file suffix instead
    ('_ao',   6): 25, ('_asa',  8): 16, ('_bm',   6): 37, ('_cav',  6): 26,
    ('_col',  1):  4, ('_col', 13): 45, ('_det',  0):  6, ('_ehl', 12): 23,
    ('_gls',  6): 17, ('_ilm',  1): 19, ('_msk',  6):  5, ('_nml',  0): 12,
    ('_nml',  8): 12, ('_opa',  8):  8, ('_spc',  1): 13, ('_vxd',  6): 33,
}

# slot -> mode type across all formats (when (slot, fmt) is missing)
SLOT_FALLBACK = {
    'albedoTexture':           4,  'albedo2Texture':          4,
    'albedoMultiplyTexture':  45,
    'normalTexture':          12,  'normal2Texture':         12,
    'detailNormalTexture':    12,
    'glossTexture':           17,  'gloss2Texture':          17,
    'specTexture':            13,  'spec2Texture':           13,
    'aoTexture':              25,  'ao2Texture':             25,
    'cavityTexture':          26,
    'emissiveTexture':        19,  'emissive2Texture':       19,
    'emissiveMultiplyTexture':24,
    'opacityMultiplyTexture':  5,
    'detailTexture':           6,
    'colorRampTexture':       45,
    'iridescenceRampTexture':  4,
    'layerBlendTexture':      37,
    'anisoSpecDirTexture':    16,
    'scatterThicknessTexture':31,
    'transmittanceTintTexture':27,
    'uvDistortionTexture':    12,  'uvDistortion2Texture':   12,
    '_ao':25, '_asa':16, '_bm':12, '_cav':26, '_col':4, '_det':6, '_ehl':23,
    '_gls':17, '_ilm':19, '_msk':5, '_nml':12, '_opa':5, '_spc':13, '_vxd':33,
}

# fmt -> mode type when no slot is known
FMT_FALLBACK = {
    0: 6, 1: 13, 6: 17, 8: 12, 10: 4, 12: 24, 13: 45, 32: 45, 44: 12, 53: 45,
}


BLOCK_FMT_TABLE = [
    (8, 4),  (8, 4),  (16, 4), (16, 4), (16, 4), (16, 4),
    (8, 4),  (8, 4),  (16, 4), (16, 4), (16, 4), (16, 4),
    (16, 4), (16, 4),
    (16, 1), (16, 1), (16, 1),
    (12, 1), (12, 1), (12, 1),
    (8, 1),  (8, 1),  (8, 1),  (8, 1),  (8, 1),  (8, 1),  (8, 1),  (8, 1),
    (4, 1),  (4, 1),  (4, 1),
    (4, 1),  (4, 1),  (4, 1),  (4, 1),  (4, 1),
    (4, 1),  (4, 1),  (4, 1),  (4, 1),  (4, 1),
    (4, 1),  (4, 1),  (4, 1),
    (2, 1),  (2, 1),  (2, 1),  (2, 1),  (2, 1),  (2, 1),  (2, 1),  (2, 1),  (2, 1),
    (1, 1),  (1, 1),  (1, 1),  (1, 1),  (1, 1),
    (4, 1),  (4, 1),  (4, 1),  (2, 1),
]


def mip_size_aligned(w, h, fmt):
    if not (0 <= fmt < len(BLOCK_FMT_TABLE)):
        return 0
    bpp, blk = BLOCK_FMT_TABLE[fmt]
    if blk == 4:
        bw = max(1, (w + 3) // 4); bh = max(1, (h + 3) // 4)
        sz = bw * bh * bpp
    else:
        sz = max(1, w) * max(1, h) * bpp
    return (sz + 15) & ~15


def total_mip_count(w, h):
    m = 1
    while w > 1 or h > 1:
        w = max(1, w // 2); h = max(1, h // 2); m += 1
    return m


def derive_type(slot, fmt):
    """Slot-driven type derivation. slot may be None / '' / 'unavailable'."""
    if slot and slot != 'unavailable':
        if (slot, fmt) in SLOT_TYPE_RULE:
            return SLOT_TYPE_RULE[(slot, fmt)]
        if slot in SLOT_FALLBACK:
            return SLOT_FALLBACK[slot]
    return FMT_FALLBACK.get(fmt, 0)


def synth_v10_tail(fmt, w, h, arr, perm, mand, opt, slot=None, misc_flags=0):
    """Return the 36-byte [0x14..0x37] tail block for a v10 TXTR header.

    `misc_flags` = the texture's resourceFlags / header miscFlags (+0x11); bit 1 (0x2) = cubemap.
    """
    streamed = mand + opt
    arr_eff = max(1, arr)
    is_streamable = (streamed > 0) and (arr_eff == 1)

    # +0x14 totalByteCount: streamed mips only, 0 for a non-streamable texture.
    ds = 0
    if is_streamable:
        ww, hh = w, h
        for _ in range(streamed):
            ds += mip_size_aligned(ww, hh, fmt)
            ww = max(1, ww // 2); hh = max(1, hh // 2)
        ds *= arr_eff

    cdn = 1 if is_streamable else 0
    # Every texture carries a semantic; an env cubemap has no material slot and takes 18.
    ttype = derive_type(slot, fmt)
    if (misc_flags & 2) and (not slot or slot == 'unavailable'):
        ttype = 18

    tail = bytearray(36)
    struct.pack_into("<I", tail, 0x00, ds)          # +0x14 totalByteCount
    tail[0x04] = perm                                # +0x18 numUnstreamableMips
    tail[0x05] = mand                                # +0x19 numMandatoryStreamableMips
    tail[0x06] = opt                                 # +0x1A numOptStreamableMips
    tail[0x07] = cdn                                 # +0x1B minStreamableMipsToLoad
    tail[0x08] = 0                                   # +0x1C numCDNStreamableMips (no CDN)
    tail[0x09] = ttype                               # +0x1D texSemantic
    # +0x0A..0x0B packedStreamCompAlg = 0 (raw BCn in starpak, no per-mip compression)

    # +0x0C..0x19 packedStreamSize[7] (u16): per streamed mip, largest first, the index of
    # its last 4 KB page in the starpak. Mips are stored raw, so stored size = aligned size.
    if is_streamable:
        ww, hh = w, h
        for i in range(min(streamed, 7)):
            ssz = mip_size_aligned(ww, hh, fmt)
            struct.pack_into("<H", tail, 0x0C + i * 2, min(0xFFFF, max(0, (ssz - 1) // 4096)))
            ww = max(1, ww // 2); hh = max(1, hh // 2)
    # CDNKeyA, rmsError[7] and CDNKeyB stay 0; rmsError only prioritises streaming.
    return bytes(tail)


def build_slot_map(matl_dirs):
    """Walk material .json files in `matl_dirs`, return texture_guid_lower -> slot_name.
    `texture_guid_lower` is the hex GUID with no '0x' prefix and no leading zeros.
    On a guid bound to multiple slots across materials, last-wins."""
    g2slot = {}
    for d in matl_dirs:
        for f in glob.glob(os.path.join(d, "**", "*.json"), recursive=True):
            try:
                doc = json.load(open(f))
            except Exception:
                continue
            txs = doc.get("$textures", {})
            tys = doc.get("$textureTypes", {})
            for k, gx in txs.items():
                if not isinstance(gx, str) or not gx.startswith("0x"):
                    continue
                g = gx[2:].lower().lstrip("0") or "0"
                slot = tys.get(k, "")
                if slot and slot != "unavailable":
                    g2slot[g] = slot
    return g2slot
