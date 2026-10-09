"""F3: displacements import as meshes; WorldVertexTransition builds on world_blend."""

import tempfile
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
REPO = HERE.parents[1]

import bsp2vmf  # noqa: E402
import matbuild  # noqa: E402
import srcmat  # noqa: E402

MOMENTUM = Path(os.environ.get("R5ED_TEST_MOMENTUM", "fixture-absent")) / "momentum" / "maps"
TRIG = MOMENTUM / "triggertests.bsp"
PLATFORM = Path(os.environ.get("R5ED_TEST_MOMENTUM", "fixture-absent")) / "platform"
SCRATCH = Path(tempfile.gettempdir()) / "r5ed-tests"


def test_worldvertextransition_classifies_to_world_blend():
    keys = {"$basetexture": "a/b", "$basetexture2": "a/c"}
    assert srcmat.classify("worldvertextransition", keys, False) == "world_blend"
    assert srcmat.classify("worldtwotextureblend", keys, False) == "world_blend"
    assert srcmat.classify("lightmapped_4wayblend", keys, False) == "world_blend"
    assert srcmat.classify("lightmappedgeneric", keys, False) == "world_opaque"
    assert srcmat.classify("worldvertextransition", keys, True) == "model_opaque"


class StubBank:
    """Records derive/flat calls and hands out stable guids; no texconv runs."""

    def __init__(self):
        self.entries = {}
        self.guids = {}
        self.next = 0x1000

    def _guid(self, key):
        if key not in self.guids:
            self.guids[key] = self.next
            self.next += 1
        return self.guids[key]

    def derive(self, image, slot):
        return self._guid(("derive", image, slot))

    def flat(self, slot, value):
        return self._guid(("flat", slot, tuple(value)))


BLEND_TYPES = {"0": "_col", "1": "_col", "2": "_nml", "3": "_nml", "4": "_gls", "5": "_gls",
               "6": "_ao", "7": "_ao", "8": "_det", "9": "_det",
               "10": "_nml", "11": "_nml", "12": "_bm"}


def _blend_templates(tmp_path):
    root = tmp_path / "templates"
    root.mkdir()
    tpl = {"name": "material/world/grass/fake_wld_bm", "shaderType": "wld_bm",
           "$textures": {str(i): f"0x{i + 1:016X}" for i in range(13)},
           "$textureTypes": BLEND_TYPES}
    name = "divided_meadows_grass_green_01_to_divided_rocky_dirt_01_wld_bm.json"
    (root / name).write_text(json.dumps(tpl))
    return root


def _build_blend(tmp_path, **kw):
    templates = _blend_templates(tmp_path)
    assets = tmp_path / "assets"
    bank, files = StubBank(), []
    args = {"albedo": "a", "normal": "n#nm", "albedo2": "b", "normal2": "m#nm",
            "surface": "dirt", "files": files}
    args.update(kw)
    matbuild.build(assets, templates, {}, bank, "world_blend",
                   "world/source/test/blend", **args)
    doc = json.loads((assets / "material/world/source/test/blend_wld_bm.json").read_text())
    return doc, bank, files


def test_world_blend_build_wires_two_albedos(tmp_path):
    doc, bank, files = _build_blend(tmp_path)
    tex = doc["$textures"]
    assert tex["0"] == f"0x{bank.guids[('derive', 'a', 'albedoTexture')]:016X}"
    assert tex["1"] == f"0x{bank.guids[('derive', 'b', 'albedoTexture')]:016X}"
    assert tex["0"] != tex["1"]
    assert tex["2"] == f"0x{bank.guids[('derive', 'n#nm', 'normalTexture')]:016X}"
    assert tex["3"] == f"0x{bank.guids[('derive', 'm#nm', 'normalTexture')]:016X}"
    flat_blend = bank.guids[("flat", "layerBlendTexture", (255, 255, 255, 255))]
    assert tex["12"] == f"0x{flat_blend:016X}"
    assert doc["surfaceProp"] == "dirt"
    assert files[-1]["_type"] == "matl"


def test_world_blend_without_a_second_layer_repeats_the_first(tmp_path):
    doc, bank, _ = _build_blend(tmp_path, albedo2=None, normal2=None)
    assert doc["$textures"]["1"] == doc["$textures"]["0"]
    assert doc["$textures"]["3"] == doc["$textures"]["2"]


_EXE = None


