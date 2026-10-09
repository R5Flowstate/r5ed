# Mesh edit mode -- end-to-end spec

Goal: model a whole map inside r5ed, end to end -- an arched gate, a round
tower, a curved wall with crenellations, weathered rock -- without Blender and
without stacking convex brushes (the "squares" look). Scope: an editable
polygon mesh with a Blender-grammar edit mode; loop cut, subdivide, inset,
bridge, bevel, spin/screw; mesh boolean; a modifier stack (array, curve
deform, displace, subdivision surface, mirror, boolean); UVs (box/cylinder
projection, seams + conformal unwrap + island pack); solid/wireframe/x-ray
shading; and a control + screenshot surface so the editor can be driven from
a CLI or an agent. Companion to
`BLENDER_PARITY_GAP_ANALYSIS.md` (sections 5, 6, 7, 9.2, 14 Wave 0/2, 16) and
`UI_REWORK_SPEC.md` (the input/keymap/modal foundation this builds on).

## Context -- what exists today

- `Mesh` objects reference a project `.glb` (`rdoc::model::Mesh.source`). Every
  generator in `rdoc/src/meshgen.rs` writes a **flat-shaded triangle soup**: each
  face owns its vertices, so there is no shared topology to edit.
- The compiler reads the glb as triangles (`rcomp/src/lib.rs` `mesh_object`),
  flips glTF CCW to engine CW in exactly one place, derives lightmap charts
  itself (`csg::chart_mesh_surface`, split by dominant axis), and emits
  triangle or convex-hull collision.
- Sub-object editing is brush-only: `vertex`/`edge`/`face` tools drive
  `brushMoveVertex` / `brushExtrudeFace` (`Viewport.tsx:114`, `:428`). Meshes
  are refused with a toast (`Viewport.tsx:1432`).
- Renderer: one textured forward pass + overlays (`renderer.ts:780-880`), a
  GPU id pick pass (`:898`), a lightmap toggle. No shading modes.
- Undo: every edit is a `Command` whose apply returns its inverse
  (`rdoc/src/command.rs`); gestures coalesce (`begin/end_gesture`).
- Headless: `r5ed-compile` verbs `apply` / `mesh` / `brush` / `show` / `build`
  edit a project on disk. `web/smoke/smoke.mjs` already drives a real Chromium
  with playwright-core and writes screenshots; `web/src/render/png.ts` encodes
  PNG. Nothing lets a CLI drive an *open* editor or ask it for a picture.

## Decisions

**D1. A new editable source format, `.r5mesh`, is authoritative; glb stays an
import/export format.** A `Mesh` whose `source` ends in `.r5mesh` is evaluated by
a pure Rust function (`rdoc::editmesh::evaluate`) wherever triangles are needed:
wasm `objectMesh`, `rcomp::mesh_object`, pick, bounds, nav soup, glb export. No
cached glb next to it -- a derived file that can drift is the trap in gap
analysis 16.6. Existing `.glb` meshes keep working unchanged.

**D2. Topology = indexed polygon mesh + derived adjacency, not half-edge.**
Faces are n-gons (corner arrays into a welded vertex table). Edges are derived
(sorted vertex pair -> face-corner list) on load and after each op. This
represents non-manifold input (stock kit meshes have it), which half-edge
cannot. Ops that need local manifoldness (loop cut, bevel, inset) check it on
the elements they touch and refuse with the offending element ids.

**D3. All geometry ops are Rust in `rdoc`, one implementation for wasm, host
and CLI.** TypeScript owns input, preview and drawing only. Output is
byte-identical across the three builds (existing architecture rule).

**D4. One op contract for UI, CLI and control.** An `OpRequest` JSON
(`{op, sel, params}`) goes in, a `Command` plus the resulting selection comes
out. The UI modal, the redo panel, `r5ed-compile mesh-op` and the control
socket all build the same request.

**D5. An edit session lives in the wasm core.** Entering edit mode loads the
`EditMesh` into a session; interactive drags mutate the session and redraw;
release commits one `Command::SetFile` for the `.r5mesh` (one undo step). Exit
commits nothing new. Selection lives in the session, not the document.

**D6. Keep engine conventions inside the format:** CCW winding (glTF), so the
single CCW->CW flip in `rcomp` stays the only flip; positions f64 (same as
brushes, needed for weld/boolean robustness); UV scale semantics identical to
brush `UvMapping` (`UV_TEXELS_PER_TILE`), so a box-unwrapped mesh and a brush
beside it tile at the same density.

