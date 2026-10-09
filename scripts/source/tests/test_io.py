"""F4: entity I/O -- VMF connections and BSP output keys become partition keyvalues."""

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
sys.path.insert(0, str(HERE / "tests"))
REPO = HERE.parents[1]

import bsp2vmf  # noqa: E402
from test_bsp import _build, _full_lumps  # noqa: E402

SURF_VMF = Path(os.environ.get("R5ED_TEST_SURF_CASTLE", "fixture-absent")) / "surf_castle.vmf"
TRIG = Path(os.environ.get("R5ED_TEST_MOMENTUM", "fixture-absent")) / "momentum" / "maps" / "triggertests.bsp"
SURF_BSP = Path(os.environ.get("R5ED_TEST_MOMENTUM", "fixture-absent")) / "momentum" / "maps" / "surf_mom_training.bsp"
SCRATCH = Path(tempfile.gettempdir()) / "r5ed-tests"

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


def _run(exe, *args):
    r = subprocess.run([exe, *args], capture_output=True, text=True, cwd=REPO)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _cube(oid, lo=0, hi=64):
    planes = [f"({lo} {hi} {hi}) ({hi} {hi} {hi}) ({hi} {lo} {hi})",
              f"({lo} {lo} {lo}) ({hi} {lo} {lo}) ({hi} {hi} {lo})",
              f"({lo} {hi} {hi}) ({lo} {lo} {hi}) ({lo} {lo} {lo})",
              f"({hi} {hi} {lo}) ({hi} {lo} {lo}) ({hi} {lo} {hi})",
              f"({hi} {hi} {hi}) ({lo} {hi} {hi}) ({lo} {hi} {lo})",
              f"({hi} {lo} {lo}) ({lo} {lo} {lo}) ({lo} {lo} {hi})"]
    sides = "".join(
        f'side\n{{\n"id" "{oid * 10 + i}"\n"plane" "{p}"\n"material" "DEV/DEV_MEASUREGENERIC01"\n'
        f'"uaxis" "[1 0 0 0] 0.25"\n"vaxis" "[0 -1 0 0] 0.25"\n}}\n' for i, p in enumerate(planes))
    return f'solid\n{{\n"id" "{oid}"\n{sides}}}\n'


def _relay_vmf():
    button = ('entity\n{\n"id" "20"\n"classname" "trigger_multiple"\n"targetname" "button"\n'
              'connections\n{\n"OnTrigger" "door_main,Open,,0,-1"\n}\n' + _cube(21) + '}\n')
    door = ('entity\n{\n"id" "30"\n"classname" "func_movelinear"\n"targetname" "door_main"\n'
            'connections\n{\n"OnFullyOpen" "button\x1bTrigger\x1b\x1b0.5\x1b-1"\n}\n' + _cube(31) + '}\n')
    dropped = "".join(
        f'entity\n{{\n"id" "{40 + i}"\n"classname" "{c}"\n"targetname" "x{i}"\n}}\n'
        for i, c in enumerate(["logic_relay", "func_button", "func_door"]))
    return f'versioninfo\n{{"editorversion" "400"}}\nworld\n{{\n"id" "1"\n"classname" "worldspawn"\n{_cube(2, 256, 320)}}}\n' \
        + button + door + dropped


def _import_report(exe, work, stem, vmf):
    proj = work / "proj"
    _run(exe, "new", str(proj), "--stem", stem)
    return json.loads(_run(exe, "import-vmf", str(proj), str(vmf)))["report"]


def _keyvalues(exe, proj, name):
    rows = json.loads(_run(exe, "list", str(proj)))
    oid = next(r["id"] for r in rows if r["name"] == name)
    return json.loads(_run(exe, "show", str(proj), oid))["kind"]["keyvalues"]


