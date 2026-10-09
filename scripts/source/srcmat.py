"""Source materials (.vmt + .vtf) -> S21 matl v23 + txtr v10 assets and a Repak manifest.

Each material is rebuilt on the stock S21 template iw4mat.py uses for the same
class, with the Source textures in its slots:

    albedoTexture          <- $basetexture (RGB, alpha kept)
    normalTexture          <- $bumpmap / $normalmap (RGB tangent-space)
    opacityMultiplyTexture <- $basetexture alpha ($translucent / $alphatest)
    emissiveTexture        <- $basetexture ($selfillum)

WorldVertexTransition (and the other two-layer blend shaders) build on the
`world_blend` template: $basetexture2 feeds the second albedo layer and the
displacement vertex alpha drives the blend.

World materials are named `world/source/<vmt path>` (what the VMF importer
references); model materials `models/source/<vmt path without models/>`
(what srcprops.py writes into each MDL). Also writes the texture-size table
the importer takes for exact brush UVs.
"""

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "iw4"))
import iw4mat  # noqa: E402
import matbuild  # noqa: E402
import skybox  # noqa: E402
import vtf  # noqa: E402
from vmt import Content  # noqa: E402

SURFACE = {
    "concrete": "concrete", "concrete_block": "concrete", "brick": "brick", "rock": "rock", "boulder": "rock",
    "gravel": "gravel", "dirt": "dirt", "mud": "dirt", "grass": "grass", "sand": "sand", "wood": "wood",
    "wood_crate": "wood", "wood_plank": "wood", "wood_solid": "wood", "metal": "metal", "metalpanel": "metalpanel",
    "metal_box": "metal", "metalgrate": "metalgrate", "metalvent": "metal", "chainlink": "metalgrate",
    "solidmetal": "solidmetal", "glass": "glass", "tile": "tile", "carpet": "carpet", "plaster": "plaster",
    "water": "water", "slime": "water", "foliage": "foliage", "rubber": "rubber", "plastic": "plastic",
    "cardboard": "paper", "paper": "paper", "snow": "snow", "ice": "ice", "asphalt": "asphalt",
}
WORLD_SHADERS = ("lightmappedgeneric", "worldvertextransition", "worldtwotextureblend", "lightmapped_4wayblend")
SKIP_PREFIXES = ("tools/", "skybox/", "detail/")


def s21_name(vmt_path, is_model):
    p = vmt_path.lower().strip("/")
    return f"models/source/{p.removeprefix('models/')}" if is_model else f"world/source/{p}"


def classify(shader, keys, is_model):
    side = "model" if is_model else "world"
    translucent = keys.get("$translucent") == "1" or keys.get("$additive") == "1"
    tested = keys.get("$alphatest") == "1"
    if shader == "water":
        return f"{side}_glass"
    if not is_model and shader in ("worldvertextransition", "worldtwotextureblend", "lightmapped_4wayblend"):
        return "world_blend"
    if keys.get("$selfillum") == "1" and not is_model:
        return "world_emissive"
    name = keys.get("$basetexture", "").lower()
    glass = "glass" in name or "window" in name
    if translucent and glass:
        return f"{side}_glass"
    # Source blends foliage, fences and grates; S21 cuts them out with alpha
    # test, and a blended card shows its square edges.
    if translucent and is_model:
        tested = True
    elif translucent:
        return "world_decal"
    if tested:
        return "model_test_2s" if is_model and keys.get("$nocull") == "1" else f"{side}_test"
    return f"{side}_opaque"


class SourceBank(iw4mat.TextureBank):
    """Texture keys are VTF paths; `<path>#nm` is a tangent-space normal map
    re-laid in the DXT5nm order iw4mat's normal slot reads (X in alpha)."""

    def __init__(self, content, assets, scratch, namespace):
        super().__init__([], assets, scratch, namespace)
        self.content = content

    def find(self, image):
        return self.content.texture(image.split("#")[0])

    def rgba(self, image):
        if image in self.decoded:
            return self.decoded[image]
        path = self.find(image)
        arr = None
        if path is not None:
            arr = vtf.read(path)[0]
            if image.endswith("#nm"):
                arr = arr.copy()
                arr[..., 3] = arr[..., 0]
        self.decoded[image] = arr
        return arr


