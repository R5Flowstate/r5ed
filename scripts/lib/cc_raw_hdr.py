"""Bake a TF2 colour-correction lookup into an S21 .raw_hdr volume.

TF2 grades after its own tonemap:  out = lut(srgb(tf2_tonemap(x)))
S21 grades before its tonemap:     out = s21_tonemap(hdr_lut(x))

So the volume stores s21_tonemap^-1(lut(srgb(tf2_tonemap(x)))), which makes the
stock S21 post chain reproduce the TF2 frame exactly. S21 tonemap function 4
scales rgb by f(max)/max with f linear up to `linear_end`, exponential shoulder
to 1 above it, so the inverse is exact for any output below 1.

    py cc_raw_hdr.py <tf2 .raw> <out .raw_hdr> [--linear-end 0.5] [--verify]
"""
import argparse
import struct
import sys

import numpy as np

TF2_SIZE = 32
S21_SIZE = 33
RAW_HDR_TRANSFER_PQ = 2

PQ_M1 = 2610.0 / 16384.0
PQ_M2 = 2523.0 / 4096.0 * 128.0
PQ_C1 = 3424.0 / 4096.0
PQ_C2 = 2413.0 / 4096.0 * 32.0
PQ_C3 = 2392.0 / 4096.0 * 32.0

# The engine maps scene 1.0 to 100 nits; PQ 1.0 is 10000 nits.
SCENE_TO_PQ = 0.01


def pq_encode(y):
    y = np.clip(y, 0.0, 1.0) ** PQ_M1
    return ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)) ** PQ_M2


def pq_decode(e):
    e = np.clip(e, 0.0, 1.0) ** (1.0 / PQ_M2)
    return (np.maximum(e - PQ_C1, 0.0) / (PQ_C2 - PQ_C3 * e)) ** (1.0 / PQ_M1)


def tf2_tonemap(x):
    return x * (10.0 * x + 0.3) / (x * (10.0 * x + 0.5) + 1.5)


def srgb_encode(v):
    v = np.clip(v, 0.0, 1.0)
    return np.where(v <= 0.0031308, v * 12.92, 1.055 * v ** (1.0 / 2.4) - 0.055)


def srgb_decode(s):
    return np.where(s <= 0.04045, s / 12.92, ((s + 0.055) / 1.055) ** 2.4)


def s21_tonemap(rgb, linear_end):
    m = rgb.max(axis=-1, keepdims=True)
    k = 1.0 - linear_end
    f = np.where(m < linear_end, m, linear_end + k * (1.0 - np.exp(-(m - linear_end) / k)))
    return rgb * np.where(m > 0.0, f / np.maximum(m, 1e-30), 0.0)


def s21_tonemap_inverse(rgb, linear_end, max_scene):
    m = rgb.max(axis=-1, keepdims=True)
    k = 1.0 - linear_end
    t = np.clip((m - linear_end) / k, 0.0, 1.0 - 1e-7)
    x = np.where(m < linear_end, m, linear_end - k * np.log1p(-t))
    x = np.minimum(x, max_scene)
    return rgb * np.where(m > 0.0, x / np.maximum(m, 1e-30), 0.0)


def trilinear(volume, coords):
    """volume[b][g][r][c], coords[..., rgb] in texel units (texel centres on integers)."""
    n = volume.shape[0]
    c = np.clip(coords, 0.0, n - 1.0)
    i0 = np.floor(c).astype(np.int64)
    i0 = np.minimum(i0, n - 2)
    f = c - i0
    r0, g0, b0 = i0[..., 0], i0[..., 1], i0[..., 2]
    fr, fg, fb = f[..., 0:1], f[..., 1:2], f[..., 2:3]
    out = 0.0
    for db, wb in ((0, 1.0 - fb), (1, fb)):
        for dg, wg in ((0, 1.0 - fg), (1, fg)):
            for dr, wr in ((0, 1.0 - fr), (1, fr)):
                out = out + volume[b0 + db, g0 + dg, r0 + dr] * (wb * wg * wr)
    return out


def load_tf2_lut(path):
    data = open(path, "rb").read()
    if len(data) != TF2_SIZE ** 3 * 3:
        sys.exit("%s: %d bytes, expected a %d^3 RGB8 lookup" % (path, len(data), TF2_SIZE))
    # r fastest, then g, then b; texels are sRGB so the sampler returns linear
    lut = np.frombuffer(data, np.uint8).reshape(TF2_SIZE, TF2_SIZE, TF2_SIZE, 3) / 255.0
    return srgb_decode(lut)


def tf1_tonemap(x):
    # Titanfall 1 engine_post has no curve: the exposure-scaled scene is clamped to the 8-bit target.
    return np.clip(x, 0.0, 1.0)


SOURCE_TONEMAP = {"tf2": tf2_tonemap, "tf1": tf1_tonemap}
source_tonemap = tf2_tonemap


def tf2_frame(x, lut_lin):
    s = srgb_encode(source_tonemap(x))
    return trilinear(lut_lin, s * (TF2_SIZE - 1))


def bake(lut_lin, linear_end):
    p = np.arange(S21_SIZE) / (S21_SIZE - 1.0)
    b, g, r = np.meshgrid(p, p, p, indexing="ij")
    code = np.stack([r, g, b], axis=-1)
    x = pq_decode(code) / SCENE_TO_PQ
    target = tf2_frame(x, lut_lin)
    h = s21_tonemap_inverse(target, linear_end, 1.0 / SCENE_TO_PQ)
    return pq_encode(h * SCENE_TO_PQ)


