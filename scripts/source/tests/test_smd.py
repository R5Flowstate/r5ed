"""Phase F9: SMD/DMX sources compile with Portal 2 studiomdl into MDL v49."""

import tempfile
import json
import shutil
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import smd2mdl  # noqa: E402
import srcprops  # noqa: E402

SCRATCH = Path(tempfile.gettempdir()) / "r5ed-tests" / "pytest"
needs_studio = pytest.mark.skipif(not smd2mdl.STUDIOMDL.is_file(), reason="portal 2 studiomdl absent")
needs_rmdlconv = pytest.mark.skipif(not srcprops.RMDLCONV.is_file(), reason="rmdlconv absent")

CORNERS = [(-16, -16, -16), (16, -16, -16), (16, 16, -16), (-16, 16, -16),
           (-16, -16, 16), (16, -16, 16), (16, 16, 16), (-16, 16, 16)]
FACES = [((0, 0, -1), (0, 1, 2, 3)), ((0, 0, 1), (4, 5, 6, 7)),
         ((0, -1, 0), (0, 1, 5, 4)), ((0, 1, 0), (3, 2, 6, 7)),
         ((-1, 0, 0), (0, 3, 7, 4)), ((1, 0, 0), (1, 5, 6, 2))]
UVS = ((0, 0), (1, 0), (1, 1), (0, 1))


def cube_smd(material="cube"):
    lines = ["version 1", "nodes", '0 "root" -1', "end",
             "skeleton", "time 0", "0 0 0 0 0 0 0", "end", "triangles"]
    for normal, quad in FACES:
        for tri in ((0, 1, 2), (0, 2, 3)):
            lines.append(material)
            for corner, uv in zip(tri, (UVS[tri[0]], UVS[tri[1]], UVS[tri[2]])):
                p = CORNERS[quad[corner]]
                lines.append("0 %g %g %g %g %g %g %g %g" % (*p, *normal, *uv))
    lines.append("end")
    return "\n".join(lines) + "\n"


def test_cube_parses_to_twelve_tris_one_material(tmp_path):
    p = tmp_path / "cube.smd"
    p.write_text(cube_smd())
    info = smd2mdl.parse_smd(p)
    assert (info.nodes, info.frames) == (1, 1)
    assert (info.triangles, info.verts) == (12, 36)
    assert info.materials == ("cube",)


def test_bad_smds_name_the_file(tmp_path):
    good = cube_smd()
    cases = [
        ("bad.smd", "SMD version 1\n"),
        ("short.smd", good.split("end")[0]),
        ("noskel.smd", good.replace("skeleton\ntime 0\n0 0 0 0 0 0 0\nend\n", "")),
        ("float.smd", good.replace("0 -16 -16 -16 0 0 -1 0 0",
                                   "0 -16 -16 -16 0 0 -1 0 banana", 1)),
        ("bone.smd", good.replace("0 -16 -16 -16", "7 -16 -16 -16", 1)),
        ("matonce.smd", good.replace("\ncube\n0 ", "\n0 ", 1)),
        ("junk.smd", good + "leftover\n"),
        ("empty.smd", good.replace("cube\n0 ", "cube\n", 1)),
    ]
    for fname, text in cases:
        p = tmp_path / fname
        p.write_text(text)
        with pytest.raises(smd2mdl.SmdError, match=fname.replace(".", r"\.")):
            smd2mdl.parse_smd(p)
    p = tmp_path / "bin.smd"
    p.write_bytes(b"\xff\xfe\x00bad")
    with pytest.raises(smd2mdl.SmdError, match=r"bin\.smd"):
        smd2mdl.parse_smd(p)
    with pytest.raises(smd2mdl.SmdError, match=r"gone\.smd"):
        smd2mdl.parse_smd(tmp_path / "gone.smd")


def test_too_many_nodes_names_the_file(tmp_path):
    nodes = "\n".join('%d "b%d" 0' % (i, i) for i in range(smd2mdl.MAX_NODES + 1))
    p = tmp_path / "many.smd"
    p.write_text("version 1\nnodes\n" + nodes + "\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\n"
                 "triangles\nm\n0 0 0 0 0 0 1 0 0\n0 1 0 0 0 0 1 1 0\n0 1 1 0 0 0 1 1 1\nend\n")
    with pytest.raises(smd2mdl.SmdError, match=r"many\.smd"):
        smd2mdl.parse_smd(p)


def _dmx(head):
    return head + '\n"DmeModel" { "id" "element" "00000000000000000000000000000000" }\n'


