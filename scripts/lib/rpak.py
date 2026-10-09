"""Reader for an UNCOMPRESSED rpak v8. Compressed input is decompressed via RSX first.

`rsx -decompresspak` writes the engine's decompressed image (cmpSize=dcmpSize, compression
flag cleared). In that FILE, pages are concatenated in page-index order. RSX's
AllocateSegments alignment is a MEMORY layout -- applying it to the file reads garbage.

A (01) patch pak still has patchCount != 0 after decompress. Its own pages start at
patchPageCount; earlier pages live in the parent pak. The patch edit stream is skipped
here -- assets whose data still sits in a parent page need that parent opened too.
"""
import mmap
import os
import struct
import shutil
import subprocess

HDR_SIZE = 0x80
ASSET_SIZE = 0x50
RSX_EXE = os.environ.get("R5ED_RSX") or shutil.which("rsx") or "rsx.exe"
PAK_HEADER_FLAGS_COMPRESSED = 0x0100


def ensure_decompressed(path):
    """Return a path to an uncompressed rpak, running RSX -decompresspak if needed."""
    path = os.path.abspath(path)
    with open(path, "rb") as f:
        head = f.read(0x80)
    if len(head) < 0x80 or struct.unpack_from("<i", head, 0)[0] != 0x6B615052:
        raise ValueError("not an rpak: %s" % path)
    flags = struct.unpack_from("<H", head, 6)[0]
    cmp_size, = struct.unpack_from("<q", head, 0x18)
    dcmp_size, = struct.unpack_from("<q", head, 0x30)
    if not (flags & PAK_HEADER_FLAGS_COMPRESSED) and cmp_size == dcmp_size:
        return path

    dec = path + ".dec.rpak"
    if os.path.isfile(dec):
        with open(dec, "rb") as f:
            dh = f.read(0x80)
        dflags = struct.unpack_from("<H", dh, 6)[0]
        dcmp, = struct.unpack_from("<q", dh, 0x18)
        ddcmp, = struct.unpack_from("<q", dh, 0x30)
        if (not (dflags & PAK_HEADER_FLAGS_COMPRESSED) and dcmp == ddcmp
                and ddcmp == dcmp_size and os.path.getsize(dec) == dcmp_size):
            return dec

    if not os.path.isfile(RSX_EXE):
        raise FileNotFoundError("rsx.exe not at %s" % RSX_EXE)
    cmd = [RSX_EXE, "-nogui", "-nocachedb", "-decompresspak", path]
    print("RSX", " ".join(cmd))
    r = subprocess.run(cmd, cwd=os.path.dirname(RSX_EXE))
    if r.returncode != 0 or not os.path.isfile(dec):
        raise RuntimeError("rsx -decompresspak failed (%s) for %s" % (r.returncode, path))
    return dec


def open_pak(path):
    """Open a pak. A `(01)` patch is opened with its base as parent so extra assets resolve."""
    path = os.path.abspath(path)
    base = path.replace("(01)", "", 1)
    if base != path:
        return Pak(path, parent=Pak(base))
    return Pak(path)


class PagePtr:
    __slots__ = ("index", "offset")

    def __init__(self, index, offset):
        self.index = index
        self.offset = offset

    def __repr__(self):
        return "PagePtr(%d,%d)" % (self.index, self.offset)

    def valid(self):
        return not (self.index == 0 and self.offset == 0)


class Asset:
    __slots__ = ("guid", "head", "data", "starpak", "opt_starpak", "page_end",
                 "dep_index", "dep_count", "header_size", "version", "type")


