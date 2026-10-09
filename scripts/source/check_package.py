"""Fails a ship when `r5ed-compile package` reports an asset it could not pack.

Shader sets are closed by close_shaders.py afterwards, and tool materials
(tools/*) are engine-side and never packed; anything else missing means the map
would reference an asset its pak does not carry, so the ship stops here.

    py check_package.py <package.log>
"""

import json
import sys
from pathlib import Path


def main():
    text = Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace")
    start = text.find("{")
    if start < 0:
        raise SystemExit("package printed no report")
    report = json.loads(text[start:])
    bad = [m for m in report.get("missing", [])
           if m.get("kind") != "shader set" and not str(m.get("name", "")).lower().startswith("tools/")]
    for m in bad:
        print(f"MISSING {m.get('kind')} {m.get('name')}: {m.get('why')}")
    if bad:
        raise SystemExit(f"package left {len(bad)} asset(s) out -- not shipping")
    print(f"package: {report.get('assets', '?')} assets, nothing missing")


if __name__ == "__main__":
    main()
