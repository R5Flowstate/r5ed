"""One S21 material from plain images, on iw4mat's stock templates.

Shared by srcmat.py (Source VMT/VTF) and objmat.py (OBJ/MTL + PNG): the caller
supplies a texture bank whose keys it understands, the template class and the
image keys per slot; this writes the matl json/uber, its depth/colpass twins,
and appends the manifest entries.
"""

import json
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "iw4"))
import iw4mat  # noqa: E402

FLAT_ALBEDO = (128, 128, 128, 255)

# A model rmdlconv writes with full-float positions (hull past +-1024 x/y or
# +-2048 z) only draws with an _rgdc shader; this stock material has both.
RGDC_TEMPLATE = ("models/world/metal/imc_metal_trim_01", "rgdc")
RGDC_CUTOUT_TEMPLATE = ("models/foliage/tree_branch_canyonland_swamp_canopy", "rgdc")
CUTOUT_CLASSES = ("model_test", "model_test_2s", "world_test")


# Template slots that were neither ours nor known, reported once per template.
_REPORTED = set()
ADDITIVE_WORDS = ("ilm", "emis", "glow", "illum")


def neutral(bank, template, key, type_name):
    """A texture for a template slot we have no layer for and no flat value from iw4mat:
    black where the name reads additive (emissive, glow), white (the multiply identity)
    elsewhere. Leaving the slot alone keeps the template's stock image, which then draws
    on top of ours: a ported screen showing the sign it was cloned from."""
    additive = any(w in f"{key} {type_name}".lower() for w in ADDITIVE_WORDS)
    if (template, key) not in _REPORTED:
        _REPORTED.add((template, key))
        print(f"  template {template}: slot {key} ({type_name or 'no type'}) bound "
              f"{'black' if additive else 'white'} instead of the template's own texture")
    return bank.flat("detailTexture", (0, 0, 0, 255) if additive else (255, 255, 255, 255))


