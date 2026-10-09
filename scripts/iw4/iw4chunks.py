"""Places the chunks iw4props.py split an oversized static model into: every prop
of a split model becomes one prop per chunk, at the chunk's offset through the
prop's own rotation and scale, keeping every other prop field. Idempotent.

usage: iw4chunks.py <project dir> <props work dir>/chunks.json
"""

import copy
import json
import sys
import uuid
from pathlib import Path

import numpy as np

DEFAULT_MODEL = "mdl/mw2/terminal/{}.rmdl"


def rotate(q, v):
    x, y, z, w = q
    u = np.array([x, y, z])
    v = np.asarray(v, float)
    return v + 2.0 * np.cross(u, np.cross(u, v) + w * v)


def main():
    proj, chunk_file = Path(sys.argv[1]), Path(sys.argv[2])
    model = sys.argv[3] if len(sys.argv) > 3 else DEFAULT_MODEL
    chunks = json.loads(chunk_file.read_text())
    path = proj / "map.json"
    doc = json.loads(path.read_text())
    by_model = {model.format(n): parts for n, parts in chunks.items()}
    out, placed = [], 0
    for o in doc["objects"]:
        k = o["kind"]
        parts = by_model.get(k.get("model")) if k.get("type") == "prop" else None
        if not parts:
            out.append(o)
            continue
        t = o["transform"]
        for i, part in enumerate(parts):
            c = copy.deepcopy(o)
            c["id"] = str(uuid.uuid4())
            c["name"] = f"{o['name']}_c{i}"
            c["kind"]["model"] = model.format(part["model"])
            offset = rotate(t["rotation"], np.asarray(part["offset"]) * np.asarray(t["scale"]))
            c["transform"]["position"] = (np.asarray(t["position"]) + offset).tolist()
            out.append(c)
            placed += 1
    doc["objects"] = out
    path.write_text(json.dumps(doc))
    print(f"{placed} chunk props for {len(chunks)} split models")


if __name__ == "__main__":
    main()
