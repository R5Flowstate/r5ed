import { DEFAULT_MATERIAL } from "../doc/model";
import type { Id, MeshBuffers, Vec3 } from "../core/api";
import { type Mat4, fromRotationTranslationScale, mat4, multiply } from "../math/mat4";
import { type Aabb, isEmptyAabb } from "../math/vec3";
import type { DocObject, Transform } from "../doc/model";
import { type Bvh, buildBvh, refitBvh } from "./bvh";
import {
  type BillboardInstance,
  type BoxInstance,
  LineBuilder,
  type LineMesh,
  boundsOf,
  boxWireframe,
  lightGizmo,
  orientedBoxWireframe,
  HOVER_COLOR,
  pathGizmo,
  SELECTION_COLOR,
  transformedBounds,
  volumeBox,
} from "./overlay";
import { buildGridLines, defaultGrid, type GridMesh, type GridSettings } from "./grid";
import { type GlyphAtlas, billboardSize } from "./atlas";
import { type PackedLightmap, lightmapRect } from "./lightmap";
import { sortNearest, takeUntilBudget, WIRE_COLOR, WIRE_LINE_BUDGET } from "./shading";
import { entityCategory, lightColor } from "./material";
import { glyphRect } from "./glyphs";
import { gizmoLines, type GizmoHandle, type GizmoMode, type GizmoSpace } from "./gizmo";
import type { RefPropLayer } from "./refprops";

/** Interleaved vertex: position(3) normal(3) uv(2). */
export const VERTEX_FLOATS = 8;

/** One material-keyed index range inside a chunk. */
export interface SubRange {
  start: number;
  count: number;
  material: string;
}

export interface GeometryChunk {
  key: string;
  vertices: Float32Array;
  indices: Uint32Array;
  /** Local-space bounds before the instance transform. */
  bounds: Aabb;
  /** One entry per material; a single-material chunk has exactly one. */
  subRanges: SubRange[];
}

export interface SceneInstance {
  id: Id;
  /** 1-based id written to the pick target; 0 means not selectable. */
  pick: number;
  chunk: number;
  /** Material keys this instance can draw with, in sub-range order. */
  materials: string[];
  transform: Mat4;
  /** World-space bounds, used for culling and the selection outline. */
  bounds: Aabb;
  selectable: boolean;
  /** UV rect of this object inside the packed lightmap atlas, or null. */
  lightmap: [number, number, number, number] | null;
  /** Drawn as wire only (a boolean cutter), never as a solid. */
  wireOnly?: boolean;
  /** Places the object-space wire when the geometry arrives already in world space. */
  wireTransform?: Mat4;
}

export interface OverlayScene {
  lines: LineMesh;
  boxes: BoxInstance[];
  billboards: BillboardInstance[];
}

/** Object-space wireframe edges, as the core's `objectEdges` draws them. */
export interface WireTable {
  positions: Float32Array;
  edges: Uint32Array;
}

export interface Scene {
  chunks: GeometryChunk[];
  instances: SceneInstance[];
  byId: Map<Id, number[]>;
  bvh: Bvh;
  bvhBoxes: Aabb[];
  grid: GridMesh;
  overlay: OverlayScene;
  /** One world-space line buffer per drawn chunk, under the global budget. */
  wire: LineMesh[];
  version: number;
  /** The reference map's static props, drawn from its table; null when there is none. */
  refs: RefPropLayer | null;
  /** The sky vista's geometry, drawn behind the world around the eye; null when there is none. */
  sky: { chunk: number } | null;
}

/**
 * The 3D skybox as the game draws it: the sky camera stands this far above
 * the vista's origin, the view's offset from it is the eye divided by the
 * scale, and the compiler turns the vista a quarter about z.
 */
export const SKY_CAMERA_ABOVE_VISTA = 152;
export const SKY_SCALE = 1000;
export const SKY_VISTA_YAW_DEGREES = 90;

/** Where the vista's model is drawn for an eye: its world transform. */
export function skyTransform(eye: Vec3): Mat4 {
  const a = (SKY_VISTA_YAW_DEGREES * Math.PI) / 180;
  const c = Math.cos(a);
  const s = Math.sin(a);
  const k = 1 - 1 / SKY_SCALE;
  return new Float32Array([c, s, 0, 0, -s, c, 0, 0, 0, 0, 1, 0, eye[0] * k, eye[1] * k, eye[2] * k - SKY_CAMERA_ABOVE_VISTA, 1]);
}

/** One material-keyed piece of a glb, in the mesh's local space. */
export interface ModelPart {
  positions: Float32Array;
  normals: Float32Array;
  uvs: Float32Array;
  indices: Uint32Array;
  material: string;
}

export interface ModelMesh {
  parts: ModelPart[];
  bounds: Aabb;
}

