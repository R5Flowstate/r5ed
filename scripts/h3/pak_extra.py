"""Adds prebuilt assets (an `_manifest.json` plus the files it names, laid out
as Repak expects) to a Repak manifest: the stock sun flare effects, which no
map geometry references, so `package` never stages them.

usage: pak_extra.py <manifest.json> <extra dir> [...]
"""

import json
import shutil
import sys
from pathlib import Path


def guid(v):
    return int(v, 16) if isinstance(v, str) else int(v)


def main():
    manifest = Path(sys.argv[1])
    doc = json.loads(manifest.read_text())
    assets = Path(doc["assetsDir"])
    have = {guid(f["$guid"]) for f in doc["files"]}
    for extra in map(Path, sys.argv[2:]):
        for f in json.loads((extra / "_manifest.json").read_text()):
            if guid(f["$guid"]) in have:
                continue
            stem = f["_path"][:-5] if f["_path"].endswith(".rpak") else f["_path"]
            copied = 0
            for src in extra.glob(stem + ".*"):
                dst = assets / src.relative_to(extra)
                dst.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(src, dst)
                copied += 1
            if not copied:
                sys.exit(f"{extra}: no files for {f['_path']}")
            doc["files"].append(f)
            have.add(guid(f["$guid"]))
            print(f"+ {f['_type']} {f['_path']}")
    manifest.write_text(json.dumps(doc, indent=1))


if __name__ == "__main__":
    main()
