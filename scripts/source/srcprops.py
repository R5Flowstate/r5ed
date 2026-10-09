"""Source models (.mdl v44-49 + .vvd/.vtx/.phy/.ani) -> S21 client rmdl v17 and S3 dedi rmdl v10.

rmdlconv drops `$cdmaterials` and names each material by the bare texture
name, so every texture name in the MDL is rewritten first to the S21 material
path `models/source/<vmt path>_rgdp` that srcmat.py builds. The `.phy` becomes
the collision model; the `.ani` rides along so a prop_dynamic's sequences
survive (rmdlconv output, no new animation conversion here).

v49 models go through one `rmdlconv -v49` batch; v44-48 go through the same
converter one model per call (a version rmdlconv rejects kills its own call,
never the rest), anything else is reported with the reason.

Writes, under <work>:
  models.json   {source mdl path: {"rmdl": apex path, "materials": [vmt paths]}}
  used.json     [{"vmt": path, "model": true}] for srcmat.py
and stages mdl_ assets into <assets>/mdl/source/... and <assets_server>/mdl/source/...
"""

import argparse
import json
import shutil
import struct
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vmt import Content  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import r5edenv  # noqa: E402
from guid import string_to_guid  # noqa: E402

RMDLCONV = r5edenv.path("R5ED_RMDLCONV", required=False) or Path("rmdlconv")
TEXTURE_STRUCT = 64  # mstudiotexture_t, v44-v49
MAGIC = b"IDST"
MIN_HDR = 0xDC  # through numcdtextures/cdtextureindex
MAX_TEX = 2048
MAX_CD = 256
MAX_SEQ = 1 << 20
SIDECARS = (".vvd", ".dx90.vtx", ".vtx", ".phy", ".ani")
LEGACY_TIMEOUT = 300


def cstr(data, off, name="mdl"):
    if off < 0 or off >= len(data):
        raise ValueError(f"{name}: string offset {off} past end of file ({len(data)} bytes)")
    end = data.find(b"\0", off)
    if end < 0:
        raise ValueError(f"{name}: unterminated string at offset {off}")
    return data[off:end].decode("latin-1")


def mdl_version(data, name="mdl"):
    if len(data) < 8 or data[:4] != MAGIC:
        raise ValueError(f"{name}: not an mdl")
    return struct.unpack_from("<i", data, 4)[0]


def mdl_sequences(data, name="mdl"):
    if len(data) < MIN_HDR or data[:4] != MAGIC:
        raise ValueError(f"{name}: not an mdl")
    n = struct.unpack_from("<i", data, 0xBC)[0]
    if not 0 <= n <= MAX_SEQ:
        raise ValueError(f"{name}: sequence count {n} out of range")
    return n


def classify_version(version):
    if version == 49:
        return "v49"
    if 44 <= version <= 48:
        return "legacy"
    return "unsupported"


def mdl_textures(data, name="mdl"):
    """(texture names, cd folders, texture table offset) of an MDL v44-49."""
    if len(data) < MIN_HDR or data[:4] != MAGIC:
        raise ValueError(f"{name}: not an mdl")
    num_tex, tex_index, num_cd, cd_index = struct.unpack_from("<iiii", data, 0xCC)
    if not 0 <= num_tex <= MAX_TEX:
        raise ValueError(f"{name}: texture count {num_tex} out of range")
    if not 0 <= num_cd <= MAX_CD:
        raise ValueError(f"{name}: cdmaterials count {num_cd} out of range")
    names = []
    for i in range(num_tex):
        base = tex_index + i * TEXTURE_STRUCT
        if base < 0 or base + TEXTURE_STRUCT > len(data):
            raise ValueError(f"{name}: texture {i} runs past end of file")
        rel = struct.unpack_from("<i", data, base)[0]
        if rel < 4 or base + rel >= len(data):
            raise ValueError(f"{name}: texture {i} name runs past end of file")
        names.append(cstr(data, base + rel, name))
    cds = []
    for i in range(num_cd):
        pos = cd_index + i * 4
        if pos < 0 or pos + 4 > len(data):
            raise ValueError(f"{name}: cdmaterials {i} runs past end of file")
        off = struct.unpack_from("<i", data, pos)[0]
        if off < 0 or off >= len(data):
            raise ValueError(f"{name}: cdmaterials {i} runs past end of file")
        cds.append(cstr(data, off, name))
    return names, cds, tex_index


