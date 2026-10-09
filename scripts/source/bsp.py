"""Compiled Source BSP (v19-25) lump reader for `bsp2vmf.py`.

Reads the header in either lump order (standard `offset, length, version` or
L4D2's `version, offset, length`), the plane/brush/side/texinfo lumps, the
model tree (nodes/leafs/leafbrushes, with the wider v25 records selected by
lump version), the entity lump, the `sprp` static-prop game lump (versions
4-12), displacement info/verts, and the pakfile lump. Some compilers LZMA
compress individual lumps (`LZMA` magic + sizes + raw LZMA1 stream); those
are decompressed on read. Game-lump entries count their uncompressed size,
regular lumps their stored size.

Every offset and count is bound against the file before use; a bad file
raises BspError naming the file.
"""

import lzma
import re
import struct
from pathlib import Path

LUMP_ENTITIES, LUMP_PLANES, LUMP_TEXDATA = 0, 1, 2
LUMP_VERTEXINFO, LUMP_NODES, LUMP_TEXINFO = 3, 5, 6
LUMP_FACES, LUMP_EDGES, LUMP_SURFEDGES = 7, 12, 13
LUMP_MODELS, LUMP_LEAFS, LUMP_LEAFBRUSHES = 14, 10, 17
LUMP_BRUSHES, LUMP_BRUSHSIDES = 18, 19
LUMP_DISPINFO, LUMP_DISPVERTS = 26, 33
LUMP_GAME, LUMP_PAKFILE = 35, 40
LUMP_TEXDATA_STRING_DATA, LUMP_TEXDATA_STRING_TABLE = 43, 44

HEADER_LUMPS = 64
HEADER_SIZE = 8 + 16 * HEADER_LUMPS
MIN_BSP_VERSION, MAX_BSP_VERSION = 19, 25
RECORD_CAP = 16_000_000


class BspError(Exception):
    pass


def _raw_lzma1(payload, props, what):
    if len(props) != 5:
        raise BspError(f"{what}: lzma props are {len(props)} bytes, want 5")
    pb = props[0]
    lc, rem = pb % 9, pb // 9
    lp, pbv = rem % 5, rem // 5
    if lc > 8 or lp > 4 or pbv > 4:
        raise BspError(f"{what}: bad lzma props byte {pb:#x}")
    ds = struct.unpack_from("<I", props, 1)[0]
    if not 4096 <= ds <= 1 << 27:
        raise BspError(f"{what}: bad lzma dict size {ds}")
    dec = lzma.LZMADecompressor(format=lzma.FORMAT_RAW, filters=[
        {"id": lzma.FILTER_LZMA1, "dict_size": ds, "lc": lc, "lp": lp, "pb": pbv}])
    try:
        out = dec.decompress(payload)
    except lzma.LZMAError as e:
        raise BspError(f"{what}: lzma decompression failed ({e})") from e
    return out


def _maybe_lzma_lump(stored, usize, what):
    if len(stored) < 17 or stored[:4] != b"LZMA":
        return stored
    usize_hdr, csize = struct.unpack_from("<II", stored, 4)
    if 12 + 5 + csize != len(stored):
        raise BspError(f"{what}: lzma sizes do not fit the lump ({len(stored)} bytes)")
    out = _raw_lzma1(stored[17:17 + csize], stored[12:17], what)
    if len(out) < usize_hdr:
        raise BspError(f"{what}: lzma lump decompresses to {len(out)} bytes, want {usize_hdr}")
    return out[:usize_hdr]