def build(assets, templates, index, bank, klass, name, *, albedo=None, normal=None, albedo2=None,
          normal2=None, opacity=None, surface="concrete", flat_albedo=FLAT_ALBEDO, two_sided=False,
          rgdc=False, emissive=None, gloss=None, spec=None, ao=None, opacity_value=None, files):
    """`albedo`/`normal`/`opacity` are bank keys (None = neutral flat).
    `normal` keys must already be in iw4mat's DXT5nm layout (X in alpha).
    `albedo2`/`normal2` feed the second layer of the `world_blend` template;
    without them it repeats the first layer. `two_sided` turns back-face
    culling off on the material and its twins. `rgdc` builds the
    full-float-position variant of a model material. `emissive` is a bank key for the
    emissive slot; without it an emissive template glows with the albedo. `gloss`,
    `spec` and `ao` are bank keys for those slots; without them the slot is flat.
    `opacity_value` (0..1) is a uniform opacity, for glass whose texture alpha is no
    opacity (opaque, or another layer's mask); it wins over `opacity` and the albedo."""
    cull = {"stateBits": [{"colorWriteRgb": 1, "cullFace": "none" if two_sided else "back"}]}
    if rgdc:
        tname, stype = RGDC_CUTOUT_TEMPLATE if klass in CUTOUT_CLASSES else RGDC_TEMPLATE
    else:
        tname, stype = iw4mat.TEMPLATES[klass]
    tpl, uber = iw4mat.load_template(templates, tname, stype)
    textures = dict(tpl.get("$textures", {}))
    by_guid = {}
    seen = {}
    for key, type_name in tpl.get("$textureTypes", {}).items():
        slot = iw4mat.slot_of(type_name)
        seen[slot] = seen.get(slot, 0) + 1
        guid = None
        if slot == "albedoTexture":
            layer = albedo if seen[slot] == 1 else (albedo2 or albedo)
            if layer:
                guid = bank.derive(layer, slot)
            if guid is None:
                guid = bank.flat(slot, flat_albedo)
        elif slot == "emissiveTexture" and (emissive or albedo):
            guid = bank.derive(emissive or albedo, slot)
        elif slot == "opacityMultiplyTexture" and opacity_value is not None:
            v = int(round(max(0.0, min(1.0, opacity_value)) * 255))
            guid = bank.flat(slot, (v, v, v, 255))
        elif slot == "opacityMultiplyTexture" and (opacity or albedo):
            guid = bank.derive(opacity or albedo, slot)
        elif slot == "normalTexture":
            layer = None
            if seen[slot] == 1:
                layer = normal
            elif seen[slot] == 2:
                layer = normal2 or normal
            if layer:
                guid = bank.derive(layer, slot)
            if guid is None:
                guid = bank.flat(slot, (128, 128, 0, 255))
        elif slot == "glossTexture":
            guid = (bank.derive(gloss, slot) if gloss else None) or bank.flat(slot, (48, 48, 48, 255))
        elif slot == "specTexture":
            guid = (bank.derive(spec, slot) if spec else None) or bank.flat(slot, (40, 40, 40, 255))
        elif slot == "aoTexture" and ao:
            guid = bank.derive(ao, slot)
        if guid is None and slot == "detailNormalTexture":
            # Source has no detail normal layer; a flat one leaves the surface as its own normal map draws it.
            guid = bank.flat(slot, (128, 128, 0, 255))
        if guid is None and slot in iw4mat.SLOT_FORMAT and slot in iw4mat.FLAT:
            guid = bank.flat(slot, iw4mat.FLAT[slot])
        if guid is None:
            guid = neutral(bank, tname, key, type_name)
        if guid is not None:
            textures[key] = f"0x{guid:016X}"
            by_guid[slot] = guid
    doc = dict(tpl)
    doc["name"] = name
    doc["$textures"] = textures
    doc["surfaceProp"] = surface
    before = doc.get("rasterizerFlags", "0x0")
    iw4mat.cull_state(cull, doc)
    if ("cull", tname, before, doc["rasterizerFlags"]) not in _REPORTED:
        _REPORTED.add(("cull", tname, before, doc["rasterizerFlags"]))
        print(f"  template {tname}: rasterizerFlags {before} -> {doc['rasterizerFlags']}")
    iw4mat.twin_materials(doc, by_guid, name, stype, index, Path(assets), files, cull)
    rel = f"material/{name}_{stype}"
    (Path(assets) / rel).parent.mkdir(parents=True, exist_ok=True)
    (Path(assets) / f"{rel}.json").write_text(json.dumps(doc, indent=1))
    if uber:
        iw4mat.copy_uber(uber, Path(assets) / f"{rel}.uber", stype, tname)
    files.append({"_type": "matl", "_path": f"{rel}.rpak", "$guid": iw4mat.string_to_guid(f"{rel}.rpak")})


def manifest(assets, name, out_dir, bank, files, stream=True):
    """Writes `<assets>/../<name>.json`: textures first, then materials and models."""
    assets = Path(assets)
    for m in sorted((assets / "mdl").rglob("*.rmdl")) if (assets / "mdl").is_dir() else []:
        rel = m.relative_to(assets).as_posix()
        files.append({"_type": "mdl_", "_path": rel, "$guid": iw4mat.string_to_guid(rel)})
    doc = {"version": 8, "name": name, "assetsDir": assets.as_posix() + "/",
           "outputDir": Path(out_dir).as_posix() + "/", "headerFlags": 32}
    if stream:
        doc["streamFileMandatory"] = "\\".join(["paks", "Win64", f"{name}.starpak"])
        doc["streamFileOptional"] = "\\".join(["paks", "Win64", f"{name}.opt.starpak"])
    doc["files"] = list(bank.entries.values()) + files
    path = assets.parent / f"{name}.json"
    path.write_text(json.dumps(doc, indent=1))
    return path, doc
