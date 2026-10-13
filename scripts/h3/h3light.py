"""Authors an H3 map's baked-light inputs into its r5ed project from the map's
own data: the sun H3 baked with, which surfaces let light through, and the
bounce albedo of every material.

- sun: direction and colour of the dominant light of H3's brightest light
  probes (dump_lighting.cs output); intensity is a parameter, calibrated with
  h3oracle.py.
- additive surfaces (light cards, glows), decals and blended glass cast no
  shadow (H3's own lightmap has full sun under its roof glass); every other
  alpha surface draws alpha-tested (leaves, grates, fences) and passes the
  share of light its alpha holes cover.
- settings.bake.albedo: mean linear albedo (luminance) of each material, times
  the render gain and a bounce gain fitted with h3oracle.py.

usage: h3light.py <project> <dump> <lighting dir> [--sun-intensity S] [--sky-intensity K]
                  [--albedo-gain G] [--bounce-gain B]
"""

import argparse
import json
import math
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
from h3mat import Sources, fit, luminance  # noqa: E402

LUM = np.array([0.2126, 0.7152, 0.0722])
CUTOUT = ("clip", "blend", "pre_multiplied")


def half3(text):
    return np.array([int(v) for v in text.strip(",").split(",")], np.uint16).view(np.float16).astype(np.float64)


def probe_sun(lighting_tsv):
    """Unit vector towards the sun and its colour, from the probes whose
    dominant light is strongest (outdoors, in the sun)."""
    dirs, cols = [], []
    for line in open(lighting_tsv):
        f = line.rstrip("\n").split("\t")
        if f[0] in ("IGP", "SCP") and len(f) > 4:
            dirs.append(half3(f[3]))
            cols.append(half3(f[4]))
    dirs, cols = np.array(dirs), np.array(cols)
    lum = cols @ LUM
    top = lum >= np.percentile(lum, 90)
    d = dirs[top] / (np.linalg.norm(dirs[top], axis=1, keepdims=True) + 1e-9)
    d = np.median(d, axis=0)
    d /= np.linalg.norm(d)
    c = cols[top].mean(axis=0)
    return d, c / c.max()


def quat_between(a, b):
    a, b = np.asarray(a, float), np.asarray(b, float)
    c = np.cross(a, b)
    q = np.array([c[0], c[1], c[2], 1.0 + a @ b])
    return list(q / np.linalg.norm(q))


def srgb_to_linear(c):
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def tint(m, usage):
    for t in m.get("tints", []):
        if t["usage"] == usage:
            import base64
            v = np.frombuffer(base64.b64decode(t["rgba"]), np.uint8)[:3].astype(np.float32) / 255.0
            return v
    return np.ones(3, np.float32)


def coverage(src, m):
    """Share of an alpha-tested material's surface that is solid, by the same
    alpha source the material's opacity texture is built from."""
    by = {}
    for t in m["textures"]:
        by.setdefault(t["usage"], []).append(t)
    dif = (by.get("diffuse") or [None])[0]
    tra = (by.get("transparency") or [None])[0]
    if tra is not None and tra["file"] is not None:
        ti = src.rgba(tra["file"])
        if ti is not None:
            a = ti[..., 3]
            if a.std() < 0.02:
                a = luminance(fit(ti[..., :3], 64))
            return float((a > 0.5).mean())
    if dif is not None and dif["file"] is not None:
        di = src.rgba(dif["file"])
        if di is not None:
            return float((di[..., 3] > 0.5).mean())
    return 1.0


