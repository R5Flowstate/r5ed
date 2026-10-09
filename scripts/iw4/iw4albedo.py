"""The bake's bounce albedo for an IW4 port: mean linear luminance per material and per prop.

Writes `settings.bake.albedo` (and `occluder_skip`) into a project's map.json:

    world/mw2/<map>/<name>    <- iw4mat.py --albedo-out
    world/<stock>/<name>     <- the stock material's albedo texture in a packaged build
    mdl/mw2/<map>/<name>.rmdl <- the model's SMD, its materials weighted by triangle area

Run after iw4mat.py and iw4props.py, on the project the build reads.
"""

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import r5edenv  # noqa: E402

TEXCONV = r5edenv.get("R5ED_TEXCONV", required=False) or "texconv"

# Props that draw but never block baked light: alpha-tested foliage cards and glass.
OCCLUDER_SKIP = ["foliage_ivy", "gardenflowers", "fern", "foliage_tree", "foliage_cod5_tree", "treefacade",
                 "window"]


def srgb_decode(c):
    return np.where(c <= 0.04045, c / 12.92, np.power((c + 0.055) / 1.055, 2.4))


# DXGI formats that store sRGB-encoded colour.
SRGB_FORMATS = {29, 72, 75, 78, 91, 93, 99}


def dds_mean(dds, scratch):
    """Mean linear colour of an albedo texture. An sRGB-format file is unpacked as sRGB,
    or texconv would linearise it before the decode below does."""
    head = Path(dds).read_bytes()[:148]
    srgb = head[84:88] == b"DX10" and int.from_bytes(head[128:132], "little") in SRGB_FORMATS
    fmt = "R8G8B8A8_UNORM_SRGB" if srgb else "R8G8B8A8_UNORM"
    subprocess.run([TEXCONV, "-nologo", "-y", "-ft", "png", "-f", fmt, "-o", str(scratch), str(dds)],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    png = Path(scratch) / f"{Path(dds).stem}.png"
    if not png.is_file():
        return None
    rgb = np.asarray(Image.open(png).convert("RGB")).astype(np.float32).reshape(-1, 3) / 255.0
    return [round(float(v), 4) for v in srgb_decode(rgb).mean(axis=0)]


def guid_names(guid):
    """A texture guid as the packaged file may name it: padded and unpadded."""
    v = int(guid, 16)
    return [f"0x{v:016X}", f"0x{v:X}"]


def stock_albedo(path, package, scratch):
    for f in sorted((package / "material").glob(f"{path}_*.json")):
        if "_colpass" in f.name:
            continue
        doc = json.loads(f.read_text())
        for k, kind in doc.get("$textureTypes", {}).items():
            if kind not in ("albedoTexture", "_col"):
                continue
            guid = doc["$textures"].get(k)
            for name in guid_names(guid) if guid else []:
                dds = package / "texture" / f"{name}.dds"
                if dds.is_file():
                    return dds_mean(dds, scratch)
    return None


def smd_albedo(smd, materials):
    """Area-weighted mean albedo of an SMD's triangles by their material."""
    lines = smd.read_text(errors="replace").splitlines()
    try:
        at = lines.index("triangles") + 1
    except ValueError:
        return None
    acc, total = np.zeros(3), 0.0
    while at + 3 < len(lines) and lines[at] != "end":
        name = lines[at].strip()
        pts = [np.array([float(x) for x in lines[at + i].split()[1:4]]) for i in (1, 2, 3)]
        at += 4
        colour = materials.get(name[:-5] if name.endswith("_rgdp") else name)
        if colour is None:
            continue
        area = 0.5 * float(np.linalg.norm(np.cross(pts[1] - pts[0], pts[2] - pts[0])))
        acc += area * np.array(colour)
        total += area
    return [round(float(v), 4) for v in acc / total] if total > 0 else None


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("project", help="r5ed project dir (map.json)")
    ap.add_argument("--converted", required=True, help="iw4mat.py --albedo-out file")
    ap.add_argument("--props-src", required=True, help="iw4props.py source dir: <model>/ref.smd")
    ap.add_argument("--package", required=True, help="a packaged build of the map: material/ and texture/")
    ap.add_argument("--model-dir", default="mdl/mw2/terminal", help="the props' model path prefix")
    args = ap.parse_args()

    project, package = Path(args.project), Path(args.package)
    doc = json.loads((project / "map.json").read_text())
    converted = json.loads(Path(args.converted).read_text())
    table, missing = {}, []
    with tempfile.TemporaryDirectory(dir=project) as scratch:
        for o in doc["objects"]:
            k = o["kind"]
            # A surface that casts no shadow is out of the bake's way and bounces nothing.
            if k["type"] != "mesh" or not k.get("cast_shadows", True):
                continue
            for s21 in k.get("material_map", {}).values():
                if s21 in table:
                    continue
                colour = converted.get(s21)
                if colour is None:
                    colour = stock_albedo(s21, package, scratch)
                if colour is None:
                    if s21 not in missing:
                        missing.append(s21)
                else:
                    table[s21] = colour
    props = 0
    for smd in sorted(Path(args.props_src).glob("*/ref.smd")):
        colour = smd_albedo(smd, converted)
        if colour is not None:
            table[f"{args.model_dir}/{smd.parent.name}.rmdl"] = colour
            props += 1
    bake = doc["settings"]["bake"]
    # Bounce keeps each surface's brightness but not its hue: saturated props and signs
    # tinted whole walls, and their colour came back as blotchy noise from the bake.
    lum = lambda c: round(0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2], 4)
    bake["albedo"] = {k: [lum(v)] * 3 for k, v in sorted(table.items())}
    bake["occluder_skip"] = OCCLUDER_SKIP
    (project / "map.json").write_text(json.dumps(doc, indent=1))
    print(f"albedo: {len(table) - props} materials, {props} prop models; {len(missing)} materials unresolved"
          + (f" ({', '.join(missing[:5])})" if missing else ""))


if __name__ == "__main__":
    main()
