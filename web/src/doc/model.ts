import type { Id, Quat, Vec3 } from "../core/api";

export type { Id, Quat, Vec3 };

export type KeyValues = Record<string, string>;

export interface MapMeta {
  stem: string;
  display_name: string;
  description: string;
  playlists: string[];
  loadscreen: string | null;
  minimap: MinimapSettings;
  sky: string | null;
  resident_paks: string[];
}

export interface MinimapSettings {
  enabled: boolean;
  origin: [number, number];
  size: number;
  resolution: number;
}

export interface Layer {
  id: Id;
  name: string;
  visible: boolean;
  locked: boolean;
}

export interface Transform {
  position: Vec3;
  rotation: Quat;
  scale: Vec3;
}

export interface UvMapping {
  u_axis: Vec3;
  v_axis: Vec3;
  offset: [number, number];
  scale: [number, number];
  rotation: number;
}

export interface BrushFace {
  normal: Vec3;
  dist: number;
  material: string;
  uv: UvMapping;
  lightmap_scale: number;
}

export interface Brush {
  faces: BrushFace[];
  contents: number;
  detail: boolean;
}

export type CollisionMode = "triangles" | "convex_hulls" | "none";

export interface Mesh {
  source: string;
  material_map: KeyValues;
  collision: CollisionMode;
  contents: number;
  cast_shadows: boolean;
  lightmap_scale: number;
  /** False for collision that draws nothing; absent means true. */
  render?: boolean;
  /** A cutter is a boolean operand: drawn as wire, skipped by the compiler. */
  role?: MeshRole;
  /** Non-destructive modifiers, evaluated top to bottom. */
  modifiers?: Modifier[];
}

export type MeshRole = "normal" | "cutter";

/** One stack entry: its kind in `type`, that kind's params beside it. */
export interface Modifier {
  type: "array" | "curve" | "displace" | "subdivision" | "mirror" | "boolean";
  enabled?: boolean;
  show_in_viewport?: boolean;
  [param: string]: unknown;
}

export interface Prop {
  model: string;
  skin: number;
  static_prop: boolean;
  solid: boolean;
  cast_shadows: boolean;
  fade_distance: number;
  keyvalues: KeyValues;
}

export type Partition = "main" | "env" | "fx" | "script" | "snd" | "spawn";

export interface Entity {
  classname: string;
  keyvalues: KeyValues;
  partition: Partition | null;
  /** Key -> object id, resolved to targetnames at compile time. */
  links: KeyValues;
}

export type VolumeKind =
  | "trigger" | "trigger_hurt" | "trigger_soundscape" | "envmap_volume"
  | "light_environment_volume" | "fog_volume" | "player_clip" | "out_of_bounds" | "custom";

export interface Volume {
  kind: VolumeKind;
  classname: string;
  brushes: Brush[];
  keyvalues: KeyValues;
}

export type LightKind = "sun" | "point" | "spot" | "area" | "sky";

export interface Light {
  kind: LightKind;
  color: [number, number, number];
  intensity: number;
  range: number;
  inner_angle: number;
  outer_angle: number;
  size: [number, number];
  cast_shadows: boolean;
  baked: boolean;
  realtime: boolean;
  keyvalues: KeyValues;
}

export type PathKind = "zipline" | "ziprail" | "spline";

export interface PathObject {
  kind: PathKind;
  points: Vec3[];
  keyvalues: KeyValues;
}

export interface ScatterRule {
  models: string[];
  density: number;
  min_weight: number;
  slope_deg: [number, number];
  height: [number, number];
  scale: [number, number];
  align_to_normal: number;
  seed: number;
  static_prop: boolean;
}

export interface TerrainLayer {
  name: string;
  material: string;
  weights: string;
  uv_scale: number;
  scatter: ScatterRule[];
}

export interface Terrain {
  heights: string;
  resolution: [number, number];
  cell_size: number;
  layers: TerrainLayer[];
  chunk_size: number;
  collision: boolean;
  contents: number;
  lightmap_scale: number;
  recipe: unknown | null;
}

export interface VoxelTerrain {
  bounds: [Vec3, Vec3];
  voxel_size: number;
  ops: unknown;
  material: string;
  uv_scale: number;
  collision: boolean;
  contents: number;
  lightmap_scale: number;
}

export type ReferenceKind = "apex_map" | "tf2_map";

/** One record of a reference map, addressed so it survives a recompile. */
export type RefRecord =
  | { type: "entity"; index: number }
  | { type: "prop"; index: number }
  | { type: "mesh"; model: number; material: string }
  | { type: "collision"; model: number; contents: number; surface: string };

