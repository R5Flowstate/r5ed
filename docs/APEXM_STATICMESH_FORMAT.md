# Apex Legends Mobile: cooked StaticMesh format

The cooked `UStaticMesh` of Apex Legends Mobile (the TDM CyberPark / "Pythas
block 0" backup), as `scripts/apexm/` reads it. Every layout here was checked
against all 9,522 StaticMesh packages in the backup: each one parses, every
LOD ends exactly at the size its prefix declares, every index is in range and
every section's triangle count matches its index range. Field names follow
UE 4.23, which the game is built on; **Tencent** marks a change from stock
4.23.

## Package

Standard UE4 package, unversioned:

- `.uasset` header, `.uexp` export payloads (an export's `SerialOffset`
  counts from the start of the `.uasset`, so its bytes are at
  `SerialOffset - TotalHeaderSize` in the `.uexp`), `.ubulk` streamed bulk
  data. The `.uexp` ends with the 4-byte package tag.
- Summary: tag `0x9E2A83C1`, `LegacyFileVersion` -7, `FileVersionUE4`,
  licensee version and custom versions all **0** (unversioned cook), package
  flags `0x80000000` (`FilterEditorOnly`). `PKG_UnversionedProperties` is not
  set: properties are ordinary tagged properties.
- Import entries are 28 bytes, export entries 104 (the 4.22-4.27 layout with
  `TemplateIndex`, no UE5 fields).

## UStaticMesh export

| Field | Type | Notes |
|---|---|---|
| tagged properties | | `StaticMaterials` (slot -> `MaterialInterface`, `MaterialSlotName`), `LightMapResolution`, `LightMapCoordinateIndex`, `ExtendedBounds`, ... |
| bHasGuid | i32 | then a GUID if set (always 0 here) |
| StripFlags | u8, u8 | `01 00`: editor data stripped |
| bCooked | u32 | 1 |
| BodySetup | i32 package index | |
| NavCollision | i32 package index | |
| LightingGuid | 16 bytes | |
| Sockets | TArray\<i32\> | |
| LODs | TArray of **LOD** | count i32, see below |
| NumInlinedLODs | u8 | |
| StripFlags | u8, u8 | `01 01`: distance fields stripped |
| Bounds | FBoxSphereBounds | origin f32x3, extent f32x3, radius f32. **This is the position quantization box.** |
| bLODsShareStaticLighting | u32 | |
| ScreenSize[8] | (u32 bCooked, f32) x 8 | FPerPlatformFloat per LOD slot |
| rest | | stock UStaticMesh tail (not needed for geometry) |

### LOD

| Field | Type | Notes |
|---|---|---|
| **SerializedSize** | u32, u32 0 | **Tencent.** Byte length of the rest of the LOD; the high word is always 0. A reader can skip a LOD with it. |
| StripFlags | u8, u8 | `01 01` |
| Sections | TArray of **Section** | |
| MaxDeviation | f32 | |
| bIsLODCookedOut | u32 | always 0 in the backup |
| bInlined | u32 | 1: buffers follow; 0: buffers are in the `.ubulk` |
| buffers | **Buffers** | when inlined |
| or bulk header | u32 flags, i32 count, i32 size on disk, i64 offset | when streamed (flags e.g. `0x501`). The data is `count` bytes at `.ubulk` offset `offset + BulkDataStartOffset` (the summary field; the stored offset is negative relative to it). Those bytes are one **Buffers** block. |
| availability info | 4+4 + 16 + 8 + **3x8** + 6x8 bytes | when streamed: DepthOnlyNumTriangles, packed flags, then the metadata of every buffer. **Tencent**: three colour-buffer metadata pairs instead of one. |
| SerializedBuffersSize, DepthOnlyIBSize, ReversedIBsSize | u32 x 3 | |

### Section (Tencent: variable length)

| Field | Type |
|---|---|
| MaterialIndex | i32 (index into `StaticMaterials`) |
| FirstIndex, NumTriangles, MinVertexIndex, MaxVertexIndex | u32 |
| bEnableCollision, bCastShadow | u32 |
| **Bounds** | FBoxSphereBounds (28 bytes); zero on some sections |
| **ClusterLevels** | TArray\<u16\> |
| **Clusters** | TArray of 16-byte nodes |

Stock 4.23 has `bForceOpaque` after `bCastShadow`; this build does not.

The clusters are a per-section triangle BVH for mobile GPU culling. A node is
8 x u16: min xyz and max xyz quantized to 11 bits (0..2047) inside the
section bounds, the first child's index (4095 on a leaf), and the first
triangle. Leaves hold ~23 triangles (18-64). `ClusterLevels` lists node counts
per tree level. Sections under ~128 triangles have none. Nothing in a port
needs them.

### Buffers

In order: StripFlags (u8, u8; `01 01` = editor data and adjacency stripped,
reversed index buffers present), then:

1. **Position buffer**: Stride u32 (12), NumVertices u32, then a bulk array
   header (element size 12, count **0**: **Tencent**, the float array is
   empty) followed by `NumVertices` x **3 x i16**, unprefixed. A position is
   `Bounds.Origin + q / 32767 * Bounds.Extent` per axis, with the render
   `Bounds` from after the LOD array. Every axis uses the full -32767..32767
   range. No mesh in the backup stores float positions.
2. **Static mesh vertex buffer**: StripFlags (u8, u8), NumTexCoords u32
   (1-5), NumVertices u32, bUseFullPrecisionUVs u32 (always 0),
   bUseHighPrecisionTangentBasis u32 (always 0), then
   - tangents: bulk array, element size **4** (**Tencent**; stock is 8), one
     per vertex. Bytes 0,1 are the tangent and bytes 2,3 the normal, each an
     octahedral unit vector with 7 bits per axis: `u = (b & 0x7f) / 63.5 - 1`,
     then the usual octahedral fold. The high bit of bytes 2 and 3 (always set
     together; never set on bytes 0,1) is the bitangent sign: set means
     `bitangent = -(normal x tangent)`, a mirrored UV.
   - UVs: bulk array, element size 4, `NumVertices x NumTexCoords` half-float
     pairs. UV1 is the lightmap channel (`LightMapCoordinateIndex` = 1).
3. **Three colour buffers** (**Tencent**: stock has one). Each is StripFlags
   (u8, u8), Stride u32, NumVertices u32, then a bulk array only when
   NumVertices > 0.
   - stream 0, stride 4: ordinary FColor vertex colour (BGRA), often a
     material blend mask;
   - stream 1, stride 8: four u16 per vertex, three in 0..8191 and the fourth
     ~255; baked per-vertex lighting by every sign, not needed when the map
     is rebaked;
   - stream 2: always empty in the backup.
4. **Index buffer**: b32Bit u32, bulk array of bytes (element size 1). Always
   16-bit in the backup.
5. Reversed index buffer, depth-only index buffer, reversed depth-only index
   buffer (same shape; present because class strip bit 4 is clear). Wireframe
   and adjacency buffers are stripped.
6. Area-weighted samplers: one per section plus one, each
   TArray\<f32\>, TArray\<i32\>, f32. Empty in the backup.

Winding is Unreal's: clockwise front faces in a left-handed, Z-up,
centimetre space. Triangles as stored give inward normals under a
right-handed cross product; the decoded normals agree with the flipped
geometric normal (median cosine 0.9999 across a 300-mesh sample).

## What the backup holds

- 9,522 StaticMesh packages; LOD counts 1 (5,937), 3 (2,480), 2 (998), 4
  (102), 5 (5). 15,806 LODs decoded: 13,778 inlined, 2,028 streamed.
- The CyberPark level is 17 sublevels in `cyberpark_uefiles/scripts`
  (`TDM_MP_CyberPark_*.umap`, each with lighting `_BuiltData`): 307
  StaticMeshActors and 27 InstancedStaticMeshComponents in `_Asset`, 28
  `ApexPlayerStart`, 41 air-wall `BlockingVolume`s, 6 ziplines, ~1,400 baked
  lights. It references 193 static meshes; 188 are in the backup. The
  missing five are engine primitives (`Cube`, `Cylinder`) and the
  zipline/grapple/god-ray props.

## Tools

`scripts/apexm/`: `upkg.py` (package), `uprops.py` (tagged properties),
`umesh.py` (this format), `glbout.py` and `mesh2glb.py`:

```
py scripts/apexm/mesh2glb.py <mesh.uasset> <out.glb> [--lod N]
py scripts/apexm/mesh2glb.py --check <dir>
```

The .glb is right-handed, Z up, centimetres (Y negated and winding reversed
from Unreal), one primitive per section with the slot's material asset name;
`r5ed-compile import-glb` takes it with `{"scale":0.3937,"up":"z"}`.

## Porting a level

`scripts/apexm/port_level.py` turns a level into an r5ed project, and
`scripts/apexm/ship_level.sh` ships it from Windows:

```
py scripts/apexm/port_level.py <backup> <backup>/cyberpark_uefiles/scripts TDM_MP_CyberPark_ <out> --stem mp_rr_cyberpark
scripts/apexm/ship_level.sh <out>
```

What the port reads and how:

- **Placements** (`uplace.py`): every StaticMeshComponent and
  InstancedStaticMeshComponent of every sublevel, its `RelativeLocation`,
  `RelativeRotation` (pitch, yaw, roll as `FRotationMatrix`, row vectors) and
  `RelativeScale3D`, composed up the `AttachParent` chain. Instances follow
  the component's tagged properties as a bulk array of **112-byte** records:
  a 4x4 matrix (row vectors, translation in row 3, whose w is stored as 0)
  then three float4 lightmap/shadowmap coordinate vectors.
- **Textures** (`utex.py`): Texture2D and Texture2DArray carry two strip-flag
  pairs, `bCooked`, the pixel-format FName and a skip offset, then
  FTexturePlatformData (SizeX, SizeY, packed slice count, pixel-format
  string, first mip, mip array, each mip a bulk-data record). The backup is
  ASTC (`PF_ASTC_8x8` and kin); mip 0 is decoded to RGBA.
- **Materials** (`umat.py`): MaterialInstanceConstant `TextureParameterValues`
  following `Parent`; base colour is `BaseColor`, `Diffuse`, `SplatColor` or
  `EmissiveMap`. The merged atlases (`T2A_*` meshes) bind a
  **Texture2DArray**: 181 tileable 256x256 surfaces, and the slice a triangle
  samples is the **R byte of the stride-4 colour stream** (FColor BGRA).
  A source texture bigger than 256 px is cut into parts on consecutive
  slices, and **G says how**: `0x11` is 2x2 parts, row-major, slices R..R+3;
  `0x10` is two parts side by side along U; `0` is one slice. The UVs span
  the whole source texture (the mobile shader picks the part per pixel), so
  the port reassembles the parts into one texture (`world.atlas_texture`)
  before splitting the triangles into one material per (slice, split).
  Sampling only slice R stretched a quarter of the texture over the surface
  (110k of the 114k atlas triangles). Verified: copied part textures match
  the slices exactly, and the row-major seams beat column-major 37 to 3.
- **Normal maps**: plain ones keep tangent-space X,Y in R,G (B empty);
  the merged-atlas `_Normal` arrays keep X,Y in **G,B**, with other surface
  data in R and A. The port rebuilds Z and writes an RGB normal map per
  material (`normalTexture`), keeping Unreal's green convention, which
  objmat passes through as it does Source's.
- **Material layers** (`world.classify`): base colour times `TintColor`;
  `M_Emissive`/`M_videoscreen_Mat`/unlit materials glow with their whole
  texture times the `Emissive` colour; `M_Prop*` materials with an
  `Emissive` colour glow through the **base colour's alpha** (a sparse mask:
  screen areas, sign letters, light panels); `BLEND_Translucent` is glass
  with the instance's `Opacity`; `TwoSided` is double-sided. Each material
  carries its template class in the .glb as `extras.r5ed_class`
  (`world_opaque`, `world_emissive`, `world_glass`), and the emissive layer as
  `emissiveTexture` with its HDR intensity in `extras.emissive_intensity`.
  objmat.py builds on that class and fills the template's emissive slot from
  `emissiveTexture`.