**D7. Texture UVs never drive lightmaps.** `chart_mesh_surface` derives charts
from geometry, not `uv0`, so none of the UV tools here can turn a map black
(gap analysis 16.7). Geometry ops *can* change charts; the compile gate covers
that.

**D8. Modifiers are a non-destructive stack on `Mesh`, evaluated in
`evaluate`.** Array, Curve, Displace, Subdivision, Mirror and Boolean; `Apply` bakes the stack into the
`.r5mesh`. Modifiers work on glb-sourced meshes too (the base is welded on
load).

## Data model

### `EditMesh` (rdoc/src/editmesh/mod.rs)

```rust
pub struct EditMesh {
    pub positions: Vec<DVec3>,            // welded vertex table
    pub faces: Vec<Face>,                 // n-gons, CCW seen from outside
    pub corners: Vec<Corner>,             // face-corner attributes, faces index into this
    pub edge_flags: Vec<(u32, u32, EdgeFlags)>, // sparse; sorted (min,max) pairs
    pub materials: Vec<String>,           // slot names; Mesh.material_map maps them to game paths
    pub smooth_angle: Option<f64>,        // auto-smooth by angle (deg); None = per-face flag only
}
pub struct Face { pub first: u32, pub len: u32, pub material: u16, pub flags: FaceFlags } // SMOOTH
pub struct Corner { pub vertex: u32, pub uv: [f32; 2], pub normal: Option<[f32; 3]> }     // custom split normal
bitflags EdgeFlags { SHARP, SEAM }
```

- Custom split normals exist because imported stock meshes carry authored
  normals; making one editable must not change its shading. An op clears
  custom normals only on the faces it rewrites (their corners recompute from
  smooth/sharp/auto-smooth); untouched faces keep theirs.
- Derived (not stored): edge table, vertex->faces, face normals, boundary and
  non-manifold sets. Rebuilt deterministically (sorted) after each op.

### `.r5mesh` file

Little-endian binary: `"R5MS"`, `u32 version=1`, `u32` counts (verts, faces,
corners, edge flags, materials), then arrays in the order above, positions as
f64, a presence bit for the normal layer, material names as u16-length UTF-8.
A `mesh-dump --json` verb renders it as JSON for review and tests.

Load validation (projects are shared, so the file is untrusted): counts under
`MAX_VERTICES` (4M, existing constant) and a face/corner cap; every index in
range; face `len >= 3` and within `corners`; all floats finite and under the
existing 1e6 magnitude; material index < materials.len(). A failing file is a
compile error naming the object, never a panic.

### `Mesh` additions (rdoc/src/model.rs)

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub modifiers: Vec<Modifier>,
#[serde(default, skip_serializing_if = "is_default")]
pub role: MeshRole,            // Normal | Cutter (operand only: never renders, never collides)
```

Old `map.json` files load unchanged (both fields default).

### Evaluation

`evaluate(base: &EditMesh, modifiers, ctx) -> EvaluatedMesh` (triangles per
material, normals, uv0, plus the polygon edge list for wireframe). Base is the
`.r5mesh`, or the glb welded by value. Triangulation of n-gons is ear clipping
in the face plane (the ear clipper in `meshgen.rs` generalised). Results are
cached per `(file hash, modifiers hash, operand transforms)` in the core.

## Operator contract

```jsonc
{ "op": "bevel",
  "object": "<uuid>",
  "sel": { "mode": "edge", "ids": [4, 9] }       // or "query": {...}
  "params": { "amount": 2.0, "segments": 3 } }
