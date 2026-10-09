"""MTL (.mtl + images) and Blender GLB materials -> S21 matl v23 + txtr v10 assets.

Each MTL material becomes `<prefix>/<name>` (what `r5ed-compile import-obj`
references) on iw4mat's world templates; a .glb source (what
`r5ed-compile import-glb` references) contributes the same table from its
glTF materials and embedded images:

Each MTL material becomes `<prefix>/<name>` (what `r5ed-compile import-obj`
references) on iw4mat's world templates:

    map_Kd                  -> albedo (opaque), or alpha-tested with its alpha
    map_d (or d < 1)        -> opacity (a separate map_d image is merged into
                               the albedo's alpha)
    map_Bump / norm         -> normal (RGB tangent space)
    Kd with no map          -> a flat albedo of that colour
"""

import argparse
import json
import struct
import sys
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "iw4"))
import iw4mat  # noqa: E402
import matbuild  # noqa: E402

FOLIAGE_WORDS = ("foliage", "leaf", "leaves", "grass", "bush", "tree", "plant", "vine", "flower", "alpha")


def parse_mtl(text):
    """Mirrors rdoc::import::obj::parse_mtl."""
    out, cur = {}, None
    for line in text.splitlines():
        parts = line.strip().split(None, 1)
        if not parts:
            continue
        key, rest = parts[0], (parts[1].strip() if len(parts) > 1 else "")
        if key == "newmtl":
            cur = out.setdefault(rest, {"dissolve": 1.0, "color": [0.8, 0.8, 0.8]})
            continue
        if cur is None:
            continue
        image = rest.split()[-1] if rest else None
        k = key.lower()
        if k == "map_kd":
            cur["diffuse"] = image
        elif k == "map_d":
            cur["alpha"] = image
        elif k in ("map_bump", "bump", "norm", "map_kn"):
            cur["normal"] = image
        elif k == "d":
            cur["dissolve"] = float(rest or 1)
        elif k == "tr":
            cur["dissolve"] = 1.0 - float(rest or 0)
        elif k == "kd":
            v = [float(t) for t in rest.split()[:3]]
            if len(v) == 3:
                cur["color"] = v
    return out


def material_path(prefix, name):
    clean = "".join(c if c.isascii() and (c.isalnum() or c in "_-") else "_" for c in name.lower())
    return f"{prefix.rstrip('/')}/{clean}"


class GlbError(Exception):
    pass


def read_glb(path):
    """ glTF 2.0 JSON document and binary chunk of a .glb. Every offset and
    length is checked against the file before it is read."""
    data = Path(path).read_bytes()
    if len(data) < 12 or data[:4] != b"glTF":
        raise GlbError(f"{path}: not a glTF binary container")
    doc, blob, at, index = None, b"", 12, 0
    while at + 8 <= len(data):
        length, kind = struct.unpack_from("<II", data, at)
        if length > len(data) - at - 8:
            raise GlbError(f"{path}: chunk {index} runs past the end of the file")
        body = data[at + 8:at + 8 + length]
        if kind == 0x4E4F534A and doc is None:
            try:
                doc = json.loads(body)
            except ValueError as e:
                raise GlbError(f"{path}: glb json is not valid: {e}")
        elif kind == 0x004E4942:
            blob = body
        at += 8 + length
        index += 1
    if doc is None:
        raise GlbError(f"{path}: glb json chunk is missing")
    return doc, blob


def glb_materials(path, doc):
    """Material name -> MTL-like maps for a glTF document. Image values are
    image indices; primitives with no material become `default`, matching what
    import-glb reports for them."""
    images, textures, mats = doc.get("images", []), doc.get("textures", []), doc.get("materials", [])
    if len(mats) > 4096:
        raise GlbError(f"{path}: {len(mats)} materials")
    out = {}
    for m in mats:
        name = m.get("name") or "default"
        pbr = m.get("pbrMetallicRoughness", {})

        def img(ref):
            ti = (ref or {}).get("index")
            if not isinstance(ti, int) or ti < 0 or ti >= len(textures):
                return None
            si = textures[ti].get("source")
            return si if isinstance(si, int) and 0 <= si < len(images) else None

        f = list(pbr.get("baseColorFactor", [1.0, 1.0, 1.0, 1.0]) + [1.0] * 4)[:4]
        extras = m.get("extras") if isinstance(m.get("extras"), dict) else {}
        out.setdefault(name, {"base": img(pbr.get("baseColorTexture")),
                              "normal": img(m.get("normalTexture")),
                              "emissive": img(m.get("emissiveTexture")),
                              "mr": img(pbr.get("metallicRoughnessTexture")),
                              "occlusion": img(m.get("occlusionTexture")),
                              # A template class the exporter chose (iw4mat.TEMPLATES key), e.g. the
                              # Apex Mobile port's world_emissive / world_glass; absent from DCC exports.
                              "klass": extras.get("r5ed_class") if isinstance(extras.get("r5ed_class"), str) else None,
                              "color": [float(c) for c in f[:3]], "dissolve": float(f[3]),
                              "alpha_mode": m.get("alphaMode", "OPAQUE"),
                              "double_sided": bool(m.get("doubleSided"))})
    unmat = any(not isinstance(p.get("material"), int)
                for mesh in doc.get("meshes", []) for p in mesh.get("primitives", []))
    if unmat:
        out.setdefault("default", {"base": None, "normal": None, "emissive": None, "mr": None, "occlusion": None, "klass": None, "color": [0.8, 0.8, 0.8],
                                   "dissolve": 1.0, "alpha_mode": "OPAQUE", "double_sided": False})
    return out