class Bsp:
    def __init__(self, path):
        self.path = str(path)
        try:
            self.data = Path(path).read_bytes()
        except OSError as e:
            raise BspError(f"{self.path}: cannot read ({e})") from e
        data = self.data
        if len(data) < HEADER_SIZE:
            raise BspError(f"{self.path}: {len(data)} bytes, smaller than a bsp header")
        ident, self.version = struct.unpack_from("<4si", data, 0)
        self._cache = {}
        if ident != b"VBSP":
            raise BspError(f"{self.path}: not a Source BSP (ident {ident!r})")
        if not MIN_BSP_VERSION <= self.version <= MAX_BSP_VERSION:
            raise BspError(f"{self.path}: BSP v{self.version}, want v{MIN_BSP_VERSION}-{MAX_BSP_VERSION}")
        std = [struct.unpack_from("<iii", data, 8 + 16 * i) for i in range(HEADER_LUMPS)]
        if all(o + s <= len(data) and (s == 0 or o >= HEADER_SIZE) for o, s, _ in std):
            self.lumps = [(o, s, v) for o, s, v in std]
            return
        l4d2 = [(o, s, v) for v, o, s in std]
        if all(o + s <= len(data) and (s == 0 or o >= HEADER_SIZE) for o, s, v in l4d2):
            self.lumps = l4d2
            return
        raise BspError(f"{self.path}: lump table points past end of file ({len(data)} bytes)")

    def lump_version(self, i):
        return self.lumps[i][2]

    def lump(self, i):
        if i in self._cache:
            return self._cache[i]
        off, size, _ = self.lumps[i]
        if off + size > len(self.data):
            raise BspError(f"{self.path}: lump {i} runs past end of file")
        stored = self.data[off:off + size]
        out = _maybe_lzma_lump(stored, size, f"{self.path}: lump {i}")
        self._cache[i] = out
        return out

    def game_lump(self, gid):
        raw = self.lump(LUMP_GAME)
        if not raw:
            return None
        if len(raw) < 4:
            raise BspError(f"{self.path}: game lump is {len(raw)} bytes")
        count = struct.unpack_from("<i", raw, 0)[0]
        if not 0 <= count <= 256:
            raise BspError(f"{self.path}: game lump claims {count} entries")
        if 4 + 16 * count > len(raw):
            raise BspError(f"{self.path}: game lump table runs past its end")
        for i in range(count):
            eid, _flags, ver, off, size = struct.unpack_from("<4sHHii", raw, 4 + 16 * i)
            if eid != gid:
                continue
            if not (0 <= off and 0 <= size <= len(self.data) - off):
                raise BspError(f"{self.path}: game lump {gid!r} runs past end of file")
            if self.data[off:off + 4] == b"LZMA":
                if off + 17 > len(self.data):
                    raise BspError(f"{self.path}: game lump {gid!r} lzma header is truncated")
                usize, csize = struct.unpack_from("<II", self.data, off + 4)
                if usize != size:
                    raise BspError(f"{self.path}: game lump {gid!r} says {size} bytes, lzma says {usize}")
                if 12 + 5 + csize > len(self.data) - off:
                    raise BspError(f"{self.path}: game lump {gid!r} lzma data runs past end of file")
                out = _raw_lzma1(self.data[off + 17:off + 17 + csize], self.data[off + 12:off + 17],
                                 f"{self.path}: game lump {gid!r}")
                if len(out) < size:
                    raise BspError(f"{self.path}: game lump {gid!r} decompresses to {len(out)}, want {size}")
                return ver, out[:size]
            return ver, self.data[off:off + size]
        return None

    def count(self, i, size, what):
        n = len(self.lump(i))
        if n % size:
            raise BspError(f"{self.path}: {what} lump is {n} bytes, not a multiple of {size}")
        n //= size
        if n > RECORD_CAP:
            raise BspError(f"{self.path}: {what} lump claims {n} records")
        return n


def entities(bsp):
    text = bsp.lump(LUMP_ENTITIES).decode("latin1").rstrip("\0")
    return [re.findall(r'"([^"]*)"\s+"([^"]*)"', block)
            for block in re.findall(r"\{(.*?)\}", text, re.S)]


def planes(bsp):
    raw = bsp.lump(LUMP_PLANES)
    n = bsp.count(LUMP_PLANES, 20, "planes")
    return [struct.unpack_from("<4f", raw, k * 20) for k in range(n)]


