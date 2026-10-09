"""Source model sources (.smd/.dmx) -> MDL v49 through Portal 2's studiomdl.

    py scripts/source/smd2mdl.py <src.smd|src.dmx|dir> <out dir> --modelname <path/model.mdl>

An SMD is Valve text: `version 1`, a `nodes` table, `skeleton` frames, then
`triangles` with a material line before every triangle and three vertex lines
each (rigid or weighted). A DMX is `<!-- dmx encoding <enc> <n> format <t> <v>
-->` keyvalues2 text or the binary form; only its header is checked here,
studiomdl reads the geometry. Either becomes one QC ($body/$sequence/
$collisionmodel) compiled with Portal 2's studiomdl into MDL v49, which
srcprops.py then converts: stage the out dir's .mdl/.vvd/.vtx/.phy next to
each other under a content root as models/<modelname> and name them in the
report srcprops.py reads.

The compile runs against an isolated game dir (Portal 2's gameinfo.txt copied
under the work dir), so the real install is never written to.
"""

import argparse
import math
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import r5edenv  # noqa: E402

_studiomdl = r5edenv.get("R5ED_STUDIOMDL", required=False)
STUDIOMDL = Path(_studiomdl) if _studiomdl else Path("studiomdl.exe")
PORTAL2 = STUDIOMDL.parents[1] / "portal2" if _studiomdl else Path("portal2")
MAX_SMD_BYTES = 256 << 20
MAX_SMD_LINES = 8 << 20
MAX_NODES = 4096
MAX_TRIS = 2_000_000
MAX_NAME = 256
MAX_DMX_HEAD = 4096
DMX_ENCODINGS = {"keyvalues2": (1, 2, 3, 4), "binary": (1, 2, 3, 4, 5, 9),
                 "binary_proto": (2,)}
SIDECARS = (".vvd", ".dx90.vtx", ".vtx", ".phy", ".ani")
SOURCES = (".smd", ".dmx")
COMPILE_TIMEOUT = 300.0

DMX_HEAD = re.compile(r"<!--\s*dmx\s+encoding\s+(\S+)\s+(\d+)\s+format\s+(\S+)\s+(\d+)\s*-->")
DMX_PROTO2 = re.compile(r"<!--\s*DMXVersion\s+binary_v(\d+)\s*-->")


class SmdError(Exception):
    pass


class CompileError(Exception):
    pass


class Smd(NamedTuple):
    nodes: int
    frames: int
    materials: tuple
    triangles: int
    verts: int


class Dmx(NamedTuple):
    encoding: str
    encoding_version: int
    format: str
    format_version: int


def _num(tok, name, lineno, what="number"):
    try:
        v = float(tok)
    except ValueError:
        raise SmdError(f"{name}:{lineno}: bad {what} {tok!r}") from None
    if not math.isfinite(v):
        raise SmdError(f"{name}:{lineno}: bad {what} {tok!r}")
    return v


def _vert(toks, name, lineno, ids):
    if len(toks) < 9:
        raise SmdError(f"{name}:{lineno}: vertex needs bone + 8 numbers, has {len(toks)}")
    try:
        bone = int(toks[0])
    except ValueError:
        raise SmdError(f"{name}:{lineno}: bad bone {toks[0]!r}") from None
    if bone not in ids:
        raise SmdError(f"{name}:{lineno}: vertex names unknown bone {bone}")
    for tok in toks[1:9]:
        _num(tok, name, lineno)
    rest = toks[9:]
    if rest:
        try:
            nlinks = int(rest[0])
        except ValueError:
            raise SmdError(f"{name}:{lineno}: bad link count {rest[0]!r}") from None
        if nlinks < 0 or nlinks > 64 or len(rest) != 1 + 2 * nlinks:
            raise SmdError(f"{name}:{lineno}: link count {rest[0]!r} does not fit the line")
        for i in range(nlinks):
            try:
                link = int(rest[1 + 2 * i])
            except ValueError:
                raise SmdError(f"{name}:{lineno}: bad link bone {rest[1 + 2 * i]!r}") from None
            if link not in ids:
                raise SmdError(f"{name}:{lineno}: link names unknown bone {link}")
            _num(rest[2 + 2 * i], name, lineno, "weight")