def extract_glb_images(path, doc, blob, dest):
    """Writes each embedded image to dest. Returns image index -> file name
    and the images skipped with their reason."""
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    images, views = doc.get("images", []), doc.get("bufferViews", [])
    if len(images) > 4096:
        raise GlbError(f"{path}: {len(images)} images")
    out, skipped = {}, []
    for i, im in enumerate(images):
        vi = im.get("bufferView")
        if not isinstance(vi, int):
            skipped.append(f"image {i}: no embedded data")
            continue
        if vi < 0 or vi >= len(views):
            raise GlbError(f"{path}: image {i} names buffer view {vi} of {len(views)}")
        v = views[vi]
        off, ln = v.get("byteOffset", 0), v.get("byteLength", 0)
        if ln < 0 or off < 0 or off + ln > len(blob):
            raise GlbError(f"{path}: image {i} runs past the end of the file")
        ext = {("image/png"): ".png", ("image/jpeg"): ".jpg"}.get(im.get("mimeType", ""))
        if ext is None:
            skipped.append(f"image {i}: mime {im.get('mimeType', '?')!r} is not png or jpeg")
            continue
        name = f"img{i:04d}{ext}"
        (dest / name).write_bytes(blob[off:off + ln])
        out[i] = name
    return out, skipped


def parse_glb(path, dest):
    """Material table of a Blender-exported .glb as MTL-like entries, with its
    embedded images extracted to dest."""
    doc, blob = read_glb(path)
    files, skipped = extract_glb_images(str(path), doc, blob, dest)
    table = {}
    for name, m in glb_materials(str(path), doc).items():
        base = files.get(m["base"])
        norm = files.get(m["normal"])
        e = {"color": m["color"], "dissolve": m["dissolve"],
             "alpha_mode": m["alpha_mode"], "double_sided": m["double_sided"]}
        if base:
            e["diffuse"] = base
            if m["alpha_mode"] in ("BLEND", "MASK"):
                e["alpha"] = base
        if norm:
            e["normal"] = norm
        emis = files.get(m["emissive"])
        if emis:
            e["emissive"] = emis
        for k in ("mr", "occlusion"):
            if files.get(m[k]):
                e[k] = files[m[k]]
        if m["klass"]:
            e["klass"] = m["klass"]
        table[name] = e
    return table, skipped


def plan(name, m, bank):
    """Template class and texture keys for one MTL-like material. Returns
    (klass, albedo, normal, surface, flat, two_sided, missing)."""
    missing = []
    diffuse = m.get("diffuse") if m.get("diffuse") and bank.find(m["diffuse"]) else None
    if m.get("diffuse") and not diffuse:
        missing.append(f"{name} {m['diffuse']}")
    alpha = m.get("alpha") if m.get("alpha") and bank.find(m["alpha"]) else None
    # Blender writes map_d (and d 0) for any texture with an alpha channel,
    # opaque or not; only alpha that actually cuts counts.
    if alpha and not bank.cuts(f"{diffuse}+{alpha}" if diffuse and alpha != diffuse else (diffuse or alpha)):
        alpha = None
    see_through = (alpha is not None or (m.get("alpha") is None and 0.0 < m["dissolve"] < 1.0)
                   or (m.get("alpha_mode") == "BLEND" and 0.0 < m["dissolve"] < 1.0))
    normal = m.get("normal") if m.get("normal") and bank.find(m["normal"]) else None
    low = name.lower()
    if m.get("alpha_mode") == "MASK" and diffuse:
        klass = "world_test"
    elif see_through and diffuse:
        klass = "world_test"
    elif see_through:
        klass = "world_glass"
    else:
        klass = "world_opaque"
    if m.get("klass") in iw4mat.TEMPLATES:
        klass = m["klass"]
    albedo = diffuse
    if diffuse and alpha and alpha != diffuse:
        albedo = f"{diffuse}+{alpha}"
    flat = tuple(int(max(0.0, min(1.0, c)) * 255) for c in m["color"]) + (255,)
    surface = ("foliage" if any(w in low for w in FOLIAGE_WORDS) and see_through
               else iw4mat.surface_prop(low))
    return klass, albedo, f"{normal}#nm" if normal else None, surface, flat, \
        see_through or m.get("double_sided", False), missing