- **Surface maps**: a material's `MIX` texture is **R metallic, G roughness,
  B ambient occlusion** (A unused). Checked against the textures: metal
  surfaces have the highest R, glass the lowest G, cloth the highest; on a
  mixed prop R lights exactly the metal panels and B the creases. A merged
  atlas has no MIX: its `_Normal` array carries **R roughness and A
  metallic** around the normal's G,B (A is 1 on the brushed and gold metal
  slices, 0 on concrete and tile). Both go into the .glb as standard glTF
  `metallicRoughnessTexture` (G roughness, B metallic) and
  `occlusionTexture`. objmat.py turns them into the templates' spec/gloss:
  gloss = 1 - roughness, spec = lerp(0.04, albedo, metallic) in linear,
  albedo darkened by metallic, AO into `aoTexture`. This applies to any .glb
  with those maps, Blender exports included.
- **Scrolling tickers** (`M_BasePanner`): an emissive LED texture tinted by
  `Param`, panned at `PannerSpeed_U/V`. They port as static emissive; the
  speed rides in `extras.uv_scroll` because no template scrolls a lit
  surface.
- **Not in the backup**: the foundation's puddle layer
  (`M_BuildWithPuddle`: `CoverTexture`, `SplatColor`, `SplatMask`) names
  three textures none of which the backup holds, so the floor ports
  without its wet patches.
