"""Source skybox tests: synthetic six-colour skies built in the test, plus fixture checks."""

import os
import struct
import sys
from pathlib import Path

import numpy as np
import pytest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import skybox  # noqa: E402
import vmt  # noqa: E402
import vtf  # noqa: E402

SURF = Path(os.environ.get("R5ED_TEST_SURF_CASTLE", "fixture-absent")) / "csgo"
HL2_DIR = Path(os.environ.get("R5ED_TEST_MOMENTUM", "fixture-absent")) / "mount" / "hl2_dir.vpk"
TEMPLATES = Path(os.environ.get("R5ED_SCRATCH", "fixture-absent")) / "templates"

COLOURS = {"bk": (255, 0, 0, 255), "dn": (0, 255, 0, 255), "ft": (0, 0, 255, 255),
           "lf": (255, 255, 0, 255), "rt": (255, 0, 255, 255), "up": (0, 255, 255, 255)}


def _vtf_rgba(w, h, pixels):
    head = bytearray(80)
    head[0:4] = b"VTF\0"
    struct.pack_into("<IIIHHIHH", head, 4, 7, 2, 80, w, h, 0, 1, 0)
    struct.pack_into("<i", head, 52, vtf.RGBA8888)
    head[56] = 1
    struct.pack_into("<i", head, 57, -1)
    struct.pack_into("<H", head, 63, 1)
    return bytes(head) + pixels.tobytes()


def _write_sky(root, sky="tides"):
    for face, colour in COLOURS.items():
        px = np.zeros((4, 4, 4), np.uint8)
        px[:, :] = colour
        (root / "materials" / "skybox").mkdir(parents=True, exist_ok=True)
        (root / "materials" / "skybox" / f"{sky}{face}.vtf").write_bytes(_vtf_rgba(4, 4, px))


def test_sky_names_follow_the_content_tree():
    assert skybox.material("Tides", "bk") == "world/source/skybox/tidesbk"
    assert skybox.face_path("Tides", "up") == "skybox/tidesup"
    assert skybox.vtf_rel("tides", "dn") == "materials/skybox/tidesdn.vtf"
    for bad in ("", "a/b", "..", ".", "x" * 129):
        with pytest.raises(skybox.SkyboxError):
            skybox.normalize_sky(bad)


def test_faces_found_through_plain_folders(tmp_path):
    _write_sky(tmp_path)
    content = vmt.Content([tmp_path])
    found = skybox.faces_of(content, "tides")
    assert sorted(found) == sorted(skybox.FACES)
    skybox.check_faces(found, "tides")


def test_missing_faces_raise_naming_the_file(tmp_path):
    _write_sky(tmp_path)
    (tmp_path / "materials" / "skybox" / "tidesup.vtf").unlink()
    (tmp_path / "materials" / "skybox" / "tidesdn.vtf").unlink()
    content = vmt.Content([tmp_path])
    found = skybox.faces_of(content, "tides")
    assert sorted(found) == ["bk", "ft", "lf", "rt"]
    with pytest.raises(skybox.SkyboxError, match=r"materials/skybox/tidesdn\.vtf.*materials/skybox/tidesup\.vtf"):
        skybox.check_faces(found, "tides")


def test_each_face_decodes_to_its_own_colour(tmp_path):
    _write_sky(tmp_path)
    content = vmt.Content([tmp_path])
    for face, colour in COLOURS.items():
        arr, hd = vtf.read(content.texture(skybox.face_path("tides", face)))
        assert (hd["width"], hd["height"]) == (4, 4)
        assert (arr == np.asarray(colour, np.uint8)).all(), f"{face} decoded to another colour"


@pytest.mark.skipif(not SURF.is_dir(), reason="surf_castle fixture absent")
def test_surf_castle_skyname_has_six_faces():
    text = (SURF.parent / "surf_castle.vmf").read_text(encoding="utf-8", errors="replace")
    line = next(l for l in text.splitlines() if "skyname" in l)
    sky = line.split('"')[-2]
    assert sky == "tides"
    content = vmt.Content([SURF])
    found = skybox.faces_of(content, sky)
    skybox.check_faces(found, sky)
    for face in skybox.FACES:
        assert vtf.size(found[face]) == (512, 512)
        shader, keys = content.material(f"skybox/{sky}{face}")
        assert shader == "unlitgeneric", "sky faces draw unlit"


@pytest.mark.skipif(not HL2_DIR.is_file(), reason="hl2_dir.vpk fixture absent")
def test_hl2_sky_faces_resolve_from_vpk():
    content = vmt.Content([HL2_DIR])
    found = skybox.faces_of(content, "sky_day01_01")
    skybox.check_faces(found, "sky_day01_01")
    assert vtf.size(found["ft"])[0] > 0


@pytest.mark.skipif(not SURF.is_dir() or not TEMPLATES.is_dir(), reason="surf_castle or templates absent")
def test_srcmat_builds_six_emissive_sky_materials(tmp_path, monkeypatch):
    import srcmat  # noqa: E402

    report = {"materials": [], "skyname": "tides"}
    rep = tmp_path / "import_report.json"
    rep.write_text('{"report": %s}' % __import__("json").dumps(report))
    assets = tmp_path / "assets"
    assets.mkdir()
    argv = ["srcmat.py", str(TEMPLATES), str(assets), "--content", str(SURF),
            "--import-report", str(rep), "--stem", "tides", "--out-dir", str(tmp_path / "out"),
            "--sizes-out", str(tmp_path / "sizes.json")]
    monkeypatch.setattr(sys, "argv", argv)
    srcmat.main()
    names = [skybox.material("tides", f) for f in skybox.FACES]
    for name in names:
        rel = f"material/{name}_wldc"
        assert (assets / f"{rel}.json").is_file(), f"{rel} was not built"


def test_srcmat_reports_a_bad_skyname_instead_of_failing(tmp_path, monkeypatch, capsys):
    import srcmat  # noqa: E402

    rep = tmp_path / "import_report.json"
    rep.write_text('{"report": {"materials": [], "skyname": "../x"}}')
    assets = tmp_path / "assets"
    assets.mkdir()
    argv = ["srcmat.py", str(tmp_path), str(assets), "--content", str(tmp_path),
            "--import-report", str(rep), "--stem", "bad", "--out-dir", str(tmp_path / "out"),
            "--sizes-out", str(tmp_path / "sizes.json")]
    monkeypatch.setattr(sys, "argv", argv)
    srcmat.main()
    assert "bad_skyname" in capsys.readouterr().out
