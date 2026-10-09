"""Removes the RSX export cache entries of the materials this run wrote, so a
rebuilt material of the same name is re-exported instead of served stale.

usage: purge_mat_cache.py <r5ed-cache/rsx dir> [<work dir>/material_map.json ...]
"""

import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from guid import string_to_guid  # noqa: E402


def guids_for(name):
    """Every S21 material path h3mat wrote, from material_map.json."""
    doc = json.loads(Path(name).read_text())
    for s21 in doc.values():
        if isinstance(s21, str) and s21.startswith("world/"):
            for suffix in ("_wldc", "_colpass_wldc"):
                yield f"material-{string_to_guid('material/' + s21 + suffix + '.rpak'):016x}"


def main():
    cache = Path(sys.argv[1])
    names = set()
    for m in sys.argv[2:]:
        names.update(guids_for(m))
    hit = 0
    for d in list(cache.iterdir()):
        for g in names:
            if d.name == g or d.name.startswith(g + "-"):
                shutil.rmtree(d, ignore_errors=True)
                hit += 1
                break
    print(f"purged {hit} cache entries of {len(names)} materials under {cache}")


if __name__ == "__main__":
    main()