def _r5ed_compile():
    global _EXE
    if _EXE is not None:
        return _EXE
    base = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
    exe = base / "debug" / ("r5ed-compile.exe" if sys.platform == "win32" else "r5ed-compile")
    if not exe.is_file():
        import shutil as _sh
        if _sh.which("cargo") is None:
            pytest.skip("cargo is not on PATH")
        subprocess.run(["cargo", "build", "-q", "-p", "r5ed-compile"], cwd=REPO, check=False)
    if not exe.is_file():
        pytest.skip("r5ed-compile binary unavailable")
    _EXE = str(exe)
    return _EXE


def _workdir(name):
    d = SCRATCH / "pytest" / name
    shutil.rmtree(d, ignore_errors=True)
    d.mkdir(parents=True, exist_ok=True)
    return d


def _import_vmf(exe, work, stem, vmf):
    proj = work / "proj"
    r = subprocess.run([exe, "new", str(proj), "--stem", stem],
                       capture_output=True, text=True, cwd=REPO)
    assert r.returncode == 0, r.stderr
    r = subprocess.run([exe, "import-vmf", str(proj), str(vmf)],
                       capture_output=True, text=True, cwd=REPO)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)["report"]


def _disp_vmf():
    nrows = ["0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"] * 5
    drows = ["0 0 0 0 0"] * 5
    orows = ["0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"] * 5
    arows = ["0 0 0 0 0", "0 0 0 0 0", "0 0 1 0 0", "0 0 0 0 0", "0 0 0 0 0"]

    def rows(vals):
        return "\n".join(f'"row{i}" "{v}"' for i, v in enumerate(vals))

    top = ('side\n{\n"id" "2"\n"plane" "(0 0 32) (64 0 32) (64 64 32)"\n'
           '"material" "TEST/WALL"\n"uaxis" "[1 0 0 0] 0.25"\n"vaxis" "[0 -1 0 0] 0.25"\n'
           '"rotation" "0"\n"lightmapscale" "16"\n'
           'dispinfo\n{\n"power" "2"\n"startposition" "[0 0 32]"\n"elevation" "0"\n"subdiv" "0"\n'
           f'normals\n{{\n{rows(nrows)}\n}}\ndistances\n{{\n{rows(drows)}\n}}\n'
           f'offsets\n{{\n{rows(orows)}\n}}\nalphas\n{{\n{rows(arows)}\n}}\n}}\n}}\n')
    sides = [top]
    for i, plane in enumerate(["(0 0 0) (64 64 0) (64 0 0)", "(64 0 0) (64 64 0) (64 64 32)",
                               "(0 0 0) (0 64 32) (0 64 0)", "(0 0 0) (64 0 0) (64 0 32)",
                               "(0 64 0) (0 64 32) (64 64 32)"], start=3):
        sides.append(f'side\n{{\n"id" "{i}"\n"plane" "{plane}"\n"material" "TEST/WALL"\n}}\n')
    return "solid\n{\n\"id\" \"1\"\n" + "".join(sides) + "}\n"


def test_synthetic_dispinfo_imports_as_a_mesh():
    work = _workdir("f3disp")
    vmf = work / "disp.vmf"
    vmf.write_text(_disp_vmf())
    report = _import_vmf(_r5ed_compile(), work, "f3disp", vmf)
    assert report["meshes"] == 1
    assert report["brushes"] == 1


@pytest.mark.skipif(not TRIG.is_file(), reason="triggertests.bsp fixture absent")
def test_triggertests_disps_import_as_meshes():
    work = _workdir("f3trig")
    stats = bsp2vmf.convert(TRIG, work / "out")
    assert stats["attached"] == 9
    report = _import_vmf(_r5ed_compile(), work, "triggertests", work / "out" / "triggertests.vmf")
    assert report["meshes"] == stats["attached"]
    assert report["brushes"] > 0


@pytest.mark.skipif(not PLATFORM.is_dir(), reason="Momentum platform fixture absent")
def test_fixture_blend_vmt_resolves_two_textures():
    import vmt
    content = vmt.Content([PLATFORM])
    shader, keys = content.material("dev/dev_blendmeasure")
    assert shader == "worldvertextransition"
    assert srcmat.classify(shader, keys, False) == "world_blend"
    assert content.texture(keys["$basetexture"]) is not None
    assert content.texture(keys["$basetexture2"]) is not None
