# r5ed architecture

r5ed is a map editor and rBSP compiler for the S21 bridge. One Rust core is
built three ways: `wasm32` for the browser editor, native inside `r5ed-host`,
and native as the `r5ed-compile` CLI. Output is identical across the three.

```
web/ (TypeScript + SolidJS + WebGPU)  <->  r5ed-wasm (rdoc, rcomp, rterrain, rbake-preview)
        |  HTTP + WS on 127.0.0.1, token-authenticated
r5ed-host (axum)  ->  rpak (asset index), native rcomp/rbake, external tools, deploy, live link
```

Units: Source inches, Z up, right-handed. Angles in `.ent` files are
`pitch yaw roll` degrees.

## Crates

| Crate | Owns |
|---|---|
| `rbsp` | rBSP container (header, monolithic/sidecar), typed codecs for every lump of v37/v47/v51, `validate` |
| `rdoc` | `.r5map` document model, edit commands + undo, project IO, prefabs, entity schema |
| `rterrain` | heightfield + volumetric terrain generation, erosion, sculpting, splat painting, scatter, terrain meshing |
| `rcomp` | compiler: CSG, render meshes, collision BVH, entities/partitions, static props, vis, dedi emit, TF2 conversion, navmesh soup |
| `rbake` | lighting: world lights, lightmaps (RTL + sky pages), light probes, CSM trees + shadow meshes, cubemaps; CPU reference and wgpu backends |
| `rpak` | read-only RPak access: header, asset table, decompression, texture/material/model extraction for previews and compile metadata |
| `r5ed-wasm` | wasm-bindgen facade implementing `web/src/core/api.ts` |
| `r5ed-host` | localhost server: editor hosting, project IO, assets, jobs, deploy, live link |
| `r5ed-compile` | CLI over `rcomp` + `rbake` + packaging |

Dependency order: `rbsp` <- `rdoc` <- `rterrain` <- `rbake` <- `rcomp` <- (`r5ed-wasm`, `r5ed-host`, `r5ed-compile`).
`rpak` is native-only and used by the host and the CLI.

## Compiler contract (`rcomp`)

```rust
pub trait ProjectFs { fn read(&self, rel: &str) -> anyhow::Result<Vec<u8>>; }

pub trait AssetInfo {
    fn material(&self, path: &str) -> Option<MaterialInfo>;
    fn model(&self, path: &str) -> Option<ModelInfo>;
    fn surface_prop(&self, name: &str) -> Option<u32>;
}

pub struct CompileOptions { pub targets: Targets, pub bake: BakeMode, pub reference_root: Option<PathBuf> }
pub fn compile(doc: &MapDoc, fs: &dyn ProjectFs, assets: &dyn AssetInfo, opts: &CompileOptions) -> Result<CompileOutput>;

pub struct CompileOutput {
    pub client: Option<MapArtifacts>,   // v51
    pub dedi: Option<MapArtifacts>,     // v47
    pub nav_soup: NavSoup,              // triangles for Recast
    pub report: Report,                 // warnings, stats, per-stage timings
}
pub struct MapArtifacts { pub bsp: rbsp::Bsp, pub ent_files: Vec<(Partition, String)> }
```

Every emitted map passes `rbsp::validate` before `compile` returns.

## Host HTTP API (`r5ed-host`)

All routes need header `X-R5ED-Token: <token>` (WebSocket: `?token=`). The
token is generated per launch and handed to the page in the URL fragment
`#t=<token>`. Requests with a foreign `Origin` or `Host` are rejected.

| Method | Route | Body / query | Response |
|---|---|---|---|
| GET | `/api/session` | | `{version, game_root, projects_root, capabilities}` |
| GET | `/api/projects` | | `[{name, modified}]` |
| POST | `/api/projects` | `{name}` | `{name}` |
| GET/PUT | `/api/projects/{name}/file?path=` | raw bytes | raw bytes |
| GET | `/api/projects/{name}/files` | | `[{path, size, modified}]` |
| GET | `/api/assets/search?q=&kind=&offset=&limit=` | | `{total, items:[{kind, path, guid}]}` |
| GET | `/api/assets/model?path=` | | `model/gltf-binary` preview (LOD0) |
| GET | `/api/assets/model_info?path=` | | `ModelInfo` JSON |
| GET | `/api/assets/material?path=` | | `MaterialInfo` JSON |
| GET | `/api/assets/texture?guid=` | | `image/vnd-ms.dds` (BC passthrough) |
| GET | `/api/assets/thumbnail?path=` | | `image/png` |
| GET | `/api/assets/surface_props` | | `{name: id}` |
| GET | `/api/maps` | | `[{stem, has_client, has_dedi}]` |
| GET | `/api/maps/{stem}/reference` | | `{chunks:[url], entities:[...], bounds}` |
| GET | `/api/maps/{stem}/chunk/{n}` | | `model/gltf-binary` |
| GET | `/api/schema` | | entity schema JSON |
| POST | `/api/jobs` | `{kind, project, params}` | `{id}` |
| GET | `/api/jobs/{id}` | | `{id, kind, state, progress, log, artifacts}` |
| POST | `/api/jobs/{id}/cancel` | | `{}` |
| WS | `/api/live` | JSON messages | live-link events |

Job kinds: `compile`, `bake`, `navmesh`, `package`, `deploy`, `reload`,
`import_tf2`, `import_map`, `export_minimap`, `export_loadscreen`.

## Browser core API (`web/src/core/api.ts`)

The TypeScript interface in `web/src/core/api.ts` is the contract between the
UI and `r5ed-wasm`. The wasm crate implements it exactly.

## Source format (`.r5map`)

A project directory:

```
<project>/map.json          MapDoc (rdoc::model)
<project>/meshes/*.glb      authored meshes
<project>/terrain/*.f32     heights, *.u8 splat weights
<project>/prefabs/*.r5prefab
<project>/bake/             cached bake products, content-hashed
<project>/build/            compiler output
```

## Entity schema

`schema/engine/*.json` holds the server entity factory table and datamap
keyfields/inputs/outputs. `rdoc::schema::generate` merges them with every key
observed in shipped map entity partitions and every key read by game scripts
into `schema/entities.json`, which drives the editor's entity forms.
