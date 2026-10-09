"""Puts the converted dedi models into the map's own server pak.

`r5ed-compile package` writes the server pak from what the stock server paks
hold, so a map's converted models (the S3 v10 rmdls in the src server pak) are
left out and the dedicated server cannot load the props once the src pak is
gone -- which it always is inside a mod. This copies them into the package's
server assets, lists them in its manifest and rebuilds the server pak.

    py merge_server_models.py <package dir> <stem> <src server manifest json> <repak exe>
"""

import json
import shutil
import subprocess
import sys
from pathlib import Path


def main():
    pkg, stem, src_manifest, repak = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3]), sys.argv[4]
    server_json = pkg / f"{stem}_server.json"
    server = json.loads(server_json.read_text(encoding="utf-8"))
    src = json.loads(src_manifest.read_text(encoding="utf-8"))
    src_assets, dst_assets = Path(src["assetsDir"]), Path(server["assetsDir"])
    have = {f["_path"] for f in server["files"]}
    added = 0
    for f in src["files"]:
        if f["_type"] != "mdl_" or f["_path"] in have:
            continue
        s, d = src_assets / f["_path"], dst_assets / f["_path"]
        if not s.is_file():
            raise SystemExit(f"missing dedi model {s}")
        d.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(s, d)
        server["files"].append({"_type": "mdl_", "_path": f["_path"], "$guid": f["$guid"]})
        added += 1
    server_json.write_text(json.dumps(server, indent=1), encoding="utf-8")
    r = subprocess.run([repak, str(server_json)], capture_output=True, text=True, errors="replace")
    out = (r.stdout + r.stderr).replace("\r", "\n")
    if "built pak" not in out or "ERROR" in out:
        raise SystemExit("server pak rebuild failed:\n" + "\n".join(out.splitlines()[-10:]))
    print(f"server pak: {added} converted models added, {len(server['files'])} models total")


if __name__ == "__main__":
    main()
