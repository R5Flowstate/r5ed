"""Close the shader refs of an r5ed-packaged map pak.

Every staged material names a shader set; world shader sets live only in the
map paks that ship them, so the map's own archive must carry the set, its VS
and PS, and the parent of any child shader. Each is read straight out of a
donor pak (pak_msw) and appended to the package manifest, then repak runs again.

usage: close_shaders.py <package_dir> <stem> <asset index json> <game root> <repak.exe>
"""
import glob
import json
import os
import struct
import subprocess
import sys
from pathlib import Path
from collections import defaultdict

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import r5edenv  # noqa: E402
import pak_msw  # noqa: E402

pkg, stem, index_path, game_root, repak = sys.argv[1:6]
man_path = os.path.join(pkg, f"{stem}.json")
man = json.load(open(man_path))
idx = json.load(open(index_path))
rows = idx["assets"] if isinstance(idx, dict) else idx
paks_of = defaultdict(list)
for r in rows:
    paks_of[r["guid"]].append(r["pak"])


def g_int(v):
    return int(v, 16) if isinstance(v, str) else int(v)


staged = {g_int(f["$guid"]) for f in man["files"] if "$guid" in f}


def resident(g):
    return any(p.startswith(("common", "startup", "ui")) for p in paks_of.get(g, ()))


DECOMP = os.path.abspath(os.environ.get("H2_PAK_SCRATCH", "paks_dec"))
RSX = r5edenv.get("R5ED_RSX", required=False) or "rsx"


def plain_pak(path):
    """A compressed stock pak, decompressed once into scratch by RSX; pak_msw
    reads page data straight from the file."""
    with open(path, "rb") as f:
        flags = struct.unpack_from("<H", f.read(8), 6)[0]
    if not flags & 0x300:
        return path
    os.makedirs(DECOMP, exist_ok=True)
    copy = os.path.join(DECOMP, os.path.basename(path))
    out = copy + ".dec.rpak"
    if not os.path.isfile(out):
        import shutil
        shutil.copyfile(path, copy)
        subprocess.run([RSX, "-nogui", "-nocachedb", "-decompresspak", copy.replace("\\", "/")],
                       cwd=DECOMP, capture_output=True)
    return out


def donor_paks(g):
    out = []
    for p in sorted((n for n in paks_of.get(g, ()) if not n.startswith(stem)), key=lambda n: os.path.getsize(os.path.join(game_root, "paks", "Win64", n))
                    if os.path.isfile(os.path.join(game_root, "paks", "Win64", n)) else 1 << 62):
        path = os.path.join(game_root, "paks", "Win64", p)
        if os.path.isfile(path):
            out.append(plain_pak(path))
    return out


want = set()
for j in glob.glob(os.path.join(pkg, "material", "**", "*.json"), recursive=True):
    ss = json.load(open(j)).get("shaderSet")
    if ss:
        want.add(("shds", g_int(ss)))

n_before = len(man["files"])
rsx_jobs = defaultdict(list)
added = []
queue = sorted(want)
seen = set()
while queue:
    kind, g = queue.pop(0)
    if (kind, g) in seen or g == 0:
        continue
    seen.add((kind, g))
    if g in staged or resident(g):
        continue
    donors = donor_paks(g)
    tag = "0x%016X" % g
    done = False
    for pak in donors:
        # Headers (set VS/PS guids, a child's parent) read fine from any pak;
        # bytecode out of a decompressed Respawn pak does not, so RSX exports it.
        via_rsx = pak.endswith(".dec.rpak")
        if kind == "shds":
            ss = pak_msw.read_shaderset(pak, g)
            if ss is None:
                continue
            if via_rsx:
                rsx_jobs[pak[:-len(".dec.rpak")]].append(("shds", g))
            else:
                os.makedirs(os.path.join(pkg, "shaderset"), exist_ok=True)
                pak_msw.write_shaderset_msw(ss, os.path.join(pkg, "shaderset", tag + ".msw"))
            man["files"].append({"_type": "shds", "_path": "shaderset/" + tag, "$guid": tag})
            queue += [("shdr", ss["vertexShaderGuid"]), ("shdr", ss["pixelShaderGuid"])]
        else:
            sh = pak_msw.read_shader(pak, g)
            if sh is None:
                continue
            if sh["parent"] is not None:
                man["files"].append({"_type": "shdr", "_path": "shader/" + tag, "$guid": tag,
                                     "$parentShader": sh["parent"]})
                queue.append(("shdr", sh["parent"]))
            else:
                if via_rsx:
                    rsx_jobs[pak[:-len(".dec.rpak")]].append(("shdr", g))
                else:
                    os.makedirs(os.path.join(pkg, "shader"), exist_ok=True)
                    pak_msw.write_shader_msw(sh, os.path.join(pkg, "shader", tag + ".msw"))
                man["files"].append({"_type": "shdr", "_path": "shader/" + tag, "$guid": tag})
        staged.add(g)
        added.append((kind, tag, os.path.basename(pak) + (" (rsx)" if via_rsx else "")))
        done = True
        break
    if not done:
        print(f"NO SOURCE {kind} {tag} paks={paks_of.get(g)}")


