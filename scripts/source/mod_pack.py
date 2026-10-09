"""A built r5ed map -> an R5Flowstate map mod folder, installed into a game's mods/.

The map's stem must already be `mp_<namespace>__<name>` (the mod system and
build.py refuse anything else, and every layer carries the stem, so it is set
before the build, never renamed after). Layout follows the Gauntlet map pack:

    mod.vdf  manifest.json  README.md  icon.png  playlists_r5_patch.txt
    paks/Win64/<map>.rpak  <map>_client_perm.rpak  <map>_client_temp.rpak  (+ starpaks)
    paks/Win64_server/<map>.rpak
    vpk/englishserver_<map>.bsp.pak000_dir.vpk  vpk/server_<map>.bsp.pak000_000.vpk

    py mod_pack.py <project> <mod dir> --id Team.Name --display "Surf Castle" [--install <game folder>]
"""

import argparse
import json
import re
import shutil
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

import push_volumes

WEBSITE = "https://r5flowstate.org/host"


def vdf(mod_id, author, name, description, stem, loc_file):
    # The map's display name is the token named after its stem, outside the mod
    # namespace, so it has to be declared as an override.
    return (f'"mod"\n{{\n\t"author"\t\t"{author}"\n\t"name"\t\t\t"{name}"\n\t"id"\t\t\t"{mod_id}"\n'
            f'\t"description"\t"{description}"\n\t"version"\t\t"1.0.0"\n\t"realm"\t\t\t"both"\n\n'
            f'\t"Maps"\n\t{{\n\t\t"{stem}"\t"1"\n\t}}\n\n'
            f'\t"LocalizationFiles"\n\t{{\n\t\t"{loc_file}"\t"1"\n\t}}\n\n'
            f'\t"LocalizationOverrides"\n\t{{\n\t\t"{stem}"\t"1"\n\t}}\n}}\n')


def localization(stem, display):
    return f'"lang"\n{{\n\t"Language" "english"\n\t"Tokens"\n\t{{\n\t\t"{stem}"\t"{display}"\n\t}}\n}}\n'


TEMPLATES = Path(__file__).resolve().parent / "mod_templates" / "movement"


def movement_keys():
    text = (TEMPLATES / "sh_movement.gnut").read_text(encoding="utf-8")
    block = text[text.index("MOVEMENT_KEYS = ["):text.index("]", text.index("MOVEMENT_KEYS = ["))]
    return re.findall(r'"([A-Za-z_]+)"', block)


def movement_vars(preset):
    """Playlist lines for a movement preset: the value, and a host setting so a
    server can tune it (range 0 .. 10x the preset, at least 100)."""
    allowed = movement_keys()
    lines = []
    for key, value in preset.items():
        if key not in allowed:
            raise SystemExit(f"movement key {key!r} is not one the Movement Lab tunes")
        v = str(value)
        if not re.fullmatch(r"-?[0-9]+(\.[0-9]+)?", v):
            raise SystemExit(f"movement value for {key} must be a number, got {v!r}")
        lines.append(f'                movement_{key:<32}"{v}"')
        if key in BOOL_KEYS:
            decl = f"bool|Movement: {key}"
        else:
            decl = f"float 0 {max(1.0, abs(float(v)) * 10):g}|Movement: {key}"
        lines.append(f'                r5f_setting_movement_{key:<20}"{decl}"')
    return lines


BOOL_KEYS = ("doubleJump", "wallrun", "climbEnabled", "wallrunSameWallAllowed", "superjumpLimit")


def install_movement_scripts(out, namespace, mod_id, stem, preset, invincible, no_melee, push_ent, fps_cap=True):
    ident = mod_id.replace(".", "_")
    cases = "\n".join(f'\t\tcase "{k}":\n\t\t\treturn "{v}"' for k, v in preset.items())
    dst = out / "scripts" / "vscripts" / namespace
    dst.mkdir(parents=True, exist_ok=True)
    names = ("sh_movement.gnut", "sv_movement.nut", "cl_movement.nut") + (("ui_fps_cap.nut",) if fps_cap else ())
    for name in names:
        text = (TEMPLATES / name).read_text(encoding="utf-8")
        text = text.replace("__NS_UPPER__", namespace.upper()).replace("__NS__", namespace).replace("__ID__", ident).replace("__MAP__", stem)
        if not preset:
            # An empty switch does not compile; surf maps take the platform preset.
            text = text.replace("\tswitch ( key )\n\t{\n__MOVEMENT_DEFAULT_CASES__\n\t}\n",
                                "\t// Surf movement is the platform preset (sh_surf_movement.gnut).\n")
        text = text.replace("__MOVEMENT_DEFAULT_CASES__", cases)
        text = text.replace("__INVINCIBLE__", "true" if invincible else "false").replace("__NO_MELEE__", "true" if no_melee else "false")
        for ph in ("__MOVEMENT_DEFAULT_CASES__", "__INVINCIBLE__", "__NO_MELEE__", "__MAP__", "__ID__", "__NS__"):
            if ph in text:
                raise SystemExit(f"{name}: template placeholder {ph} was not filled")
        (dst / name).write_text(text, encoding="utf-8")
    # Push triggers become volumes both VMs register, so the push is predicted.
    push_volumes.write(push_ent, namespace, dst / "sh_push_volumes.gnut")
    rson = "".join([
        'When: "SERVER"\n', "Scripts:\n", "[\n", f"\t{namespace}/sh_movement.gnut\n", f"\t{namespace}/sh_push_volumes.gnut\n",
        f"\t{namespace}/sv_movement.nut\n", "]\n\n",
        'When: "CLIENT"\n', "Scripts:\n", "[\n", f"\t{namespace}/sh_movement.gnut\n", f"\t{namespace}/sh_push_volumes.gnut\n",
        f"\t{namespace}/cl_movement.nut\n", "]\n",
    ] + (["\n", 'When: "UI"\n', "Scripts:\n", "[\n", f"\t{namespace}/ui_fps_cap.nut\n", "]\n"] if fps_cap else []))
    (out / "scripts" / "vscripts" / "scripts.rson").write_text(rson, encoding="utf-8")


