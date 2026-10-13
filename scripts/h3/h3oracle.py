"""Measures an H3 port's bake against H3's own baked lighting, on the same
surface points.

H3 keeps its per-pixel lightmap as two DXT5 texture arrays: four linear-SH RGB
images and one dominant-light colour image, each a base + residual slice pair
with sqrt(L / maxL) in alpha and (rgb / L + 1) / 2 in colour. dump_lighting.cs
writes them (sh.bin, intensity.bin, lighting.tsv with the scales) and
dump_lightmap_geo.cs writes the BSP triangles with their lightmap texcoords.

Points are spread over every H3 lightmap triangle; each is read from H3's
lightmap and from the S21 bake at the same barycentrics. Printed: correlation
of log light, and the S21/H3 ratio per band of H3 brightness. A port whose
light has H3's shape has the same ratio in every band.

usage: h3oracle.py <lighting dir> <project> <client bsp> [--step 24]
"""

import argparse
import json
from pathlib import Path

import numpy as np
from scipy.spatial import cKDTree

LUM = np.array([0.2126, 0.7152, 0.0722])
MESH = np.dtype([("fi", "<u4"), ("tc", "<u2"), ("fv", "<u2"), ("lvc", "<u2"), ("vt", "u1"), ("cube", "u1"),
                 ("u0", "4u1"), ("ls", "<u2", 2), ("fade", "<u2"), ("ms", "<u2"), ("flags", "<u4")])
SORT = np.dtype([("td", "<u2"), ("lm", "<i2"), ("u0", "<u2"), ("lvc", "<u2"), ("fv", "<u4")])
SIZE = 1024


