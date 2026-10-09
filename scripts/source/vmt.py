"""Valve material (.vmt) reader and a case-insensitive content index.

A VMT is KeyValues text: `"Shader" { "$key" "value" ... }` with `//` comments,
unquoted tokens, nested blocks (Proxies, patch replace/insert) and platform
conditionals (`[$X360]`). `patch` materials pull their base through `include`.
"""

from pathlib import Path

import gma
import vpk


def tokens(text):
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c.isspace():
            i += 1
        elif text.startswith("//", i):
            j = text.find("\n", i)
            i = n if j < 0 else j
        elif c in "{}":
            yield c
            i += 1
        elif c == '"':
            j = text.find('"', i + 1)
            j = n if j < 0 else j
            yield text[i + 1:j]
            i = j + 1
        elif c == "[":
            j = text.find("]", i)
            i = n if j < 0 else j + 1
        else:
            j = i
            while j < n and not text[j].isspace() and text[j] not in '{}"':
                j += 1
            yield text[i:j]
            i = j


def parse(text):
    """(shader, {lower key: value or nested dict})."""
    toks = list(tokens(text))

    def block(pos):
        out = {}
        while pos < len(toks) and toks[pos] != "}":
            key = toks[pos].lower()
            pos += 1
            if pos < len(toks) and toks[pos] == "{":
                val, pos = block(pos + 1)
                pos += 1
            elif pos < len(toks):
                val = toks[pos]
                pos += 1
            else:
                break
            out[key] = val
        return out, pos

    if len(toks) < 2 or toks[1] != "{":
        raise ValueError("not a vmt")
    body, _ = block(2)
    return toks[0].lower(), body


class Content:
    """Every file under the given content roots, looked up case-insensitively.

    A root is a folder, a `*_dir.vpk` file or a `.gma` file; earlier roots win.
    """

    def __init__(self, roots):
        self.files = {}
        self.vpks = []
        self.gmas = []
        for root in roots:
            path = Path(root)
            if path.is_file() and path.suffix.lower() == ".vpk":
                pack = vpk.Vpk(path)
                self.vpks.append(pack)
                for rel in pack.paths():
                    self.files.setdefault(rel, vpk.VpkFile(pack, rel))
            elif path.is_file() and path.suffix.lower() == ".gma":
                pack = gma.Gma(path)
                self.gmas.append(pack)
                for rel in pack.paths():
                    self.files.setdefault(rel, gma.GmaFile(pack, rel))
            else:
                for p in path.rglob("*"):
                    if p.is_file():
                        self.files.setdefault(p.relative_to(path).as_posix().lower(), p)

    def find(self, rel):
        return self.files.get(rel.replace("\\", "/").lower().lstrip("/"))

    def material(self, name):
        """(shader, keys) of `materials/<name>.vmt`, patches resolved; None when absent."""
        name = name.replace("\\", "/").lower().removesuffix(".vmt")
        path = self.find(f"materials/{name}.vmt")
        if path is None:
            return None
        shader, keys = parse(path.read_text(encoding="utf-8", errors="replace"))
        for _ in range(4):
            if shader != "patch":
                break
            base = self.material(str(keys.get("include", "")).removeprefix("materials/"))
            if base is None:
                return None
            shader, merged = base[0], dict(base[1])
            for part in ("replace", "insert"):
                if isinstance(keys.get(part), dict):
                    merged.update(keys[part])
            keys = merged
        return shader, keys

    def texture(self, name):
        name = str(name).replace("\\", "/").lower().removesuffix(".vtf")
        return self.find(f"materials/{name}.vtf")
