# Advanced routes

These routes work and have shipped maps, but each needs tools from outside this repository and
more judgement than the one-command Source route. The scripts are complete; the orchestration
is a sequence you run step by step. Setup first: [AGENTS.md](../../AGENTS.md).

Every route ends the same way as the Source route: a project in `$R5ED_PROJECTS`, then
`scripts/mod_map.sh <project> <Team.Name> "<Display>" <mod dir> [source paks...]`, which builds,
bakes, packages, closes the shader set, runs the gates and installs the map as a mod. The ship
helpers in `scripts/source/ship_common.sh` are the reference for that tail.

## Halo 2 (PC / Vista `.map`)

Needs: [Reclaimer](https://github.com/Gravemind2401/Reclaimer) (cloned beside `scripts/h2/h2dump`).

1. Build `scripts/h2/h2dump` with `-p:ReclaimerDir=<clone>`; run `h2dump.exe <map> <dump dir>`.
   It writes the level meshes, bitmaps and `gameplay.json` (spawns, weapons, vehicles).
2. `py scripts/h2/h2import.py <dump> <project>` builds the r5ed project.
3. `py scripts/h2/h2mat.py <dump> <work dir> --project <project>` rebuilds every Halo material from
   the original bitmaps on a stock S21 template and writes the source pak manifest
   (`<stem>_src.json`); re-run h2import with `--overrides <material map>` to point the project at them.
4. Ship with `scripts/mod_map.sh`.

`scripts/h2/bloodgulch_*.json` are the material tables of the Blood Gulch port, as an example.

## Halo 3

Needs: Reclaimer (for `scripts/h3/h3dump`) and [TagTool](https://github.com/TheGuardians-CI/TagTool)
for the gameplay dump (`scripts/h3/dump_gameplay.cs`, run as a TagTool script).

`h3dump` -> `h3mesh.py` (terrain split by blend layers) -> `h3mat.py` (materials from the original
bitmaps) -> `merge_gameplay.py <dump dir>` -> `h3env.py` (sun, fog, sky) -> ship.

## Call of Duty: Modern Warfare 2 (IW4)

Needs: [OpenAssetTools](https://github.com/Laupetin/OpenAssetTools) with world dumpers
(GfxWorld, clipMap, ComWorld). Stock OpenAssetTools does not dump the world yet; a fork that does
is required for this route.

`iw4world.py` / `iw4statics.py` / `iw4import.py` (geometry, static models, project) ->
`iw4ents.py` (script models and physics props: MW2 places models in three sources, import all
three) -> `iw4mat.py` (materials on stock templates) -> `iw4props.py` (props; needs a v49
studiomdl, `R5ED_STUDIOMDL`) -> `iw4env.py` (lights, fog, colour grade) -> ship.

## Apex Legends Mobile (Unreal Engine 4 cooked content)

`scripts/apexm/port_level.py` reads a cooked level (`.umap` / `.uasset` / `.uexp` / `.ubulk`)
into `world.glb`, `airwall.glb` and an r5ed project; `preview.py` renders it in Blender before
baking; `scripts/apexm/ship_level.sh <out dir>` ships it. The mesh format is described in
[docs/APEXM_STATICMESH_FORMAT.md](../APEXM_STATICMESH_FORMAT.md).

## Titanfall 2

`r5ed-compile import-tf2 <bsp> <project>` imports a Titanfall 2 (rBSP v37) map as a project.
Materials, models and effects go through the TF2 tools in
[bspconv](https://github.com/R5Flowstate/bspconv) (`tools/tf2mat`, `tools/tf2fx`).

## The gates (every route)

`scripts/h2/gate_pak.py <map.rpak> <package manifest>` must print `GATE PASSED` before a map is
installed: every guid reference resolves (in the pak or the game's resident paks), no reference
slot is listed twice, every staged shader is valid bytecode, and RSX's shader validation flags
nothing. A pak that fails any of these crashes or refuses to load in game.
