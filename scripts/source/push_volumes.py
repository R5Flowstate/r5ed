"""Source trigger_push volumes of a converted map, as a script both VMs run.

The map script registers the same boxes on the server and the client, so the
SDK's push (SourcePush_AddVolume) is predicted. Pushes come from the map's
_script.ent, where import-vmf wrote each trigger_push as a trigger_multiple
with editorclass trigger_source_push and its world box as mins/maxs.

    py push_volumes.py <map>_script.ent <namespace> <out sh_push_volumes.gnut>
"""
import math
import re
import sys
from pathlib import Path

# Source trigger_push spawnflags.
SF_CLIENTS = 0x1
SF_ONCE = 0x80


def _vec(text):
    """Source UTIL_StringToFloatArray: whitespace-separated, missing fields are 0."""
    out = []
    for token in text.split()[:3]:
        m = re.match(r"[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?", token)
        out.append(float(m.group(0)) if m else 0.0)
    return out + [0.0] * (3 - len(out))


def forward(angles):
    """Source AngleVectors forward for pitch yaw roll in degrees."""
    pitch, yaw = math.radians(angles[0]), math.radians(angles[1])
    return [math.cos(pitch) * math.cos(yaw), math.cos(pitch) * math.sin(yaw), -math.sin(pitch)]


def parse(ent_path):
    """[(mins, maxs, push)] for every push a player can touch, and the skipped ones as [(reason, block)]."""
    text = Path(ent_path).read_text(encoding="latin-1")
    volumes, skipped = [], []
    for block in re.findall(r"\{[^{}]*\}", text):
        kv = dict(re.findall(r'"([^"]*)"\s+"([^"]*)"', block))
        if kv.get("editorclass") != "trigger_source_push":
            continue
        flags = int(kv.get("spawnflags", "0") or 0)
        if kv.get("StartDisabled", "0") == "1":
            skipped.append(("starts disabled", kv.get("targetname", "")))
            continue
        if not flags & SF_CLIENTS:
            skipped.append(("not for players", kv.get("targetname", "")))
            continue
        if flags & SF_ONCE:
            # A one-shot push removes itself after the first touch; no surf map uses it yet.
            raise SystemExit(f"{ent_path}: trigger_push with the once flag is not supported ({kv})")
        if "mins" not in kv or "maxs" not in kv:
            raise SystemExit(f"{ent_path}: push trigger without mins/maxs ({kv})")
        speed = float(kv.get("pushspeed", "40") or 40)
        direction = forward(_vec(kv.get("pushdir", "0 0 0")))
        volumes.append((_vec(kv["mins"]), _vec(kv["maxs"]), [d * speed for d in direction]))
    return volumes, skipped


def _num(x):
    s = f"{x:.3f}".rstrip("0").rstrip(".")
    return "0" if s in ("", "-0") else s


def _sq(v):
    return "<" + ", ".join(_num(x) for x in v) + ">"


def render(namespace, volumes):
    lines = [
        "global function " + namespace + "__RegisterPushVolumes",
        "",
        "// The map's push triggers. Server and client register the same list, so the push is predicted.",
        "void function " + namespace + "__RegisterPushVolumes()",
        "{",
        "\tSourcePush_ClearVolumes()",
    ]
    for mins, maxs, push in volumes:
        lines.append(f"\tSourcePush_AddVolume( {_sq(mins)}, {_sq(maxs)}, {_sq(push)} )")
    lines += ["}", ""]
    return "\n".join(lines)


def write(ent_path, namespace, out_path):
    volumes, skipped = parse(ent_path)
    Path(out_path).write_text(render(namespace, volumes), encoding="utf-8")
    for reason, name in skipped:
        print(f"push skipped ({reason}): {name or '<unnamed>'}")
    print(f"{len(volumes)} push volumes -> {out_path}")
    return len(volumes)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    write(sys.argv[1], sys.argv[2], sys.argv[3])
