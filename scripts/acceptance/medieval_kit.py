"""M11 acceptance: a medieval kit modelled only in r5ed, scripted through the
CLI's mesh ops (no external DCC), then compiled and baked.

    py scripts/acceptance/medieval_kit.py <r5ed-compile exe> <project dir> [--bake final|preview|none]

Each step prints what it did; any op the core refuses stops the run with the
core's message. The project is created fresh (an existing dir is refused).
"""
import argparse
import json
import math
import subprocess
import sys
import uuid
from pathlib import Path

DEV = "world/dev/dev_gray_512"


class Kit:
    def __init__(self, exe: str, project: Path):
        self.exe = exe
        self.project = project

    def cli(self, *args: str, stdin: str | None = None) -> str:
        r = subprocess.run([self.exe, *args], capture_output=True, text=True, input=stdin)
        if r.returncode != 0:
            raise SystemExit(f"FAILED: r5ed-compile {' '.join(args[:2])}: {(r.stdout + r.stderr).strip()[-600:]}")
        return r.stdout

    def ids(self, out: str) -> list[str]:
        return json.loads(out)["ids"]

    def mesh(self, name: str, spec: dict, position=(0.0, 0.0, 0.0), material: str = DEV) -> str:
        req = {
            "spec": spec,
            "material": material,
            "name": name,
            "editable": True,
            "transform": {"position": list(position), "rotation": [0, 0, 0, 1], "scale": [1, 1, 1]},
        }
        mid = self.ids(self.cli("mesh", str(self.project), "-", stdin=json.dumps(req)))[0]
        print(f"  + {name} {spec['shape']} -> {mid}")
        return mid

    def op(self, obj: str, op: str, sel: dict, params: dict | None = None) -> dict:
        req = {"op": op, "object": obj, "sel": sel, "params": params or {}}
        out = json.loads(self.cli("mesh-op", str(self.project), "-", stdin=json.dumps(req)))
        r = out["report"]
        warn = f"  warnings {r['warnings']}" if r.get("warnings") else ""
        print(f"    {op:16} -> V{r['verts']} E{r['edges']} F{r['faces']}{warn}")
        return out

    def info(self, obj: str) -> dict:
        return json.loads(self.cli("mesh-info", str(self.project), obj))

    def closed(self, obj: str, what: str) -> None:
        i = self.info(obj)
        if not i.get("closed") or i.get("non_manifold") or i.get("degenerate"):
            raise SystemExit(f"FAILED: {what} is not a clean closed mesh: {json.dumps(i)[:400]}")
        print(f"    {what}: closed, euler {i.get('euler')}")

    def apply(self, command: dict) -> list[str]:
        return self.ids(self.cli("apply", str(self.project), "-", stdin=json.dumps(command)))

    def layer(self) -> str:
        return json.loads((self.project / "map.json").read_text(encoding="utf-8"))["layers"][0]["id"]


def q(mode: str, query: dict) -> dict:
    return {"mode": mode, "query": query}


def box(lo, hi) -> dict:
    return {"kind": "box", "min": list(lo), "max": list(hi)}


def tower(k: Kit) -> None:
    print("round tower: cylinder -> loop cuts -> inset windows -> boolean arrow slits -> cylinder UV")
    r, h = 192.0, 768.0
    t = k.mesh("tower", {"shape": "cylinder", "radius": r, "height": h, "segments": 16})
    # One vertical side edge: the loop cut runs around the ring it belongs to.
    k.op(t, "loop_cut", q("edge", box((r - 8, -8, -8), (r + 8, 8, h + 8))), {"cuts": 3})
    # The band between the second and third cut holds the windows.
    band_lo, band_hi = h * 0.5 - 4, h * 0.75 + 4
    k.op(t, "inset", q("face", box((-r - 8, -r - 8, band_lo), (r + 8, r + 8, band_hi))),
         {"thickness": 20, "depth": -12, "individual": True})
    k.closed(t, "tower after inset")
    # Arrow slits: thin boxes through the lower wall, cut out and removed.
    for i, ang in enumerate((0.0, 90.0, 180.0, 270.0)):
        a = math.radians(ang + 11.25)
        cx, cy = math.cos(a) * r, math.sin(a) * r
        cut = k.mesh(f"slit_{i}", {"shape": "box", "size": [64.0, 64.0, 112.0]}, (cx, cy, 200.0))
        # A slit is tall and narrow: scale the cutter's x/y down around its centre.
        k.op(cut, "scale", q("vertex", {"kind": "all"}), {"scale": [0.2, 0.2, 1.0]})
        k.op(t, "boolean", q("face", {"kind": "all"}), {"operation": "difference", "operand": cut})
        k.cli("remove", str(k.project), cut)
    k.closed(t, "tower after slits")
    k.op(t, "uv_cylinder", q("face", {"kind": "all"}), {"axis": "object_z", "scale": "world", "texels_per_tile": 128})