def is_large(data):
    """rmdlconv's rule: a hull past +-1023 on x/y or +-2047 on z is written with
    full-float positions, which only _rgdc materials draw."""
    if len(data) < 0x80:
        raise ValueError("not an mdl")
    lo = struct.unpack_from("<3f", data, 0x68)
    hi = struct.unpack_from("<3f", data, 0x74)
    return lo[0] < -1023 or hi[0] > 1023 or lo[1] < -1023 or hi[1] > 1023 or lo[2] < -2047 or hi[2] > 2047


def rename_textures(data, new_names, tex_index):
    """Appends each new name to the file and points its texture at it."""
    if tex_index < 0 or tex_index > len(data):
        raise ValueError("texture table runs past end of file")
    for name in new_names:
        if len(name.encode("latin-1")) > 1024:
            raise ValueError(f"replacement material name too long: {name[:64]!r}")
    out = bytearray(data)
    for i, name in enumerate(new_names):
        base = tex_index + i * TEXTURE_STRUCT
        if base + 4 > len(out):
            raise ValueError("texture table runs past end of file")
        while len(out) % 4:
            out.append(0)
        pos = len(out)
        out += name.encode("latin-1") + b"\0"
        struct.pack_into("<i", out, base, pos - base)
    struct.pack_into("<i", out, 0x4C, len(out))
    return bytes(out)


def resolve(content, texname, cds):
    """The VMT a model texture uses: the first `$cdmaterials` folder that has it."""
    tex = texname.replace("\\", "/").lower()
    for cd in cds or [""]:
        cand = (cd.replace("\\", "/").lower().strip("/") + "/" + tex).strip("/")
        if content.find(f"materials/{cand}.vmt"):
            return cand
    return (cds[0].replace("\\", "/").lower().strip("/") + "/" + tex).strip("/") if cds else tex


def s21_model_material(vmt_path):
    p = vmt_path.lower().removeprefix("models/")
    return f"models/source/{p}"


