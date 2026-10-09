"""Phase F6: older MDL (v44-48) version routing, header bounds checks, and an
HL2 model out of Momentum's hl2_dir.vpk converting with its sequences."""

import tempfile
import os
import json
import shutil
import struct
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

import srcprops  # noqa: E402

HL2_DIR = Path(os.environ.get("R5ED_TEST_MOMENTUM", "fixture-absent")) / "mount" / "hl2_dir.vpk"
SCRATCH = Path(tempfile.gettempdir()) / "r5ed-tests" / "pytest"
needs_hl2 = pytest.mark.skipif(not HL2_DIR.is_file(), reason="hl2_dir.vpk fixture absent")
needs_rmdlconv = pytest.mark.skipif(not srcprops.RMDLCONV.is_file(), reason="rmdlconv absent")


def _mdl(version, textures=("BrickWall",), cds=("models\\props/",), seqs=3):
    blob = bytearray(srcprops.MIN_HDR)
    blob[0:4] = b"IDST"
    struct.pack_into("<i", blob, 4, version)
    struct.pack_into("<3f", blob, 0x68, -10, -10, -10)
    struct.pack_into("<3f", blob, 0x74, 10, 10, 10)
    struct.pack_into("<i", blob, 0xBC, seqs)
    tex_index, cd_index = len(blob), len(blob) + len(textures) * srcprops.TEXTURE_STRUCT
    struct.pack_into("<iiii", blob, 0xCC, len(textures), tex_index, len(cds), cd_index)
    blob += b"\0" * (len(textures) * srcprops.TEXTURE_STRUCT + len(cds) * 4)
    strings = b""
    for i, tex in enumerate(textures):
        pos = len(blob) + len(strings)
        strings += tex.encode() + b"\0"
        struct.pack_into("<i", blob, tex_index + i * srcprops.TEXTURE_STRUCT, pos - (tex_index + i * 64))
    for i, cd in enumerate(cds):
        pos = len(blob) + len(strings)
        strings += cd.encode() + b"\0"
        struct.pack_into("<i", blob, cd_index + i * 4, pos)
    blob += strings
    struct.pack_into("<i", blob, 0x4C, len(blob))
    return bytes(blob)


def test_versions_route_to_the_right_batch():
    assert srcprops.classify_version(49) == "v49"
    for v in (44, 45, 46, 47, 48):
        assert srcprops.classify_version(v) == "legacy"
    for v in (0, 43, 50, 253):
        assert srcprops.classify_version(v) == "unsupported"


def test_synthetic_v44_header_parses():
    data = _mdl(44)
    assert srcprops.mdl_version(data, "t.mdl") == 44
    assert srcprops.mdl_sequences(data, "t.mdl") == 3
    names, cds, tex_index = srcprops.mdl_textures(data, "t.mdl")
    assert (names, cds) == (["BrickWall"], ["models\\props/"])
    assert not srcprops.is_large(data)
    renamed = srcprops.rename_textures(data, ["models/source/brick_rgdp"], tex_index)
    assert srcprops.mdl_textures(renamed, "t.mdl")[0] == ["models/source/brick_rgdp"]


def test_bad_mdls_name_the_file():
    good = _mdl(48)
    cases = [
        (b"nope", "t.mdl"),
        (good[:100], "short.mdl"),
        (b"XXXX" + good[4:], "magic.mdl"),
    ]
    tape = bytearray(good)
    struct.pack_into("<i", tape, 0xCC, 1 << 30)
    cases.append((bytes(tape), "manytex.mdl"))
    tape = bytearray(good)
    struct.pack_into("<i", tape, 0xD0, len(good) + 500)
    cases.append((bytes(tape), "texoff.mdl"))
    tape = bytearray(good)
    struct.pack_into("<i", tape, struct.unpack_from("<i", good, 0xD4)[0], len(good) + 500)
    cases.append((bytes(tape), "cdoff.mdl"))
    cases.append((good[: struct.unpack_from("<i", good, 0xD0)[0]] + b"\1" * 64, "noname.mdl"))
    for data, name in cases:
        with pytest.raises(ValueError, match=name.replace(".", r"\.")):
            srcprops.mdl_textures(data, name)
    with pytest.raises(ValueError, match="short.mdl"):
        srcprops.mdl_version(good[:4], "short.mdl")
    with pytest.raises(ValueError):
        srcprops.is_large(good[:8])
    with pytest.raises(ValueError):
        srcprops.rename_textures(good, ["x"], len(good) + 9)


def _workdir(name):
    d = SCRATCH / name
    shutil.rmtree(d, ignore_errors=True)
    d.mkdir(parents=True, exist_ok=True)
    return d


def _run_main(work, models):
    (work / "report.json").write_text(json.dumps({"models": models}))
    argv = ["srcprops.py", str(work / "report.json"), str(work / "props"),
            "--content", str(HL2_DIR),
            "--assets", str(work / "assets"), "--assets-server", str(work / "assets_server"),
            "--stem", "f6test", "--out-dir-server", str(work / "out_server")]
    old = sys.argv
    sys.argv = argv
    try:
        srcprops.main()
    finally:
        sys.argv = old
    return json.loads((work / "props" / "props_report.json").read_text())


@needs_hl2
@needs_rmdlconv
def test_hl2_v48_crow_converts_with_sequences():
    work = _workdir("f6crow")
    report = _run_main(work, ["models/crow.mdl"])
    assert report["staged"] == ["mdl/source/crow.rmdl"]
    assert report["failed"] == [] and report["missing"] == []
    assert (work / "assets" / "mdl" / "source" / "crow.rmdl").is_file()
    assert (work / "assets_server" / "mdl" / "source" / "crow.rmdl").is_file()
    staged_mdl = work / "props" / "game48" / "models" / "crow.mdl"
    assert staged_mdl.is_file()
    assert staged_mdl.with_suffix(".ani").is_file()
    models = json.loads((work / "props" / "models.json").read_text())
    assert models["models/crow.mdl"]["version"] == 48
    assert models["models/crow.mdl"]["sequences"] >= 1
    used = json.loads((work / "props" / "used.json").read_text())
    assert any(u["vmt"] == "models/crow/crow01" for u in used)


@needs_hl2
@needs_rmdlconv
def test_hl2_v44_reports_a_reason_and_leaves_the_rest_alone():
    work = _workdir("f6mix")
    report = _run_main(work, ["models/props_c17/oildrum001.mdl", "models/crow.mdl"])
    assert report["staged"] == ["mdl/source/crow.rmdl"]
    assert "models/props_c17/oildrum001.mdl" in report["failed"]
    reason = report["unsupported"]["models/props_c17/oildrum001.mdl"]
    assert "v44" in reason
