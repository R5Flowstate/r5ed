# r5ed

A map editor and rBSP compiler for R5Flowstate (Apex Legends S21 bridge). One Rust core is built three
ways: `wasm32` for the browser editor, native inside the editor host, and native
as the headless CLI. All three produce byte-identical maps.

**Porting a map?** Start at [AGENTS.md](AGENTS.md): setup, the one-command routes (Source/CS:GO
maps, models as maps) and the rules. Written for people and for AI agents.

Units are Source inches, Z up, right-handed. Angles in `.ent` files are
`pitch yaw roll` degrees.

## Crates

| Crate | What it is |
|---|---|
| `rbsp` | the rBSP container: header, monolithic and sidecar layouts, typed codecs for every lump of v37/v47/v51, and `validate` |
| `rdoc` | the `.r5map` document: model, edit commands with undo, project IO, prefabs, entity schema |
| `rterrain` | heightfield and volumetric terrain: generation, erosion, sculpting, splat painting, scatter, meshing |
| `rbake` | lighting: world lights, lightmaps, probes, cascaded shadow maps, cubemaps, with a CPU reference and a wgpu backend |
| `rcomp` | the compiler: CSG, render meshes, collision BVH, entities and partitions, static props, vis, the dedicated-server emit, Titanfall 2 conversion, navmesh soup |
| `rir` | the types the compiler and the baker exchange |
| `rpak` | read-only rpak access: header, asset table, decompression, texture, material and model extraction |
| `r5ed-wasm` | the browser core: a wasm-bindgen facade implementing `web/src/core/api.ts` |
| `r5ed-host` | the localhost server: editor hosting, project IO, assets, jobs, deploy, live link |
| `r5ed-compile` | the CLI over `rcomp`, `rbake` and the packers |

## Build

Requires a Rust toolchain with the `wasm32-unknown-unknown` target for the
browser core, Node for the web build, and Python for the wasm build script.

```
cargo build --release
py scripts/build_wasm.py
npm --prefix web install
npm --prefix web run build
```

`scripts/build_wasm.py` and `scripts/build-wasm.ps1` are the same build: a cargo
build for `wasm32-unknown-unknown` with the widest target-feature set the
toolchain accepts, then `wasm-bindgen --target web` into `web/public/pkg`. Both
fail if the gzipped core goes over 3 MB. `py scripts/build_wasm.py --help` and
`pwsh -File scripts/build-wasm.ps1 -?` list the switches.

## The editor

`r5ed-host` serves the editor on the loopback interface and nothing else. It
binds `127.0.0.1` only, generates a token per launch, and hands it to the page
in the URL fragment. Every request needs `X-R5ED-Token`, and a foreign `Origin`
or `Host` is refused.

```
r5ed-host --game-root <install> --projects <dir> [--port 8787]
```

Open the printed URL. The token is already in it. The session endpoint reports
where the install and the projects live and what the build can do; the map list,
the asset search and the job panel all hang off the same origin.

The editor opens a project directory, applies every edit through the same
`rdoc` commands the CLI applies, and shows the document, the outliner, the
inspector and a WebGPU viewport. Meshes are edited as brushes; terrain is
sculpted and splat-painted; entities are placed by class from the schema. The
viewport draws brushes, authored meshes, terrain and prop bounds, and the light
preview shows the per-object lightmap the viewport bake produced.

## Smoke test

`scripts/smoke.sh` builds the wasm core, the web bundle, the host and the CLI,
starts the host against a throwaway config, and walks every editor feature in a
real Chromium: the top bar, every object kind, the inspector for each, brush
operations, the vertex/edge/face/clip/voxel/sculpt/paint tools, the gizmo,
dialogs, shortcuts, save/open, the asset browser and the build jobs. It prints
one line per step and exits nonzero if any step fails or logs a console error.

```
scripts/smoke.sh                 # build everything, then run
scripts/smoke.sh --no-build      # reuse the last build
scripts/smoke.sh -- --only brush # run the steps whose name contains "brush"
scripts/smoke.sh --keep          # leave the host running afterwards
```

No game install is needed: the config points at an empty game root, so the
asset tool is never run and a stub stands in for it. Screenshots of failed
steps land in `target/smoke/out`. It needs Node, Xvfb and a Chromium
(`R5ED_CHROMIUM`, `/opt/pw-browsers/chromium` by default).

Headless Chromium loses the WebGPU device on the first `getCurrentTexture()`,
so the browser runs headed under Xvfb with Vulkan on SwiftShader. SwiftShader
drops a line segment with an end behind the camera instead of clipping it, so
the perspective grid only draws its far edge there; a hardware GPU draws it
whole.

## The CLI

`r5ed-compile` authors, compiles and packages projects from a shell. Every verb
prints JSON on stdout and its progress lines on stderr, so a script can chain
them. Exit status is nonzero on failure with the reason on stderr.

