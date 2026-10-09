# Source engine maps -> S21 via r5ed

Goal: a Hammer map (`.vmf` plus its `materials/` and `models/`), or a compiled
Source `.bsp` when no VMF exists, becomes a playable S21 map the same way Halo 2
(`scripts/h2`) and MW2 (`scripts/iw4`) do: geometry and entities in an r5ed
project, textures rebuilt on stock S21 material templates, models through
`rmdlconv`, then build, package, close shaders, gate, deploy. Fixture:
`surf_castle` (CS:GO; 45 solids, 119 entities, 9 MDL v49, 26 VMT, 43 VTF).

## What a Source map is made of

| Piece | Where | Notes |
|---|---|---|
| Brushes | VMF `world { solid { side } }`, brush entities `entity { solid }`, `hidden { ... }` wrappers | three-point planes, `uaxis "[x y z offset] scale"`, `lightmapscale` |
| Displacements | `side { dispinfo { power startposition normals distances offsets alphas ... } }` | 2^power+1 grid over a 4-sided face |
| Entities | VMF `entity`, keyvalues + `connections` (I/O) | brush entities own solids |
| Props | `prop_static` / `prop_dynamic*` `model "models/x.mdl"` | the bulk of modern maps' geometry (surf_castle's castle is 9 MDLs) |
| Materials | `materials/<path>.vmt` (KeyValues; `patch`/`include`, shader + `$basetexture`, `$bumpmap`, `$basetexture2` blend, `$translucent`, `$alphatest`, `$selfillum`, `$surfaceprop`, `$detail`) | `LightmappedGeneric`/`WorldVertexTransition` = world, `VertexLitGeneric` = model |
| Textures | `materials/<path>.vtf` (7.2-7.5; DXT1/3/5, BGRA/RGBA/BGR/RGB 8888/888, I8, IA88, A8, BGRX, RGBA16161616F) | skyboxes are 6 VTFs `<name>{bk,dn,ft,lf,rt,up}` |
| Models | `models/<path>.mdl` v44-49 + `.vvd` `.vtx` `.phy` | `$cdmaterials` folders + texture names resolve VMTs |
| Compiled map | `maps/<name>.bsp` v19-21 | brush/brushside/plane/texinfo lumps, disp lumps, `sprp` game lump, entity lump, pakfile zip |

Units are inches on both sides; import is 1:1.

## Phases

### S1 -- VMF import that means something in Apex (rdoc, in the editor and the CLI)

- Layout (done: `89d0a6e`): solids in world/entities, `hidden`, editor blocks skipped.
- Host serves the entity schema from a built-in copy (no working-directory dependency).
- Entity mapping, table-driven (`rdoc/src/import/source_entities.rs`):
  - spawns `info_player_{start,terrorist,counterterrorist,deathmatch,teamspawn}`
    -> `info_spawnpoint_human` (phase keys, team from the Source class);
  - `prop_static` -> `Prop { static_prop, model: "mdl/source/<stem>/<mdl path>.rmdl", skin, fade }`,
    `prop_dynamic*` -> dynamic `Prop`; `modelscale`/`uniformscale` -> transform scale;
  - `trigger_multiple`/`trigger_once`/`trigger_push`/`trigger_teleport`/`trigger_hurt`
    -> `Volume` objects of the matching class (the compiler writes `*coll`/`model`);
    `func_detail`, `func_brush`, `func_wall`, `func_illusionary` -> brushes
    (`func_illusionary` render-only, contents 0);
  - `info_teleport_destination`, `trigger_push` keys (`pushdir`, `speed`),
    `trigger_teleport` `target` carry over; Source-only keys are dropped;
  - `light_environment` -> sun `Light` (angles/pitch, `_light` colour and brightness),
    `light`/`light_spot` -> point/spot lights; `env_cubemap` -> bake cubemap points;
  - `worldspawn skyname` -> recorded for S2 (skybox);
  - editor/logic-only classes (logic_*, point_template, filter_*, game_*, point_*command,
    env_tonemap_controller, postprocess_controller, shadow_control, env_cascade_light,
    water_lod_control, func_dustmotes, ...) are not imported; the import returns a
    REPORT (counts per dropped class, per dropped key) instead of 300 validation lines.