def parse_smd(path):
    """(nodes, frames, materials, triangles, verts) of an SMD, bounds-checked."""
    name = str(path)
    try:
        size = Path(path).stat().st_size
    except OSError as e:
        raise SmdError(f"{name}: cannot read: {e}") from e
    if size > MAX_SMD_BYTES:
        raise SmdError(f"{name}: file too large for an smd ({size} bytes)")
    try:
        text = Path(path).read_bytes().decode("ascii")
    except UnicodeDecodeError as e:
        raise SmdError(f"{name}: not ascii text: {e}") from e
    lines = text.splitlines()
    if len(lines) > MAX_SMD_LINES:
        raise SmdError(f"{name}: too many lines ({len(lines)})")
    rows = [(i + 1, l.strip()) for i, l in enumerate(lines) if l.strip()]
    pos = 0

    def take(kind):
        nonlocal pos
        if pos >= len(rows):
            raise SmdError(f"{name}: file ends where {kind} was expected")
        lineno, line = rows[pos]
        pos += 1
        return lineno, line

    lineno, first = take("`version 1`")
    if first != "version 1":
        raise SmdError(f"{name}:{lineno}: not an smd (first line is {first!r})")
    lineno, line = take("`nodes`")
    if line != "nodes":
        raise SmdError(f"{name}:{lineno}: expected `nodes`, found {line!r}")
    ids = set()
    while True:
        lineno, line = take("a node or `end`")
        if line == "end":
            break
        parts = line.split('"')
        if len(parts) != 3:
            raise SmdError(f"{name}:{lineno}: bad node line {line!r}")
        try:
            nid, parent = int(parts[0]), int(parts[2])
        except ValueError:
            raise SmdError(f"{name}:{lineno}: bad node line {line!r}") from None
        if len(parts[1]) > MAX_NAME:
            raise SmdError(f"{name}:{lineno}: node name too long")
        if nid in ids or nid < 0:
            raise SmdError(f"{name}:{lineno}: duplicate node id {nid}")
        ids.add(nid)
        if len(ids) > MAX_NODES:
            raise SmdError(f"{name}:{lineno}: too many nodes (over {MAX_NODES})")
    lineno, line = take("`skeleton`")
    if line != "skeleton":
        raise SmdError(f"{name}:{lineno}: expected `skeleton`, found {line!r}")
    frames = 0
    while True:
        lineno, line = take("a skeleton line or `end`")
        if line == "end":
            break
        toks = line.split()
        if toks[0] == "time":
            if len(toks) != 2:
                raise SmdError(f"{name}:{lineno}: bad time line {line!r}")
            try:
                t = int(toks[1])
            except ValueError:
                raise SmdError(f"{name}:{lineno}: bad time {toks[1]!r}") from None
            if t < 0 or t > 1 << 24:
                raise SmdError(f"{name}:{lineno}: bad time {toks[1]!r}")
            frames += 1
            continue
        if len(toks) != 7:
            raise SmdError(f"{name}:{lineno}: bone line needs bone + 6 numbers")
        try:
            bone = int(toks[0])
        except ValueError:
            raise SmdError(f"{name}:{lineno}: bad bone {toks[0]!r}") from None
        if bone not in ids:
            raise SmdError(f"{name}:{lineno}: frame names unknown bone {bone}")
        for tok in toks[1:]:
            _num(tok, name, lineno)
    if not frames:
        raise SmdError(f"{name}: skeleton holds no frames")
    lineno, line = take("`triangles`")
    if line != "triangles":
        raise SmdError(f"{name}:{lineno}: expected `triangles`, found {line!r}")
    mats, ntris = [], 0
    while True:
        lineno, line = take("a material or `end`")
        if line == "end":
            break
        if len(line.split()) != 1:
            raise SmdError(f"{name}:{lineno}: a vertex where a material was expected "
                           "(a material line starts every triangle)")
        if len(line) > MAX_NAME:
            raise SmdError(f"{name}:{lineno}: material name too long")
        for _ in range(3):
            vl, vt = take("three vertex lines per triangle")
            _vert(vt.split(), name, vl, ids)
        ntris += 1
        if ntris > MAX_TRIS:
            raise SmdError(f"{name}:{lineno}: too many triangles (over {MAX_TRIS})")
        if line not in mats:
            mats.append(line)
    if pos != len(rows):
        lineno, _ = rows[pos]
        raise SmdError(f"{name}:{lineno}: junk after the triangles block")
    if not ntris:
        raise SmdError(f"{name}: no triangles")
    return Smd(len(ids), frames, tuple(mats), ntris, 3 * ntris)