- **What is world geometry**: the `_Asset`, `_ZipLine`, `_Main_Random` and
  `_AirWall` sublevels. Buildings come in three parts: the textured mesh, an
  `_E` emissive overlay (light strips, screens), and `_Col_<surface>`
  collision proxies; the port renders the first two and lets the visual
  mesh collide. `AirWall_art` is a 46-triangle shell around the whole arena:
  it becomes invisible move-clip (contents `0x00A30000`). Decals, FX, light
  cards and the sky dome are left out. The dome (`SM_TDM_CyberPark_sky` at
  400x, radius ~6 km) is 3.6x past the engine's +-65535 coordinate limit;
  the map shows the vista's sky instead.
- **Glass**: `BLEND_Translucent` instances carry their opacity as the scalar
  `Opacity`; the texture's alpha is no opacity (on `M_Prop*` it is the
  emissive mask). The .glb puts the scalar in `baseColorFactor[3]` with
  `alphaMode: BLEND`, and objmat fills the glass template's
  `opacityMultiplyTexture` with that value, flat. Before, the slot was
  derived from the albedo's alpha, so the glass drew solid or blotchy.
- **Lighting** (`ulight.py`, `port_level.py --lighting night|day`): the
  level's own lights, resolved through their `AttachParent` chains.
  `_SkyBox` holds a `DirectionalLight` (rotation -65.5 / 215.3, intensity
  3, colour (137, 161, 184): a cool moon) and a `SkyLight` (0.7 over a night
  panorama whose cooked average brightness is 0.48). `_Light_Outdoor`,
  `_Light_Indoor`, `_LABakeLight` and `_Main_Random` hold 1,180 rect, 119
  point and 97 spot lights, all Static, nearly all without shadows. `_Light`
  holds 7 `BoxReflectionCapture`s and the post-process volumes (exposure
  bias ~0, EV -1..-0.1; a distance fog 30-44 m that r5ed has no
  counterpart for). `night` ports all of it:
  - the moon as the sun (direction, linear colour, intensity 3);
  - the sky light as the bake's sky ambient (colour times intensity times
    the cubemap's average);
  - each local light as an r5ed point or spot. A rect light becomes a
    40/85 degree spot along its normal, because r5ed compiles an area light
    as `light_environment`. Brightness matches the Unreal illuminance
    halfway to the attenuation radius: candelas from the units (legacy
    unitless = 1/625 cd; lumens over the sphere, cone or cosine hemisphere),
    or the legacy exponent falloff for non-inverse-squared lights, then
    `_light` = 255 * 4 * lux, clamped to 4000. Range = half the attenuation
    radius, because the baker reaches zero at twice the range: the light
    ends where Unreal's does. At range = R, CyberPark's lights reached
    twice as far as in the original, and the bake walked eight times the
    volume per light. `--local-light-scale` scales every one;
  - `--light-scale` scales all of the baked light (moon, sky ambient, local
    lights) and leaves emissive surfaces alone. 1 keeps the original values.
    At 1, a preview with the baker's own falloff lights the streets evenly
    and washes out the neon. Below 1, the signs carry the scene as they do on
    mobile;
  - the reflection captures as `cubemap_points`, which replace the grid.

  `day` is the clear-day preset first shipped (sun 2.5 at 50 / 35 degrees,
  warm; blue sky 0.7; no local lights), with the same cubemaps. Neither
  adds a `sky` light: that kind compiles to a `sky_camera`, and the
  compiler seats its own at the vista.
- **Spawns**: `ApexPlayerStart` in `_FixedPoint`, `PlayerStartCamp` 1/2 ->
  team 2/3, origin from the capsule's root component less ~90 cm.
- **Space**: Unreal left-handed cm -> right-handed inches: Y negated, yaw
  negated, scale 1/2.54. Negating Y also turns Unreal's clockwise winding
  counter-clockwise, so triangles keep their stored order unless the
  instance matrix itself mirrors.