def surface_prop(keys, name):
    sp = str(keys.get("$surfaceprop", "")).lower()
    return SURFACE.get(sp) or iw4mat.surface_prop(name)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("templates", help="RSX export of the template materials (iw4mat's set)")
    ap.add_argument("assets", help="Repak assets dir to write")
    ap.add_argument("--content", nargs="+", required=True)
    ap.add_argument("--import-report", help="r5ed-compile import-vmf output: its brush materials")
    ap.add_argument("--used", help="srcprops.py used.json: model materials")
    ap.add_argument("--stem", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--sizes-out", required=True, help="import-vmf --options JSON with texture sizes")
    args = ap.parse_args()

    content = Content(args.content)
    assets = Path(args.assets)
    vtf.set_scratch(assets.parent / "vtfscratch")
    bank = SourceBank(content, assets, assets.parent / "texscratch", f"source/{args.stem}")
    index = iw4mat.template_index(args.templates)

    wanted = []
    skyname = None
    if args.import_report:
        rep = json.loads(Path(args.import_report).read_text(encoding="utf-8"))
        wanted += [{"vmt": m, "model": False} for m in (rep.get("report") or rep)["materials"]]
        skyname = (rep.get("report") or rep).get("skyname") or None
    if args.used:
        wanted += json.loads(Path(args.used).read_text(encoding="utf-8"))

    files, sizes, report, done = [], {}, {}, set()
    for item in wanted:
        vmt_path, is_model = item["vmt"].lower(), item["model"]
        rgdc = item.get("variant") == "rgdc"
        if (vmt_path, is_model, rgdc) in done:
            continue
        done.add((vmt_path, is_model, rgdc))
        if not is_model and vmt_path.startswith(SKIP_PREFIXES):
            continue
        mat = content.material(vmt_path)
        if mat is None:
            report.setdefault("missing_vmt", []).append(vmt_path)
            mat = ("vertexlitgeneric" if is_model else "lightmappedgeneric", {})
        shader, keys = mat
        klass = classify(shader, keys, is_model)
        report.setdefault(klass, []).append(vmt_path)
        base = keys.get("$basetexture")
        bump = keys.get("$bumpmap") or keys.get("$normalmap")
        base2 = keys.get("$basetexture2")
        bump2 = keys.get("$bumpmap2") or keys.get("$normalmap2")
        if base and bank.find(base) is None:
            report.setdefault("missing_vtf", []).append(f"{vmt_path} {base}")
            base = None
        if bump and bank.find(bump) is None:
            report.setdefault("missing_vtf", []).append(f"{vmt_path} {bump}")
            bump = None
        if base2 and bank.find(base2) is None:
            report.setdefault("missing_vtf", []).append(f"{vmt_path} {base2}")
            base2 = None
        if bump2 and bank.find(bump2) is None:
            report.setdefault("missing_vtf", []).append(f"{vmt_path} {bump2}")
            bump2 = None
        if base:
            w, h = vtf.size(bank.find(base))
            sizes[vmt_path] = [w, h]
        # World faces draw both sides: surf ramps and thin brushes are seen from behind.
        matbuild.build(assets, args.templates, index, bank, klass, s21_name(vmt_path, is_model),
                       albedo=base, normal=f"{bump}#nm" if bump else None,
                       albedo2=base2, normal2=f"{bump2}#nm" if bump2 else None,
                       surface=surface_prop(keys, vmt_path),
                       flat_albedo=(40, 70, 90, 255) if shader == "water" else matbuild.FLAT_ALBEDO,
                       two_sided=keys.get("$nocull") == "1" or not is_model, rgdc=rgdc, files=files)

    name = f"{args.stem}_src"
    if skyname:
        try:
            sky = skybox.normalize_sky(skyname)
        except skybox.SkyboxError as e:
            report.setdefault("bad_skyname", []).append(str(e))
            sky = None
        if sky is not None:
            skybox.build_faces(assets, args.templates, index, bank, sky,
                               skybox.faces_of(content, skyname), files, report)
    _, doc = matbuild.manifest(assets, name, args.out_dir, bank, files)
    files = doc["files"]
    Path(args.sizes_out).write_text(json.dumps({"texture_sizes": sizes}, indent=1))
    print(f"{sum(1 for f in files if f['_type'] == 'matl')} materials, {len(bank.entries)} textures, "
          f"{sum(1 for f in files if f['_type'] == 'mdl_')} models -> {name}.json")
    print({k: len(v) for k, v in report.items()})
    for k in ("missing_vmt", "missing_vtf", "bad_skyname"):
        for line in report.get(k, []):
            print(f"  {k}: {line}")


if __name__ == "__main__":
    main()