def mean_albedo(src, m):
    by = {}
    for t in m["textures"]:
        by.setdefault(t["usage"], []).append(t)
    dif = (by.get("diffuse") or [None])[0]
    if dif is None or dif["file"] is None:
        return None
    di = src.rgba(dif["file"])
    if di is None:
        return None
    rgb = np.clip(fit(di[..., :3], 64) * tint(m, "albedo"), 0, 1)
    det = (by.get("diffuse_detail") or [None])[0]
    if det is not None and det["file"] is not None:
        dd = src.rgba(det["file"])
        if dd is not None:
            # H3 albedo = base * detail * 2 in gamma space.
            rgb = np.clip(rgb * 2.0 * dd[..., :3].reshape(-1, 3).mean(axis=0), 0, 1)
    return float(srgb_to_linear(rgb).reshape(-1, 3).mean(axis=0) @ LUM)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("project")
    ap.add_argument("dump")
    ap.add_argument("lighting")
    ap.add_argument("--work", help="h3mat work dir (decoded bitmap cache)")
    ap.add_argument("--sun-intensity", type=float)
    ap.add_argument("--sky-intensity", type=float)
    ap.add_argument("--albedo-gain", type=float, default=1.0,
                    help="linear albedo gain h3mat applies to the materials (ALBEDO_GAIN)")
    ap.add_argument("--bounce-gain", type=float, default=1.0,
                    help="extra gain on the bake albedo only: matches H3's fill-to-sun ratio in shade")
    a = ap.parse_args()

    proj, dump = Path(a.project), Path(a.dump)
    path = proj / "map.json"
    doc = json.loads(path.read_text())
    mats = json.loads((dump / "materials.json").read_text())
    src = Sources(dump, Path(a.work) if a.work else dump.parent / "mat")
    bake = doc["settings"]["bake"]

    to_sun, colour = probe_sun(Path(a.lighting) / "lighting.tsv")
    sun = next(o for o in doc["objects"] if o["kind"].get("type") == "light" and o["kind"].get("kind") == "sun")
    sun["transform"]["rotation"] = quat_between([0, 0, -1], -to_sun)
    sun["kind"]["color"] = [round(float(v), 4) for v in colour]
    if a.sun_intensity is not None:
        sun["kind"]["intensity"] = a.sun_intensity
    if a.sky_intensity is not None:
        bake["sky_intensity"] = a.sky_intensity

    transmission, albedo, clear = {}, {}, []
    for o in doc["objects"]:
        k = o["kind"]
        if k.get("type") != "mesh":
            continue
        for path_ in k["material_map"].values():
            h3name = path_.rsplit("/", 1)[-1].split("__")[0]
            m = mats.get(h3name)
            if m is None:
                continue
            alpha = m.get("alpha", "opaque")
            see_through = alpha == "additive" or (alpha != "opaque" and "decal" in h3name)                 or (alpha == "blend" and "glass" in h3name)
            if see_through and k.get("render", True):
                if k["cast_shadows"]:
                    clear.append(o["name"])
                k["cast_shadows"] = False
            elif alpha in CUTOUT:
                k["cast_shadows"] = True
                if path_ not in transmission:
                    transmission[path_] = round(1.0 - coverage(src, m), 3)
            if path_ not in albedo:
                v = mean_albedo(src, m)
                if v is not None:
                    albedo[path_] = [round(min(v * a.albedo_gain * a.bounce_gain, 0.9), 4)] * 3
    bake["shadow_transmission"] = {k: v for k, v in sorted(transmission.items()) if v > 0.0}
    bake["albedo"] = dict(sorted(albedo.items()))
    path.write_text(json.dumps(doc, indent=1))

    pitch = math.degrees(math.asin(to_sun[2]))
    yaw = math.degrees(math.atan2(to_sun[1], to_sun[0]))
    print(f"sun pitch {pitch:.1f} yaw {yaw:.1f} colour {np.round(colour, 3).tolist()} "
          f"intensity {sun['kind']['intensity']}, sky {bake['sky_intensity']}")
    print(f"{len(clear)} meshes made see-through: {', '.join(sorted(clear))}")
    for k, v in bake["shadow_transmission"].items():
        print(f"  pass {v:.3f}  {k}")
    print(f"albedo for {len(albedo)} materials, median {np.median([v[0] for v in albedo.values()]):.3f}")


if __name__ == "__main__":
    main()