def check_dmx(path):
    """(encoding, format) of a DMX from its header line; the body is studiomdl's job."""
    name = str(path)
    try:
        with open(path, "rb") as f:
            head = f.read(MAX_DMX_HEAD + 2)
    except OSError as e:
        raise SmdError(f"{name}: cannot read: {e}") from e
    stop = head.find(b"\n")
    if stop < 0 or stop > MAX_DMX_HEAD:
        raise SmdError(f"{name}: dmx header runs past {MAX_DMX_HEAD} bytes")
    try:
        line = head[:stop].decode("ascii").strip()
    except UnicodeDecodeError as e:
        raise SmdError(f"{name}: dmx header is not ascii: {e}") from e
    m = DMX_HEAD.fullmatch(line)
    if m:
        enc, enc_v, fmt, fmt_v = m.group(1).lower(), m.group(2), m.group(3), m.group(4)
        if enc not in DMX_ENCODINGS:
            raise SmdError(f"{name}: unsupported dmx encoding {m.group(1)!r}")
        if len(fmt) > MAX_NAME:
            raise SmdError(f"{name}: dmx format name too long")
        for v in (enc_v, fmt_v):
            if int(v) > 1 << 20:
                raise SmdError(f"{name}: dmx version number too large")
        if int(enc_v) not in DMX_ENCODINGS[enc]:
            raise SmdError(f"{name}: unsupported dmx encoding {m.group(1)!r} version {enc_v}")
        return Dmx(enc, int(enc_v), fmt, int(fmt_v))
    m = DMX_PROTO2.fullmatch(line)
    if m:
        if int(m.group(1)) > 1 << 20:
            raise SmdError(f"{name}: dmx version number too large")
        return Dmx("binary_proto", int(m.group(1)), "", 0)
    raise SmdError(f"{name}: not a dmx (bad header line {line[:80]!r})")


def _qc_token(value, what, name="qc"):
    if not value or len(value) > MAX_NAME or value != value.strip() or any(c in value for c in '"\r\n'):
        raise SmdError(f"{name}: bad {what} {value!r}")
    return value


def _qc_rel(value, what, suff, name="qc"):
    value = _qc_token(value, what, name).replace("\\", "/")
    parts = value.split("/")
    if value.startswith("/") or any(p in ("", ".", "..") for p in parts):
        raise SmdError(f"{name}: bad {what} {value!r}")
    if not value.lower().endswith(suff):
        raise SmdError(f"{name}: bad {what} {value!r}")
    return value


