"""Builds a Repak-ready bundle of the stock effects a map's FX entities name:
the effect closure (each effect plus the children it references), the materials
and textures those effects point at, and any model an operator renders -- minus
everything the client already has resident, which is common*/startup*.

    fx_bundle.py <rsx export dir> <out dir> <index assets.json> <effect name>...

The closure is walked over the .efct_childrefs/.efct_assetrefs sidecars RSX
writes, so a name that resolves nowhere is an error rather than a silent hole.
"""

import json
import re
import shutil
import struct
import sys
from pathlib import Path

RESIDENT = ("common", "startup", "ui", "english", "subtitle", "mdl.rpak", "base")
SIDECARS = (".efct_def", ".efct_assetrefs", ".efct_childrefs")


def effect_names(exported):
    names = {}
    for f in Path(exported).rglob("*.efct_def"):
        m = re.findall(rb"[A-Za-z_][A-Za-z0-9_]{5,70}", f.read_bytes()[:0x200])
        names[f.stem] = m[0].decode("latin-1", "replace") if m else ""
    return names


def sidecar(exported, guid, suffix):
    """RSX names an export after the guid with its leading zeros stripped."""
    for name in (guid, guid[2:].lstrip("0") or "0"):
        p = Path(exported) / "efct" / "effect" / f"0x{name}{suffix}"
        if p.is_file():
            return p
    return None


def closure(exported, roots):
    by_name = {v.lower(): k for k, v in effect_names(exported).items()}
    todo, seen, assets = [], set(), set()
    for n in roots:
        if n.lower() not in by_name:
            raise SystemExit(f"no exported effect named {n}")
        todo.append(by_name[n.lower()])
    while todo:
        g = todo.pop()
        if g in seen:
            continue
        seen.add(g)
        for side in SIDECARS[1:]:
            p = sidecar(exported, g, side)
            if p is None:
                continue
            raw = p.read_bytes()
            guids = [f"0x{v:016X}" for v in struct.unpack(f"<{len(raw) // 8}Q", raw)]
            (todo.extend if side == ".efct_childrefs" else assets.update)(guids)
    return sorted(seen), sorted(assets)


def main():
    exported, out, index, roots = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3], sys.argv[4:]
    rows = json.loads(Path(index).read_text())["assets"]
    paks, path_of = {}, {}
    for r in rows:
        paks.setdefault(f"0x{r['guid']:016X}", []).append(r["pak"])
        path_of[r["path"].lower()] = f"0x{r['guid']:016X}"

    effects, assets = closure(exported, roots)
    tex = set()
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    files = []

    for g in effects:
        rel = f"effect/{g}"
        (out / "effect").mkdir(exist_ok=True)
        n = 0
        for side in SIDECARS:
            p = sidecar(exported, g, side)
            if p is not None:
                shutil.copyfile(p, out / f"{rel}{side}")
                n += 1
        if not n:
            raise SystemExit(f"{g}: no efct sidecars exported")
        files.append({"_type": "efct", "_path": rel, "$guid": int(g, 16)})

    for g in assets:
        resident = any(p.startswith(RESIDENT) for p in paks.get(g, ()))
        key = next((k for k, v in path_of.items() if v == g and k.startswith("material/")), None)
        if key is None:
            print(f"  asset {g}: not a material ({paks.get(g)}), skipped")
            continue
        stem = Path(key).with_suffix("").as_posix()[len("material/"):]
        src = None
        for p in Path(exported).rglob("*.json"):
            d = json.loads(p.read_text())
            if d.get("shaderType") and f"{d['name']}_{d['shaderType']}".lower() == stem.lower():
                src = p
                break
        if src is None:
            print(f"  material {key} not exported, skipped")
            continue
        rel = f"material/{stem}"
        if resident:
            print(f"  matl {rel:60s} resident")
            continue
        (out / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, out / f"{rel}.json")
        uber = src.with_suffix(".uber")
        if uber.is_file():
            shutil.copyfile(uber, out / f"{rel}.uber")
        print(f"  matl {rel:60s} shipped")
        files.append({"_type": "matl", "_path": f"{rel}.rpak", "$guid": int(g, 16)})
        for v in json.loads(src.read_text()).get("$textures", {}).values():
            if isinstance(v, str) and v.startswith("0x"):
                tex.add(v[2:].upper())

    for g in sorted(tex):
        if any(p.startswith(RESIDENT) for p in paks.get(f"0x{g}", ())):
            continue
        dds = next(Path(exported).rglob(f"texture/0x{g[2:].lstrip('0')}.dds"), None) \
            or next(Path(exported).rglob(f"texture/0x{g}.dds"), None)
        if dds is None:
            print(f"  texture 0x{g} not exported, skipped")
            continue
        (out / "texture").mkdir(exist_ok=True)
        shutil.copyfile(dds, out / "texture" / (dds.stem + ".dds"))
        shutil.copyfile(dds.with_suffix(".json"), out / "texture" / (dds.stem + ".json"))
        print(f"  txtr  texture/0x{g}")
        files.append({"_type": "txtr", "_path": f"texture/0x{g}", "$guid": f"0x{g}",
                      **json.loads(dds.with_suffix(".json").read_text())})

    for p in sorted(Path(exported).rglob("mdl/*/*.rmdl")):
        name = p.parent.name
        key = next((k for k in path_of if k.endswith(f"/{name}.rmdl")), None)
        if key is None or path_of[key] not in assets:
            continue
        guid = path_of[key]
        rel = key
        (out / rel).parent.mkdir(parents=True, exist_ok=True)
        for ext in (".rmdl", ".vg", ".vg_static"):
            s = p.with_suffix(ext)
            if s.is_file():
                shutil.copyfile(s, out / rel.replace(".rmdl", ext))
        print(f"  mdl_ {rel:60s} {guid}")
        files.append({"_type": "mdl_", "_path": rel, "$guid": guid})

    (out / "_manifest.json").write_text(json.dumps(files, indent=1))
    counts = {}
    for f in files:
        counts[f["_type"]] = counts.get(f["_type"], 0) + 1
    print(f"{len(roots)} roots -> {counts}")


if __name__ == "__main__":
    main()