def texnames(bsp):
    blob = bsp.lump(LUMP_TEXDATA_STRING_DATA)
    raw = bsp.lump(LUMP_TEXDATA_STRING_TABLE)
    if len(raw) % 4:
        raise BspError(f"{bsp.path}: texdata string table is {len(raw)} bytes")
    names = []
    for (o,) in struct.iter_unpack("<i", raw):
        if not 0 <= o < len(blob):
            raise BspError(f"{bsp.path}: texdata string offset {o} is out of range")
        end = blob.find(b"\0", o)
        if end < 0:
            raise BspError(f"{bsp.path}: texdata string at {o} is unterminated")
        if end - o > 256:
            raise BspError(f"{bsp.path}: texdata string at {o} is longer than 256 bytes")
        names.append(blob[o:end].decode("latin1"))
    return names


def texdata_of(bsp):
    raw = bsp.lump(LUMP_TEXDATA)
    n = bsp.count(LUMP_TEXDATA, 32, "texdata")
    return [struct.unpack_from("<i", raw, k * 32 + 12)[0] for k in range(n)]


def texinfo(bsp):
    raw = bsp.lump(LUMP_TEXINFO)
    n = bsp.count(LUMP_TEXINFO, 72, "texinfo")
    return [(struct.unpack_from("<4f", raw, k * 72), struct.unpack_from("<4f", raw, k * 72 + 16),
             struct.unpack_from("<i", raw, k * 72 + 64)[0],
             struct.unpack_from("<i", raw, k * 72 + 68)[0]) for k in range(n)]


def brushes(bsp):
    raw = bsp.lump(LUMP_BRUSHES)
    n = bsp.count(LUMP_BRUSHES, 12, "brushes")
    recs = [struct.unpack_from("<3i", raw, k * 12) for k in range(n)]
    nsides = bsp.count(LUMP_BRUSHSIDES, 8, "brushsides")
    for k, (first, count, _contents) in enumerate(recs):
        if not (0 <= first and 0 <= count and first + count <= nsides):
            raise BspError(f"{bsp.path}: brush {k} names sides {first}..{first + count} of {nsides}")
    return recs


def sides(bsp):
    """(planenum, texinfo, bevel, dispinfo) per brush side; v25 sides are
    16-byte records with a 32-bit planenum and the bevel flag at offset 12."""
    raw = bsp.lump(LUMP_BRUSHSIDES)
    ver = bsp.lump_version(LUMP_BRUSHSIDES)
    nplanes = bsp.count(LUMP_PLANES, 20, "planes")
    ntex = bsp.count(LUMP_TEXINFO, 72, "texinfo")
    out = []
    if ver == 0:
        n = bsp.count(LUMP_BRUSHSIDES, 8, "brushsides")
        for k in range(n):
            pn, ti, di, bv = struct.unpack_from("<Hhhh", raw, k * 8)
            if pn >= nplanes:
                raise BspError(f"{bsp.path}: side {k} names plane {pn} of {nplanes}")
            if not -1 <= ti < ntex:
                raise BspError(f"{bsp.path}: side {k} names texinfo {ti} of {ntex}")
            out.append((pn, ti, bv & 1, di))
    elif ver == 1:
        n = bsp.count(LUMP_BRUSHSIDES, 16, "brushsides")
        for k in range(n):
            pn, ti, di = struct.unpack_from("<Ihh", raw, k * 16)
            bv = struct.unpack_from("<i", raw, k * 16 + 12)[0]
            if pn >= nplanes:
                raise BspError(f"{bsp.path}: side {k} names plane {pn} of {nplanes}")
            if not -1 <= ti < ntex:
                raise BspError(f"{bsp.path}: side {k} names texinfo {ti} of {ntex}")
            out.append((pn, ti, bv & 1, di))
    else:
        raise BspError(f"{bsp.path}: brushsides lump v{ver} is not supported")
    return out


def models(bsp):
    raw = bsp.lump(LUMP_MODELS)
    n = bsp.count(LUMP_MODELS, 48, "models")
    return [struct.unpack_from("<i", raw, k * 48 + 36)[0] for k in range(n)]


