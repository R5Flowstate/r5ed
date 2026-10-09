# Porting a Source engine map

CS:GO, CS:S, Momentum and other Source maps (VBSP v19-21 `.bsp`, `.bsp.bz2`, or a Hammer
`.vmf`) become an R5Flowstate map mod in one command: surf, bhop, kz and deathmatch maps.
Setup first: [AGENTS.md](../../AGENTS.md).

## The one command

```
bash scripts/source/ship_bsp.sh "<path>/surf_foo_v2.bsp.bz2" SurfFoo "Surf Foo" > ship_SurfFoo.log 2>&1
```

- Arguments: the map file, a mod name (letters and digits), the display name.
- The mod id is `<MOD_TEAM>.<Name>` (`MOD_TEAM` defaults to `Community`); the mod is written to
  `$MODS_DIR/<Name>` (default `$R5ED_PROJECTS/mods/<Name>`) and installed and enabled in the game.
- The map stem becomes `mp_<team>_<name>__<map>`; keep it at most 63 characters (`SHORT=` overrides
  the map part). It is fixed before the build and never renamed after.
- `surf_*` maps default to `MOVEMENT=surf INVINCIBLE=1 NO_MELEE=1`; `MOVEMENT=none` turns that off.
  Other presets: `scripts/source/mod_templates/movement/*.json`.
- `BLURB="Surf Foo. Map made by <author>."` -- always credit the author.
- A `.vmf` goes through `scripts/source/ship.sh` directly:
  `STEM=... DISPLAY=... MOD_ID=... MOD_DIR=... MOVEMENT=surf bash scripts/source/ship.sh <stem> <vmf> <content roots...>`
  (content roots = folders holding the map's `materials/` and `models/`).
- `SKIP_DEPLOY=1` stops after the gate, without touching the game.
- A run takes 10-15 minutes: import, models, materials, source paks, build and bake, package,
  shader closure, server models, gate, mod install.
- Read the log: `grep -E "^==|FAILED|missing_vmt|TELEPORTS|MOVERS|GATE|installed" ship_SurfFoo.log`

The first ship also exports the material templates (`scripts/lib/export_templates.py`), and the
first build indexes the install; both take several minutes once and are reused after.

## Models as maps

A single model can be a map: `scripts/source/ship_glb.sh <stem> <model.glb|.blend|.fbx|.dae> <scale> [y|z]`
and `scripts/source/ship_obj.sh <stem> <model.obj> <scale> [y|z]` (the last argument is the
model's up axis). Non-glTF formats go through Blender (`R5ED_BLENDER`). Set `DISPLAY`, `MOD_ID`
and `MOD_DIR` as for `ship.sh`.

## What the pipeline does for you

| Source thing | Becomes | Where |
|---|---|---|
| compiled BSP | VMF (exact brushes from the BSP brushes) + the pakfile as a content root | `bsp2vmf.py` |
| brushes, func_detail/brush/wall | world brushes | `crates/rdoc/src/import/vmf.rs` |
| func_illusionary, func_water_analog | render-only brushes | `crates/rdoc/src/import/source.rs` |
| func_door/button/rotating/movelinear/train | static brushes in their spawn pose (reported `MOVERS FROZEN`) | |
| trigger_teleport | a trigger with the destination resolved at import; unresolved targets are reported | map mod script |
| trigger_push | a trigger carrying `pushdir` / `pushspeed` | map mod script |
| trigger_hurt / playerclip | Apex trigger_hurt / clip brush | |
| weapon_* / item_* / game_* / logic_* / point_* | dropped (reported) | |
| spawns (T/CT/deathmatch/start) | `info_spawnpoint_human` | |
| prop_static/dynamic (MDL v49) | rmdl for client and server | `srcprops.py`, `merge_server_models.py` |
| VMT/VTF | S21 materials and textures on stock templates; translucent foliage becomes alpha test | `srcmat.py`, `matbuild.py` |
| light_environment | sun from VRAD's own direction math, HDR brightness, capped at a stock sun's strength | `source.rs` |
| `_ambient` | the bake's sky colour and intensity | `vmf.rs` |
| nodraw / blocklight faces | shadow-only geometry for lightmaps and cascaded shadows | `csg` |
| no env_cubemap | cubemaps placed automatically over every floor | `rbake` |
| every map | environment entities (colour correction, fog, wind) and the character select / ceremony / podium rooms | `rcomp environment` |

The map's mod script (`scripts/source/mod_templates/movement/*.nut`, installed per mod) carries
the movement preset as defaults (a playlist var `movement_<key>` overrides), invincibility and
no-melee switches, and the teleport and push trigger handlers. The `[MAPMOD]` tag in the server
log shows each trigger set up and firing.

## Known gaps (report them)

- **Stock CS:GO materials not in the map's pakfile** draw a flat colour and are listed as
  `missing_vmt`. Most community maps pack their textures; otherwise pass a folder holding the
  missing `materials/` as an extra content root (from a game you own).
- **Displacements** (terrain) are counted and skipped; `bsp2vmf` prints a `displacement(s)` warning.
- **Doors, buttons and rotators do not move** (frozen in their spawn pose).
- **L4D2-style lump headers** are refused.
- CS weapon pickups are dropped; loadouts come from the playlist.

## Symptom -> cause -> fix

| symptom | cause | fix |
|---|---|---|
| ship waits "for the ship lock" | another ship is running | expected; one ship at a time |
| ship sits at package for a long time | another RSX is running (RSX runs one at a time) | let it finish; never run two ships |
| `classname ... is not a known entity class` | an entity the schema does not know | it should be in the dropped list; report it |
| map dark / no sun | the map has no light_environment | add a sun in the editor, or pass one through the VMF |
| sun through walls | the walls were not occluders | check nodraw/skybox brushes in the source |
| no reflections | no floor under the cubemap grid | add env_cubemap to the source map |
| boosters or teleports do nothing | the map mod script did not load | check `[MAPMOD]` in the server log; is the mod enabled on the server? |
| `Model ... not found` on the server | converted models missing from the server pak | the `server models` step failed; see its log |
| large prop invisible | the model needs the full-float material variant | `srcprops.py` handles it; report the model name |
| leaves opaque | translucent foliage built as glass | the alpha-test template should be used; report the material |
| `GATE FAILED` | the built pak would crash or fail to load in game | read the gate lines; never deploy past it |

## Done means

- `GATE PASSED` and `installed` in the log.
- Report: missing materials, frozen movers, unresolved teleports, displacements skipped, and that
  the map is untested until someone plays it.