def curtain_wall(k: Kit) -> None:
    print("curved curtain wall: box with a merlon -> bevel -> array fit_length -> curve along a path -> box UV")
    w = k.mesh("wall", {"shape": "box", "size": [128.0, 48.0, 320.0]}, (0.0, 0.0, 0.0))
    k.op(w, "add_primitive", {"mode": "face", "ids": []},
         {"spec": {"shape": "box", "size": [56.0, 48.0, 48.0]},
          "transform": {"position": [0.0, 0.0, 184.0], "rotation": [0, 0, 0, 1], "scale": [1, 1, 1]}})
    k.op(w, "bevel", q("edge", {"kind": "all"}), {"amount": 3, "segments": 2, "profile": 0.5})
    k.closed(w, "wall block")
    k.op(w, "uv_box", q("face", {"kind": "all"}), {"scale": "world", "texels_per_tile": 128})
    # The path the wall follows: a quarter arc from the tower towards the gate.
    pts = [[600.0 + 900.0 * math.sin(t), -900.0 + 900.0 * math.cos(t), 0.0] for t in (0.0, 0.4, 0.8, 1.2, 1.5708)]
    path_obj = {
        "id": str(uuid.uuid4()), "name": "wall_path", "layer": k.layer(), "parent": None,
        "transform": {"position": [0, 0, 0], "rotation": [0, 0, 0, 1], "scale": [1, 1, 1]},
        "hidden": False, "locked": False,
        "kind": {"type": "path", "kind": "spline", "points": pts, "keyvalues": {}},
    }
    path_id = k.apply({"op": "add_objects", "objects": [path_obj]})[0]
    print(f"  + wall_path spline -> {path_id}")
    arc = sum(math.dist(pts[i], pts[i + 1]) for i in range(len(pts) - 1))
    k.op(w, "modifier_add", {"mode": "face", "ids": []},
         {"modifier": {"type": "array", "fit_length": round(arc, 1), "relative_offset": [1, 0, 0], "merge": 0.5}})
    k.op(w, "modifier_add", {"mode": "face", "ids": []}, {"modifier": {"type": "curve", "path": path_id, "axis": "x"}})


def gate(k: Kit) -> None:
    print("arched gate: arch -> bevel -> mirrored pillar -> spun ring -> box UV")
    g = k.mesh("gate_arch", {"shape": "arch", "width": 320.0, "height": 384.0, "thickness": 48.0, "depth": 96.0, "segments": 12},
               (1500.0, 0.0, 0.0))
    k.op(g, "bevel", q("edge", {"kind": "sharp", "min_angle_deg": 60}), {"amount": 4, "segments": 2, "profile": 0.5})
    k.closed(g, "gate arch")
    k.op(g, "uv_box", q("face", {"kind": "all"}), {"scale": "world", "texels_per_tile": 128})
    p = k.mesh("gate_pillar", {"shape": "box", "size": [64.0, 112.0, 448.0]}, (1500.0 + 220.0, 0.0, 0.0))
    k.op(p, "bevel", q("edge", {"kind": "all"}), {"amount": 6, "segments": 3, "profile": 0.5})
    k.op(p, "uv_box", q("face", {"kind": "all"}), {"scale": "world", "texels_per_tile": 128})
    # The mirror's plane passes through the pillar object's origin; the twin
    # stands on the far side of the gate.
    k.op(p, "translate", q("vertex", {"kind": "all"}), {"translate": [0.0, 0.0, 0.0]})
    k.op(p, "modifier_add", {"mode": "face", "ids": []}, {"modifier": {"type": "mirror", "axes": [True, False, False], "mirror_object": g}})
    ring = k.mesh("gate_ring", {"shape": "plane", "size": [16.0, 16.0], "divisions": [1, 1]}, (1500.0, 0.0, 0.0))
    k.op(ring, "translate", q("vertex", {"kind": "all"}), {"translate": [40.0, 0.0, 400.0]})
    k.op(ring, "spin", q("face", {"kind": "all"}),
         {"axis": [0, 1, 0], "center": [0.0, 0.0, 400.0], "angle_deg": 360, "steps": 16, "dupli": False, "merge_ends": True})
    k.closed(ring, "gate ring")


def rock(k: Kit) -> None:
    print("rock outcrop: sphere -> subdivision 2 -> displace")
    rk = k.mesh("rock", {"shape": "sphere", "radius": 160.0, "segments": 12, "rings": 8}, (800.0, 900.0, 60.0))
    k.op(rk, "uv_box", q("face", {"kind": "all"}), {"scale": "world", "texels_per_tile": 128})
    k.op(rk, "modifier_add", {"mode": "face", "ids": []}, {"modifier": {"type": "subdivision", "levels": 2}})
    k.op(rk, "modifier_add", {"mode": "face", "ids": []},
         {"modifier": {"type": "displace", "strength": 48, "mid_level": 0.5, "direction": "normal", "space": "local",
                       "texture": {"kind": "perlin", "scale": 160, "octaves": 3, "seed": 7}}})


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("exe")
    ap.add_argument("project")
    ap.add_argument("--bake", default="final", choices=["final", "preview", "none"])
    a = ap.parse_args()
    project = Path(a.project)
    if project.exists():
        raise SystemExit(f"{project} exists; the kit builds a fresh project")
    k = Kit(a.exe, project)
    k.cli("new", str(project), "--stem", "mp_rr_medieval_kit")
    k.cli("brush", str(project), "--min=-2048,-2048,-64", "--max=3072,2048,0", f"--material={DEV}")
    k.cli("light", str(project), "--kind=sun", "--origin=0,0,2048", "--intensity=1")
    k.cli("entity", str(project), "--class=info_player_start", "--origin=-600,0,16")
    tower(k)
    curtain_wall(k)
    gate(k)
    rock(k)
    print("build")
    k.cli("build", str(project), "--bake", a.bake)
    print("DONE: the kit compiled" + ("" if a.bake == "none" else f" and baked ({a.bake})"))


if __name__ == "__main__":
    main()