def test_bsp_output_keys_reach_the_vmf():
    work = _workdir("io-bsp")
    lumps = _full_lumps()
    lumps[0] = (b'{\n"classname" "worldspawn"\n}\n'
                b'{\n"classname" "trigger_multiple"\n"model" "*1"\n"targetname" "b1"\n'
                b'"OnTrigger" "relay1,Trigger,,0,-1"\n'
                b'"OnStartTouch" "tone\x1bSetBloomScale\x1b0.2\x1b0\x1b-1"\n}\n')
    src = work / "io.bsp"
    src.write_bytes(_build(lumps))
    bsp2vmf.convert(src, work / "out")
    vmf = (work / "out" / "io.vmf").read_text(encoding="latin1")
    assert '"OnTrigger" "relay1,Trigger,,0,-1"' in vmf
    assert '"OnStartTouch" "tone\x1bSetBloomScale\x1b0.2\x1b0\x1b-1"' in vmf


def test_button_door_relay_round_trips_through_cli():
    work = _workdir("io-relay")
    vmf = work / "relay.vmf"
    vmf.write_text(_relay_vmf())
    exe = _r5ed_compile()
    report = _import_report(exe, work, "io_relay", vmf)
    for c in ("logic_relay", "func_button", "func_door"):
        assert report["dropped_classes"].get(c) == 1, f"{c} stays dropped and listed"
    proj = work / "proj"
    assert _keyvalues(exe, proj, "button")["OnTrigger"] == "door_main,Open,,0,-1"
    assert _keyvalues(exe, proj, "door_main")["OnFullyOpen"] == "button,Trigger,,0.5,-1"


@pytest.mark.skipif(not SURF_VMF.is_file(), reason="surf_castle.vmf fixture absent")
def test_surf_castle_connections_carry_over():
    work = _workdir("io-surf")
    report = _import_report(_r5ed_compile(), work, "io_surf", SURF_VMF)
    assert report["dropped_classes"].get("logic_auto") == 2
    exe = _r5ed_compile()
    rows = json.loads(_run(exe, "list", str(work / "proj")))
    names = [r["name"] for r in rows if r["kind"] == "volume"]
    assert names, "the surf triggers import"
    seen = 0
    for name in names[:6]:
        for k, v in _keyvalues(exe, work / "proj", name).items():
            if k.startswith("On"):
                assert "\x1b" not in v, "ESC normalizes to commas"
                assert len(v.split(",")) == 5, f"{k} keeps target,input,param,delay,refire"
                seen += 1
    assert seen > 0, "surf trigger outputs land on the volumes"


@pytest.mark.skipif(not TRIG.is_file(), reason="triggertests.bsp fixture absent")
def test_momentum_bsp_outputs_import_as_keyvalues():
    work = _workdir("io-trig")
    bsp2vmf.convert(TRIG, work / "out")
    vmf = work / "out" / "triggertests.vmf"
    assert '"OnStartTouch"' in vmf.read_text(encoding="latin1")
    report = _import_report(_r5ed_compile(), work, "io_trig", vmf)
    assert report["volumes"] > 0
    exe = _r5ed_compile()
    rows = json.loads(_run(exe, "list", str(work / "proj")))
    seen = 0
    for r in rows:
        if r["kind"] != "volume":
            continue
        for k, v in _keyvalues(exe, work / "proj", r["name"]).items():
            if k.startswith("On"):
                assert "\x1b" not in v
                assert len(v.split(",")) == 5
                seen += 1
        if seen > 0:
            break
    assert seen > 0, "a converted trigger carries its outputs"


@pytest.mark.skipif(not SURF_BSP.is_file(), reason="surf_mom_training.bsp fixture absent")
def test_momentum_esc_outputs_survive_bsp2vmf():
    work = _workdir("io-surfbsp")
    bsp2vmf.convert(SURF_BSP, work / "out")
    vmf = (work / "out" / "surf_mom_training.vmf").read_text(encoding="latin1")
    assert "\x1b" in vmf, "the ESC-separated lump outputs reach the VMF"
    assert '"OnMapSpawn"' in vmf