/**
 * A shipped map the document is built on. Each decompiled object is listed in
 * `sources` with the record it stands for: while the object is hidden the
 * map's own record compiles; once it is shown (an edit shows it) the object
 * compiles in its place; once it is deleted the record is dropped.
 */
export interface Reference {
  kind: ReferenceKind;
  source: string;
  promoted: string[];
  removed_entities: number[];
  sources?: Record<string, RefRecord>;
}

/** The record a decompiled object stands for, if any reference lists it. */
export function referenceRecord(objects: readonly DocObject[], id: string): RefRecord | undefined {
  for (const o of objects) {
    if (o.kind.type !== "reference") continue;
    const r = o.kind.reference.sources?.[id];
    if (r) return r;
  }
  return undefined;
}

export type ObjectKind =
  | { type: "brush"; brush: Brush }
  | { type: "mesh"; mesh: Mesh }
  | { type: "prop"; prop: Prop }
  | { type: "entity"; entity: Entity }
  | { type: "volume"; volume: Volume }
  | { type: "light"; light: Light }
  | { type: "path"; path: PathObject }
  | { type: "terrain"; terrain: Terrain }
  | { type: "voxel_terrain"; voxel_terrain: VoxelTerrain }
  | { type: "reference"; reference: Reference }
  | { type: "group" }
  | { type: "prefab_instance"; prefab_instance: { prefab: string } };

export interface DocObject {
  id: Id;
  name: string;
  layer: Id;
  parent: Id | null;
  transform: Transform;
  hidden: boolean;
  locked: boolean;
  kind: ObjectKind;
}

export interface BakeSettings {
  texel_size: number;
  bounces: number;
  samples: number;
  sky_color: [number, number, number];
  sky_intensity: number;
  ambient_occlusion: boolean;
  probe_spacing: number;
  cubemap_spacing: number;
}

export interface NavmeshSettings {
  enabled: boolean;
  hulls: string[];
  bounds: [Vec3, Vec3] | null;
}

export interface VisSettings {
  portals: boolean;
}

export interface Settings {
  bake: BakeSettings;
  navmesh: NavmeshSettings;
  vis: VisSettings;
}

export interface MapDoc {
  format: number;
  meta: MapMeta;
  layers: Layer[];
  objects: DocObject[];
  settings: Settings;
}

/**
 * The core serialises an object kind with its fields inline next to `type`
 * (`{type: "brush", faces: ...}`); the UI nests them under the variant name
 * (`{type: "brush", brush: {faces: ...}}`). These two convert at the boundary.
 */
export function kindFromWire(wire: unknown): ObjectKind {
  if (!wire || typeof wire !== "object") return wire as ObjectKind;
  const { type, ...fields } = wire as { type: string } & Record<string, unknown>;
  if (type === "group") return { type: "group" };
  return { type, [type]: fields } as unknown as ObjectKind;
}

export function kindToWire(kind: ObjectKind): Record<string, unknown> {
  if (kind.type === "group") return { type: "group" };
  const fields = (kind as unknown as Record<string, unknown>)[kind.type] as Record<string, unknown>;
  return { type: kind.type, ...fields };
}

export function objectFromWire(wire: unknown): DocObject {
  const o = wire as DocObject & { kind: unknown };
  return o.kind === undefined ? o : { ...o, kind: kindFromWire(o.kind) };
}

export function objectToWire(object: DocObject): Record<string, unknown> {
  return { ...object, kind: kindToWire(object.kind) };
}

/** Material new brushes and meshes get: the 512x512 grey dev texture. */
export const DEFAULT_MATERIAL = "world/dev/dev_gray_512";

/** The shipped dev material set, as maps name them. */
export const DEV_MATERIALS: readonly string[] = [
  "world/dev/dev_gray_512",
  "world/dev/dev_white_512",
  "world/dev/dev_white_512_v_color",
  "world/dev/dev_blue_512",
  "world/dev/dev_brown_512",
  "world/dev/dev_green_512",
  "world/dev/dev_ground_512",
  "world/dev/dev_orange_512",
  "world/dev/dev_red_512",
  "world/dev/dev_yellow_512",
  "world/dev/dev_blue_light_01",
  "world/dev/dev_blue_orange_01",
  "world/dev/dev_green_light",
  "world/dev/dev_red_light_01",
  "world/dev/dev_white_light",
  "world/dev/dev_yellow_light_01",
  "world/dev/dev_light_vertex_color",
  "models/world/dev/dev_gray_512",
  "models/world/dev/test_pattern_2x2",
];