def write_qc(path, *, modelname, bodies, sequence, collision=None, cdmaterials=(),
             surfaceprop="concrete", staticprop=True, mass=100.0, concave=True,
             sequence_name="idle", fps=30):
    """Writes a QC compiling the given sources; bodies is [(label, file)]."""
    if not bodies:
        raise SmdError("qc: no bodies")
    if not math.isfinite(mass) or mass <= 0:
        raise SmdError(f"qc: bad mass {mass!r}")
    model = _qc_rel(modelname, "modelname", ".mdl")
    seen = set()
    for label, src in bodies:
        _qc_token(label, "body label")
        src = _qc_token(src, "body source")
        if src.lower() not in seen and not src.lower().endswith(SOURCES):
            raise SmdError(f"qc: body source {src!r} is not smd or dmx")
        seen.add(src.lower())
    seq = _qc_token(sequence, "sequence source")
    if not seq.lower().endswith(SOURCES):
        raise SmdError(f"qc: sequence source {seq!r} is not smd or dmx")
    if collision is not None:
        collision = _qc_token(collision, "collision source")
        if not collision.lower().endswith(SOURCES):
            raise SmdError(f"qc: collision source {collision!r} is not smd or dmx")
    cds = [_qc_rel(c, "cdmaterials", "", name="qc").rstrip("/") for c in cdmaterials]
    out = [f'$modelname "{model}"']
    for label, src in bodies:
        out.append(f'$body "{label}" "{src}"')
    if staticprop:
        out.append("$staticprop")
    out.append(f'$surfaceprop "{_qc_token(surfaceprop, "surfaceprop")}"')
    if cds:
        out += [f'$cdmaterials "{c}"' for c in cds]
    else:
        out.append('$cdmaterials ""')
    out.append(f'$sequence {_qc_token(sequence_name, "sequence name")} "{seq}" fps {fps}')
    if collision is not None:
        out += [f'$collisionmodel "{collision}"', "{", f"$mass {mass:g}"]
        if concave:
            out.append("$concave")
        out.append("}")
    out.append("")
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(out), encoding="ascii")
    return path


def run_studiomdl(studiomdl, game, qc, timeout=COMPILE_TIMEOUT):
    """Runs studiomdl on a QC; returns its log, raising CompileError on failure."""
    if not Path(studiomdl).is_file():
        raise CompileError(f"{qc}: studiomdl not found at {studiomdl}")
    try:
        r = subprocess.run([str(studiomdl), "-game", str(game), "-nopause", str(qc)],
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                           text=True, errors="replace", timeout=timeout)
    except subprocess.TimeoutExpired as e:
        raise CompileError(f"{qc}: studiomdl took longer than {timeout:g}s") from e
    if r.returncode != 0:
        tail = "\n".join(r.stdout.splitlines()[-15:])
        raise CompileError(f"{qc}: studiomdl failed (exit {r.returncode})\n{tail}")
    return r.stdout

