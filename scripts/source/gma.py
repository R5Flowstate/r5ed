"""Garry's Mod addon (.gma, versions 1-3) listing and extraction.

Header: `GMAD`, one version byte (1-3), steamid u64, timestamp u64, then only
for v2+ a required-content string list (strings up to the first empty one),
then name, description and author strings and an addon version u32. The file
table repeats file number u32 (0 ends it), path string, size s64 and crc u32;
the file bytes follow back to back, then a trailing addon crc u32 the game
ignores (writers may store 0), so this reader parses but never verifies crcs.
"""

import argparse
import struct
from pathlib import Path

SIG = b"GMAD"
MAX_STR = 1 << 20
MAX_PATH_STR = 4096
MAX_FILES = 250_000
MAX_REQUIRED = 4096


class GmaError(Exception):
    pass


class GmaFile:
    """One file inside a Gma; duck-types the Path reads Content callers use."""

    def __init__(self, gma, rel):
        self._gma = gma
        self._rel = rel

    def read_bytes(self):
        return self._gma.read(self._rel)

    def read_text(self, encoding="utf-8", errors="replace"):
        return self.read_bytes().decode(encoding, errors)

    def __str__(self):
        return f"{self._gma.path}!{self._rel}"

    def __repr__(self):
        return f"GmaFile({self})"


class Gma:
    def __init__(self, path):
        self.path = Path(path)
        data = self.path.read_bytes()
        name = str(self.path)
        n = len(data)
        if n < 5 or data[:4] != SIG:
            raise GmaError(f"{name}: not a gma")
        version = data[4]
        if version not in (1, 2, 3):
            raise GmaError(f"{name}: unsupported gma version {version}")
        if n < 21:
            raise GmaError(f"{name}: gma header truncated")
        self.format_version = version
        self.steamid, self.timestamp = struct.unpack_from("<QQ", data, 5)
        off = 21
        self.required = []
        if version > 1:
            while True:
                s, off = self._cstr(data, off, MAX_STR, "required content", name)
                if not s:
                    break
                self.required.append(s)
                if len(self.required) > MAX_REQUIRED:
                    raise GmaError(f"{name}: gma required-content list too long")
        self.name, off = self._cstr(data, off, MAX_STR, "addon name", name)
        self.description, off = self._cstr(data, off, MAX_STR, "addon description", name)
        self.author, off = self._cstr(data, off, MAX_STR, "addon author", name)
        if off + 4 > n:
            raise GmaError(f"{name}: gma header truncated")
        self.addon_version = struct.unpack_from("<i", data, off)[0]
        off += 4
        self._entries = {}
        self._data = data
        count = 0
        while True:
            if off + 4 > n:
                raise GmaError(f"{name}: gma file table truncated")
            num, = struct.unpack_from("<I", data, off)
            off += 4
            if num == 0:
                break
            count += 1
            if count > MAX_FILES:
                raise GmaError(f"{name}: gma holds more than {MAX_FILES} files")
            raw, off = self._cstr(data, off, MAX_PATH_STR, "file path", name, encoding="latin-1")
            rel = raw.replace("\\", "/").strip().strip("/").lower()
            if not rel:
                raise GmaError(f"{name}: gma file table holds an empty path")
            if off + 12 > n:
                raise GmaError(f"{name}: gma file table entry for {raw!r} truncated")
            size, crc = struct.unpack_from("<qI", data, off)
            off += 12
            if size < 0:
                raise GmaError(f"{name}: {raw!r} has a negative size ({size})")
            if rel not in self._entries:
                self._entries[rel] = (size, crc)
        self._base = off
        if self._base + sum(s for s, _ in self._entries.values()) + 4 > n:
            raise GmaError(f"{name}: gma file data runs past end of file")
        off = self._base
        for rel, (size, crc) in self._entries.items():
            self._entries[rel] = (off, size, crc)
            off += size

    @staticmethod
    def _cstr(data, off, cap, what, name, encoding="utf-8"):
        stop = data.find(b"\0", off)
        if stop < 0:
            raise GmaError(f"{name}: gma {what} runs past end of file")
        if stop - off > cap:
            raise GmaError(f"{name}: gma {what} longer than {cap} bytes")
        try:
            return data[off:stop].decode(encoding), stop + 1
        except UnicodeDecodeError as e:
            raise GmaError(f"{name}: gma {what} is not {encoding}: {e}") from e

    def paths(self):
        return sorted(self._entries)

    def entries(self):
        """[(path, size, crc)] sorted by path."""
        return [(rel, size, crc) for rel, (_, size, crc) in sorted(self._entries.items())]

    def __len__(self):
        return len(self._entries)

    def __contains__(self, rel):
        return rel.replace("\\", "/").lower().lstrip("/") in self._entries

    def info(self, rel):
        """(size, crc) of one file; raises when absent."""
        key = rel.replace("\\", "/").lower().lstrip("/")
        hit = self._entries.get(key)
        if hit is None:
            raise GmaError(f"{self.path}: no such file {rel!r}")
        _, size, crc = hit
        return size, crc

    def read(self, rel):
        key = rel.replace("\\", "/").lower().lstrip("/")
        hit = self._entries.get(key)
        if hit is None:
            raise GmaError(f"{self.path}: no such file {rel!r}")
        off, size, _ = hit
        chunk = self._data[off:off + size]
        if len(chunk) != size:
            raise GmaError(f"{self.path}: {rel!r} data runs past end of file")
        return bytes(chunk)

    def extract(self, dst):
        """Writes every file under `dst`; returns the written rel paths."""
        root = Path(dst)
        out = []
        for rel in self.paths():
            parts = Path(rel).parts
            if Path(rel).is_absolute() or ".." in parts:
                raise GmaError(f"{self.path}: refusing to extract {rel!r} outside {root}")
            target = root / rel
            if root.resolve() not in target.resolve().parents:
                raise GmaError(f"{self.path}: refusing to extract {rel!r} outside {root}")
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(self.read(rel))
            out.append(rel)
        return out


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("gma")
    ap.add_argument("out", nargs="?", help="content root to extract into")
    ap.add_argument("--list", action="store_true", help="print the file table instead of extracting")
    args = ap.parse_args()
    pack = Gma(args.gma)
    print(f"{pack.name!r} by {pack.author!r} (gma v{pack.format_version}, {len(pack)} files)")
    if args.list or args.out is None:
        for rel, size, crc in pack.entries():
            print(f"{size:>10} {crc:08x} {rel}")
        return
    written = pack.extract(args.out)
    print(f"{len(written)} files -> {args.out}")


if __name__ == "__main__":
    main()