def model_brushes(bsp):
    """Brush indices per model, by walking each model's node tree. Lump
    versions select the record widths (v25 nodes are 48 bytes, leaf
    brush ranges 32-bit)."""
    nraw = bsp.lump(LUMP_NODES)
    nver = bsp.lump_version(LUMP_NODES)
    lraw = bsp.lump(LUMP_LEAFS)
    lver = bsp.lump_version(LUMP_LEAFS)
    if nver == 0:
        nn = bsp.count(LUMP_NODES, 32, "nodes")
        children = [struct.unpack_from("<2i", nraw, k * 32 + 4) for k in range(nn)]
    elif nver == 1:
        nn = bsp.count(LUMP_NODES, 48, "nodes")
        children = [struct.unpack_from("<2i", nraw, k * 48 + 4) for k in range(nn)]
    else:
        raise BspError(f"{bsp.path}: nodes lump v{nver} is not supported")
    lbraw = bsp.lump(LUMP_LEAFBRUSHES)
    lbver = bsp.lump_version(LUMP_LEAFBRUSHES)
    if lbver == 0:
        nlb = bsp.count(LUMP_LEAFBRUSHES, 2, "leafbrushes")
        leafbrushes = struct.unpack("<%dH" % nlb, lbraw) if nlb else ()
    elif lbver == 1:
        nlb = bsp.count(LUMP_LEAFBRUSHES, 4, "leafbrushes")
        leafbrushes = struct.unpack("<%dI" % nlb, lbraw) if nlb else ()
    else:
        raise BspError(f"{bsp.path}: leafbrushes lump v{lbver} is not supported")
    if lver in (0, 1):
        nl = bsp.count(LUMP_LEAFS, 32, "leafs")
        ranges = [struct.unpack_from("<2H", lraw, k * 32 + 24) for k in range(nl)]
    elif lver == 2:
        nl = bsp.count(LUMP_LEAFS, 56, "leafs")
        ranges = [struct.unpack_from("<2I", lraw, k * 56 + 44) for k in range(nl)]
    else:
        raise BspError(f"{bsp.path}: leafs lump v{lver} is not supported")
    for k, (first, count) in enumerate(ranges):
        if first + count > nlb:
            raise BspError(f"{bsp.path}: leaf {k} names leafbrushes {first}..{first + count} of {nlb}")
    nbrushes = bsp.count(LUMP_BRUSHES, 12, "brushes")
    out = []
    for m, head in enumerate(models(bsp)):
        if head < 0:
            out.append([])
            continue
        if head >= nn:
            raise BspError(f"{bsp.path}: model {m} names headnode {head} of {nn}")
        seen, stack, found, visited = set(), [head], [], set()
        while stack:
            x = stack.pop()
            if x < 0:
                leaf = -1 - x
                if leaf >= nl:
                    raise BspError(f"{bsp.path}: model {m} reaches leaf {leaf} of {nl}")
                first, count = ranges[leaf]
                for t in leafbrushes[first:first + count]:
                    if t >= nbrushes:
                        raise BspError(f"{bsp.path}: model {m} reaches brush {t} of {nbrushes}")
                    if t not in seen:
                        seen.add(t)
                        found.append(t)
                continue
            if x >= nn:
                raise BspError(f"{bsp.path}: model {m} reaches node {x} of {nn}")
            if x in visited:
                continue
            visited.add(x)
            stack.extend(children[x])
        out.append(sorted(found))
    return out


SPRITE_STRIDES = {4: 56, 5: 60, 6: 64, 7: 68, 8: 72, 9: 72}
MIN_SPRP_VERSION, MAX_SPRP_VERSION = 4, 12