def test_dmx_headers_accept_known_encodings(tmp_path):
    heads = [
        ("<!-- dmx encoding keyvalues2 1 format model 1 -->", ("keyvalues2", 1, "model", 1)),
        ("<!-- dmx encoding binary 5 format model 1 -->", ("binary", 5, "model", 1)),
        ("<!-- DMXVersion binary_v2 -->", ("binary_proto", 2, "", 0)),
    ]
    for i, (head, want) in enumerate(heads):
        p = tmp_path / f"m{i}.dmx"
        p.write_text(_dmx(head))
        assert tuple(smd2mdl.check_dmx(p)) == want


def test_bad_dmx_names_the_file(tmp_path):
    p = tmp_path / "bad.dmx"
    for text in ["no header here\n",
                 "<!-- dmx encoding rot13 1 format model 1 -->\n",
                 "<!-- dmx encoding keyvalues2 9 format model 1 -->\n",
                 "<!-- dmx encoding keyvalues2 1 -->\n"]:
        p.write_text(text)
        with pytest.raises(smd2mdl.SmdError, match=r"bad\.dmx"):
            smd2mdl.check_dmx(p)
    p.write_bytes(b"x" * (smd2mdl.MAX_DMX_HEAD + 8))
    with pytest.raises(smd2mdl.SmdError, match=r"bad\.dmx"):
        smd2mdl.check_dmx(p)


def test_qc_lists_body_sequence_and_collision(tmp_path):
    qc = smd2mdl.write_qc(tmp_path / "m.qc", modelname="f9/cube.mdl",
                           bodies=[("Body", "cube.smd")], sequence="cube.smd",
                           collision="cube.smd", cdmaterials=["models/props"])
    text = qc.read_text()
    assert '$modelname "f9/cube.mdl"' in text
    assert '$body "Body" "cube.smd"' in text
    assert '$sequence idle "cube.smd"' in text
    assert '$collisionmodel "cube.smd"' in text and "$concave" in text
    plain = smd2mdl.write_qc(tmp_path / "p.qc", modelname="cube.mdl",
                              bodies=[("Body", "cube.dmx")], sequence="cube.dmx",
                              staticprop=False)
    assert "$staticprop" not in plain.read_text()
    assert "$collisionmodel" not in plain.read_text()
    for bad in ["../evil.mdl", "/abs/cube.mdl", "cube.txt", ""]:
        with pytest.raises(smd2mdl.SmdError):
            smd2mdl.write_qc(tmp_path / "m.qc", modelname=bad,
                              bodies=[("Body", "cube.smd")], sequence="cube.smd")


def _workdir(name):
    d = SCRATCH / name
    shutil.rmtree(d, ignore_errors=True)
    d.mkdir(parents=True, exist_ok=True)
    return d


@needs_studio
def test_cube_smd_compiles_to_mdl_v49():
    work = _workdir("f9cube")
    src = work / "cube.smd"
    src.write_text(cube_smd())
    res = smd2mdl.build([src], work / "out", modelname="f9/cube.mdl",
                        collision="cube.smd")
    assert res["mdl"].is_file() and res["mdl"].stat().st_size > 0
    assert (work / "out" / "studiomdl.log").is_file()
    assert res["materials"] == ["cube"]
    data = res["mdl"].read_bytes()
    assert srcprops.mdl_version(data, "cube.mdl") == 49
    names, _, _ = srcprops.mdl_textures(data, "cube.mdl")
    assert names == ["cube"]
    assert res["mdl"].with_name("cube.vvd").is_file()
    assert res["mdl"].with_name("cube.phy").is_file()


@needs_studio
@needs_rmdlconv
def test_compiled_cube_converts_through_srcprops():
    work = _workdir("f9props")
    src = work / "cube.smd"
    src.write_text(cube_smd())
    res = smd2mdl.build([src], work / "out", modelname="f9/cube.mdl",
                        collision="cube.smd")
    root = work / "content" / "models" / "f9"
    root.mkdir(parents=True)
    for f in res["files"]:
        shutil.copyfile(f, root / f.name)
    (work / "report.json").write_text(json.dumps({"models": ["models/f9/cube.mdl"]}))
    argv = ["srcprops.py", str(work / "report.json"), str(work / "props"),
            "--content", str(work / "content"),
            "--assets", str(work / "assets"), "--assets-server", str(work / "assets_server"),
            "--stem", "f9test", "--out-dir-server", str(work / "out_server")]
    old = sys.argv
    sys.argv = argv
    try:
        srcprops.main()
    finally:
        sys.argv = old
    report = json.loads((work / "props" / "props_report.json").read_text())
    assert report["staged"] == ["mdl/source/f9/cube.rmdl"]
    assert report["failed"] == [] and report["missing"] == []
    assert (work / "assets" / "mdl" / "source" / "f9" / "cube.rmdl").is_file()
    assert (work / "assets_server" / "mdl" / "source" / "f9" / "cube.rmdl").is_file()