/** Where the scene builder gets geometry that lives outside the document. */
export interface SceneSource {
  /** World-space triangulated geometry of a brush, volume, path or terrain. */
  objectMesh(id: Id): MeshBuffers | null;
  /** Object-space wireframe edges of a brush, volume or mesh, or null. */
  objectEdges(id: Id): WireTable | null;
  /** Object-space wireframe edges of an asset model path, or null. */
  modelEdges(path: string): WireTable | null;
  /** Cached glb geometry for a project or game model path, or null. */
  model(path: string): ModelMesh | null;
  /**
   * World-space bounds of an object for overlay purposes, independent of
   * whether it has render geometry. Null when the object has no extent.
   */
  bounds(object: DocObject, world: Mat4): Aabb | null;
}

export interface SceneObject {
  doc: DocObject;
  world: Mat4;
  /** World-space bounds when the object has any, else null. */
  worldBounds: Aabb | null;
  /** Layer visibility and object hidden/locked flags combined. */
  hidden: boolean;
  /** Ancestors' transforms, outermost first, for bounds a source cannot give. */
  ancestors: readonly Transform[];
}

function joinLines(a: LineMesh, b: LineMesh): LineMesh {
  if (a.count === 0) return b;
  if (b.count === 0) return a;
  const positions = new Float32Array(a.positions.length + b.positions.length);
  positions.set(a.positions);
  positions.set(b.positions, a.positions.length);
  const colors = new Float32Array(a.colors.length + b.colors.length);
  colors.set(a.colors);
  colors.set(b.colors, a.colors.length);
  return { positions, colors, count: a.count + b.count };
}

/**
 * Ids every reference object in the scene lists as its own records, and the
 * static prop records among them: an object draws those, or they were deleted.
 */
function referencedIds(objects: readonly SceneObject[]): { ids: Set<Id>; props: Set<number> } {
  const ids = new Set<Id>();
  const props = new Set<number>();
  for (const o of objects) {
    if (o.doc.kind.type !== "reference") continue;
    for (const [id, record] of Object.entries(o.doc.kind.reference.sources ?? {})) {
      ids.add(id);
      if (record.type === "prop") props.add(record.index);
    }
  }
  return { ids, props };
}

/** True when every edge pair indexes a finite position. */
function saneWire(wire: WireTable | null): wire is WireTable {
  if (!wire || wire.edges.length === 0 || wire.edges.length % 2 !== 0) return false;
  for (let i = 0; i < wire.edges.length; i++) {
    const v = wire.edges[i] * 3;
    if (v + 2 >= wire.positions.length) return false;
    if (!Number.isFinite(wire.positions[v]) || !Number.isFinite(wire.positions[v + 1]) || !Number.isFinite(wire.positions[v + 2])) {
      return false;
    }
  }
  return true;
}

/**
 * Expands one object's wire edges under a column-major transform into the
 * line buffers, returning the next segment offset.
 */
function emitWire(
  wire: WireTable,
  m: ArrayLike<number>,
  positions: Float32Array,
  colors: Float32Array,
  at: number,
): number {
  const segs = wire.edges.length / 2;
  for (let s = 0; s < segs; s++) {
    for (let e = 0; e < 2; e++) {
      const v = wire.edges[s * 2 + e] * 3;
      const x = wire.positions[v];
      const y = wire.positions[v + 1];
      const z = wire.positions[v + 2];
      const o = (at + s) * 6 + e * 3;
      positions[o] = m[0] * x + m[4] * y + m[8] * z + m[12];
      positions[o + 1] = m[1] * x + m[5] * y + m[9] * z + m[13];
      positions[o + 2] = m[2] * x + m[6] * y + m[10] * z + m[14];
      const c = (at + s) * 8 + e * 4;
      colors[c] = WIRE_COLOR[0];
      colors[c + 1] = WIRE_COLOR[1];
      colors[c + 2] = WIRE_COLOR[2];
      colors[c + 3] = WIRE_COLOR[3];
    }
  }
  return at + segs;
}

/** The edit handles of the vertex, edge and face tools. */
const MARKER_COLOR: readonly [number, number, number, number] = [1, 0.85, 0.2, 1];

export interface SceneOptions {
  showGrid: boolean;
  showLightGizmos: boolean;
  showEntityBillboards: boolean;
  showVolumes: boolean;
  showPaths: boolean;
  showOutlines: boolean;
  grid: GridSettings;
  atlas: GlyphAtlas | null;
  /** Packed lightmap atlas, or null when the preview is off. */
  lightmaps: PackedLightmap | null;
  /** Ids whose core geometry must be rebuilt even if nothing else changed. */
  invalidate: ReadonlySet<Id>;
  selection: ReadonlySet<Id>;
  hover: Id | null;
  /** World scale applied to a billboard so it keeps a constant screen size. */
  billboardScale: number;
  /**
   * The edit handles of the vertex, edge and face tools: small boxes at the
   * corners and lines over the edges, drawn over the geometry.
   */
  markers: { points: Vec3[]; edges: [Vec3, Vec3][]; size: number } | null;
  /** The mesh edit-mode overlay, already coloured; drawn over the geometry. */
  editOverlay: LineMesh | null;
  /** The transform gizmo to draw, or null when there is nothing selected. */
  gizmo: {
    mode: GizmoMode;
    space: GizmoSpace;
    pivot: Vec3;
    rotation: [number, number, number, number];
    /** Handle length in world units, kept constant on screen by the viewport. */
    size: number;
    active: GizmoHandle | null;
  } | null;
  /**
   * Wireframe build: the eye the distance LOD sorts from, the global segment
   * budget, and the one object to draw when set (the solid-mode wire overlay
   * on the edited object). Null outside wireframe/overlay drawing.
   */
  wire: { eye: Vec3; budget: number; only: Id | null } | null;
}

