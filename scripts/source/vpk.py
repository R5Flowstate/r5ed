"""Valve pak (`*_dir.vpk`, versions 1 and 2) listing and extraction.

v1 has a 12-byte header (signature, version, tree size); v2 a 28-byte header
with four extra section sizes. The tree maps extension -> path -> filename to
an 18-byte entry (crc, preload length, archive index, offset, length,
0xFFFF). Preload bytes sit inline after each entry. Archive index 0x7FFF
means the bytes live in the dir file's own data section; otherwise they are
an absolute slice of the numbered `<prefix>_NNN.vpk` archive.
"""

import re
import struct
from pathlib import Path

SIG = 0x55AA1234
EMBEDDED = 0x7FFF
ENTRY = struct.Struct("<IHHIIH")


class VpkError(Exception):
    pass


def _archive_path(dir_path, index):
    m = re.match(r"(.*)_dir$", Path(dir_path).stem, re.IGNORECASE)
    if m is None:
        raise VpkError(f"{dir_path}: not a *_dir.vpk, cannot find archive {index}")
    return Path(dir_path).parent / f"{m.group(1)}_{index:03d}.vpk"


class VpkFile:
    """One file inside a Vpk; duck-types the Path reads Content callers use."""

    def __init__(self, vpk, rel):
        self._vpk = vpk
        self._rel = rel

    def read_bytes(self):
        return self._vpk.read(self._rel)

    def read_text(self, encoding="utf-8", errors="replace"):
        return self.read_bytes().decode(encoding, errors)

    def __str__(self):
        return f"{self._vpk.dir_path}!{self._rel}"

    def __repr__(self):
        return f"VpkFile({self})"


class Vpk:
    def __init__(self, dir_path):
        self.dir_path = Path(dir_path)
        data = self.dir_path.read_bytes()
        name = str(self.dir_path)
        if len(data) < 12 or struct.unpack_from("<I", data, 0)[0] != SIG:
            raise VpkError(f"{name}: not a vpk")
        version = struct.unpack_from("<I", data, 4)[0]
        if version == 1:
            head, tree_size = 12, struct.unpack_from("<I", data, 8)[0]
        elif version == 2:
            if len(data) < 28:
                raise VpkError(f"{name}: vpk v2 header truncated")
            head, tree_size = 28, struct.unpack_from("<I", data, 8)[0]
        else:
            raise VpkError(f"{name}: unsupported vpk version {version}")
        if head + tree_size > len(data):
            raise VpkError(f"{name}: vpk tree runs past end of file")
        self._base = head + tree_size
        self._data = data
        self._entries = {}
        self._parse(name, head)

    def _cstr(self, name, off, end):
        stop = self._data.find(b"\0", off, end)
        if stop < 0:
            raise VpkError(f"{name}: vpk tree string runs past its section")
        return self._data[off:stop].decode("latin-1"), stop + 1

    def _parse(self, name, off):
        end = self._base
        while True:
            ext, off = self._cstr(name, off, end)
            if not ext:
                return
            ext = ext.lower()
            while True:
                path, off = self._cstr(name, off, end)
                if not path:
                    break
                path = path.replace("\\", "/").strip().strip("/")
                while True:
                    stem, off = self._cstr(name, off, end)
                    if not stem:
                        break
                    if off + ENTRY.size > end:
                        raise VpkError(f"{name}: vpk entry runs past its section")
                    crc, pre_len, archive, eo, el, term = ENTRY.unpack_from(self._data, off)
                    off += ENTRY.size
                    if term != 0xFFFF:
                        raise VpkError(f"{name}: vpk entry terminator is {term:#x}")
                    if off + pre_len > end:
                        raise VpkError(f"{name}: vpk preload runs past its section")
                    pre = bytes(self._data[off:off + pre_len])
                    off += pre_len
                    rel = f"{path}/{stem}.{ext}".lower() if path else f"{stem}.{ext}".lower()
                    self._entries.setdefault(rel, (crc, archive, eo, el, pre))

    def paths(self):
        return sorted(self._entries)

    def __len__(self):
        return len(self._entries)

    def __contains__(self, rel):
        return rel.replace("\\", "/").lower().lstrip("/") in self._entries

    def read(self, rel):
        key = rel.replace("\\", "/").lower().lstrip("/")
        hit = self._entries.get(key)
        if hit is None:
            raise VpkError(f"{self.dir_path}: no such file {rel!r}")
        _, archive, eo, el, pre = hit
        if archive == EMBEDDED:
            size = len(self._data)
            if self._base + eo + el <= size:
                off = self._base + eo
            elif eo >= self._base and eo + el <= size:
                off = eo
            else:
                raise VpkError(f"{self.dir_path}: {rel!r} data runs past end of file")
            chunk = self._data[off:off + el]
        else:
            ap = _archive_path(self.dir_path, archive)
            try:
                f = open(ap, "rb")
            except OSError as e:
                raise VpkError(f"{self.dir_path}: cannot read {ap}: {e}") from e
            with f:
                f.seek(0, 2)
                if eo + el > f.tell():
                    raise VpkError(f"{self.dir_path}: {rel!r} data runs past end of {ap}")
                f.seek(eo)
                chunk = f.read(el)
            if len(chunk) != el:
                raise VpkError(f"{self.dir_path}: {rel!r} data runs past end of {ap}")
        return pre + chunk