```

- `sel.query` (so a CLI can select without indices): `all`, `none`,
  `normal {dir, max_angle_deg}`, `material {name}`, `box {min,max}` (world),
  `boundary`, `non_manifold`, `loop {edge}`, `ring {edge}`, `linked {from}`,
  `sharp {min_angle_deg}`. Queries resolve to ids in the core, so the UI and the
  CLI select identically.
- Result: `{ command, selection: {mode, ids}, report: {verts, edges, faces,
  warnings[]} }`. Element ids are indices and are only valid until the next
  op; every op returns the new selection so chains work.
- Params are a serde struct per op with Blender's defaults. Unknown keys are
  rejected; every numeric param is clamped to a documented range.
- **Adjust Last Operation (`F9`) and the redo panel** fall out of this: the
  panel edits `params` and re-runs `undo + apply(op)`. Only mesh ops are in
  scope here. `F9` is currently bound to `help` (`keymap.ts:328`); move help
  to `F1` as Blender does.

## Sequence

Ordered by dependency. The drive hooks come first because every later gate is
a scripted screenshot; loop cut and inset precede bevel because bevel reuses
their ring traversal and offset code and is the hardest op.

### M0 -- Drive hooks: control socket, screenshot, CLI

**Page-side dispatcher** `web/src/control/dispatch.ts`: a single
`handle(msg) -> reply` that performs a verb against the live editor. Two
transports reach it:

1. **Host relay** (drive the user's open tab). The page opens
   `WS /api/editor` (token-authenticated like `/api/live`). The host exposes
   `POST /api/editor/{verb}` and forwards to the most recent editor socket,
   returning its reply. The host writes `{port, token}` to a user-only session
   file so the CLI can find it.
2. **Headless** (no tab open). `scripts/drive.mjs` reuses the smoke harness:
   launches Chromium via playwright-core against the host URL and calls the
   same `handle` through `page.evaluate`. On Windows with a real GPU pass
   `R5ED_CHROMIUM` + `R5ED_CHROMIUM_ARGS` (smoke already supports both).

**Verbs** (JSON in, JSON out):

| Verb | Params | Reply |
|---|---|---|
| `state` | | mode, active object, selection counts, undo depth, shading, last op, console errors since last call |
| `open` | `{project}` | loaded object count |
| `select` | `{ids \| query}` object mode | selected ids |
| `edit` | `{object, enter: bool, mode?: vertex\|edge\|face}` | element counts |
| `edit.select` | `sel` as in the op contract | ids |
| `op` | `OpRequest` | op result |
| `undo` / `redo` | | undo depth |
| `view` | `{eye, target, fov}` or `{frame: selection\|all}` or `{axis: top\|front\|right\|...}`, `ortho?` | camera |
| `shading` | `{mode, xray?, wire_overlay?, overlays?}` | shading state |
| `wait` | `{idle: true, timeout_ms}` | resolves when no asset/texture/job is pending and a frame has presented |
| `screenshot` | `{width, height, overlays: bool}` | PNG bytes (base64) |

**Screenshot**: render one frame into an offscreen `rgba8unorm` target at the
requested size (not the swap texture, which is not `COPY_SRC`), copy to a
mapped buffer, encode with `render/png.ts`. Hover highlights and the gizmo are
off unless `overlays: true`, so shots are reproducible. Size capped at 4096^2.

**CLI**: `r5ed-compile ctl <verb> [json] [--out file.png]` talks to the host via
the session file; `--out` is required for `screenshot` (no default directory --
callers choose the disk). Offline verbs that need no browser:
`r5ed-compile mesh-op <project> <OpRequest>` (applies through `rdoc`, same code
as the UI), `mesh-info <project> <object>` (counts, bounds, non-manifold /
boundary / degenerate lists, material slots), `mesh-dump --json`.

Optional last step: a thin `r5ed` MCP server over the same `/api/editor/*`
routes (shape of the existing `r5game` MCP), so an agent session calls
`r5ed_op` / `r5ed_screenshot` directly.

Gate: a script opens a fixture project, frames all, takes a shot; enters edit
mode on a box, selects `normal {dir:[0,0,1]}`, runs `extrude`, shot again;
`undo`; third shot is byte-identical to the first.

### M1 -- EditMesh core (no UI)

- `rdoc/src/editmesh/`: types, `.r5mesh` read/write, adjacency, validation,
  `evaluate`, triangulation, normals (flat / smooth / auto-smooth / sharp
  edges / custom).
- Conversions (each a `Command`): **Make Editable** (glb -> `.r5mesh`: weld by
  position within 1e-4, keep UVs and normals per corner, glTF material names
  become slots); **Brush -> Mesh** (reuses `brush::triangulate` polygons before
  triangulation: each brush face becomes one n-gon, its `UvMapping` projected
  into corner UVs, `lightmap_scale` carried to the mesh; this is the path to
  bevelling a blockout); `MeshSpec` generators gain a `.r5mesh` output with
  shared vertices so a new primitive is editable from birth.
- Integration: wasm `objectMesh`, `pick`, `object_bounds`, `rcomp::mesh_object`,
  nav soup and glb export accept `.r5mesh`.
- Base ops (all through the op contract): `translate/rotate/scale` of a
  selection (with pivot + proportional falloff reusing the existing 6
  falloffs), `delete {verts|edges|faces|only_faces|dissolve}`,
  `merge {center|cursor|collapse|by_distance}`, `extrude_region` (along average
  normal, with `individual` variant), `fill` (edge loop -> n-gon),
  `recalc_normals {outside|inside}`, `flip`, `shade {smooth|flat}`,
  `mark {sharp|seam, clear}`, `set_material {slot}`, `separate {selected|loose}`,
  `split`.

Gates (Rust tests, native and wasm identical bytes):
- Round trip `glb -> r5mesh -> evaluate` preserves triangle count, positions
  (1e-5) and shading on a stock-decompiled chunk from the existing decompile
  fixtures.
- Every op on a closed fixture set (box, cylinder, stairs, arch, torus, a brush
  room) keeps it closed, consistently wound (the `closed()` edge-sum test in
  `meshgen.rs` tests) and Euler-consistent; no degenerate faces (area < 1e-9).
- Determinism: same input + op -> same file bytes, twice, across builds.
- A project with an `.r5mesh` compiles and passes `rbsp::validate`; the
  compiled mesh's lightmap page count equals the glb-sourced twin.
- Fuzzed `.r5mesh` bytes never panic the loader.

### M2 -- Edit mode UI

- `Tab` toggles edit mode on the active Mesh (auto-offers Make Editable on a
  glb; brushes keep their existing vertex/edge/face tools and gain
  "Convert to Mesh"). Header shows mode + element counts.
- New keymap context `edit_mesh` above `viewport` (gap analysis 16.14):
  `1`/`2`/`3` vertex/edge/face select (object mode keeps them for tools),
  `A` / `Alt+A` / `Ctrl+I`, `L` linked under cursor, `Ctrl+L` linked,
  `Alt+click` loop, `Ctrl+Alt+click` ring, `Ctrl+Numpad+/-` grow/shrink,
  `G/R/S` with axis/numeric input from the existing modal transform, `E`
  extrude, `X` delete menu, `M` merge menu, `F` fill, `Shift+N` recalc outside,
  `Alt+N` normals menu, `P` separate, `H`/`Alt+H` hide/reveal elements.
- Component picking: the core does a CPU ray/screen-distance pick against the
  session mesh (vertices and edges by projected pixel distance, faces by ray);
  box select uses projected positions. Without x-ray, occluded elements are
  excluded by a depth test against the rendered depth; with x-ray they are
  included (gap analysis 16.3 -- pick and draw must agree).
- Overlays: vertex points, edges (polygon edges only, not triangulation
  diagonals), face dots in face mode, selected/active colours, sharp and seam
  edge colours, optional face normals. Drawn from a core-built buffer that is
  rebuilt only on topology change; drags update positions only.
- Drags follow D5: the session mutates, one `SetFile` on release.

Gates: smoke steps drive each chord through `ctl`; screenshots of vertex,
edge and face selections on the fixture box; a 50k-face mesh keeps a drag
interactive (frame time recorded in `state`), and one drag is one undo step.

### M3 -- Viewport shading: solid, wireframe, x-ray

- `renderer.ts` gains `shading: "textured" | "solid" | "wireframe"` plus
  `xray: number` (0 = off) and `wireOverlay: boolean`.
- **Solid**: the existing surface pipeline with a second fragment entry: studio
  lighting (camera-relative key + fill + hemisphere ambient), colour by
  material albedo (`MaterialAlbedo`, already loaded), object (hashed id) or
  single; optional backface culling. No textures bound, so it is the fast mode
  on huge maps.
- **Wireframe**: a batched line pass. Edges come from `evaluate` (polygon
  edges) for meshes, from brush polygons for brushes, and from triangles with
  coplanar diagonals suppressed for references and props. One line buffer per
  chunk, a global line budget, and distance LOD beyond it (gap analysis 16.4:
  11k instances must not become 11k draws).
- **X-ray**: surfaces alpha-blended at `xray` with depth write off; the pick
  pass then resolves the nearest *any* hit, matching Blender, and edit-mode
  picking includes occluded elements.
- Keys: `Z` pie, `Shift+Z` toggle wireframe <-> previous, `Alt+Z` x-ray. The
  `showLightmaps` toggle stays orthogonal (it applies in textured and solid).
- Edit mode defaults to solid + wire overlay on the edited object, as Blender.

Gates: shots of a reference map in each mode; wireframe line count stays under
budget on the largest fixture; x-ray pick selects an occluded object; solid
mode frame time on the largest fixture is below textured.

### M4 -- Loop cut, edge slide, subdivide

- `loop_cut {edge, cuts: 1..64, smoothness, factor: -1..1, even, flipped}` on
  `Ctrl+R`: hover shows the ring preview (core returns the ring's cut
  polylines), wheel/PageUp/PageDown change `cuts`, click confirms and enters
  slide, RMB centres.
- Ring traversal crosses **quads only**; it stops at triangles, n-gons,
  boundary edges and non-manifold edges (a closed ring wraps). Each crossed
  quad splits into `cuts+1` quads; new corner UVs and custom normals
  interpolate along the rail, so a textured kit mesh keeps its mapping
  ("correct UVs").
- `edge_slide {factor, even, flipped, clamp}` on `G G`: slides the selected
  loop along adjacent rails; with `even`, the slid loop keeps constant distance
  from one side.

- `subdivide {cuts: 1..32, smoothness: 0..1, fractal: 0..1, seed}` on selected
  edges/faces: quads split into a `(cuts+1)^2` grid, triangles into a
  triangle grid, n-gons into a centre fan of quads; neighbouring unselected
  faces are split too so no T-junctions remain (Blender's corner-pattern
  "straight cut"). `smoothness` pushes new verts toward the smooth limit,
  `fractal` displaces them along the normal (weathering).
- `unsubdivide {iterations}` and `dissolve {verts|edges|limited(angle)}` for
  clean-up after booleans.

Gates: cut counts and face counts exact on a cylinder, a torus (closed ring)
and a stair (ring stops at the triangle cap); UV continuity across the cut
(no seam introduced); slide `factor` +/-1 lands exactly on the neighbour loop;
subdivide of one box face with cuts 2 leaves the mesh closed with no
T-junctions (every edge has exactly 2 faces).

### M5 -- Inset and bridge

- `inset {thickness, depth, even_offset: true, relative_offset: false,
  boundary: true, outset: false, individual: false, edge_rail: false}` on `I`
  (`I` again toggles individual; `Ctrl` while dragging adjusts depth).
- Region inset offsets the region boundary inward in each face's plane; with
  `even_offset` the offset at a corner is divided by `sin(half angle)` so the
  rim has constant width; the rim is a quad strip. `individual` insets each
  face separately. Offsets that would self-overlap are clamped at the first
  collision and reported (no inverted faces).
- New rim faces inherit material and UV by interpolation from their face.
- `bridge_edge_loops {cuts: 0..64, twist, reverse, interpolation:
  linear|path|surface, smoothness, merge: false}` (edge menu): pairs two or
  more selected closed/open edge loops by nearest alignment (minimal summed
  distance over all rotations, then `twist`), connects them with quads, and
  deletes faces bounded by the loops when a loop was selected via faces (so
  "select two facing faces -> bridge" makes a tunnel, as Blender). Loops of
  unequal vertex count are resampled with triangles at the shortfall.

Gates: inset of a box top gives the expected 5 faces with rim width equal to
`thickness` at every point (`even_offset`); individual inset on a 4x4 grid;
clamp report on an over-thick inset; closed/wound invariants hold; bridging
two facing faces of two boxes joined into one mesh yields one closed genus-1
mesh (Euler characteristic 0); bridging 8- and 12-vertex loops stays closed.

### M6 -- Bevel

- `bevel {affect: edges|vertices, offset_type: offset|width|depth|percent,
  amount, segments: 1..64, profile: 0..1 (0.5 = circular),
  clamp_overlap: true, loop_slide: true, material: -1, harden_normals: false,
  mark_seam: false, mark_sharp: false}` on `Ctrl+B` (edges) and `Ctrl+Shift+B`
  (vertices); mouse distance sets `amount`, wheel sets `segments`.
- Per beveled edge: two new rails offset along the adjacent faces (the inset
  offset code), joined by a `segments`-strip following a superellipse profile
  of exponent from `profile`.
- Vertex patches by number of beveled edges meeting:
  - 1: the rail terminates on the unbevelled neighbour edge (with
    `loop_slide`, along it).
  - 2: straight continuation.
  - 3 at a convex corner (every blockout box corner): grid-fill patch whose
    boundary is the three profiles -- matches Blender's default.
  - otherwise: a single cut-off polygon (Blender's `Cutoff` intersection
    type). Documented divergence; `Grid Fill` for n >= 4 is a follow-up.
- `clamp_overlap` limits `amount` to the largest value at which no two rails
  cross (computed per edge, then the minimum across the selection, as
  Blender).
- New faces: material from the adjacent face (`-1`) or the given slot; bevel
  strip faces are smooth, originals keep their flags, and the strip boundary
  is marked sharp. `harden_normals` writes custom split normals on the
  original faces' corners equal to their face normal, so large flats stay flat
  and the bevel catches the light -- the 2-texel lit edge that is this op's
  reason to exist (gap analysis 5.2).
- A bevel's strip is small, so its lightmap charts are small; the compile gate
  checks the chart count and texel sanity, since this is the op most likely to
  stress `chart_mesh_surface`.

Gates: bevel all edges of a box with segments 1, 3, 8 and profile 0.5 yields
the Blender counts (e.g. segments 1: 26 faces); every rail is at `amount`
along its face (offset type `offset`); clamp prevents inversion on a thin
slab; vertex bevel on a box corner; a bevelled, harden-normals box compiles,
bakes, and a screenshot of the baked result shows lit edges.

### M7 -- Spin, screw, add-in-edit, join

- `spin {axis, center, angle_deg: -360..360, steps: 1..512, dupli: false,
  merge_ends: true}` (spin tool, header shows axis/center): sweeps the
  selected profile (edges or faces) around the axis; a full 360 with
  `merge_ends` welds the seam so a lathed tower wall is closed. Created faces
  wind outward (checked against the swept normal, flipped if needed).
- `screw {axis, center, angle_deg per turn, turns: 1..64, height per turn,
  steps per turn}`: spin plus axial offset -- spiral stairs, twisted columns.
- `Shift+A` in edit mode adds a primitive (every `MeshSpec` shape) *into* the
  edited mesh at the 3D cursor, selected, so a tower gets a buttress without a
  new object.
- `join` (`Ctrl+J`, object mode): merges selected Mesh objects into the active
  one (materials merged by name, transforms baked into the active's space);
  `separate` (M1) is its inverse. Brushes in the selection are converted with
  Brush -> Mesh first -- this is the answer to "brushes are convex only".

Gates: spin a 2D arch profile 360 deg with 32 steps -> closed mesh, seam welded
(no duplicate verts on the seam); screw 3 turns -> vertex count exact; joined
box + brush compiles as one object.

### M8 -- UV: projection, seams, unwrap, pack

- `U` menu in edit mode: `uv_box`, `uv_cylinder`, `uv_planar` (project from an
  axis or the view), `uv_unwrap`, `uv_pack`, `uv_reset`; `mark_seam` /
  `clear_seam` on edges (`Ctrl+E` edge menu too). All work on selected faces
  (all if none).
- `uv_box {scale: world|fit, texels_per_tile, offset, rotation, space:
  object|world}`: each face picks its dominant axis and projects onto that
  plane with the same axis conventions and density as `brush::project_uv`, so a
  box-unwrapped mesh and a brush beside it align. `space: world` makes
  textures continuous across separately placed objects (brush-like), `object`
  moves with the object.
- `uv_cylinder {axis: object_z|world_z|fit, center, seam_angle_deg,
  scale: world|fit, cap: planar|none}`: `u = angle * radius` (world length, so
  density matches the box mode), `v` = height along the axis; the seam is split
  at `seam_angle`; faces whose normal is within 30 deg of the axis get planar
  cap projection. `fit` axis = principal axis of the selection's vertices.
- `uv_unwrap {method: conformal, fill_holes: true, margin, scale: world|fit}`:
  islands = face groups connected across non-seam edges; each island is
  flattened by least-squares conformal maps (LSCM: two pinned vertices at the
  island's farthest pair, sparse normal equations solved by conjugate
  gradient in f64, pure Rust). `scale: world` rescales each island so its UV
  area over world area matches `texels_per_tile`, which keeps stone the same
  size on a round tower and on the flat wall beside it.
- `uv_pack {margin, rotate: true}`: packs islands into the 0..1 square
  (largest first, shelf packer, 90-degree rotation) -- only for non-tiling
  trim/atlas materials; tiling world textures keep `scale: world` and skip
  packing.
- All UV ops write corner UVs only; projection boundaries get `SEAM`.
  Viewport aid: a checker texture in solid mode (`shading.color = uv_checker`)
  to judge stretch, plus a `mesh-info` stretch report (worst islands by
  area ratio and angle distortion).

Gates: box projection of a box mesh equals a same-size brush's UVs corner for
corner; cylinder projection of a cylinder gives a single seam, uniform texel
density (area ratio uv/world constant within 1%); unwrap of a cylinder with
one vertical seam is angle-preserving (max angle distortion < 1 deg); unwrap
is deterministic; compiled lightmap page count unchanged by every UV op (D7).

### M9 -- Modifier stack: array, curve, displace, subdivision, mirror

- `Modifier` enum with `enabled`, `show_in_viewport`, and per-kind params;
  UI stack in the Inspector (add, reorder, toggle, Apply, remove); `Apply`
  bakes into the `.r5mesh` (a glb-sourced mesh is made editable first).
  Evaluated top to bottom inside `evaluate`, cached per stack hash.
- `Array {count: 1..1024 | fit_length, relative_offset: Option<DVec3>
  (default [1,0,0], factor of bounds), constant_offset: Option<DVec3>,
  object_offset: Option<Id>, merge: Option<f64 distance>, merge_first_last,
  uv_offset: [f64;2]}`. Each copy's transform composes the enabled offsets;
  `object_offset` reads another object's transform relative to this one (the
  Blender rotating-array trick). Output vertex total is checked against
  `MAX_VERTICES` before building. Crenellations = one merlon + array.
- `Curve {path: Id (a PathObject), axis: X|Y|Z|-X|-Y|-Z}`: deforms the mesh
  along the path -- the mesh's `axis` coordinate maps to arc length, the other
  two to the path's rotation-minimising frame. Array (fit_length = path
  length) then Curve gives a crenellated wall that follows a curved line.
- `Displace {strength, mid_level, direction: normal|x|y|z, space:
  local|world, texture: Noise {kind, scale, octaves, seed}}`: reuses
  `rterrain`'s noise functions (the fractals the terrain recipe uses), so
  weathered rock and terrain speak the same vocabulary. Works best after
  Subdivision.
- `Subdivision {levels: 0..4, kind: catmull_clark|simple,
  keep_sharp_edges: true}`: Catmull-Clark on n-gons; `SHARP` edges act as
  creases when `keep_sharp_edges` (rocks, rounded merlons).
- `Mirror {axes: [x,y,z], merge: Option<f64>, clip: true, mirror_object:
  Option<Id>}`: symmetric arches and gatehouses modelled as one half; `clip`
  keeps edit-mode verts on the plane.
- `Boolean` joins the same stack in M10.
- Compile: modifier output is ordinary geometry. Triangle collision copies
  naturally; `convex_hulls` becomes one hull per loose part. For repeating a
  *stock model*, a static prop array (instancing) is the right tool -- out of
  scope here and noted in the UI tooltip.

Gates: fence run with fit_length and merge produces one welded strip (no
internal duplicate verts); rotating array via object offset closes a ring;
array + curve along a quarter circle keeps every merlon upright (frame test);
subdivision level 2 of a cube gives Catmull-Clark counts (98 verts, 96 faces);
displace is deterministic per seed; mirror with merge leaves no duplicate verts
on the plane; undo of a modifier edit is one step; a map with a 200-count array
compiles and its collision BVH validates.

### M10 -- Mesh boolean

- Destructive op `boolean {operation: difference|union|intersect, operand:
  Id | "unselected"}` (edit mode: selected faces vs. the rest, as Blender's
  Intersect (Boolean)), and a `Boolean` modifier with an object operand (Mesh
  or Brush; brushes triangulate through `brush::triangulate`). Setting an
  operand offers to set it `role: Cutter`, which the viewport draws as wire and
  the compiler skips. Doorways, arrow slits and window arches are cutters.
- Solver (pure Rust, builds for wasm32-unknown-unknown): mesh arrangement --
  triangle-triangle intersection with exact orientation predicates (the
  `robust` crate's Shewchuk predicates), split faces re-triangulated per face
  with a constrained triangulation (`spade`), then each piece classified
  inside/outside the other operand by generalised winding number, kept or
  dropped by `operation`, and coplanar pieces resolved by normal direction.
  Output is re-welded into n-gons where pieces share a plane and material
  so faces stay editable.
- Preconditions: both operands closed and consistently wound (the M1
  validation). A non-manifold operand is refused with its bad edges, and the
  error names `merge by distance` / `fill` as the fix.
- A worst-case budget (intersection pair count) aborts with a report instead
  of hanging the tab; modifier evaluation is cached (D1) so a boolean
  re-evaluates only when an operand moves.
- Material and UV of kept faces are preserved; cut faces take the operand's
  material (Blender behaviour) unless `keep_material: true`.

Gates: box minus cylinder, box union box (touching and overlapping), coplanar
face cases, a cutter exactly on an edge; every result closed and wound;
randomized fuzz of transforms over the fixture set never produces an open
result from closed inputs; native and wasm outputs byte-identical; a doorway cut
through a bevelled wall compiles, bakes and screenshots clean.

### M11 -- Acceptance: a medieval kit built only in r5ed

Scripted through `ctl` + `mesh-op`, no external DCC: a round tower (cylinder
-> loop cuts -> inset windows -> boolean arrow slits -> cylinder UV), a curved
curtain wall (box merlon -> array fit_length -> curve along a path ->
bevel), an arched gate (arch profile -> spin/extrude -> bevel -> mirror), a
rock outcrop (icosphere -> subdivision 2 -> displace), all world-scale UV'd.
The map compiles, bakes, passes `rbsp::validate`, and a screenshot set in
solid, wireframe and textured is produced for review. Loading it in game is
the user's run.

## Security

- `.r5mesh` and `map.json` modifiers are untrusted input (projects are
  shared): validation in M1 and a structural cap on every op's output
  (`MAX_VERTICES`, segment/cut/count clamps) before allocation.
- Every op has a work budget; the boolean and array have explicit pre-checks
  so a crafted project cannot freeze the tab or the host compile job.
- `/api/editor/*` and `WS /api/editor` use the existing token, Origin and Host
  checks; the host relays only the verb table above (no eval, no file paths --
  `open` takes a project name that goes through `projects.rs` path checks).
  The session file is written user-only. Screenshot size is capped.

## Out of scope

Sculpt on meshes, vertex colours, a 2D UV editor window (UVs are driven by
ops + checker view + stretch report), lightmap-UV authoring (charts stay
compiler-derived, D7), further modifiers (solidify, bevel modifier, decimate,
shrinkwrap come next on the same stack), knife tool, prop-instance arrays. Gap analysis section 15 lists the Blender features that stay out
permanently.

## Critical files

| Area | Files |
|---|---|
| Core | `crates/rdoc/src/editmesh/{mod,io,adjacency,eval,ops/*}.rs`, `model.rs` (Mesh.modifiers/role), `meshgen.rs`, `brush.rs` (to-mesh), `command.rs` (unchanged: SetFile) |
| Compile | `crates/rcomp/src/lib.rs` `mesh_object`, nav soup |
| Wasm | `crates/r5ed-wasm/src/lib.rs` (session, `meshOp`, `editBuffers`, `editPick`), `web/src/core/api.ts` |
| Host / CLI | `crates/r5ed-host/src/api.rs` (`/api/editor`), new `editor_link.rs`; `crates/r5ed-compile/src/lib.rs` (`mesh-op`, `mesh-info`, `mesh-dump`, `ctl`) |
| Web | `web/src/control/dispatch.ts`, `render/renderer.ts` + `shaders.ts` (shading, xray, wire, offscreen shot), `render/overlay.ts` (edit overlays), `input/keymap.ts` (`edit_mesh` context, `F9`, `Z`), `ui/Viewport.tsx`, `ui/Inspector.tsx` (modifier stack, redo panel), `state/editor.ts` |
| Harness | `scripts/drive.mjs`, `web/smoke/smoke.mjs` (new steps), fixtures |

## Verification

Each phase closes on three kinds of evidence: Rust invariant tests (closed,
wound, Euler, deterministic, native == wasm), a scripted `ctl` run producing
screenshots checked in review, and for M1/M6/M8/M9 a compile + bake of a
fixture map passing `rbsp::validate` (M1, M6, M9, M10, M11). In-game confirmation of a baked
bevel/boolean map is the user's run, not the harness's.