export function defaultSceneOptions(): SceneOptions {
  return {
    showGrid: true,
    showLightGizmos: true,
    showEntityBillboards: true,
    showVolumes: true,
    showPaths: true,
    showOutlines: true,
    grid: defaultGrid(),
    atlas: null,
    lightmaps: null,
    invalidate: new Set(),
    selection: new Set(),
    hover: null,
    markers: null,
    editOverlay: null,
    billboardScale: 64,
    gizmo: null,
    wire: null,
  };
}

const TRANSLUCENT_HINTS = ["glass", "water", "trigger", "lightning", "energy", "portals", "grate"];

/** Materials the host reported as translucent (no depth write). */
const translucentMaterials = new Set<string>();

export function markTranslucent(material: string): void {
  translucentMaterials.add(material);
}

/** Tool materials (sky shell, clip, nodraw) mark geometry the game never draws. */
export function isHiddenMaterial(material: string): boolean {
  return /^tools[\\/]/i.test(material);
}

/** Whether a material is drawn in the translucent pass. */
export function isTranslucentMaterial(material: string): boolean {
  if (translucentMaterials.has(material)) return true;
  const lower = material.toLowerCase();
  return TRANSLUCENT_HINTS.some((hint) => lower.includes(hint));
}

/** Interleaves a mesh's attributes into the shared vertex layout. */
function interleave(positions: Float32Array, normals: Float32Array | null, uvs: Float32Array | null): Float32Array {
  const vertexCount = Math.floor(positions.length / 3);
  const vertices = new Float32Array(vertexCount * VERTEX_FLOATS);
  for (let i = 0; i < vertexCount; i++) {
    const o = i * VERTEX_FLOATS;
    vertices[o] = positions[i * 3];
    vertices[o + 1] = positions[i * 3 + 1];
    vertices[o + 2] = positions[i * 3 + 2];
    vertices[o + 3] = normals ? normals[i * 3] : 0;
    vertices[o + 4] = normals ? normals[i * 3 + 1] : 0;
    vertices[o + 5] = normals ? normals[i * 3 + 2] : 1;
    vertices[o + 6] = uvs ? uvs[i * 2] : 0;
    vertices[o + 7] = uvs ? uvs[i * 2 + 1] : 0;
  }
  return vertices;
}

function indicesFrom(source: ArrayLike<number>, vertexCount: number): Uint32Array {
  const count = Math.min(source.length, vertexCount * 3);
  const out = new Uint32Array(count);
  for (let i = 0; i < count; i++) {
    const v = source[i];
    out[i] = Number.isInteger(v) && v >= 0 && v < vertexCount ? v : 0;
  }
  return out;
}

function coreChunk(key: string, mesh: MeshBuffers): GeometryChunk {
  const vertices = interleave(mesh.positions, mesh.normals, mesh.uvs);
  const vertexCount = vertices.length / VERTEX_FLOATS;
  const indices = indicesFrom(mesh.indices, vertexCount);
  return {
    key,
    vertices,
    indices,
    bounds: boundsOf(vertices, VERTEX_FLOATS),
    subRanges: mesh.ranges.length
      ? mesh.ranges
          .filter((r) => r.count > 0 && r.start >= 0 && r.start + r.count <= indices.length)
          .map((r) => ({ start: r.start, count: r.count, material: r.material || DEFAULT_MATERIAL }))
      : [{ start: 0, count: indices.length, material: DEFAULT_MATERIAL }],
  };
}

function modelChunk(key: string, model: ModelMesh): GeometryChunk | null {
  const parts = model.parts.filter((p) => p.indices.length >= 3);
  if (parts.length === 0) return null;
  let vertexTotal = 0;
  let indexTotal = 0;
  for (const part of parts) {
    vertexTotal += Math.floor(part.positions.length / 3);
    indexTotal += part.indices.length;
  }
  if (vertexTotal === 0) return null;
  const vertices = new Float32Array(vertexTotal * VERTEX_FLOATS);
  const indices = new Uint32Array(indexTotal);
  const subRanges: SubRange[] = [];
  let vOff = 0;
  let iOff = 0;
  for (const part of parts) {
    const piece = interleave(part.positions, part.normals, part.uvs);
    vertices.set(piece, vOff * VERTEX_FLOATS);
    const local = indicesFrom(part.indices, Math.floor(part.positions.length / 3));
    for (let i = 0; i < local.length; i++) indices[iOff + i] = local[i] + vOff;
    subRanges.push({ start: iOff, count: local.length, material: part.material || DEFAULT_MATERIAL });
    vOff += piece.length / VERTEX_FLOATS;
    iOff += local.length;
  }
  return { key, vertices, indices, bounds: model.bounds, subRanges };
}