# The engine copies playlist names into 32-byte buffers; a longer id is truncated and
# +launchplaylist silently falls back to the default playlist.
PLAYLIST_ID_MAX = 31


def playlist_id(pid):
    if len(pid) > PLAYLIST_ID_MAX:
        raise SystemExit(f"playlist id {pid!r} is over {PLAYLIST_ID_MAX} chars")
    return pid


def playlist(mod_id, playlist_name, display, blurb, stem, extra_vars=()):
    abbrev = re.sub(r"[^A-Z]", "", display.upper())[:4] or "MAP"
    extra = "\n".join(extra_vars)
    return f"""playlists
{{
    Playlists
    {{
        {playlist_name}
        {{
            inherit survival_dev
            vars
            {{
                for_mod                                 "{mod_id}"
                r5f_mode_title                          "{display}"
                r5f_mode_blurb                          "{blurb}"
                name                                    "{display.upper()}"
                description                             "{blurb}"
                abbreviation                            "{abbrev}"
                lobbytitle                              "{display}"
                map_name                                "{display}"

                loot_drones_spawn_count                 0
                loot_drones_vault_key_count             0
                disable_minimap                         0
{extra}
            }}
            gamemodes {{ survival {{ maps {{
                {stem}       1
            }} }} }}
        }}
    }}
}}
"""


