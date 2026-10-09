"""A compiled Source BSP (v19-25) -> a Hammer VMF plus a content root.

    py bsp2vmf.py <map.bsp> <out dir>

Writes <out dir>/<stem>.vmf and extracts the embedded pakfile into
<out dir>/content/ (pass it to ship.sh as a content root). World solids come
from model 0's brushes, brush entities from their `*N` model, static props
become prop_static entities, displacements become `dispinfo` blocks on the
side their face was built from. Bevel sides and sides with texinfo -1 are
skipped; anything that cannot be converted stops the run.
"""

import io
import math
import shutil
import sys
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bsp  # noqa: E402
from bsp import BspError  # noqa: E402

MAX_PAK_FILES = 200000
MAX_PAK_TOTAL = 2 << 30
MAX_PAK_FILE = 512 << 20
EPS = 0.01


def dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def unit(a):
    length = math.sqrt(dot(a, a))
    return tuple(x / length for x in a)


def rotate(rows, v):
    return tuple(dot(r, v) for r in rows)


def placement(kv):
    """A brush entity's (rotation rows, origin): VBSP stores its model
    relative to the entity origin and unrotated."""
    def vec(key):
        parts = kv.get(key, "0 0 0").split()
        return tuple(float(x) for x in parts) if len(parts) == 3 else (0.0, 0.0, 0.0)
    origin = vec("origin")
    pitch, yaw, roll = (math.radians(a) for a in vec("angles"))
    if not any(origin) and not (pitch or yaw or roll):
        return None
    sp, cp, sy, cy, sr, cr = math.sin(pitch), math.cos(pitch), math.sin(yaw), math.cos(yaw), math.sin(roll), math.cos(roll)
    rows = ((cp * cy, sr * sp * cy - cr * sy, cr * sp * cy + sr * sy),
            (cp * sy, sr * sp * sy + cr * cy, cr * sp * sy - sr * cy),
            (-sp, sr * cp, cr * cp))
    return rows, origin


def clip(poly, n, d):
    """Keeps the part of `poly` behind plane n.x = d."""
    out = []
    for i, p in enumerate(poly):
        q = poly[(i + 1) % len(poly)]
        dp, dq = dot(n, p) - d, dot(n, q) - d
        if dp <= EPS:
            out.append(p)
        if (dp < -EPS and dq > EPS) or (dp > EPS and dq < -EPS):
            t = dp / (dp - dq)
            out.append(tuple(p[k] + (q[k] - p[k]) * t for k in range(3)))
    return out


def face_polygon(planes, i):
    """The face plane i keeps on a convex brush, as a polygon."""
    n, d = planes[i]
    seed = (1.0, 0.0, 0.0) if abs(n[0]) < 0.9 else (0.0, 1.0, 0.0)
    u = unit(cross(n, seed))
    v = cross(n, u)
    c = tuple(n[k] * d for k in range(3))
    big = 131072.0
    poly = [tuple(c[k] + (u[k] * sx + v[k] * sy) * big for k in range(3))
            for sx, sy in ((-1, -1), (1, -1), (1, 1), (-1, 1))]
    for j, (m, e) in enumerate(planes):
        if j != i:
            poly = clip(poly, m, e)
            if len(poly) < 3:
                return []
    return poly


def poly_area(poly):
    total = (0.0, 0.0, 0.0)
    for k in range(1, len(poly) - 1):
        a = tuple(poly[k][j] - poly[0][j] for j in range(3))
        b = tuple(poly[k + 1][j] - poly[0][j] for j in range(3))
        c = cross(a, b)
        total = tuple(total[j] + c[j] for j in range(3))
    return 0.5 * math.sqrt(dot(total, total))


def fmt(x):
    r = round(x, 4)
    return str(int(r)) if r == int(r) else f"{r:g}"


