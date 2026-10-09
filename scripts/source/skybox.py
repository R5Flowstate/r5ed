"""Source 2D skybox (worldspawn skyname) -> six S21 world_emissive materials.

A Source sky is six VTFs, `skybox/<name>{bk,dn,ft,lf,rt,up}`, drawn on an
inward-facing cube around the sky camera. Each face becomes a `world_emissive`
material `world/source/skybox/<name><face>` (the face's own pixels as albedo
and emissive). The compiler (rcomp::sky) draws the cube itself; this module
only finds the faces and builds the materials, through folders and VPKs like
every other content lookup.
"""

import argparse
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "iw4"))
import iw4mat  # noqa: E402
import matbuild  # noqa: E402
import vtf  # noqa: E402
from vmt import Content  # noqa: E402

FACES = ("bk", "dn", "ft", "lf", "rt", "up")


class SkyboxError(Exception):
    pass


def normalize_sky(name):
    sky = str(name).replace("\\", "/").strip().lower()
    if not sky or "/" in sky or sky in (".", ".."):
        raise SkyboxError(f"bad skyname {name!r}")
    if len(sky) > 128:
        raise SkyboxError(f"skyname {name!r} is longer than 128 characters")
    return sky


def face_path(sky, face):
    return f"skybox/{normalize_sky(sky)}{face}"


def material(sky, face):
    return f"world/source/{face_path(sky, face)}"


def vtf_rel(sky, face):
    return f"materials/{face_path(sky, face)}.vtf"


def faces_of(content, sky):
    """face -> content path for every face present; absent faces are skipped."""
    found = {}
    for face in FACES:
        path = content.texture(face_path(sky, face))
        if path is not None:
            found[face] = path
    return found


def check_faces(found, sky):
    missing = [vtf_rel(sky, f) for f in FACES if f not in found]
    if missing:
        raise SkyboxError(f"skybox {normalize_sky(sky)!r} is missing {', '.join(missing)}")


def build_faces(assets, templates, index, bank, sky, found, files, report):
    """Builds the world_emissive material of every face in found."""
    for face in FACES:
        if face not in found:
            report.setdefault("missing_vtf", []).append(f"{material(sky, face)} {face_path(sky, face)}")
            continue
        report.setdefault("world_emissive", []).append(material(sky, face))
        matbuild.build(assets, templates, index, bank, "world_emissive", material(sky, face),
                       albedo=face_path(sky, face), surface=iw4mat.surface_prop(face_path(sky, face)),
                       files=files)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("templates", help="RSX export of the template materials (iw4mat's set)")
    ap.add_argument("assets", help="Repak assets dir to write")
    ap.add_argument("--content", nargs="+", required=True)
    ap.add_argument("--skyname", required=True)
    ap.add_argument("--stem", required=True)
    ap.add_argument("--out-dir", required=True)
    args = ap.parse_args()

    from srcmat import SourceBank  # noqa: E402

    content = Content(args.content)
    assets = Path(args.assets)
    vtf.set_scratch(assets.parent / "vtfscratch")
    bank = SourceBank(content, assets, assets.parent / "texscratch", f"source/{args.stem}")
    index = iw4mat.template_index(args.templates)

    found = faces_of(content, args.skyname)
    check_faces(found, args.skyname)
    files, report = [], {}
    build_faces(assets, args.templates, index, bank, normalize_sky(args.skyname), found, files, report)

    name = f"{args.stem}_sky"
    _, doc = matbuild.manifest(assets, name, args.out_dir, bank, files)
    print(f"{sum(1 for f in doc['files'] if f['_type'] == 'matl')} sky materials, "
          f"{len(bank.entries)} textures -> {name}.json")


if __name__ == "__main__":
    main()