def static_props(bsp):
    """prop_static dicts (model, origin, angles, skin, solid, fademaxdist,
    scale) from the `sprp` game lump. Versions 4-9 have fixed strides;
    10-12 derive the stride from the lump and validate every record."""
    hit = bsp.game_lump(b"prps")
    if hit is None:
        return []
    ver, data = hit
    if not MIN_SPRP_VERSION <= ver <= MAX_SPRP_VERSION:
        raise BspError(f"{bsp.path}: static props v{ver}, want v{MIN_SPRP_VERSION}-{MAX_SPRP_VERSION}")
    if len(data) < 4:
        raise BspError(f"{bsp.path}: static props lump is {len(data)} bytes")
    nnames = struct.unpack_from("<i", data, 0)[0]
    if not 0 <= nnames <= 65536:
        raise BspError(f"{bsp.path}: static props name {nnames} model names")
    off = 4 + 128 * nnames
    if off + 4 > len(data):
        raise BspError(f"{bsp.path}: static props names run past the lump")
    names = [data[4 + 128 * k:4 + 128 * (k + 1)].split(b"\0")[0].decode("latin1")
             for k in range(nnames)]
    nleaves = struct.unpack_from("<i", data, off)[0]
    if not 0 <= nleaves <= 1 << 20:
        raise BspError(f"{bsp.path}: static props name {nleaves} leaf table entries")
    off += 4 + 2 * nleaves
    if off + 4 > len(data):
        raise BspError(f"{bsp.path}: static props leaves run past the lump")
    nprops = struct.unpack_from("<i", data, off)[0]
    if not 0 <= nprops <= 1 << 20:
        raise BspError(f"{bsp.path}: static props name {nprops} prop records")
    off += 4
    if nprops == 0:
        return []
    if ver in SPRITE_STRIDES:
        stride = SPRITE_STRIDES[ver]
        if len(data) - off != stride * nprops:
            raise BspError(f"{bsp.path}: static props v{ver} body is {len(data) - off} bytes for "
                           f"{nprops} props, want {stride * nprops}")
    else:
        rem = len(data) - off
        if rem % nprops or not 72 <= rem // nprops <= 256:
            raise BspError(f"{bsp.path}: static props v{ver} has a {rem}-byte body for {nprops} props")
        stride = rem // nprops
    out = []
    for k in range(nprops):
        base = off + k * stride
        ox, oy, oz, ax, ay, az = struct.unpack_from("<6f", data, base)
        if not all(abs(v) != float("inf") and v == v for v in (ox, oy, oz, ax, ay, az)):
            raise BspError(f"{bsp.path}: static prop {k} has a bad origin/angles")
        ptype, _firstleaf, _leafcount = struct.unpack_from("<3H", data, base + 24)
        solid = data[base + 30]
        skin = struct.unpack_from("<i", data, base + 32)[0]
        fademax = struct.unpack_from("<f", data, base + 40)[0]
        if ptype >= len(names):
            raise BspError(f"{bsp.path}: static prop {k} names model {ptype} of {len(names)}")
        scale = 1.0
        if ver >= 11 and stride >= 76:
            scale = struct.unpack_from("<f", data, base + stride - 4)[0]
            if scale != scale or abs(scale) == float("inf"):
                raise BspError(f"{bsp.path}: static prop {k} has a bad scale")
        out.append({"model": names[ptype],
                    "origin": (ox, oy, oz), "angles": (ax, ay, az),
                    "skin": skin, "solid": solid != 0,
                    "fademaxdist": fademax if fademax > 0 else 0.0,
                    "scale": scale if scale > 0 else 1.0})
    return out


DISPINFO_SIZE = 176
DISPVERT_SIZE = 20