/** Composes an object's own transform with its ancestors', outermost first. */
export function composeWorld(local: Transform, ancestors: readonly Transform[]): Mat4 {
  const out = mat4();
  fromRotationTranslationScale(out, local.rotation, local.position, local.scale);
  for (const ancestor of ancestors) {
    const outer = mat4();
    fromRotationTranslationScale(outer, ancestor.rotation, ancestor.position, ancestor.scale);
    // The parent is applied last, so its scale does reach the child's offset.
    multiply(out, outer, out);
  }
  return out;
}

export function worldTranslation(m: Mat4): Vec3 {
  return [m[12], m[13], m[14]];
}

export function transformPointBy(m: Mat4, p: Vec3): Vec3 {
  return [
    m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
    m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
    m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
  ];
}

/** The world direction of a local +Z axis, for lights and gizmos. */
export function localZDirection(m: Mat4): Vec3 {
  const v: Vec3 = [m[8], m[9], m[10]];
  const len = Math.hypot(v[0], v[1], v[2]);
  return len > 1e-9 ? [v[0] / len, v[1] / len, v[2] / len] : [0, 0, -1];
}

/** The way a light shines: its local -Z, so an unrotated sun points down. */
export function lightForward(m: Mat4): Vec3 {
  const z = localZDirection(m);
  return [-z[0], -z[1], -z[2]];
}

/** World bounds of a local box under a transform. */
const IDENTITY: Mat4 = mat4();

/** A mesh whose source is the document's own `.r5mesh`, not an asset model. */
export function isEditableMesh(object: DocObject): boolean {
  return object.kind.type === "mesh" && object.kind.mesh.source.endsWith(".r5mesh");
}

export function instanceBounds(local: Aabb, world: Mat4): Aabb {
  return transformedBounds(local, world);
}

const RENDERABLE = new Set(["brush", "mesh", "prop", "terrain", "voxel_terrain"]);

/**
 * Assembles the draw list for a frame. Geometry is cached by key across frames
 * so a prop model is only uploaded once, however many instances reference it.
 */
export class SceneBuilder {
  private source: SceneSource;
  private chunks: GeometryChunk[] = [];
  private lookup = new Map<string, number>();
  private version = 0;
  /** Object ids whose cached core geometry must be rebuilt on the next build. */
  private stale = new Set<Id>();
  /** Editable meshes with a modifier stack: a boolean or curve reads other objects. */
  private stacked = new Set<Id>();
  private referenced: { objects: readonly SceneObject[] | null; ids: Set<Id>; props: Set<number> } = { objects: null, ids: new Set(), props: new Set() };
  private refs: RefPropLayer | null = null;
  private skyModel: string | null = null;
  private lastObjects: readonly SceneObject[] | null = null;
  private refDocs: DocObject[] = [];
  private overlayDocs: DocObject[] = [];
  private overlayHidden: boolean[] = [];
  /** Object-space wire edges per chunk key; null when the chunk draws no lines. */
  private wireLocal = new Map<string, WireTable | null>();
  /** Last budgeted wire buffers, keyed by a quantized eye plus the scene state. */
  private wireBuilt: { key: string; instances: SceneInstance[]; mesh: LineMesh[] } | null = null;
  private statics: {
    objects: readonly SceneObject[];
    lights: boolean;
    paths: boolean;
    atlas: GlyphAtlas | null;
    volumes: boolean;
    lines: LineMesh;
    boxes: BoxInstance[];
    markers: { center: Vec3; color: [number, number, number, number]; rect: BillboardInstance["rect"] }[];
  } | null = null;
  private last: {
    objects: readonly SceneObject[];
    chunkCount: number;
    lightmaps: PackedLightmap | null;
    instances: SceneInstance[];
    byId: Map<Id, number[]>;
    bvhBoxes: Aabb[];
    bvh: Scene["bvh"];
  } | null = null;

  constructor(source: SceneSource) {
    this.source = source;
  }

  /** Marks ids whose core geometry changed after an edit. */
  invalidate(ids: Iterable<Id>): void {
    for (const id of ids) this.stale.add(id);
    for (const id of this.stacked) this.stale.add(id);
    this.version++;
  }

  private cached(key: string, build: () => GeometryChunk | null): number | null {
    const hit = this.lookup.get(key);
    if (hit !== undefined) return hit;
    const made = build();
    if (!made || made.indices.length === 0) return null;
    const index = this.chunks.length;
    this.chunks.push(made);
    this.lookup.set(key, index);
    return index;
  }