class ImageBank(iw4mat.TextureBank):
    """Keys are image paths relative to the MTL folder; `a+b` is image a with
    image b's alpha (or luminance) as its alpha; `n#nm` is a normal map re-laid
    with X in alpha. A glTF metallic-roughness map `mr` (G roughness, B metallic)
    turns into the spec/gloss the templates take: `mr#gloss` (1 - roughness, in
    alpha), `base&mr#spec` (lerp(0.04, albedo, metallic) in linear) and
    `base&mr#dmetal` (albedo darkened by metallic, which moves into the spec);
    `occ#ao` is an occlusion map's R in alpha; `<key>@a<0-255>` is any of these with a
    uniform alpha (a glass's opacity)."""

    def __init__(self, roots, assets, scratch, namespace):
        super().__init__([], assets, scratch, namespace)
        self.roots = [Path(r) for r in roots]
        self.by_name = {}
        for root in self.roots:
            for p in root.rglob("*"):
                if p.suffix.lower() in (".png", ".jpg", ".jpeg", ".tga", ".bmp", ".dds"):
                    self.by_name.setdefault(p.name.lower(), p)

    def find(self, image):
        name = image.split("#")[0].split("+")[0].split("&")[0]
        for root in self.roots:
            p = root / name
            if p.is_file():
                return p
        return self.by_name.get(Path(name.replace("\\", "/")).name.lower())

    def _load(self, name):
        p = self.find(name)
        if p is None:
            return None
        if p.suffix.lower() == ".dds":
            return super().rgba(str(p.with_suffix("")))
        return np.asarray(Image.open(p).convert("RGBA")).copy()

    def cuts(self, image):
        """True when at least 1% of the texels are below half opacity."""
        arr = self.rgba(image)
        return arr is not None and float((arr[..., 3] < 128).mean()) >= 0.01

    def rgba(self, image):
        if image in self.decoded:
            return self.decoded[image]
        # `<key>@a<0-255>`: that image with a uniform alpha (a glass's opacity).
        inner, at, level = image.rpartition("@a")
        if at and level.isdigit():
            arr = self.rgba(inner)
            if arr is not None:
                arr = arr.copy()
                arr[..., 3] = min(255, int(level))
            self.decoded[image] = arr
            return arr
        base, _, tail = image.partition("#")
        if tail in ("gloss", "spec", "dmetal", "ao"):
            arr = self._pbr(base, tail)
            self.decoded[image] = arr
            return arr
        color_key, _, alpha_key = base.partition("+")
        arr = self._load(color_key)
        if arr is not None and alpha_key:
            a = self._load(alpha_key)
            if a is not None:
                if a.shape[:2] != arr.shape[:2]:
                    a = np.asarray(Image.fromarray(a).resize(arr.shape[1::-1], Image.BILINEAR))
                has_alpha = a[..., 3].min() < 255
                arr[..., 3] = a[..., 3] if has_alpha else a[..., :3].mean(-1).astype(np.uint8)
        if arr is not None and tail == "nm":
            arr[..., 3] = arr[..., 0]
        self.decoded[image] = arr
        return arr

    def _pbr(self, key, tail):
        """Metallic-roughness and occlusion maps as the spec/gloss/AO the templates take."""
        first, _, second = key.partition("&")
        if tail in ("gloss", "ao"):
            src = self._load(first)
            if src is None:
                return None
            v = (255 - src[..., 1]) if tail == "gloss" else src[..., 0]
            return np.stack([v, v, v, v], -1).astype(np.uint8)
        base, mr = self._load(first), self._load(second)
        if base is None or mr is None:
            return None
        if mr.shape[:2] != base.shape[:2]:
            mr = np.asarray(Image.fromarray(mr).resize(base.shape[1::-1], Image.BILINEAR))
        metal = mr[..., 2:3].astype(np.float32) / 255.0
        lin = (base[..., :3].astype(np.float32) / 255.0) ** 2.2
        if tail == "spec":
            out = (0.04 * (1 - metal) + lin * metal) ** (1 / 2.2)
        else:
            out = (lin * (1 - metal)) ** (1 / 2.2)
        rgba = np.concatenate([np.clip(out * 255, 0, 255), base[..., 3:4].astype(np.float32)], -1)
        return rgba.astype(np.uint8)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("templates", help="RSX export of the template materials (iw4mat's set)")
    ap.add_argument("mtl", help="the .mtl (or Blender .glb) to build materials for")
    ap.add_argument("assets", help="Repak assets dir to write")
    ap.add_argument("--stem", required=True)
    ap.add_argument("--prefix", required=True, help="the material_prefix given to import-obj")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--images", nargs="*", default=[], help="more folders to look for images in")
    ap.add_argument("--only", help="import-obj report: build just the materials it used")
    args = ap.parse_args()

    src = Path(args.mtl)
    assets = Path(args.assets)
    if src.suffix.lower() == ".glb":
        mats, skipped = parse_glb(src, assets.parent / f"{args.stem}_glb_images")
        bank = ImageBank([assets.parent / f"{args.stem}_glb_images", *args.images],
                         assets, assets.parent / "texscratch", f"glb/{args.stem}")
    else:
        mats = parse_mtl(src.read_text(encoding="utf-8", errors="replace"))
        skipped = []
        bank = ImageBank([src.parent, *args.images], assets, assets.parent / "texscratch", f"obj/{args.stem}")
    if args.only:
        rep = json.loads(Path(args.only).read_text(encoding="utf-8"))
        used = set((rep.get("report") or rep)["materials"])
        mats = {k: v for k, v in mats.items() if k in used}
    index = iw4mat.template_index(args.templates)

    files, report = [], {}
    for s in skipped:
        report.setdefault("skipped_image", []).append(s)
    for name, m in mats.items():
        klass, albedo, normal, surface, flat, two_sided, missing = plan(name, m, bank)
        if missing:
            report.setdefault("missing_image", []).extend(missing)
        report.setdefault(klass, []).append(name)
        emissive = m.get("emissive") if m.get("emissive") and bank.find(m["emissive"]) else None
        mr = m.get("mr") if m.get("mr") and bank.find(m["mr"]) else None
        occ = m.get("occlusion") if m.get("occlusion") and bank.find(m["occlusion"]) else None
        gloss = spec = None
        if mr:
            gloss = f"{mr}#gloss"
            plain = albedo and "+" not in albedo and "#" not in albedo
            if plain:
                spec = f"{albedo}&{mr}#spec"
                albedo = f"{albedo}&{mr}#dmetal"
        # Glass is see-through by its factor (glTF baseColorFactor alpha, MTL d), not by the
        # albedo's alpha, when that alpha cuts nothing or the exporter chose the glass class
        # itself (the Apex Mobile port: the alpha there is another layer's mask).
        opacity_value = None
        if klass in ("world_glass", "model_glass") and "+" not in (albedo or "") and 0.0 < m["dissolve"] < 1.0 \
                and (m.get("klass") == klass or not (m.get("alpha") and bank.cuts(m["alpha"]))):
            opacity_value = m["dissolve"]
            # Some glass shaders read the albedo's alpha rather than the opacity slot
            # (CyberPark's glass drew solid with only the slot filled): carry it in both.
            level = int(round(opacity_value * 255))
            if albedo:
                albedo = f"{albedo}@a{level}"
            else:
                flat = tuple(flat[:3]) + (level,)
        matbuild.build(assets, args.templates, index, bank, klass, material_path(args.prefix, name),
                       albedo=albedo, normal=normal, surface=surface, emissive=emissive,
                       gloss=gloss, spec=spec, ao=f"{occ}#ao" if occ else None,
                       flat_albedo=flat, two_sided=two_sided, opacity_value=opacity_value, files=files)

    path, doc = matbuild.manifest(assets, f"{args.stem}_src", args.out_dir, bank, files)
    print(f"{sum(1 for f in doc['files'] if f['_type'] == 'matl')} materials, {len(bank.entries)} textures -> {path.name}")
    print({k: len(v) for k, v in report.items()})
    for line in report.get("missing_image", []):
        print(f"  missing_image: {line}")


if __name__ == "__main__":
    main()