def dxt5_blocks(raw, w, h, blocks):
    """The (n, 4, 4, 4) RGBA texels of the given (by, bx) DXT5 blocks."""
    b = np.frombuffer(raw, np.uint8).reshape(h // 4, w // 4, 16)[blocks[:, 0], blocks[:, 1]]
    a0, a1 = b[:, 0].astype(np.float32), b[:, 1].astype(np.float32)
    bits = np.zeros(len(b), np.uint64)
    for i in range(6):
        bits |= b[:, 2 + i].astype(np.uint64) << np.uint64(8 * i)
    ai = np.stack([((bits >> np.uint64(3 * i)) & np.uint64(7)).astype(np.int64) for i in range(16)], 1)
    gt = (a0 > a1)[:, None]
    pal = np.empty((len(b), 8), np.float32)
    pal[:, 0], pal[:, 1] = a0, a1
    for i in range(1, 7):
        seven = ((7 - i) * a0 + i * a1) / 7
        five = ((5 - i) * a0 + i * a1) / 5 if i < 5 else (0.0 if i == 5 else 255.0)
        pal[:, 1 + i] = np.where(gt[:, 0], seven, five)
    alpha = np.take_along_axis(pal, ai, 1) / 255.0
    c0 = b[:, 8].astype(np.uint32) | b[:, 9].astype(np.uint32) << 8
    c1 = b[:, 10].astype(np.uint32) | b[:, 11].astype(np.uint32) << 8

    def rgb(c):
        return np.stack([((c >> 11) & 31) / 31.0, ((c >> 5) & 63) / 63.0, (c & 31) / 31.0], -1)

    p0, p1 = rgb(c0), rgb(c1)
    cp = np.stack([p0, p1, (2 * p0 + p1) / 3, (p0 + 2 * p1) / 3], 1)
    ci = b[:, 12].astype(np.uint32) | b[:, 13].astype(np.uint32) << 8 | b[:, 14].astype(np.uint32) << 16 \
        | b[:, 15].astype(np.uint32) << 24
    ci = np.stack([(ci >> (2 * i)) & 3 for i in range(16)], 1).astype(np.int64)
    col = np.take_along_axis(cp, ci[..., None].repeat(3, -1), 1)
    return np.concatenate([col, alpha[..., None]], -1).reshape(-1, 4, 4, 4).astype(np.float32)


def h3_lightmap(lighting, px, py):
    """Linear-SH radiance (4, n, 3) and dominant colour (n, 3) at texels px, py."""
    scales = {}
    for line in open(lighting / "lighting.tsv"):
        f = line.split("\t")
        if f[0] == "SCALE":
            scales[int(f[1])] = float(f[2])
    sl = SIZE * SIZE
    blocks = np.stack([py // 4, px // 4], 1)
    i = np.arange(len(px))

    def image(raw, k, maxl):
        base = dxt5_blocks(raw[2 * k * sl:(2 * k + 1) * sl], SIZE, SIZE, blocks)[i, py % 4, px % 4]
        res = dxt5_blocks(raw[(2 * k + 1) * sl:(2 * k + 2) * sl], SIZE, SIZE, blocks)[i, py % 4, px % 4]
        a = base[:, 3] + res[:, 3]
        return ((base[:, :3] + res[:, :3] - 0.5) * 2 - 1) * (a * a * maxl)[:, None]

    sh = (lighting / "sh.bin").read_bytes()
    dom = (lighting / "intensity.bin").read_bytes()
    return np.stack([image(sh, k, scales[k]) for k in range(4)]), image(dom, 0, scales[4])


def irradiance(sh, n):
    """E / pi for normals n from linear-SH radiance (TagTool basis order)."""
    c0, c1 = 0.28209479, 0.48860251
    y = np.stack([np.full(len(n), c0), -c1 * n[:, 1], c1 * n[:, 2], -c1 * n[:, 0]])
    a = np.array([np.pi, 2 * np.pi / 3, 2 * np.pi / 3, 2 * np.pi / 3])
    return np.einsum("k,kn,knc->nc", a, y, sh) / np.pi


def s21_triangles(bsp):
    lump = lambda i, dt: np.fromfile(f"{bsp}.{i:04x}.bsp_lump", dt)
    pos = lump(0x03, "<f4").reshape(-1, 3).astype(np.float64)
    meshes, sorts, idx = lump(0x50, MESH), lump(0x52, SORT), lump(0x4F, "<u2")
    lit = lump(0x49, "<u4").reshape(-1, 8)
    heads = lump(0x53, np.uint8).reshape(-1, 8)
    data = np.memmap(f"{bsp}.0062.bsp_lump", np.uint8, mode="r")
    pages, at = [], 0
    for h in heads:
        w, hh = int(h[4]) | int(h[5]) << 8, int(h[6]) | int(h[7]) << 8
        n = w * hh
        pages.append((data[at:at + n * 4].reshape(hh, w, 4), data[at + n * 4:at + n * 8].reshape(hh, w, 4)))
        at += n * 8
    tris, uvs, page = [], [], []
    for m in meshes:
        pg = int(sorts[m["ms"]]["lm"])
        if int(m["vt"]) != 2 or pg < 0 or pg >= len(pages):
            continue
        fi, tc = int(m["fi"]), int(m["tc"])
        v = lit[int(sorts[m["ms"]]["fv"]) + idx[fi:fi + 3 * tc].astype(np.int64)]
        tris.append(pos[v[:, 0]].reshape(-1, 3, 3))
        uvs.append(v[:, 5:7].copy().view(np.float32).reshape(-1, 3, 2))
        page.append(np.full(tc, pg))
    return np.concatenate(tris), np.concatenate(uvs), np.concatenate(page), pages


def sample(img, uv):
    h, w = img.shape[:2]
    x = np.clip((uv[:, 0] % 1.0) * w, 0, w - 1).astype(int)
    y = np.clip((uv[:, 1] % 1.0) * h, 0, h - 1).astype(int)
    return img[y, x] / 255.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("lighting", type=Path)
    ap.add_argument("project", type=Path)
    ap.add_argument("bsp")
    ap.add_argument("--step", type=float, default=24.0, help="spacing of the sample points, in units")
    a = ap.parse_args()

    meta = json.loads((a.project / "h2_gameplay.json").read_text())
    centre, scale = np.asarray(meta["centre"], float), float(meta["scale"])
    doc = json.loads((a.project / "map.json").read_text())
    bake = doc["settings"]["bake"]
    sun = next(o for o in doc["objects"] if o["kind"].get("kind") == "sun")
    x, y, z, w = sun["transform"]["rotation"]
    u, v = np.array([x, y, z]), np.array([0.0, 0.0, -1.0])
    to_sun = -(v + 2 * np.cross(u, np.cross(u, v) + w * v))
    sun_lum = float(np.asarray(sun["kind"]["color"]) @ LUM) * sun["kind"]["intensity"]
    sky_lum = float(np.asarray(bake["sky_color"]) @ LUM) * bake["sky_intensity"]

    rec = np.fromfile(a.lighting / "lmgeo.bin", np.float32).reshape(-1, 25)[:, 1:].reshape(-1, 3, 8)
    hp = (rec[..., :3].astype(np.float64) - centre) * scale
    tris, uvs, page, pages = s21_triangles(a.bsp)
    dist, j = cKDTree(tris.mean(1)).query(hp.mean(1), distance_upper_bound=2.0)
    rng = np.random.default_rng(1)
    pts, nrm, h3uv, lm, sky_d, vis = [], [], [], [], [], []
    for i in np.where(np.isfinite(dist))[0]:
        tri = hp[i]
        area = 0.5 * np.linalg.norm(np.cross(tri[1] - tri[0], tri[2] - tri[0]))
        k = int(min(64, max(1, area / (a.step * a.step))))
        s, r = np.sqrt(rng.random(k)), rng.random(k)
        bary = np.stack([1 - s, s * (1 - r), s * r], 1)
        order = [int(np.argmin(np.linalg.norm(tris[j[i]] - c, axis=1))) for c in tri]
        n = bary @ rec[i, :, 3:6]
        nrm.append(n / (np.linalg.norm(n, axis=1, keepdims=True) + 1e-9))
        pts.append(bary @ tri)
        h3uv.append(bary @ rec[i, :, 6:8])
        uv = bary @ uvs[j[i]][order]
        img0, img1 = pages[page[j[i]]]
        t0, t1 = sample(img0, uv), sample(img1, uv)
        lm.append(t0[:, :3] * (t0[:, 3:4] * 3.984375 + 1 / 64))
        sky_d.append(t1[:, :3] * 2.007874 - 1)
        vis.append(t1[:, 3])
    nrm, h3uv, lm, sky_d, vis = (np.concatenate(v) for v in (nrm, h3uv, lm, sky_d, vis))
    px = np.clip((h3uv[:, 0] % 1) * SIZE, 0, SIZE - 1).astype(int)
    py = np.clip((h3uv[:, 1] % 1) * SIZE, 0, SIZE - 1).astype(int)
    sh, _ = h3_lightmap(a.lighting, px, py)
    h3 = irradiance(sh, nrm) @ LUM

    dl = np.linalg.norm(sky_d, axis=1)
    nd = np.einsum("ij,ij->i", sky_d, nrm)
    sky = sky_lum * np.clip(0.5 * dl * (nd + np.maximum(nd, dl * dl)), 0, 1)
    sun_t = sun_lum * vis * np.clip(nrm @ to_sun, 0, None)
    bounce = lm @ LUM
    s21 = bounce + sky + sun_t

    ok = h3 > 0.01
    corr = np.corrcoef(np.log(np.maximum(s21[ok], 1e-4)), np.log(h3[ok]))[0, 1]
    print(f"{len(h3)} points  corr(log light) {corr:.3f}")
    q = np.percentile(h3, [25, 50, 90])
    bands = (("H3 top 10%", h3 >= q[2]), ("H3 50-90%", (h3 >= q[1]) & (h3 < q[2])),
             ("H3 25-50%", (h3 >= q[0]) & (h3 < q[1])), ("H3 low 25%", h3 < q[0]))
    for name, sel in bands:
        r = np.median(s21[sel]) / max(np.median(h3[sel]), 1e-6)
        print(f"  {name:11s} H3 {np.median(h3[sel]):.3f}  S21 {np.median(s21[sel]):.3f} "
              f"(bounce {np.median(bounce[sel]):.3f} sky {np.median(sky[sel]):.3f} sun {np.median(sun_t[sel]):.3f})  ratio {r:.3f}")


if __name__ == "__main__":
    main()