def apex_model_path(mdl_rel):
    s = mdl_rel.replace("\\", "/").lower().removeprefix("models/").removesuffix(".mdl")
    return f"mdl/source/{s}.rmdl"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("report", help="r5ed-compile import-vmf output (JSON) naming the models")
    ap.add_argument("work")
    ap.add_argument("--content", nargs="+", required=True, help="content roots holding models/ and materials/")
    ap.add_argument("--assets", required=True)
    ap.add_argument("--assets-server", required=True)
    ap.add_argument("--stem", required=True)
    ap.add_argument("--out-dir-server", required=True)
    args = ap.parse_args()

    work = Path(args.work)
    game = work / "game"
    game48 = work / "game48"
    client, dedi = work / "client", work / "dedi"
    for d in (game, game48, client, dedi):
        shutil.rmtree(d, ignore_errors=True)
        d.mkdir(parents=True)
    content = Content(args.content)
    report = json.loads(Path(args.report).read_text(encoding="utf-8"))
    models = report["report"]["models"] if "report" in report else report["models"]

    def stage(root, rel, data, tex_index, new):
        dst = root / "models" / Path(rel.lower().removeprefix("models/"))
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_bytes(rename_textures(data, new, tex_index))
        for ext in SIDECARS:
            side = content.find(rel[:-4] + ext)
            if side is not None:
                dst.with_name(dst.stem + ext).write_bytes(side.read_bytes())

    out, used, missing, unsupported = {}, {}, [], {}
    legacy = []
    for rel in models:
        mdl = content.find(rel)
        if mdl is None:
            missing.append(rel)
            continue
        data = mdl.read_bytes()
        try:
            version = mdl_version(data, rel)
            names, cds, tex_index = mdl_textures(data, rel)
            seqs = mdl_sequences(data, rel)
        except ValueError as e:
            unsupported[rel] = str(e)
            continue
        group = classify_version(version)
        if group == "unsupported":
            unsupported[rel] = f"{rel}: unsupported mdl version {version}"
            continue
        vmts = [resolve(content, n, cds) for n in names]
        variant = "rgdc" if is_large(data) else "rgdp"
        new = [f"{s21_model_material(v)}_{variant}" for v in vmts]
        stage(game48 if group == "legacy" else game, rel, data, tex_index, new)
        if group == "legacy":
            legacy.append(rel)
        out[rel] = {"rmdl": apex_model_path(rel), "materials": vmts, "version": version,
                    "variant": variant, "sequences": seqs}
        for v in vmts:
            used[f"{v}|{variant}"] = {"vmt": v, "model": True, "variant": variant}

    def run_batch(flag, src, dst, logname, extra=(), mode="w", timeout=None):
        log = open(work / logname, mode)
        try:
            r = subprocess.run([str(RMDLCONV), flag, str(src), str(dst), *extra, "-nopause"],
                               stdout=log, stderr=subprocess.STDOUT, timeout=timeout)
            return r.returncode
        finally:
            log.close()

    if any(p.suffix.lower() == ".mdl" for p in game.rglob("*")):
        run_batch("-v49", game / "models", client, "rmdlconv_client.log",
                  ["-targetversion", "17", "-vertexcolor"])
    legacy_rc = {}
    one_in, one_out = work / "legacy_one_in", work / "legacy_one_out"
    for n, rel in enumerate(legacy):
        shutil.rmtree(one_in, ignore_errors=True)
        shutil.rmtree(one_out, ignore_errors=True)
        src = game48 / "models" / Path(rel.lower().removeprefix("models/"))
        dst = one_in / "models" / Path(rel.lower().removeprefix("models/"))
        dst.parent.mkdir(parents=True, exist_ok=True)
        for f in src.parent.glob(src.stem + "*"):
            shutil.copyfile(f, dst.parent / f.name)
        try:
            rc = run_batch("-v49", one_in / "models", one_out, "rmdlconv_legacy.log",
                           ["-targetversion", "17", "-vertexcolor"],
                           mode="a" if n else "w", timeout=LEGACY_TIMEOUT)
        except subprocess.TimeoutExpired:
            rc = "timeout"
        legacy_rc[rel] = rc
        if list(one_out.rglob("*.rmdl")):
            for f in one_out.rglob("*"):
                if f.is_file() and f.suffix.lower() in (".rmdl", ".vg", ".vg_static"):
                    target = client / f.relative_to(one_out)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(f, target)
    if any(p.suffix.lower() == ".rmdl" for p in client.rglob("*")):
        run_batch("-v17", client, dedi, "rmdlconv_dedi.log")

    staged, failed = [], []
    for rel, info in out.items():
        sub = Path(rel.lower().removeprefix("models/")).with_suffix("")
        c = client / sub.with_suffix(".rmdl")
        s = dedi / sub.with_suffix(".rmdl")
        if not c.is_file() or not s.is_file():
            failed.append(rel)
            if rel in legacy_rc and rel not in unsupported:
                unsupported[rel] = (f"{rel}: rmdlconv -v49 produced no output "
                                    f"for mdl v{info['version']} (exit {legacy_rc[rel]})")
            continue
        dst_c = Path(args.assets) / info["rmdl"]
        dst_s = Path(args.assets_server) / info["rmdl"]
        dst_c.parent.mkdir(parents=True, exist_ok=True)
        dst_s.parent.mkdir(parents=True, exist_ok=True)
        for ext in (".rmdl", ".vg", ".vg_static"):
            if c.with_suffix(ext).is_file():
                shutil.copyfile(c.with_suffix(ext), dst_c.with_suffix(ext))
        shutil.copyfile(s, dst_s)
        staged.append(info["rmdl"])

    server = Path(args.assets_server)
    files = [{"_type": "mdl_", "_path": r, "$guid": f"0x{string_to_guid(r):016X}"} for r in staged]
    manifest = {"version": 8, "dedi": True, "keepDevOnly": False, "keepClientOnly": False, "name": f"{args.stem}_src",
                "assetsDir": server.as_posix() + "/", "outputDir": Path(args.out_dir_server).as_posix() + "/",
                "compressLevel": 6, "files": files}
    server.mkdir(parents=True, exist_ok=True)
    (server.parent / f"{args.stem}_src_server.json").write_text(json.dumps(manifest, indent=1))
    (work / "models.json").write_text(json.dumps(out, indent=1))
    (work / "used.json").write_text(json.dumps(list(used.values()), indent=1))
    for rel in unsupported:
        if rel not in failed:
            failed.append(rel)
    (work / "props_report.json").write_text(json.dumps(
        {"staged": staged, "failed": sorted(failed), "missing": missing, "unsupported": unsupported}, indent=1))
    print(f"models: {len(staged)} staged, {len(failed)} failed ({len(unsupported)} unsupported), "
          f"{len(missing)} missing; {len(used)} model materials")
    for rel in sorted(failed) + missing:
        print(f"  not converted: {rel}" + (f" ({unsupported[rel]})" if rel in unsupported else ""))


if __name__ == "__main__":
    main()