- Tool materials: `TOOLS/TOOLSSKYBOX` -> sky shell (the collision the sun needs),
  `TOOLSNODRAW` -> no render, `TOOLSTRIGGER` -> the volume's own material,
  `TOOLSCLIP`/`TOOLSPLAYERCLIP`/`TOOLSNPCCLIP` -> collision-only with clip contents,
  `TOOLSBLACK`/`TOOLSINVISIBLE` as their Apex twins.
- Other materials rename to `world/source/<stem>/<vmt path>` (what S2 builds).
- UVs: Source `u = (p . axis) / scale + offset` in texels; r5ed's mapping needs the
  texture size, so the importer takes an optional `{vmt path: [w, h]}` table (S2
  writes it) and converts exactly; without it, 512x512 is assumed and reported.
- Displacements (`dispinfo`) -> one mesh per displaced face (positions, normals,
  alpha as vertex colour for blend materials), collision triangles.
- CLI verb `r5ed-compile import-vmf <project> <vmf> [--materials <json>] [--models <json>]`.

### S2 -- Materials and textures (`scripts/source/srcmat.py`)

- VMT reader: KeyValues with `patch`/`include`/`insert`/`replace`, proxies ignored,
  search the map folder, then a mounted game `materials/` folder, then `.vpk` dirs.
- VTF decoder (pure Python + numpy): formats listed above, first frame, largest
  mip, cubemap faces for skyboxes; HDR formats tonemapped.
- Classification onto the stock templates `iw4mat.py` / `h2mat.py` already use:
  `LightmappedGeneric` -> world `detail` (base + normal [+ detail]);
  `WorldVertexTransition` -> world `blend` (two layers, vertex alpha from displacements);
  `$alphatest`/`$translucent` -> `test`/glass; `$selfillum` -> `emissive`;
  `VertexLitGeneric` -> `model_opaque` / `model_test` / `model_glass`.
- `$surfaceprop` -> an S21 surface name from `surfaceproperties.rson`
  (concrete, metal, wood, dirt, grass, glass, water, ...; unknown -> concrete + report).
- Output: `<stem>_src.rpak` + a `{vmt path: [w, h]}` size table for S1 UVs.
- Skybox: the 6 faces become the map's sky (a textured sky-shell material first;
  a real S21 sky asset later).

### S3 -- Models (`scripts/source/srcprops.py`)

- For each MDL a prop references: resolve every texture through `$cdmaterials` to
  its VMT, rewrite the MDL texture-name table to the S21 material path
  `models/source/<stem>/<vmt path>_rgdp` (rmdlconv drops `$cdmaterials` and hashes
  the bare name), then `rmdlconv -v49 ... -targetversion 17 -vertexcolor`
  (client; `.phy` becomes the collision) and `-v17 ... ` for the dedi v10.
- Model materials go through S2 as the `model_*` families.
- MDL versions below 49 (HL2 v44-48): rmdlconv `-v48` path when present, else report.

### S4 -- Ship (`scripts/source/ship.sh`)

The Halo 2 `ship.sh` order, unchanged: import -> srcmat -> srcprops ->
`build --bake final --install` -> `package` -> `close_shaders.py` ->
`gate_pak.py ... || exit 1` -> deploy 6 files -> `register`.

### S5 -- Compiled BSP input (`scripts/source/bsp2vmf.py`)

For maps shipped without a VMF: read BSP v19-21 (planes, brushes, brushsides,
texinfo/texdata, dispinfo/dispverts, entity lump, `sprp` static props, pakfile zip),
write an equivalent VMF (brushes rebuilt from their sides; static props as
`prop_static` entities) and extract the pakfile, then run S1-S4 unchanged.

## Out of scope

Source gameplay semantics that S21 scripts do not provide (surf air-strafe,
CS weapon equips, VScript `.nut` logic, I/O `connections` beyond teleport/push),
Source water shader, detail sprites, overlays/decals (`info_overlay`), HDR
lightmaps (the map is re-baked by r5ed).

## Gates

- S1: the fixture imports with 0 unknown-class errors in the report's "kept" set,
  every kept entity validates against `schema/entities.json`, brush count 45.
- S2: every VTF format has a decode test against a known pixel; every VMT the fixture
  uses resolves; the pak passes `gate_pak.py`.
- S3: all 9 fixture MDLs convert; each rmdl's material GUIDs resolve in `<stem>_src.rpak`.
- S4: `ship.sh surf_castle` ends with the gate green and 6 files deployed; loading it
  in game is the user's run.
