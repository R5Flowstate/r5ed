"""Where the external tools, the game and the scratch space are, for every script.

Each setting is an environment variable; r5ed.config.json at the repo root (copy
r5ed.config.example.json) fills any that are unset. A required setting that is
missing stops the script with the name to set.
"""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CONFIG = Path(os.environ.get("R5ED_CONFIG") or REPO / "r5ed.config.json")

# setting -> what it is
SETTINGS = {
    "R5ED_GAME_ROOT": "the R5Flowstate game folder (holds paks/Win64)",
    "R5ED_RSX": "rsx.exe (https://github.com/R5Flowstate/rsx)",
    "R5ED_REPAK": "repak.exe (https://github.com/R5Flowstate/RePak)",
    "R5ED_REVPK": "revpk.exe (the R5Flowstate SDK)",
    "R5ED_RMDLCONV": "rmdlconv.exe (https://github.com/R5Flowstate/rmdlconv)",
    "R5ED_TEXCONV": "texconv.exe (https://github.com/microsoft/DirectXTex/releases)",
    "R5ED_RECAST": "the navmesh baker (the R5Flowstate SDK)",
    "R5ED_BLENDER": "blender.exe, for the formats only Blender reads",
    "R5ED_STUDIOMDL": "a v49 studiomdl.exe (Portal 2 authoring tools), only for DMX sources",
    "R5ED_PROJECTS": "where r5ed projects live",
    "R5ED_SCRATCH": "scratch space for exports and staging (needs tens of GB free)",
    "R5ED_BASEMENT": "the stock-room file scripts/basement/make_basement.py writes",
    "R5ED_BIN": "the folder holding r5ed-compile (default: target/release)",
}

_file: dict | None = None


def _config() -> dict:
    global _file
    if _file is None:
        _file = json.loads(CONFIG.read_text(encoding="utf-8")) if CONFIG.is_file() else {}
    return _file


def get(name: str, required: bool = True) -> str | None:
    v = os.environ.get(name) or _config().get(name)
    if not v and required:
        raise SystemExit(f"{name} is not set: {SETTINGS.get(name, '')}. "
                         f"Set it in the environment or in {CONFIG.name}.")
    return v or None


def path(name: str, required: bool = True) -> Path | None:
    v = get(name, required)
    return Path(v) if v else None


def scratch(*parts: str) -> Path:
    p = path("R5ED_SCRATCH").joinpath(*parts)
    p.mkdir(parents=True, exist_ok=True)
    return p


def export_all() -> None:
    """Copy every configured setting into os.environ, for child processes."""
    for name in SETTINGS:
        v = get(name, required=False)
        if v:
            os.environ[name] = v


if __name__ == "__main__":
    # `py scripts/lib/r5edenv.py` prints the settings as shell exports (used by scripts/env.sh)
    for name in SETTINGS:
        v = get(name, required=False)
        if v:
            print(f"export {name}='{v}'")
    sys.exit(0)