  private chunkFor(object: DocObject): number | null {
    const kind = object.kind;
    // An editable mesh draws through the core, which runs its modifier stack.
    const editable = isEditableMesh(object);
    if (editable && kind.type === "mesh" && (kind.mesh.modifiers?.length ?? 0) > 0) this.stacked.add(object.id);
    else this.stacked.delete(object.id);
    if (editable || kind.type === "brush" || kind.type === "volume" || kind.type === "path" || kind.type === "terrain") {
      // The key is per object; a stale id drops it so the mesh is re-read.
      const key = `core:${object.id}`;
      if (this.stale.has(object.id)) {
        this.stale.delete(object.id);
        this.lookup.delete(key);
        this.wireLocal.delete(key);
      }
      const index = this.cached(key, () => {
        const mesh = this.source.objectMesh(object.id);
        if (!mesh || mesh.indices.length === 0) return null;
        return coreChunk(key, mesh);
      });
      this.fetchWire(key, () => this.source.objectEdges(object.id));
      return index;
    }
    if (kind.type === "mesh") {
      const key = `glb:mesh:${kind.mesh.source}`;
      const index = this.cached(key, () => this.loadModel(kind.mesh.source, key));
      this.fetchWire(key, () => this.source.objectEdges(object.id));
      return index;
    }
    if (kind.type === "prop") {
      const key = `glb:prop:${kind.prop.model}`;
      const index = this.cached(key, () => this.loadModel(kind.prop.model, key));
      this.fetchWire(key, () => this.source.modelEdges(kind.prop.model));
      return index;
    }
    return null;
  }

  /** Caches one chunk's object-space wire edges, tolerating a missing core. */
  private fetchWire(key: string, load: () => WireTable | null): void {
    if (this.wireLocal.has(key)) return;
    let wire: WireTable | null = null;
    try {
      wire = load();
    } catch {
      wire = null;
    }
    this.wireLocal.set(key, saneWire(wire) ? wire : null);
  }

  /** Object-space wire edges of an asset model, cached per path. */
  private wireForModel(path: string): WireTable | null {
    const key = `glb:prop:${path}`;
    if (!this.wireLocal.has(key)) this.fetchWire(key, () => this.source.modelEdges(path));
    return this.wireLocal.get(key) ?? null;
  }

  /** `key` names the chunk the way its wire edges are cached, so wire lookups find them. */
  private loadModel(path: string, key: string): GeometryChunk | null {
    const model = this.source.model(path);
    if (!model) return null;
    return modelChunk(key, model);
  }

  /** Draws an imported map's static props from its table, or stops with null. */
  setRefProps(layer: RefPropLayer | null): void {
    this.refs = layer;
    layer?.setHidden(this.referenced.props);
    this.version++;
  }

  get refProps(): RefPropLayer | null {
    return this.refs;
  }

  private skyChunk(): Scene["sky"] {
    const path = this.skyModel;
    if (!path) return null;
    const chunk = this.cached(`glb:prop:${path}`, () => this.loadModel(path, `glb:prop:${path}`));
    return chunk === null ? null : { chunk };
  }

  /** The map's sky vista model, or null for none. */
  setSky(model: string | null): void {
    const next = model && model.trim() ? model.trim() : null;
    if (next === this.skyModel) return;
    this.skyModel = next;
    this.version++;
  }

  /** Chunk count; used by tests and by the asset panel's cache readout. */
  get chunkCount(): number {
    return this.chunks.length;
  }

  /** Drops cached geometry for ids the document no longer contains. */
  prune(live: ReadonlySet<Id>): void {
    for (const id of [...this.stale]) if (!live.has(id)) this.stale.delete(id);
    for (const id of [...this.stacked]) if (!live.has(id)) this.stacked.delete(id);
  }