def build(sources, out_dir, *, modelname, bodies=None, sequence=None, collision=None,
          cdmaterials=(), surfaceprop="concrete", staticprop=True, mass=100.0,
          concave=True, sequence_name="idle", studiomdl=None, game=None,
          timeout=COMPILE_TIMEOUT):
    """Compiles SMD/DMX sources to MDL v49; returns {mdl, files, materials, log}."""
    exe = Path(studiomdl) if studiomdl else STUDIOMDL
    files = [Path(s) for s in sources]
    if not files:
        raise SmdError("no model sources")
    out = Path(out_dir)
    work = out / "work"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    staged = []
    for src in files:
        if src.suffix.lower() not in SOURCES:
            raise SmdError(f"{src}: not a model source (smd or dmx)")
        if not src.is_file():
            raise SmdError(f"{src}: no such source file")
        dst = work / src.name
        if dst.exists():
            raise SmdError(f"{src}: duplicate source name {src.name!r}")
        shutil.copyfile(src, dst)
        staged.append(dst)
    materials = []
    for dst in staged:
        if dst.suffix.lower() == ".smd":
            for m in parse_smd(dst).materials:
                if m not in materials:
                    materials.append(m)
        else:
            check_dmx(dst)
    names = [d.name for d in staged]

    def pick(value, what, default):
        if value is None:
            return default
        if value not in names:
            raise SmdError(f"{value}: {what} is not one of the sources")
        return value

    body_files = [pick(f, "body", None) for _, f in (bodies or [])] if bodies else []
    if bodies and len(body_files) != len({b.lower() for b in body_files}):
        raise SmdError("qc: a body source is listed twice")
    body_arg = list(bodies) if bodies else [("Body", names[0])]
    seq = pick(sequence, "sequence", names[0])
    col = pick(collision, "collision", None) if collision is not None else None
    qc = write_qc(work / "model.qc", modelname=modelname, bodies=body_arg,
                  sequence=seq, collision=col, cdmaterials=cdmaterials,
                  surfaceprop=surfaceprop, staticprop=staticprop, mass=mass,
                  concave=concave, sequence_name=sequence_name)
    if game is None:
        gamedir = work / "game"
        gamedir.mkdir()
        info = PORTAL2 / "gameinfo.txt"
        if not info.is_file():
            raise CompileError(f"portal 2 not found at {PORTAL2} (pass --game)")
        shutil.copyfile(info, gamedir / "gameinfo.txt")
    else:
        gamedir = Path(game)
    log = run_studiomdl(exe, gamedir, qc, timeout)
    (out / "studiomdl.log").write_text(log, encoding="utf-8", errors="replace")
    rel = modelname.replace("\\", "/")
    got = []
    mdl = gamedir / "models" / rel
    if not mdl.is_file() or not mdl.stat().st_size:
        tail = "\n".join(log.splitlines()[-15:])
        raise CompileError(f"{qc}: studiomdl produced no {rel}\n{tail}")
    targets = [mdl] + [mdl.parent / (mdl.stem + s) for s in SIDECARS]
    for cand in targets:
        if cand.is_file() and cand.stat().st_size:
            dst = out / cand.name
            shutil.copyfile(cand, dst)
            got.append(dst)
    return {"mdl": out / mdl.name, "files": got, "materials": materials, "log": log}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("src", help=".smd/.dmx file or a dir of them")
    ap.add_argument("out", help="dir for the .mdl, sidecars and studiomdl.log")
    ap.add_argument("--modelname", required=True, help="game-relative path, e.g. props/cube.mdl")
    ap.add_argument("--body", action="append", default=[], metavar="LABEL=FILE",
                    help="a $body part (repeatable; FILE is a source basename)")
    ap.add_argument("--sequence", default=None, help="source basename for the idle sequence")
    ap.add_argument("--sequence-name", default="idle")
    ap.add_argument("--collision", default=None, help="source basename for the collision model")
    ap.add_argument("--no-concave", action="store_true")
    ap.add_argument("--cdmaterials", nargs="*", default=[])
    ap.add_argument("--surfaceprop", default="concrete")
    ap.add_argument("--no-staticprop", action="store_true")
    ap.add_argument("--mass", type=float, default=100.0)
    ap.add_argument("--studiomdl", default=None)
    ap.add_argument("--game", default=None)
    ap.add_argument("--timeout", type=float, default=COMPILE_TIMEOUT)
    args = ap.parse_args()

    src = Path(args.src)
    if src.is_dir():
        found = sorted(p for p in src.iterdir() if p.is_file() and p.suffix.lower() in SOURCES)
        if not found:
            raise SmdError(f"{src}: no smd or dmx sources")
    elif src.is_file():
        found = [src]
    else:
        raise SmdError(f"{src}: no such input")
    bodies = []
    for spec in args.body:
        if "=" not in spec:
            raise SmdError(f"{spec!r}: --body is LABEL=FILE")
        bodies.append(tuple(spec.split("=", 1)))
    res = build(found, args.out, modelname=args.modelname,
                bodies=bodies or None, sequence=args.sequence,
                collision=args.collision, cdmaterials=args.cdmaterials,
                surfaceprop=args.surfaceprop, staticprop=not args.no_staticprop,
                mass=args.mass, concave=not args.no_concave, sequence_name=args.sequence_name,
                studiomdl=args.studiomdl, game=args.game, timeout=args.timeout)
    size = res["mdl"].stat().st_size
    sides = [p.name for p in res["files"] if p != res["mdl"]]
    print(f"mdl: {res['mdl'].name} ({size} bytes), {len(sides)} sidecars; "
          f"{len(res['materials'])} materials")
    if res["materials"]:
        print("  materials: " + ", ".join(res["materials"]))


if __name__ == "__main__":
    try:
        main()
    except (SmdError, CompileError) as e:
        print(f"smd2mdl: error: {e}", file=sys.stderr)
        sys.exit(1)
