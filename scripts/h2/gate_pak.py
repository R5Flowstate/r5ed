"""Deploy gate for an r5ed-packaged map pak. Exits 1 if any check fails.

A pak that fails any of these dies in the client with "Pak file consistency
error" or an AV during pak load, so nothing may be deployed until all pass:
  guid refs   every guid-ref slot resolves (map pak or resident common paks)
  slots       no guid-ref slot is listed twice
  shaders     every staged shader entry is DXBC or an in-range reuse ref
  rsx         rsx -validateshaders flags nothing

usage: gate_pak.py <map.rpak> <package manifest json>
"""
import json
import os
import re
import subprocess
import sys

LIB = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "lib")
sys.path.insert(0, LIB)
import r5edenv  # noqa: E402

GUIDREFS = os.path.join(LIB, "gate_guidrefs.py")
SLOTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "gate_slots.py")
RSX = r5edenv.get("R5ED_RSX", required=False) or "rsx"
SCRATCH = str(r5edenv.scratch("tmp"))


def run(cmd, cwd=None):
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, errors="replace")
    return r.stdout + r.stderr


def main():
    # The guid-ref check runs from its own directory, so relative paths would not resolve.
    pak, manifest = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    failed = []

    out = run([sys.executable, GUIDREFS, pak, "client"], cwd=os.path.dirname(GUIDREFS))
    m = re.search(r"UNRESOLVED (\d+)", out)
    print("guid refs :", m.group(0) if m else "NO RESULT")
    if not m or m.group(1) != "0":
        failed.append("guid refs")
        print("\n".join(l for l in out.splitlines() if "missing" in l))

    out = run([sys.executable, SLOTS, pak])
    m = re.search(r"DUPLICATE (\d+)", out)
    print("slots     :", m.group(0) if m else "NO RESULT")
    if not m or m.group(1) != "0":
        failed.append("slots")

    import pak_msw
    bad = n = 0
    for f in json.load(open(manifest))["files"]:
        if f["_type"] != "shdr":
            continue
        g = f["$guid"]
        sh = pak_msw.read_shader(pak, int(g, 16) if isinstance(g, str) else g)
        n += 1
        if sh["parent"] is not None:
            continue
        for i, e in enumerate(sh["entries"]):
            if e["buf"] is not None and e["buf"][:4] != b"DXBC":
                bad += 1
            if e["buf"] is None and e["ref"] != 0xFFFF and e["ref"] >= i:
                bad += 1
    print(f"shaders   : {n} staged, {bad} problems")
    if bad:
        failed.append("shaders")

    os.makedirs(SCRATCH, exist_ok=True)
    csv = os.path.join(SCRATCH, "gate_validate.csv").replace("\\", "/")
    out = run([RSX, "-nogui", "-nocachedb", "-validateshaders", "--validateout", csv, pak], cwd=SCRATCH)
    m = re.search(r"(\d+) FLAGGED", out)
    print("rsx       :", m.group(0) if m else "NO RESULT")
    if not m or m.group(1) != "0":
        failed.append("rsx")

    if failed:
        print("GATE FAILED:", ", ".join(failed), "-- do not deploy")
        sys.exit(1)
    print("GATE PASSED")


if __name__ == "__main__":
    main()