  /**
   * World-space wireframe buffers, one per drawn chunk: authored instances by
   * bounds distance, reference props by origin, nearest first under the global
   * budget. Whole items only, so an object never draws half its edges. The
   * result is cached by quantized eye, so a still camera reuses it.
   */
  private buildWire(
    instances: SceneInstance[],
    spec: { eye: Vec3; budget: number; only: Id | null } | null,
    refsVersion: number,
  ): LineMesh[] {
    // Cutters draw as wire in every shading mode; outside wireframe drawing
    // they are the only wire built.
    const cuttersOnly = !spec;
    if (!spec) {
      if (!instances.some((i) => i.wireOnly)) return [];
      spec = { eye: [0, 0, 0], budget: WIRE_LINE_BUDGET, only: null };
    }
    if (spec.budget <= 0) return [];
    const eye = spec.eye;
    const q = (v: number): number => Math.round(v / 128);
    const key = `${q(eye[0])},${q(eye[1])},${q(eye[2])}|${spec.budget}|${this.chunks.length}|${this.wireLocal.size}|${refsVersion}|${instances.length}|${spec.only ?? ""}|${cuttersOnly}`;
    if (this.wireBuilt && this.wireBuilt.key === key && this.wireBuilt.instances === instances) return this.wireBuilt.mesh;
    const dists: number[] = [];
    const counts: number[] = [];
    const itemChunk: number[] = [];
    const kinds: ({ type: "instance"; index: number } | { type: "prop"; record: number })[] = [];
    for (let i = 0; i < instances.length; i++) {
      const inst = instances[i];
      if (spec.only !== null && inst.id !== spec.only && !inst.wireOnly) continue;
      if (cuttersOnly && !inst.wireOnly) continue;
      const wire = this.wireLocal.get(this.chunks[inst.chunk]?.key ?? "");
      const segs = wire ? wire.edges.length / 2 : 0;
      if (segs === 0) continue;
      const b = inst.bounds;
      const cx = (b.min[0] + b.max[0]) / 2 - eye[0];
      const cy = (b.min[1] + b.max[1]) / 2 - eye[1];
      const cz = (b.min[2] + b.max[2]) / 2 - eye[2];
      dists.push(Math.hypot(cx, cy, cz));
      counts.push(segs);
      itemChunk.push(inst.chunk);
      kinds.push({ type: "instance", index: i });
    }
    // Reference props belong to the map, never to the edited object.
    const layer = spec.only === null && !cuttersOnly ? this.refs : null;
    if (layer) {
      const t = layer.table;
      for (let r = 0; r < t.count; r++) {
        if (layer.isHidden(r)) continue;
        const path = t.models[t.model[r]] ?? "";
        const wire = this.wireForModel(path);
        const segs = wire ? wire.edges.length / 2 : 0;
        if (segs === 0) continue;
        const chunk = this.lookup.get(`glb:prop:${path}`) ?? -1;
        if (chunk < 0) continue;
        const dx = t.origin[r * 3] - eye[0];
        const dy = t.origin[r * 3 + 1] - eye[1];
        const dz = t.origin[r * 3 + 2] - eye[2];
        dists.push(Math.hypot(dx, dy, dz));
        counts.push(segs);
        itemChunk.push(chunk);
        kinds.push({ type: "prop", record: r });
      }
    }
    const order = sortNearest(dists);
    const fitting = takeUntilBudget(order.map((i) => counts[i]), spec.budget);
    // One line buffer per chunk, nearest chunk first.
    const groups = new Map<number, typeof kinds>();
    for (let k = 0; k < fitting; k++) {
      const taken = order[k];
      const list = groups.get(itemChunk[taken]);
      if (list) list.push(kinds[taken]);
      else groups.set(itemChunk[taken], [kinds[taken]]);
    }
    const out: LineMesh[] = [];
    for (const items of groups.values()) {
      let total = 0;
      for (const item of items) {
        total += item.type === "instance"
          ? (this.wireLocal.get(this.chunks[instances[item.index].chunk]?.key ?? "")?.edges.length ?? 0) / 2
          : (this.wireForModel(layer!.table.models[layer!.table.model[item.record]] ?? "")?.edges.length ?? 0) / 2;
      }
      const positions = new Float32Array(total * 6);
      const colors = new Float32Array(total * 8);
      let at = 0;
      for (const item of items) {
        if (item.type === "instance") {
          const inst = instances[item.index];
          const wire = this.wireLocal.get(this.chunks[inst.chunk]?.key ?? "");
          if (wire) at = emitWire(wire, inst.wireTransform ?? inst.transform, positions, colors, at);
        } else {
          const w = this.wireForModel(layer!.table.models[layer!.table.model[item.record]] ?? "");
          if (w) {
            const s = layer!.slotOf[item.record] * 24;
            at = emitWire(w, layer!.instances.subarray(s, s + 16), positions, colors, at);
          }
        }
      }
      out.push({ positions, colors, count: total * 2 });
    }
    this.wireBuilt = { key, instances, mesh: out };
    return out;
  }