def displacements(bsp):
    """(power, start, vertstart, mapface) per displacement, in lump order."""
    raw = bsp.lump(LUMP_DISPINFO)
    if not raw:
        return []
    if bsp.lump_version(LUMP_DISPINFO) != 0:
        raise BspError(f"{bsp.path}: dispinfo lump v{bsp.lump_version(LUMP_DISPINFO)} is not supported")
    n = bsp.count(LUMP_DISPINFO, DISPINFO_SIZE, "dispinfo")
    nverts = bsp.count(LUMP_DISPVERTS, DISPVERT_SIZE, "dispverts")
    nfaces = bsp.count(LUMP_FACES, 56, "faces")
    out = []
    for k in range(n):
        start = struct.unpack_from("<3f", raw, k * DISPINFO_SIZE)
        vertstart, _tristart, power = struct.unpack_from("<3i", raw, k * DISPINFO_SIZE + 12)
        mapface = struct.unpack_from("<H", raw, k * DISPINFO_SIZE + 36)[0]
        if power not in (2, 3, 4):
            raise BspError(f"{bsp.path}: displacement {k} has power {power}")
        if not (0 <= vertstart and vertstart + (2 ** power + 1) ** 2 <= nverts):
            raise BspError(f"{bsp.path}: displacement {k} names verts past the dispverts lump")
        if mapface >= nfaces:
            raise BspError(f"{bsp.path}: displacement {k} names face {mapface} of {nfaces}")
        out.append({"power": power, "start": start, "vertstart": vertstart, "mapface": mapface})
    return out


def disp_verts(bsp, vertstart, power):
    """(field xyz, dist, alpha) rows of one displacement, in lump order."""
    raw = bsp.lump(LUMP_DISPVERTS)
    n = (2 ** power + 1) ** 2
    out = []
    for j in range(n):
        f0, f1, f2, dist, alpha = struct.unpack_from("<5f", raw, (vertstart + j) * DISPVERT_SIZE)
        out.append(((f0, f1, f2), dist, alpha))
    return out


def disp_face(bsp, mapface):
    """(plane normal xyz, dist, texinfo, corners) of the face a displacement
    lives on. Corners follow the face winding; the lump-ordered displacement
    grid starts at the corner nearest the displacement start, running towards
    the next corner in this winding."""
    faces = bsp.lump(LUMP_FACES)
    planenum = struct.unpack_from("<H", faces, mapface * 56)[0]
    side = faces[mapface * 56 + 2]
    firstedge = struct.unpack_from("<i", faces, mapface * 56 + 4)[0]
    numedges = struct.unpack_from("<h", faces, mapface * 56 + 8)[0]
    texinfo = struct.unpack_from("<h", faces, mapface * 56 + 10)[0]
    nplanes = bsp.count(LUMP_PLANES, 20, "planes")
    if planenum >= nplanes:
        raise BspError(f"{bsp.path}: face {mapface} names plane {planenum} of {nplanes}")
    ntex = bsp.count(LUMP_TEXINFO, 72, "texinfo")
    if not -1 <= texinfo < ntex:
        raise BspError(f"{bsp.path}: face {mapface} names texinfo {texinfo} of {ntex}")
    planes = bsp.lump(LUMP_PLANES)
    nx, ny, nz, dist = struct.unpack_from("<4f", planes, planenum * 20)
    if side:
        nx, ny, nz, dist = -nx, -ny, -nz, -dist
    surf = bsp.lump(LUMP_SURFEDGES)
    if not 3 <= numedges <= 1024 or firstedge < 0 or (firstedge + numedges) * 4 > len(surf):
        raise BspError(f"{bsp.path}: face {mapface} names edges past the surfedges lump")
    edges = bsp.lump(LUMP_EDGES)
    vinfo = bsp.lump(LUMP_VERTEXINFO)
    idx = struct.unpack_from("<%di" % numedges, surf, firstedge * 4)
    corners = []
    for s in idx:
        if abs(s) * 4 + 4 > len(edges):
            raise BspError(f"{bsp.path}: face {mapface} names an edge past the edges lump")
        a, b = struct.unpack_from("<2H", edges, abs(s) * 4)
        vi = a if s >= 0 else b
        if (vi + 1) * 12 > len(vinfo):
            raise BspError(f"{bsp.path}: face {mapface} names a vertex past the vertex lump")
        corners.append(struct.unpack_from("<3f", vinfo, vi * 12))
    if len(corners) != 4:
        raise BspError(f"{bsp.path}: displacement face {mapface} has {len(corners)} corners, want 4")
    return (nx, ny, nz, dist), texinfo, corners