def icon(path, display):
    img = Image.new("RGB", (256, 256), (24, 28, 36))
    d = ImageDraw.Draw(img)
    try:
        font = ImageFont.load_default(size=30)
    except TypeError:
        font = ImageFont.load_default()
    words = display.upper().split()
    lines, cur = [], ""
    for w in words:
        if len(cur) + len(w) + 1 > 10 and cur:
            lines.append(cur)
            cur = w
        else:
            cur = f"{cur} {w}".strip()
    lines.append(cur)
    y = 128 - len(lines) * 20
    for line in lines:
        w = d.textlength(line, font=font)
        d.text(((256 - w) / 2, y), line, fill=(235, 235, 235), font=font)
        y += 40
    d.rectangle([8, 8, 247, 247], outline=(200, 60, 40), width=4)
    img.save(path)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("project")
    ap.add_argument("mod_dir")
    ap.add_argument("--id", required=True, help="Team.Name, e.g. CafeFPS.SurfCastle")
    ap.add_argument("--display", required=True)
    ap.add_argument("--blurb", default="")
    ap.add_argument("--install", help="game root: copy the mod into <root>/mods/<Team>-<Name> and enable it")
    ap.add_argument("--movement", help="JSON {classvar: value} applied to every player on the map (surf, bhop, ...)")
    ap.add_argument("--invincible", action="store_true", help="players take no damage on the map (needs --movement)")
    ap.add_argument("--no-melee", action="store_true", help="players have no melee on the map (needs --movement)")
    ap.add_argument("--spawn-lift", type=float, help="height above a spawnpoint players are placed at (default 16, needs --movement)")
    ap.add_argument("--spawn-points", help="'x y z yaw;...' where players enter the map instead of its spawnpoints (needs --movement)")
    ap.add_argument("--loadscreen", help="<stem>_loadscreen.rpak to ship (build_loadscreen.py --from-image)")
    args = ap.parse_args()

    project = Path(args.project)
    stem = json.loads((project / "map.json").read_text(encoding="utf-8"))["meta"]["stem"]
    m = re.fullmatch(r"mp_([a-z0-9_]+?)__([a-z0-9_]+)", stem)
    if not m or len(stem) > 63:
        raise SystemExit(f"{stem}: a mod map must be named mp_<namespace>__<name> (<= 63 chars)")
    namespace = m.group(1)
    team, name = args.id.split(".", 1)
    blurb = args.blurb or f"{args.display}, converted to R5Flowstate with r5ed."
    pkg = project / "build" / "package"
    out = Path(args.mod_dir)
    for sub in ("paks/Win64", "paks/Win64_server", "vpk"):
        (out / sub).mkdir(parents=True, exist_ok=True)

    wanted = [(pkg / f"{stem}.rpak", "paks/Win64"),
              (pkg / "paks/Win64" / f"{stem}_client_perm.rpak", "paks/Win64"),
              (pkg / "paks/Win64" / f"{stem}_client_temp.rpak", "paks/Win64"),
              (pkg / "paks/Win64_server" / f"{stem}.rpak", "paks/Win64_server"),
              (pkg / "vpk" / f"englishserver_{stem}.bsp.pak000_dir.vpk", "vpk"),
              (pkg / "vpk" / f"server_{stem}.bsp.pak000_000.vpk", "vpk")]
    wanted += [(p, "paks/Win64") for p in sorted(pkg.glob(f"{stem}*.starpak"))]
    wanted += [(p, "paks/Win64") for p in sorted((pkg / "paks/Win64").glob(f"{stem}*.starpak"))]
    for src, dst in wanted:
        if not src.is_file():
            raise SystemExit(f"missing build output {src}")
        shutil.copyfile(src, out / dst / src.name)

    if args.loadscreen:
        ls = Path(args.loadscreen)
        if ls.name != f"{stem}_loadscreen.rpak" or not ls.is_file():
            raise SystemExit(f"--loadscreen must be an existing {stem}_loadscreen.rpak, got {ls}")
        shutil.copyfile(ls, out / "paks/Win64" / ls.name)

    loc_file = f"resource/{namespace}__english.txt"
    (out / "resource").mkdir(parents=True, exist_ok=True)
    (out / loc_file).write_text(localization(stem, args.display), encoding="utf-8")
    (out / "mod.vdf").write_text(vdf(args.id, team, args.display, blurb, stem, loc_file), encoding="utf-8")
    (out / "manifest.json").write_text(json.dumps({"name": name, "version_number": "1.0.0", "website_url": WEBSITE,
                                                   "description": blurb[:250],
                                                   "dependencies": []}, indent=2) + "\n", encoding="utf-8")
    extra = []
    if args.movement:
        if namespace != args.id.lower().replace(".", "_"):
            raise SystemExit(f"map namespace {namespace} must be the mod id's ({args.id.lower().replace('.', '_')})")
        preset = json.loads(Path(args.movement).read_text(encoding="utf-8"))
        surf = "__surf_" in stem
        if surf:
            # The platform owns surf movement and its fps cap (sh_surf_movement.gnut) on every __surf_ map.
            preset = {}
        extra = movement_vars(preset)
        for flag, var, label in ((args.invincible, "mapmod_invincible", "Players take no damage"),
                                 (args.no_melee, "mapmod_no_melee", "No melee")):
            if flag:
                extra.append(f'                {var:<41}"1"')
                extra.append(f'                r5f_setting_{var:<29}"bool|{label}"')
        if args.spawn_lift is not None:
            extra.append(f'                {"mapmod_spawn_lift":<41}"{args.spawn_lift:g}"')
        if args.spawn_points:
            extra.append(f'                {"mapmod_spawn_points":<41}"{args.spawn_points}"')
        install_movement_scripts(out, namespace, args.id, stem, {k: str(v) for k, v in preset.items()},
                                 args.invincible, args.no_melee,
                                 Path(args.project) / "build" / "maps" / f"{stem}_script.ent", fps_cap=not surf)
    elif args.invincible or args.no_melee:
        raise SystemExit("--invincible and --no-melee ride the map script, which --movement installs")
    (out / "playlists_r5_patch.txt").write_text(
        playlist(args.id, playlist_id(namespace), args.display, blurb, stem, extra),
        encoding="utf-8")
    (out / "README.md").write_text(f"# {args.display}\n\n{blurb}\n\nAdds the map `{stem}` and a free-roam "
                                   f"playlist. Server and players both install it.\n", encoding="utf-8")
    if not (out / "icon.png").is_file():
        icon(out / "icon.png", args.display)
    print(f"mod {args.id} -> {out}")

    if args.install:
        root = Path(args.install)
        dst = root / "mods" / f"{team}-{name}"
        if dst.exists():
            shutil.rmtree(dst)
        shutil.copytree(out, dst)
        mods = root / "mods" / "mods.vdf"
        text = mods.read_text(encoding="utf-8")
        if f'"{args.id}"' not in text:
            text = text.rstrip().rstrip("}").rstrip() + f'\n\t"{args.id}"\t\t"1"\n}}\n'
            mods.write_text(text, encoding="utf-8")
        print(f"installed -> {dst}, enabled in mods.vdf")


if __name__ == "__main__":
    main()