class Pak:
    def __init__(self, path, parent=None):
        path = ensure_decompressed(path)
        self.path = path
        self.parent = parent
        self._fp = open(path, "rb")
        self.buf = buf = mmap.mmap(self._fp.fileno(), 0, access=mmap.ACCESS_READ)
        magic, self.version, self.flags = struct.unpack_from("<iHH", buf, 0)
        if magic != 0x6B615052:
            raise ValueError("not an rpak: magic=%08X" % magic)
        if self.version != 8:
            raise ValueError("only rpak v8 supported, got %d" % self.version)
        (self.cmp_size,) = struct.unpack_from("<q", buf, 0x18)
        (self.dcmp_size,) = struct.unpack_from("<q", buf, 0x30)
        (self.sp_len, self.opt_sp_len, self.num_segments, self.num_pages,
         self.patch_count) = struct.unpack_from("<hhHHh", buf, 0x48)
        (self.num_pointers, self.num_assets, self.num_guid_refs,
         self.num_deps, self.num_ext_refs, self.ext_refs_size) = struct.unpack_from("<6i", buf, 0x54)

        if self.flags & PAK_HEADER_FLAGS_COMPRESSED or self.cmp_size != self.dcmp_size:
            raise ValueError("pak still compressed after RSX -- %s" % path)

        # Patch headers sit immediately after the 0x80 file header, BEFORE streaming paths.
        off = HDR_SIZE
        self.patch_stream_size = 0
        self.first_page = 0
        if self.patch_count:
            self.patch_stream_size, self.first_page = struct.unpack_from("<ii", buf, off)
            off += 8 + 16 * self.patch_count + 2 * self.patch_count

        off += self.sp_len + self.opt_sp_len
        self.segments = [struct.unpack_from("<iiq", buf, off + 16 * i) for i in range(self.num_segments)]
        off += 16 * self.num_segments
        self.pages = [struct.unpack_from("<iiI", buf, off + 12 * i) for i in range(self.num_pages)]
        off += 12 * self.num_pages
        self.pointers = [PagePtr(*struct.unpack_from("<ii", buf, off + 8 * i)) for i in range(self.num_pointers)]
        off += 8 * self.num_pointers
        self.assets_off = off
        off += ASSET_SIZE * self.num_assets
        self.guid_refs = [PagePtr(*struct.unpack_from("<ii", buf, off + 8 * i)) for i in range(self.num_guid_refs)]
        off += 8 * self.num_guid_refs
        off += 4 * self.num_deps
        if self.num_ext_refs:
            off += 4 * self.num_ext_refs + self.ext_refs_size
        # v8 stores two extra sized blocks between the relation table and the page data;
        # skipping them shifts every page offset (the file-size check below is the proof).
        (self.unk74, self.unk78) = struct.unpack_from("<II", buf, 0x74)
        off += self.unk74 + self.unk78
        if self.patch_count:
            off += self.patch_stream_size

        self.page_file_off = [None] * self.num_pages
        for i in range(self.first_page, self.num_pages):
            self.page_file_off[i] = off
            off += self.pages[i][2]
        self.page_data_end = off
        if self.page_data_end != len(buf):
            raise ValueError("page data ends at %d but file is %d bytes -- table math is wrong"
                             % (self.page_data_end, len(buf)))

        self.assets = []
        for i in range(self.num_assets):
            a = Asset()
            b = self.assets_off + ASSET_SIZE * i
            (a.guid,) = struct.unpack_from("<Q", buf, b)
            a.head = PagePtr(*struct.unpack_from("<ii", buf, b + 0x10))
            a.data = PagePtr(*struct.unpack_from("<ii", buf, b + 0x18))
            (a.starpak, a.opt_starpak) = struct.unpack_from("<qq", buf, b + 0x20)
            (a.page_end,) = struct.unpack_from("<H", buf, b + 0x30)
            (a.dep_index,) = struct.unpack_from("<I", buf, b + 0x38)
            (a.dep_count,) = struct.unpack_from("<i", buf, b + 0x40)
            (a.header_size, a.version) = struct.unpack_from("<II", buf, b + 0x44)
            a.type = buf[b + 0x4C:b + 0x50].decode("ascii", "replace")
            self.assets.append(a)

        # page-local relocation set: {(pageIdx, offset)} whose stored qword is a PagePtr
        self.reloc = set((p.index, p.offset) for p in self.pointers)
        self.guid_ref_set = set((p.index, p.offset) for p in self.guid_refs)
        # per-page sorted offsets so a range query is a bisect, not a scan of every relocation
        self.reloc_by_page = {}
        for p in self.pointers:
            self.reloc_by_page.setdefault(p.index, []).append(p.offset)
        for v in self.reloc_by_page.values():
            v.sort()

    def reloc_range(self, page, lo, hi):
        """Relocation offsets in [lo, hi) on `page`, relative to lo."""
        import bisect
        offs = self.reloc_by_page.get(page)
        if not offs:
            return []
        i = bisect.bisect_left(offs, lo)
        j = bisect.bisect_left(offs, hi)
        return [offs[k] - lo for k in range(i, j)]

    def _owner(self, page_idx):
        if 0 <= page_idx < self.num_pages and self.page_file_off[page_idx] is not None:
            return self
        if self.parent is None:
            raise ValueError("page %d lives in the parent pak (%s)" % (page_idx, self.path))
        return self.parent._owner(page_idx)

    def src(self, ptr):
        """(mmap, file_offset) for a PagePtr, following parent pages when needed."""
        owner = self._owner(ptr.index)
        return owner.buf, owner.page_file_off[ptr.index] + ptr.offset

    def file_off(self, ptr):
        buf, off = self.src(ptr)
        if buf is not self.buf:
            raise ValueError("page %d is in the parent -- use src()/read() not file_off+buf" % ptr.index)
        return off

    def close(self):
        if getattr(self, "buf", None) is not None:
            self.buf.close()
            self.buf = None
        if getattr(self, "_fp", None) is not None:
            self._fp.close()
            self._fp = None

    def read(self, ptr, size):
        buf, o = self.src(ptr)
        return buf[o:o + size]

    def deref(self, ptr):
        """Follow a stored PagePtr at `ptr` (must be a registered relocation)."""
        buf, o = self.src(ptr)
        idx, offs = struct.unpack_from("<ii", buf, o)
        return PagePtr(idx, offs)

    def cstr(self, ptr, limit=1024):
        buf, o = self.src(ptr)
        end = buf.find(b"\x00", o, o + limit)
        return buf[o:end].decode("utf-8", "replace") if end >= 0 else ""

    def by_type(self, fourcc):
        return [a for a in self.assets if a.type == fourcc]