# RSX exports the bytecode assets that came from compressed donors, one run per pak.
import shutil
for copy, jobs in rsx_jobs.items():
    out_dir = copy + ".rsx"
    shutil.rmtree(out_dir, ignore_errors=True)
    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, "guids.txt"), "w") as f:
        f.write("".join("0x%016X\n" % g for _, g in jobs))
    subprocess.run([RSX, "-nogui", "-nocachedb", "-export", "--exporttypes", "shdr,shds",
                    "--exportguids", os.path.join(out_dir, "guids.txt"), "--exportdir", out_dir,
                    os.path.join(game_root, "paks", "Win64", os.path.basename(copy))],
                   cwd=DECOMP, capture_output=True)
    for kind, g in jobs:
        sub = "shader" if kind == "shdr" else "shaderset"
        src = os.path.join(out_dir, sub, "0x%X.msw" % g)
        if not os.path.isfile(src):
            sys.exit(f"RSX exported nothing for {kind} 0x{g:016X} from {os.path.basename(copy)}")
        os.makedirs(os.path.join(pkg, sub), exist_ok=True)
        shutil.copyfile(src, os.path.join(pkg, sub, "0x%016X.msw" % g))

# Every asset must follow the assets it names: the engine checks each reference
# against the assets registered so far while it loads the pak in table order.
def deps_of(f):
    t, path = f["_type"], f["_path"]
    out = []
    if t == "matl":
        j = os.path.join(pkg, path[:-len(".rpak")] + ".json") if path.endswith(".rpak") else os.path.join(pkg, path + ".json")
        if os.path.isfile(j):
            d = json.load(open(j))
            for k in ("$colpassMaterial", "$depthShadowMaterial", "$depthPrepassMaterial", "$depthVSMMaterial",
                      "$depthShadowTightMaterial", "shaderSet", "$textureAnimation"):
                if d.get(k):
                    out.append(g_int(d[k]))
            out += [g_int(v) for v in d.get("$textures", {}).values() if v]
    elif t == "shds":
        b = open(os.path.join(pkg, path + ".msw"), "rb").read()
        out += [struct.unpack_from("<Q", b, 5)[0], struct.unpack_from("<Q", b, 13)[0]]
    elif t == "shdr" and "$parentShader" in f:
        out.append(g_int(f["$parentShader"]))
    return out


files = man["files"]
guid_at = {g_int(f["$guid"]): i for i, f in enumerate(files) if "$guid" in f}
rank = {"txtr": 0, "shdr": 1, "shds": 2, "matl": 3}
order, state = [], {}


def visit(i):
    if state.get(i) == 2:
        return
    if state.get(i) == 1:
        return  # a cycle (none expected); keep the current order
    state[i] = 1
    for g in deps_of(files[i]):
        j = guid_at.get(g)
        if j is not None and j != i:
            visit(j)
    state[i] = 2
    order.append(i)


for i in sorted(range(len(files)), key=lambda i: (rank.get(files[i]["_type"], 4), i)):
    visit(i)
man["files"] = [files[i] for i in order]
late = sum(1 for k, i in enumerate(order) for g in deps_of(files[i]) if g in guid_at and order.index(guid_at[g]) > k)
print("forward references after ordering:", late)
json.dump(man, open(man_path, "w"), indent=1)
for a in added:
    print("added", *a)
# Repak reuses the stream offsets of an archive already at the output path and
# appends to the starpak there without checking the data, so both start fresh.
out_dir = man.get("outputDir", pkg)
stale = [os.path.join(out_dir, man["name"] + ext) for ext in (".rpak", ".rpak.pre_cbremap")]
if man.get("streamFileMandatory"):
    stale.append(os.path.join(out_dir, os.path.basename(man["streamFileMandatory"].replace("\\", "/"))))
for path in stale:
    if os.path.exists(path):
        os.remove(path)
out = subprocess.run([repak, man_path], capture_output=True, text=True)
print("repak exit", out.returncode)
if out.returncode:
    print(out.stdout[-3000:], out.stderr[-2000:])