| Verb | What it does |
|---|---|
| `new <project> [--stem <stem>]` | creates an empty project |
| `apply <project> <command.json>` | applies one `rdoc` command (`-` for stdin) |
| `brush <project> --min <x,y,z> --max <x,y,z> [--material <name>]` | adds a box brush |
| `mesh <project> <request.json>` | adds an authored mesh from a mesh request |
| `entity <project> --class <name> [--origin x,y,z] [--kv k=v]` | adds any entity class |
| `prop <project> --model <mdl/...> [--origin x,y,z] [--scale <f>]` | adds a prop |
| `light <project> --kind point\|sun\|spot\|area\|sky [--origin x,y,z]` | adds a light |
| `terrain <project> <request.json>` | evaluates a terrain recipe and adds it |
| `build <project> [--out <dir>] [--targets client,dedi] [--bake none\|preview\|final] [--assets <tables.json>] [--install]` | compiles the maps |
| `bake <project> [--mode none\|preview\|final] [--out <dir>]` | solves the lighting on its own and caches the product |
| `package <project> [<stage>] [--out <dir>]` | writes every archive a deploy places |
| `loadscreen <project> [<image>]` | encodes the loading screen as a `uiia` |
| `minimap <project> [<image>]` | renders the overview and writes its resource files |
| `register <project> [--out <dir>]` | writes the launcher and playlist registration |
| `import-map <stem> <project> [--from <path>] [--force]` | imports a map the install carries as a project |
| `import-tf2 <bsp> <project> [--stem <stem>] [--vmt-dir <d>] [--material-dir <d>]` | imports a Titanfall 2 v37 map as a project |
| `list`, `show <id>`, `remove <id...>` | inspect and edit objects |
| `validate <project>` | checks the document and every entity against the schema |
| `materials`, `classes`, `schema` | print the dev material set, the class names, and the schema path and digest |

### External tools

Named by environment variable, or found beside the executable or on the search
path.

| Variable | Tool | Used by |
|---|---|---|
| `R5ED_GAME_ROOT` | the game install | `build --install`, `package`, `import-map` |
| `R5ED_RSX` | the asset tool | `build --install`, `package`, `import-map` |
| `R5ED_REPAK` | the pak packer | `package`, `loadscreen`, `minimap` |
| `R5ED_REVPK` | the archive packer | `package` |
| `R5ED_RECAST` | the navmesh baker | the host's navmesh job |

A project authored from scratch compiles against no install at all: the
compiler falls back to its documented defaults for a material or model name the
tables do not carry. Naming the install resolves real materials and model bounds
instead, and is what a map that ships needs.

## The source format

A project directory:

```
<project>/map.json          the document
<project>/meshes/*.glb      authored meshes
<project>/terrain/*.f32     heights, *.u8 splat weights
<project>/prefabs/*.r5prefab
<project>/import/           an imported map's source descriptor and partitions
<project>/bake/             cached bake products, content-hashed
<project>/build/            compiler output
```

## Entity schema

`schema/entities.json` is generated: the server entity factory table, the
datamap key fields, every key observed in a shipped map's partitions and every
key a game script reads, merged into one form per class. It currently carries
**288 classes** and **15598 keys**: 233 point classes, 55 brush classes. The
`schema` verb prints the count, the digest and the path, and
`r5ed-schema --check` regenerates it and fails when the checked-in file differs
from what the sources say.

## Terrain types

Sixteen generators, each with its own parameter set and its own defaults:
`rolling_hills`, `mountains`, `alpine`, `plateau`, `canyon`, `badlands`,
`dunes`, `islands`, `volcanic`, `craters`, `glacial`, `rivers`, `coastline`,
`farmland`, `arena`, `pits`. A terrain is a node graph over a generator, so
filters, blends, masks and detail fields compose; `heightmap` reads an existing
heightmap. A sculpt stroke, auto-paint rules and scatter rules all evaluate over
the same heightfield and write back through a project file.

## Engine constraints

These are the engine's, not the editor's, and the compiler enforces them:

- The client map is rBSP v51 and the dedicated-server map is v47. A map is never
  one target.
- A client map ships in two wrap archives, `_client_perm` and `_client_temp`,
  split by lump: geometry in one, entities, visibility, cubemaps and the game
  lump in the other, with the mesh table and its bounds in both. A lump in
  neither archive is refused rather than dropped.
- A dedicated map ships as a VPK: `englishserver_<stem>.bsp.pak000_dir.vpk` and
  `server_<stem>.bsp.pak000_000.vpk`. Its sidecar names are lowercase hex,
  because the engine lowercases the path it looks a lump up by and a manifest
  that names an uppercase sidecar finds nothing.
- A pak goes out uncompressed with the native-loader header flag and no
  encoding flag. The only build-time compression the packer offers sets an
  encoding flag this engine cannot decode.
- A client archive that carries a model cannot carry a starpak beside it, so a
  model that only streams its vertex groups is named in the package report
  rather than dropped.
- A map stem the launcher will not offer is refused by `register`; it has to be
  `mp_rr_<name>` or `mp_lobby`.
- Every emitted map passes `rbsp::validate` before the compile returns.

## License

GNU Affero General Public License v3.0 or later ([LICENSE](LICENSE)). The repository contains no
game assets: everything game-side is read from your own install.