def scene_samples(count, seed):
    rng = np.random.default_rng(seed)
    x = np.exp(rng.uniform(np.log(1e-4), np.log(30.0), size=(count, 3)))
    grey = np.exp(rng.uniform(np.log(1e-4), np.log(30.0), size=(count // 4, 1))).repeat(3, axis=1)
    return np.concatenate([x, grey * rng.uniform(0.9, 1.1, size=grey.shape)])


def trilinear_matrix(coords, n):
    import scipy.sparse as sp

    c = np.clip(coords, 0.0, n - 1.0)
    i0 = np.minimum(np.floor(c).astype(np.int64), n - 2)
    f = c - i0
    rows, cols, vals = [], [], []
    idx = np.arange(len(c))
    for db in (0, 1):
        wb = f[:, 2] if db else 1.0 - f[:, 2]
        for dg in (0, 1):
            wg = f[:, 1] if dg else 1.0 - f[:, 1]
            for dr in (0, 1):
                wr = f[:, 0] if dr else 1.0 - f[:, 0]
                rows.append(idx)
                cols.append(((i0[:, 2] + db) * n + (i0[:, 1] + dg)) * n + (i0[:, 0] + dr))
                vals.append(wb * wg * wr)
    return sp.csr_matrix((np.concatenate(vals), (np.concatenate(rows), np.concatenate(cols))), shape=(len(c), n ** 3))


def fit(lut_lin, linear_end, seed_codes, count=400000, smooth=1e-3):
    """Grid codes whose trilinear reconstruction best matches the TF2 frame in display space.

    Point-sampling the target at grid nodes is exact only at the nodes; this weights
    each sample by how strongly a code error shows up after decode, tonemap and sRGB.
    """
    import scipy.sparse as sp
    import scipy.sparse.linalg as spla

    n = S21_SIZE
    x = scene_samples(count, 7)
    want = srgb_encode(tf2_frame(x, lut_lin))
    a = trilinear_matrix(pq_encode(x * SCENE_TO_PQ) * (n - 1), n)
    reg = smooth * sp.identity(n ** 3)

    def shown(codes):
        return srgb_encode(s21_tonemap(pq_decode(codes) / SCENE_TO_PQ, linear_end))

    seed = seed_codes.reshape(-1, 3)
    grid = seed.copy()
    eps = 1e-4
    for _ in range(4):
        recon = np.clip(a @ grid, 0.0, 1.0)
        got = shown(recon)
        step = np.zeros_like(grid)
        for ch in range(3):
            bumped = recon.copy()
            bumped[:, ch] += eps
            slope = (shown(bumped)[:, ch] - got[:, ch]) / eps
            j = sp.diags(slope) @ a
            lhs = j.T @ j + reg
            rhs = j.T @ (want[:, ch] - got[:, ch]) + smooth * (seed[:, ch] - grid[:, ch])
            step[:, ch], _ = spla.cg(lhs, rhs, rtol=1e-8, maxiter=2000)
        grid = np.clip(grid + step, 0.0, 1.0)
    return grid.reshape(seed_codes.shape)


def pack(codes):
    q = np.clip(np.rint(codes * 1023.0), 0, 1023).astype(np.uint32)
    words = q[..., 0] | (q[..., 1] << 10) | (q[..., 2] << 20)
    return struct.pack("<I", RAW_HDR_TRANSFER_PQ) + words.astype("<u4").tobytes()


def unpack(blob):
    words = np.frombuffer(blob, "<u4", offset=4).reshape(S21_SIZE, S21_SIZE, S21_SIZE)
    return np.stack([words & 1023, (words >> 10) & 1023, (words >> 20) & 1023], axis=-1) / 1023.0


def s21_frame(x, volume_codes, linear_end):
    coord = pq_encode(x * SCENE_TO_PQ) * (S21_SIZE - 1)
    h = pq_decode(trilinear(volume_codes, coord)) / SCENE_TO_PQ
    return s21_tonemap(h, linear_end)


def verify(lut_lin, blob, linear_end):
    vol = unpack(blob)
    rng = np.random.default_rng(1)
    # log-uniform scene values from deep shadow to well past the shoulder
    x = np.exp(rng.uniform(np.log(1e-4), np.log(30.0), size=(200000, 3)))
    grey = np.exp(np.linspace(np.log(1e-4), np.log(30.0), 64))[:, None].repeat(3, axis=1)
    for label, xs in (("random", x), ("grey ramp", grey)):
        want = srgb_encode(tf2_frame(xs, lut_lin)) * 255.0
        got = srgb_encode(s21_frame(xs, vol, linear_end)) * 255.0
        err = np.abs(want - got).max(axis=-1)
        print("%-9s srgb8 error: mean %.3f  p99 %.3f  max %.3f" % (label, err.mean(), np.percentile(err, 99), err.max()))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("src")
    ap.add_argument("dst")
    ap.add_argument("--linear-end", type=float, default=0.5, help="mat_debug_tonemapping_linear_segment_end")
    ap.add_argument("--no-fit", action="store_true", help="point-sample the grid instead of fitting it")
    ap.add_argument("--verify", action="store_true")
    ap.add_argument("--source", choices=sorted(SOURCE_TONEMAP), default="tf2", help="game whose post chain graded the lookup")
    a = ap.parse_args()
    global source_tonemap
    source_tonemap = SOURCE_TONEMAP[a.source]

    lut_lin = load_tf2_lut(a.src)
    codes = bake(lut_lin, a.linear_end)
    if not a.no_fit:
        codes = fit(lut_lin, a.linear_end, codes)
    blob = pack(codes)
    open(a.dst, "wb").write(blob)
    print("%s: %d bytes" % (a.dst, len(blob)))
    if a.verify:
        verify(lut_lin, blob, a.linear_end)


if __name__ == "__main__":
    main()