  build(objects: readonly SceneObject[], options: SceneOptions): Scene {
    // Instances and their BVH depend on the objects, the loaded geometry and
    // the lightmaps; a frame that only moved the camera reuses them.
    const reuse =
      this.last !== null &&
      this.last.objects === objects &&
      this.last.chunkCount === this.chunks.length &&
      this.last.lightmaps === options.lightmaps &&
      this.stale.size === 0 &&
      options.invalidate.size === 0;
    const instances: SceneInstance[] = reuse ? this.last!.instances : [];
    const byId = reuse ? this.last!.byId : new Map<Id, number[]>();
    const bvhBoxes: Aabb[] = reuse ? this.last!.bvhBoxes : [];
    const lines = new LineBuilder();
    const boxes: BoxInstance[] = [];
    const billboards: BillboardInstance[] = [];
    for (const id of options.invalidate) this.stale.add(id);
    let pick = 1;
    // A decompiled object the shipped map still supplies is flagged hidden so
    // the build leaves it to the map; the viewport still draws it.
    // Overlays and the referenced set depend only on a few kinds of object; an
    // edit to anything else keeps them.
    const overlayDocs: DocObject[] = [];
    const overlayHidden: boolean[] = [];
    const refDocs: DocObject[] = [];
    if (this.lastObjects !== objects) {
      for (const o of objects) {
        const t = o.doc.kind.type;
        if (t === "reference") refDocs.push(o.doc);
        if (t === "light" || t === "path" || t === "entity" || t === "volume" || t === "reference") {
          overlayDocs.push(o.doc);
          overlayHidden.push(o.hidden);
        }
      }
    }
    const sameList = (a: readonly DocObject[], b: readonly DocObject[]): boolean => a.length === b.length && a.every((d, i) => d === b[i]);
    if (this.lastObjects !== objects) {
      if (!sameList(refDocs, this.refDocs)) {
        this.referenced = { objects, ...referencedIds(objects) };
        this.refs?.setHidden(this.referenced.props);
      }
      const sameHidden = overlayHidden.length === this.overlayHidden.length && overlayHidden.every((h, i) => h === this.overlayHidden[i]);
      if (this.statics && sameHidden && sameList(overlayDocs, this.overlayDocs)) this.statics.objects = objects;
      this.overlayHidden = overlayHidden;
      this.refDocs = refDocs;
      this.overlayDocs = overlayDocs;
      this.lastObjects = objects;
    }
    const referenced = this.referenced.ids;
    // Light and path gizmos and entity markers only change with the objects.
    const keepStatic =
      this.statics !== null &&
      this.statics.objects === objects &&
      this.statics.lights === options.showLightGizmos &&
      this.statics.paths === options.showPaths &&
      this.statics.atlas === (options.showEntityBillboards ? options.atlas : null) &&
      this.statics.volumes === options.showVolumes;
    const staticLines = keepStatic ? null : new LineBuilder();
    const markers: { center: Vec3; color: [number, number, number, number]; rect: BillboardInstance["rect"] }[] = keepStatic ? this.statics!.markers : [];
    const drawn = (o: SceneObject): boolean => !o.hidden && (!o.doc.hidden || referenced.has(o.doc.id));

    const volumeBoxes: BoxInstance[] = keepStatic ? this.statics!.boxes : [];
    // Nothing the object walk produces can have changed: skip it.
    const walk = !(reuse && keepStatic);
    for (const object of walk ? objects : []) {
      const doc = object.doc;
      if (!drawn(object)) continue;
      const kind = doc.kind;
      if (!reuse && RENDERABLE.has(kind.type)) {
        const chunk = this.chunkFor(doc);
        if (chunk !== null) {
          const local = this.chunks[chunk].bounds;
          // The core hands editable meshes over already in world space.
          const transform = isEditableMesh(doc) ? IDENTITY : object.world;
          const world = object.worldBounds ?? instanceBounds(local, transform);
          bvhBoxes.push(world);
          const at = instances.length;
          instances.push({
            id: doc.id,
            pick,
            chunk,
            materials: this.chunks[chunk].subRanges.map((r) => r.material),
            transform,
            bounds: world,
            selectable: !doc.locked,
            lightmap: lightmapRect(options.lightmaps, doc.id),
            wireOnly: kind.type === "mesh" && kind.mesh.role === "cutter",
            wireTransform: isEditableMesh(doc) ? object.world : undefined,
          });
          pick++;
          const list = byId.get(doc.id);
          if (list) list.push(at);
          else byId.set(doc.id, [at]);
        }
      }
      switch (kind.type) {
        case "volume":
          if (staticLines && options.showVolumes && object.worldBounds) volumeBoxes.push(volumeBox(object.worldBounds, kind.volume.kind));
          break;
        case "light":
          if (staticLines && options.showLightGizmos) {
            lightGizmo(
              staticLines,
              worldTranslation(object.world),
              kind.light.kind,
              kind.light.range || 256,
              lightColor(kind.light.kind),
              lightForward(object.world),
              kind.light.outer_angle || 45,
            );
          }
          break;
        case "path":
          if (staticLines && options.showPaths) {
            const points = kind.path.points.map((p) => transformPointBy(object.world, p));
            const color: [number, number, number, number] = kind.path.kind === "spline" ? [0.5, 0.8, 1, 1] : [0.95, 0.6, 0.95, 1];
            pathGizmo(staticLines, points, color);
          }
          break;
        case "entity":
          if (staticLines && options.showEntityBillboards && options.atlas) {
            const category = entityCategory(kind.entity.classname);
            markers.push({
              center: worldTranslation(object.world),
              color: [...BILLBOARD_TINTS[category], 0.92],
              rect: glyphRect(options.atlas, category),
            });
          }
          break;
        default:
          break;
      }
    }

    if (options.showOutlines) {
      for (const object of objects) {
        if (!drawn(object)) continue;
        const selected = options.selection.has(object.doc.id);
        const hovered = options.hover === object.doc.id;
        if (!selected && !hovered) continue;
        const color = selected ? SELECTION_COLOR : HOVER_COLOR;
        // Drawn geometry gets its own box turned with it; the rest, the box
        // the object reports.
        const drawnAt = byId.get(object.doc.id);
        if (drawnAt && drawnAt.length > 0) {
          for (const i of drawnAt) {
            const inst = instances[i];
            orientedBoxWireframe(this.chunks[inst.chunk].bounds, inst.transform, color, lines);
          }
          continue;
        }
        const b = object.worldBounds;
        if (!b || isEmptyAabb(b)) continue;
        boxWireframe(b, color, lines);
      }
    }

    if (options.markers) {
      const h = Math.max(0.5, options.markers.size);
      for (const p of options.markers.points) {
        boxWireframe({ min: [p[0] - h, p[1] - h, p[2] - h], max: [p[0] + h, p[1] + h, p[2] + h] }, MARKER_COLOR, lines);
      }
      for (const [a, b] of options.markers.edges) lines.segment(a, b, MARKER_COLOR);
    }

    if (options.editOverlay && options.editOverlay.count > 0) lines.append(options.editOverlay);

    if (options.gizmo) {
      gizmoLines(
        lines,
        options.gizmo.pivot,
        options.gizmo.rotation,
        options.gizmo.mode,
        options.gizmo.space,
        options.gizmo.size,
        options.gizmo.active,
      );
    }

    const staticMesh = staticLines ? staticLines.build() : this.statics!.lines;
    if (staticLines) {
      this.statics = {
        objects,
        lights: options.showLightGizmos,
        paths: options.showPaths,
        atlas: options.showEntityBillboards ? options.atlas : null,
        volumes: options.showVolumes,
        lines: staticMesh,
        markers,
        boxes: volumeBoxes,
      };
    }
    boxes.push(...volumeBoxes);
    const size = billboardSize(1, 1) * options.billboardScale;
    for (const m of markers) billboards.push({ center: m.center, size, color: m.color, rect: m.rect });
    // An edit that moved objects but kept the instance list refits the tree.
    const sameInstances =
      !reuse &&
      this.last !== null &&
      this.last.instances.length === instances.length &&
      instances.every((inst, i) => inst.id === this.last!.instances[i].id && inst.chunk === this.last!.instances[i].chunk);
    const bvh = reuse ? this.last!.bvh : sameInstances ? refitBvh(this.last!.bvh, bvhBoxes) : buildBvh(bvhBoxes);
    // Prop models arrive over time; each one that has joins the reference layer.
    this.refs?.bindModels((path) => this.cached(`glb:prop:${path}`, () => this.loadModel(path, `glb:prop:${path}`)), this.chunks);
    // Chunks the layer just added are its own; the document's instances need no rebuild for them.
    this.last = { objects, chunkCount: this.chunks.length, lightmaps: options.lightmaps, instances, byId, bvhBoxes, bvh };
    return {
      chunks: this.chunks,
      instances,
      byId,
      bvh,
      bvhBoxes,
      grid: options.showGrid ? buildGridLines(options.grid) : { positions: new Float32Array(0), weight: new Float32Array(0), count: 0 },
      overlay: { lines: joinLines(staticMesh, lines.build()), boxes, billboards },
      wire: this.buildWire(instances, options.wire, this.refs?.version ?? -1),
      version: ++this.version,
      refs: this.refs,
      sky: this.skyChunk(),
    };
  }
}