class Writer:
    def __init__(self, path, planes, brushes, sides, names, texdata, texinfo):
        self.path = path
        self.planes = [((p[0], p[1], p[2]), p[3]) for p in planes]
        self.brushes = brushes
        self.sides = sides
        for k, td in enumerate(texdata):
            if not 0 <= td < len(names):
                raise BspError(f"{path}: texdata {k} names string {td} of {len(names)}")
        self.names = names
        self.texdata = texdata
        self.texinfo = texinfo
        self.next_id = 1
        self.dropped_brushes = 0
        self.planned = {}

    def nid(self):
        self.next_id += 1
        return self.next_id

    def material(self, ti):
        return self.names[self.texdata[self.texinfo[ti][3]]]

    def axes(self, ti, normal, place=None):
        if ti < 0:
            seed = (1.0, 0.0, 0.0) if abs(normal[0]) < 0.9 else (0.0, 1.0, 0.0)
            u = unit(cross(normal, seed))
            return (u, 0.0, 0.25), (cross(normal, u), 0.0, 0.25)
        urow, vrow = self.texinfo[ti][0], self.texinfo[ti][1]
        out = []
        for row in (urow, vrow):
            axis, shift = tuple(row[:3]), row[3]
            if place:
                axis = rotate(place[0], axis)
                shift -= dot(axis, place[1])
            length = math.sqrt(dot(axis, axis)) or 1.0
            out.append((tuple(x / length for x in axis), shift, 1.0 / length))
        return out

    def keep_sides(self, recs, planes):
        keep = []
        for i, s in enumerate(recs):
            poly = face_polygon(planes, i)
            if len(poly) >= 3 and poly_area(poly) > 0.01:
                keep.append((s, planes[i]))
        if len(keep) < 4:
            self.dropped_brushes += 1
            return None
        return keep

    def plan(self, bi):
        """Kept (side, plane) pairs of a brush, or None when no solid is left."""
        if bi in self.planned:
            return self.planned[bi]
        first, count, _contents = self.brushes[bi]
        recs = [s for s in self.sides[first:first + count] if not s[2] and s[1] != -1]
        planes = [(self.planes[s[0]][0], self.planes[s[0]][1]) for s in recs]
        keep = self.keep_sides(recs, planes)
        self.planned[bi] = keep
        return keep

    def synth(self, plane, texinfo, corners):
        """A one-unit backing brush for a displacement whose base brush is
        gone, so its dispinfo block has a side to live on. Front side first."""
        (nx, ny, nz), d = plane[:3], plane[3]
        n = (nx, ny, nz)
        cx = tuple(sum(c[k] for c in corners) / 4 for k in range(3))
        planes = [(n, d), (tuple(-x for x in n), -d + 1.0)]
        cyc = [(corners[0], corners[1]), (corners[1], corners[2]),
               (corners[2], corners[3]), (corners[3], corners[0])]
        for a, b in cyc:
            e = unit(cross(n, tuple(b[k] - a[k] for k in range(3))))
            if dot(e, tuple(cx[k] - a[k] for k in range(3))) > 0:
                e = tuple(-x for x in e)
            planes.append((e, dot(e, a)))
        recs = [(0, texinfo, 0, -1)] * 6
        return self.keep_sides(recs, planes)

    def disp_block(self, disp):
        n = 2 ** disp["power"] + 1
        rows = {}
        for key in ("normals", "distances", "offsets", "offset_normals", "alphas"):
            vals = disp["normals"] if key == "offset_normals" else disp[key]
            w = 3 * n if key in ("normals", "offsets", "offset_normals") else n
            rows[key] = [" ".join(fmt(v) for v in vals[i * w:(i + 1) * w]) for i in range(n)]
        tags = 2 * 4 ** disp["power"]
        per = 2 * (n - 1)
        tagrows = [" ".join(["0"] * per) for _ in range(tags // per)]
        lines = [f'\t\t\tdispinfo\n\t\t\t{{\n\t\t\t\t"power" "{disp["power"]}"',
                 f'\t\t\t\t"startposition" "[{fmt(disp["start"][0])} {fmt(disp["start"][1])} '
                 f'{fmt(disp["start"][2])}]"',
                 '\t\t\t\t"elevation" "0"\n\t\t\t\t"subdiv" "0"']
        for key in ("normals", "distances", "offsets", "offset_normals", "alphas"):
            lines.append(f"\t\t\t\t{key}\n\t\t\t\t{{")
            lines.extend(f'\t\t\t\t\t"row{i}" "{row}"' for i, row in enumerate(rows[key]))
            lines.append("\t\t\t\t}")
        lines.append("\t\t\t\ttriangle_tags\n\t\t\t\t{")
        lines.extend(f'\t\t\t\t\t"row{i}" "{row}"' for i, row in enumerate(tagrows))
        lines.append('\t\t\t\t}\n\t\t\t\t"allowed_verts" "10 -1 -1 -1"\n\t\t\t}')
        return "\n".join(lines)

    def solid(self, keep, disps, place=None):
        """A kept brush as a VMF solid; `place` (rotation rows, origin) moves a
        brush entity's model-local planes into the world."""
        if keep is None:
            return None
        if place:
            keep = [(s, (rotate(place[0], n), d + dot(rotate(place[0], n), place[1]))) for s, (n, d) in keep]
        lines = [f'\tsolid\n\t{{\n\t\t"id" "{self.nid()}"']
        for ki, (s, (n, d)) in enumerate(keep):
            seed = (1.0, 0.0, 0.0) if abs(n[0]) < 0.9 else (0.0, 1.0, 0.0)
            a = unit(cross(n, seed))
            b = cross(n, a)
            p0 = tuple(n[k] * d for k in range(3))
            p1 = tuple(p0[k] + a[k] * 64 for k in range(3))
            p2 = tuple(p0[k] + b[k] * 64 for k in range(3))
            pts = " ".join("(" + " ".join(fmt(c) for c in p) + ")" for p in (p0, p1, p2))
            (ua, us, uscale), (va, vs, vscale) = self.axes(s[1], n, place)
            lines.append(
                f'\t\tside\n\t\t{{\n\t\t\t"id" "{self.nid()}"\n\t\t\t"plane" "{pts}"\n'
                f'\t\t\t"material" "{self.material(s[1])}"\n'
                f'\t\t\t"uaxis" "[{fmt(ua[0])} {fmt(ua[1])} {fmt(ua[2])} {fmt(us)}] {uscale:.6g}"\n'
                f'\t\t\t"vaxis" "[{fmt(va[0])} {fmt(va[1])} {fmt(va[2])} {fmt(vs)}] {vscale:.6g}"\n'
                f'\t\t\t"rotation" "0"\n\t\t\t"lightmapscale" "16"\n\t\t\t"smoothing_groups" "0"')
            for disp in disps.get(ki, ()):
                lines.append(self.disp_block(disp))
            lines.append("\t\t}")
        lines.append("\t}")
        return "\n".join(lines)


def disp_data(bsp_obj, disp):
    """Position/normal/distance/offset/alpha grids of one displacement, plus
    its face plane and texinfo. Normals come from the subdivided surface,
    oriented by the face normal; elevation is baked into the lump's offsets,
    so it emits as 0."""
    (nx, ny, nz, dist), texinfo, corners = bsp.disp_face(bsp_obj, disp["mapface"])
    sp = disp["start"]
    k = min(range(4), key=lambda i: (corners[i][0] - sp[0]) ** 2 + (corners[i][1] - sp[1]) ** 2
            + (corners[i][2] - sp[2]) ** 2)
    c0, cU, cV, cUV = corners[k], corners[(k + 1) % 4], corners[(k - 1) % 4], corners[(k + 2) % 4]
    n = 2 ** disp["power"]
    verts = bsp.disp_verts(bsp_obj, disp["vertstart"], disp["power"])

    def base(i, j):
        s, t = j / n, i / n
        return [c0[c] * (1 - s) * (1 - t) + cU[c] * s * (1 - t) + cV[c] * (1 - s) * t + cUV[c] * s * t
                for c in range(3)]

    pos = []
    for i in range(n + 1):
        for j in range(n + 1):
            bb = base(i, j)
            (f0, f1, f2), dd, _al = verts[i * (n + 1) + j]
            pos.append((bb[0] + f0 + nx * dd, bb[1] + f1 + ny * dd, bb[2] + f2 + nz * dd))

    def at(i, j):
        i = min(max(i, 0), n)
        j = min(max(j, 0), n)
        return pos[i * (n + 1) + j]

    normals = []
    for i in range(n + 1):
        for j in range(n + 1):
            du = tuple(at(i, j + 1)[c] - at(i, j - 1)[c] for c in range(3))
            dv = tuple(at(i + 1, j)[c] - at(i - 1, j)[c] for c in range(3))
            nv = cross(du, dv)
            if dot(nv, (nx, ny, nz)) < 0:
                nv = tuple(-x for x in nv)
            length = math.sqrt(dot(nv, nv))
            normals.append(tuple(x / length for x in nv) if length > 0 else (nx, ny, nz))
    distances, offsets, alphas = [], [], []
    for (f0, f1, f2), dd, al in verts:
        distances.append(dd)
        offsets.extend((f0, f1, f2))
        alphas.append(al)
    flat_normals = [v for nv in normals for v in nv]
    return {"power": disp["power"], "start": disp["start"], "plane": (nx, ny, nz, dist),
            "texinfo": texinfo, "corners": corners, "normals": flat_normals,
            "distances": distances, "offsets": offsets, "alphas": alphas}


def match_disps(bsp_obj, planned):
    """Splits displacements into side-attached ((brush, kept-side index) ->
    grids) and orphaned ones whose base brush is gone."""
    attached, orphans, unattached = {}, [], 0
    disps = bsp.displacements(bsp_obj)
    if not disps:
        return attached, orphans, unattached
    indexed = [(bi, keep) for bi, keep in planned.items() if keep is not None]
    for disp in disps:
        try:
            data = disp_data(bsp_obj, disp)
        except BspError:
            unattached += 1
            continue
        nx, ny, nz, dist = data["plane"]
        texinfo, corners = data["texinfo"], data["corners"]
        hit = None
        for bi, keep in indexed:
            brush_planes = [p for _, p in keep]
            for ki, (_, (pn, pd)) in enumerate(keep):
                if abs(dot(pn, (nx, ny, nz))) < 1 - 1e-5:
                    continue
                if abs(pd - dist) >= 0.05 and abs(pd + dist) >= 0.05:
                    continue
                if not all(dot(pl[0], c) - pl[1] <= 0.5 for pl in brush_planes for c in corners):
                    continue
                hit = (bi, ki)
                break
            if hit is not None:
                break
        if hit is None:
            orphans.append(data)
        else:
            attached.setdefault(hit, []).append(data)
    return attached, orphans, unattached


def extract_pak(bsp_obj, src, content):
    pak = bsp_obj.lump(bsp.LUMP_PAKFILE)
    files = 0
    if not pak:
        return 0
    try:
        z = zipfile.ZipFile(io.BytesIO(pak))
    except zipfile.BadZipFile as e:
        raise BspError(f"{src}: pakfile is not a zip ({e})") from e
    infos = z.infolist()
    if len(infos) > MAX_PAK_FILES:
        raise BspError(f"{src}: pakfile names {len(infos)} files")
    total = 0
    for info in infos:
        name = info.filename.replace("\\", "/").strip()
        parts = [p for p in name.split("/") if p not in ("", ".")]
        if not parts or name.startswith("/") or ".." in parts or ":" in name:
            raise BspError(f"{src}: pakfile entry {info.filename!r} escapes the content root")
        if info.is_dir():
            continue
        if info.file_size > MAX_PAK_FILE:
            raise BspError(f"{src}: pakfile entry {info.filename!r} is {info.file_size} bytes")
        total += info.file_size
        if total > MAX_PAK_TOTAL:
            raise BspError(f"{src}: pakfile extracts past {MAX_PAK_TOTAL} bytes")
        dst = content.joinpath(*[p.lower() for p in parts])
        dst.parent.mkdir(parents=True, exist_ok=True)
        try:
            with z.open(info) as fin, open(dst, "wb") as fout:
                shutil.copyfileobj(fin, fout, 1 << 20)
        except NotImplementedError as e:
            raise BspError(f"{src}: pakfile entry {info.filename!r} uses an unsupported feature ({e})") from e
        except zipfile.BadZipFile as e:
            raise BspError(f"{src}: pakfile entry {info.filename!r} is corrupt ({e})") from e
        files += 1
    return files


def convert(src, out):
    src, out = Path(src), Path(out)
    try:
        bsp_obj = bsp.Bsp(src)
        per_model = bsp.model_brushes(bsp_obj)
        ents = bsp.entities(bsp_obj)
        names = bsp.texnames(bsp_obj)
        texdata = bsp.texdata_of(bsp_obj)
        texinfo = bsp.texinfo(bsp_obj)
        w = Writer(str(src), bsp.planes(bsp_obj), bsp.brushes(bsp_obj), bsp.sides(bsp_obj),
                   names, texdata, texinfo)
        for brushes in per_model:
            for bi in brushes:
                w.plan(bi)
        matched, orphans, unattached = match_disps(bsp_obj, w.planned)
        synth = []
        for data in orphans:
            keep = w.synth(data["plane"], data["texinfo"], data["corners"])
            if keep is None:
                unattached += 1
                continue
            synth.append((keep, {0: [data]}))
        props = bsp.static_props(bsp_obj)
        ndisps = len(bsp.displacements(bsp_obj))
    except BspError as e:
        raise SystemExit(str(e)) from e

    world = next((e for e in ents if dict(e).get("classname") == "worldspawn"), None)
    if world is None:
        raise SystemExit(f"{src}: no worldspawn")
    parts = ['versioninfo\n{\n\t"editorversion" "400"\n\t"mapversion" "1"\n\t"formatversion" "100"\n}',
             "world\n{\n\t\"id\" \"1\"\n" + "".join(f'\t"{k}" "{v}"\n' for k, v in world if k != "model")]
    by_brush = {}
    for (bi, ki), grids in matched.items():
        by_brush.setdefault(bi, {})[ki] = grids

    def emit(bi, place=None):
        return w.solid(w.planned[bi], by_brush.get(bi, {}), place)

    for bi in per_model[0]:
        s = emit(bi)
        if s:
            parts.append(s)
    for keep, disps in synth:
        s = w.solid(keep, disps)
        if s:
            parts.append(s)
    parts.append("}")

    used_models = set()
    for e in ents:
        kv = dict(e)
        if kv.get("classname") == "worldspawn":
            continue
        body = "".join(f'\t"{k}" "{v}"\n' for k, v in e if not (k == "model" and v.startswith("*")))
        solids = []
        m = kv.get("model", "")
        if m.startswith("*"):
            if not m[1:].isdigit() or int(m[1:]) >= len(per_model):
                raise SystemExit(f"{src}: entity {kv.get('classname')} names model {m}, past the model lump")
            idx = int(m[1:])
            used_models.add(idx)
            place = placement(kv)
            solids = [s for s in (emit(bi, place) for bi in per_model[idx]) if s]
        parts.append(f'entity\n{{\n\t"id" "{w.nid()}"\n' + body + "\n".join(solids) + ("\n" if solids else "") + "}")
    for p in props:
        lines = [f'entity\n{{\n\t"id" "{w.nid()}"\n\t"classname" "prop_static"',
                 f'\t"model" "{p["model"]}"',
                 f'\t"origin" "{fmt(p["origin"][0])} {fmt(p["origin"][1])} {fmt(p["origin"][2])}"',
                 f'\t"angles" "{fmt(p["angles"][0])} {fmt(p["angles"][1])} {fmt(p["angles"][2])}"',
                 f'\t"skin" "{p["skin"]}"', f'\t"solid" "{1 if p["solid"] else 0}"']
        if p["fademaxdist"] > 0:
            lines.append(f'\t"fademaxdist" "{p["fademaxdist"]:g}"')
        if p["scale"] != 1.0:
            lines.append(f'\t"modelscale" "{p["scale"]:g}"')
        lines.append("}")
        parts.append("\n".join(lines))

    out.mkdir(parents=True, exist_ok=True)
    (out / f"{src.stem}.vmf").write_text("\n".join(parts) + "\n", encoding="latin1")

    try:
        content = out / "content"
        files = extract_pak(bsp_obj, src, content)
    except BspError as e:
        raise SystemExit(str(e)) from e

    nbrushes = len(w.brushes)
    brush_orphans = nbrushes - len(set().union(*per_model)) if per_model else nbrushes
    unused = len(per_model) - 1 - len(used_models)
    attached = sum(len(v) for v in matched.values()) + len(synth)
    print(f"{src.name}: v{bsp_obj.version}, world brushes {len(per_model[0])}, brush entities {len(used_models)}, "
          f"static props {len(props)}, pakfile files {files}, brushes dropped {w.dropped_brushes}")
    if unused:
        print(f"  {unused} brush model(s) no entity names (not converted)")
    if brush_orphans:
        print(f"  {brush_orphans} brush(es) in no model (not converted)")
    if ndisps:
        print(f"  {attached}/{ndisps} displacement(s) as dispinfo ({len(synth)} on synthetic bases)")
    if unattached:
        print(f"  WARNING {unattached} displacement(s) not converted")
    return {"brushes": sum(1 for v in w.planned.values() if v is not None),
            "dropped": w.dropped_brushes, "props": len(props), "files": files,
            "attached": attached, "unattached": unattached, "disps": ndisps}


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    convert(sys.argv[1], sys.argv[2])


if __name__ == "__main__":
    main()