const BILLBOARD_TINTS: Record<string, [number, number, number]> = {
  trigger: [0.95, 0.55, 0.3],
  info_target: [0.5, 0.8, 0.95],
  light: [0.98, 0.9, 0.55],
  env: [0.6, 0.9, 0.5],
  func: [0.7, 0.7, 0.95],
  prop: [0.8, 0.8, 0.82],
  path: [0.9, 0.6, 0.9],
  player: [0.55, 0.9, 0.85],
  weapon: [0.85, 0.65, 0.6],
  vehicle: [0.75, 0.75, 0.65],
  npc: [0.85, 0.5, 0.65],
  gameplay: [0.65, 0.9, 0.75],
  debug: [0.9, 0.45, 0.45],
  other: [0.85, 0.85, 0.85],
};

/** Object ids whose instance bounds intersect a world-space box. */
export function instancesInBox(scene: Scene, min: Vec3, max: Vec3): SceneInstance[] {
  const out: SceneInstance[] = [];
  for (const instance of scene.instances) {
    const b = instance.bounds;
    if (b.min[0] <= max[0] && b.max[0] >= min[0] && b.min[1] <= max[1] && b.max[1] >= min[1] && b.min[2] <= max[2] && b.max[2] >= min[2]) {
      out.push(instance);
    }
  }
  return out;
}
