import { pickFarthest, pickScene, pickSceneAll } from "../render/scenepick";
import { PROP_FLAG_SOLID, REF_PROP_TABLE, RefPropLayer, parseRefProps, refPropId } from "../render/refprops";
import type { Core, EditBuffers, Id, MeshBuffers, Vec3 } from "../core/api";
import { loadCore } from "../core/load";
import { DocumentStore, parseDoc, type ValidationIssue } from "../doc/store";
import { type Command, replaceObjects, setMeta, setSettings, setFile, batch, removeObjects, addObjects } from "../doc/command";
import {
  DEFAULT_MATERIAL,
  objectToWire,
  referenceRecord,
  type DocObject,
  type Layer,
  type MapDoc,
  type RefRecord,
  type Settings,
  type Transform,
} from "../doc/model";
import { type EntitySchema, emptySchema, parseSchema } from "../doc/schema";
import {
  HostClient,
  type JobKind,
  type JobStatus,
  type MaterialPrefetch,
  type ProjectListEntry,
  type SessionInfo,
  liveUrl,
  readHashParam,
  readToken,
  stripHash,
} from "../host/client";
import { LiveLink, type LiveMessage, type SocketLike, type LiveStatusLabel } from "../host/live";
import { type Aabb, emptyAabb, isEmptyAabb, unionAabb } from "../math/vec3";
import type { Mat4 } from "../math/mat4";
import { type Quat, quatConjugate, quatFromAxisAngle, quatFromSourceAngles, quatMul, quatNormalize, quatRotate, quatSlerp, sourceAnglesFromQuat } from "../math/quat";
import { add, dot, mul, normalize, scale, sub } from "../math/vec3";

const AXIS_UNITS: Vec3[] = [
  [1, 0, 0],
  [0, 1, 0],
  [0, 0, 1],
];
import { centredBox } from "../render/boxdrag";
import { type MeshRequestOptions, type MeshSpec, meshRequestJson } from "../render/meshgen";
import {
  type ModelMesh,
  type ModelPart,
  type Scene,
  type SceneObject,
  type SceneSource,
  SceneBuilder,
  composeWorld,
  defaultSceneOptions,
  instanceBounds,
  isEditableMesh,
  type SceneOptions,
  type WireTable,
} from "../render/scene";
import { boundsOf } from "../render/overlay";
import { editModeShading, type ShadingState } from "../render/shading";
import { type PackedLightmap, type LightmapPreviews, packLightmaps } from "../render/lightmap";
import { parseGltf } from "../render/gltf";
import {
  state,
  setState,
  setDocumentFlags,
  setProjectInfo,
  setShading,
  upsertJob,
  addToSelection,
  clearSelection,
  select,
  setLive,
  setIsolate,
  setHover,
  bumpPanels,
  touchMeshEdit,
} from "./stores";
import type { Keymap } from "../input/keymap";
import { loadKeymap } from "../input/prefs";
import {
  type EditSelMode,
  type LastMeshOp,
  type MeshEditState,
  type MeshOpResult,
  type PickReply,
  opRequest,
  parseOpResult,
  parsePickReply,
  rerunRequest,
  selJson,
} from "./meshEdit";

export class EditorError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "EditorError";
  }
}

const POLL_MS = 400;

/** Jobs that compile, bake or pack the project as it is on disk. */
const JOBS_READING_THE_PROJECT = new Set<JobKind>(["compile", "bake", "package", "navmesh", "export_minimap", "export_loadscreen"]);

/** Where under the build directory the host lets a deploy write (`r5ed_host::deploy::ALLOWED`). */
const DEPLOY_DIRS = ["maps", "vpk", "paks/Win64", "paks/Win64_server", "platform/scripts", "platform/cfg"];

/** The project directories whose files belong to the document. */
const PROJECT_FILE_DIRS = ["meshes", "terrain", "prefabs", "import"];

/** FNV-1a over the bytes: cheap enough to run on every save. */
function fileHash(bytes: Uint8Array): number {
  let h = 0x811c9dc5 ^ bytes.length;
  for (let i = 0; i < bytes.length; i++) h = Math.imul(h ^ bytes[i], 0x01000193);
  return h >>> 0;
}

export interface AssetEntry {
  kind: string;
  path: string;
  guid: string;
}

export interface EditorSourceInfo {
  version: string;
  gameRoot: string;
}

/** The shader suffixes a material is shipped under, in the order to try. */
export const MATERIAL_SHADER_SUFFIXES: readonly string[] = [
  "_wldc",
  "_wldu",
  "_rgdp",
  "_rgdu",
  "_rgdc",
  "_sknp",
  "_sknu",
  "_sknc",
  "_rgbs",
  "_ptcu",
  "_ptcs",
  "",
];

/** The asset browser's kind names, as the type codes the host's index stores. */
export const ASSET_KIND_CODES: Record<string, string> = {
  model: "mdl_",
  material: "matl",
  texture: "txtr",
  map: "wrap",
};

/** The browser kind name of an index type code; an unknown code passes through. */
export function assetKindName(code: string): string {
  if (code === "mdl") return "model";
  return Object.entries(ASSET_KIND_CODES).find(([, c]) => c === code)?.[0] ?? code;
}

/** One material's albedo, ready to upload. */
export interface MaterialAlbedo {
  /** Normalised `0x` hex guid, the form the texture route takes. */
  albedo: string;
  bytes: Uint8Array;
  /** The material writes no depth: the game draws it blended. */
  translucent: boolean;
}

/**
 * A host call that can fail transiently (an export racing another), tried a
 * few times before its failure is kept for the session.
 */
async function retried<T>(call: () => Promise<T>, attempts = 3): Promise<T> {
  for (let i = 1; ; i++) {
    try {
      return await call();
    } catch (e) {
      if (i >= attempts) throw e;
      await new Promise((r) => setTimeout(r, 500 * i));
    }
  }
}

export type MeshOptions = Omit<MeshRequestOptions, "material"> & { material?: string };

/**
 * The albedo texture a material names, as a `0x` hex guid. Slot 0 is the
 * albedo in every material the paks ship (`albedoTexture` or `_col` in
 * `$textureTypes`), and the host writes the value as hex because a JSON number
 * past 2^53 loses precision in a browser.
 */
export function albedoSlot(textures: Record<string, string>): string | null {
  const raw = textures["0"];
  if (typeof raw !== "string") return null;
  const body = raw.replace(/^0[xX]/, "");
  if (body.length === 0 || body.length > 16 || !/^[0-9a-fA-F]+$/.test(body)) return null;
  return `0x${body.padStart(16, "0").toLowerCase()}`;
}

/** The kinds the toolbar can create; a brush goes through the core. */
export type NewObjectKind =
  | "brush" | "mesh" | "prop" | "entity" | "light" | "volume"
  | "path" | "terrain" | "voxel_terrain" | "group";

export const NEW_OBJECT_KINDS: { id: NewObjectKind; label: string }[] = [
  { id: "brush", label: "Brush" },
  { id: "mesh", label: "Mesh" },
  { id: "prop", label: "Prop" },
  { id: "entity", label: "Entity" },
  { id: "light", label: "Light" },
  { id: "volume", label: "Volume" },
  { id: "path", label: "Path" },
  { id: "terrain", label: "Terrain" },
  { id: "voxel_terrain", label: "Voxel terrain" },
  { id: "group", label: "Group" },
];

/** Samples per side of the flat heightfield a new terrain starts with. */
const FLAT_TERRAIN_SAMPLES = 64;

/** Where a material problem came from, so one list can hold both producers. */
export type MaterialWarningSource = "host" | "gpu";

export interface MaterialWarning {
  material: string;
  message: string;
  source: MaterialWarningSource;
}

/**
 * The application controller. It owns the wasm core, the document mirror, the
 * host client, the live link and the scene builder, and is the only thing the
 * Solid components call into.
 */
export class Editor {
  core: Core | null = null;
  store: DocumentStore | null = null;
  host: HostClient | null = null;
  live: LiveLink | null = null;
  scene: Scene | null = null;
  schema: EntitySchema = emptySchema();
  keymap: Keymap = loadKeymap();
  projects: ProjectListEntry[] = [];
  session: EditorSourceInfo | null = null;
  assetResults: AssetEntry[] = [];
  assetTotal = 0;
  messages: string[] = [];
  /** Material problems the renderer and the host found, mirrored for the panels. */
  materialWarnings: MaterialWarning[] = [];
  /** The host token, read once from the fragment and never from the URL again. */
  token: string | null = null;

  private sceneBuilder: SceneBuilder | null = null;
  private sceneObjects: SceneObject[] = [];
  private sourceCache: SceneSource | null = null;
  private modelCache = new Map<string, ModelMesh | null>();
  /** Bumped whenever a model lands, so the scene list is rebuilt around it. */
  private modelVersion = 0;
  private sceneListKey: unknown[] = [];
  private meshCache = new Map<Id, MeshBuffers | null>();
  private boundsCache = new Map<Id, Aabb>();
  /** Wireframe edges per object and per asset model, for the wireframe pass. */
  private edgeCache = new Map<Id, WireTable | null>();
  private modelEdgeCache = new Map<string, WireTable | null>();
  private jobTimers = new Map<string, number>();
  private liveSeen = new Set<Id>();
  private revisionCounter = 0;
  /** The drag in progress, whose edits collapse into one undo step. */
  private gesture: { steps: number; start: Map<Id, Transform>; touched: Set<Id> } | null = null;
  /** Commands applied so far, so a gesture knows how many steps its last edit took. */
  private applied = 0;
  /** A hash of each project file as last written, so a save skips unchanged ones. */
  private savedFiles = new Map<string, number>();

  /** The packed preview lightmap atlas, or null when the preview is off. */
  lightmaps: PackedLightmap | null = null;

  /** Drops every cached mesh and bounds so the next scene build refetches. */
  markDirty(): void {
    this.meshCache.clear();
    this.boundsCache.clear();
    this.edgeCache.clear();
    this.modelEdgeCache.clear();
    this.revisionCounter++;
  }

  /** Marks only the ids whose geometry changed. */
  invalidate(ids: Iterable<Id>): void {
    const list = [...ids];
    // A modifier stack can read any other object (a boolean operand, a curve).
    if (list.length > 0 && this.store) {
      for (const o of this.store.snapshot().objects.values()) {
        if (isEditableMesh(o) && o.kind.type === "mesh" && (o.kind.mesh.modifiers?.length ?? 0) > 0) list.push(o.id);
      }
    }
    for (const id of list) {
      this.meshCache.delete(id);
      this.boundsCache.delete(id);
      this.edgeCache.delete(id);
    }
    this.sceneBuilder?.invalidate(list);
  }

  /** Loads the wasm core, reads the host token and fetches the session. */
  async init(wasmUrl?: string): Promise<void> {
    this.token = readToken(globalThis.location?.hash ?? "");
    // `p=<project>` in the fragment opens that project once the session is up.
    const openOnStart = readHashParam(globalThis.location?.hash ?? "", "p");
    stripHash(globalThis.location?.href ?? "", (clean) => {
      globalThis.history?.replaceState(null, "", clean);
    });
    this.core = await loadCore(wasmUrl);
    this.store = DocumentStore.empty(this.core);
    this.sourceCache = null;
    this.sceneBuilder = new SceneBuilder(this.source());
    {
      this.host = new HostClient(this.token);
      try {
        const info: SessionInfo = await this.host.session();
        this.session = { version: info.version, gameRoot: info.game_root };
        this.projects = await this.host.projects();
        this.schema = parseSchema(await this.host.schema());
      } catch (e) {
        this.note(e instanceof Error ? e.message : String(e));
      }
    }
    this.syncFlags();
    setProjectInfo("", this.store.meta.stem);
    if (openOnStart && this.projects.some((p) => p.name === openOnStart)) {
      try {
        await this.openProject(openOnStart);
      } catch (e) {
        this.note(e instanceof Error ? e.message : String(e));
      }
    }
  }

  note(message: string): void {
    setState("ui", "status", message);
    this.messages = message ? [message, ...this.messages].slice(0, 32) : this.messages;
    bumpPanels();
  }

  private syncFlags(): void {
    // Panels re-read the whole document on a revision; a drag publishes once, at its end.
    if (!this.store || this.gesture) return;
    const snap = this.store.snapshot();
    setDocumentFlags(snap.dirty, snap.canUndo, snap.canRedo);
    // Map settings and an undo of them both change the stem the top bar shows.
    if (snap.doc.meta?.stem && snap.doc.meta.stem !== state.stem) setState("stem", snap.doc.meta.stem);
  }

  private requireCore(): Core {
    if (!this.core) throw new EditorError("the wasm core is not loaded");
    return this.core;
  }

  private requireStore(): DocumentStore {
    if (!this.store) throw new EditorError("no document is open");
    return this.store;
  }

  /** Applies a command, refreshes the scene and reports the touched ids. */
  apply(command: Command): Id[] {
    const store = this.requireStore();
    const touched = store.apply(command);
    this.applied++;
    this.invalidate(touched);
    this.syncFlags();
    return touched;
  }

  applyJson(text: string): Id[] {
    const store = this.requireStore();
    const touched = store.applyJson(text);
    this.applied++;
    this.invalidate(touched);
    this.syncFlags();
    return touched;
  }

  undo(): Id[] {
    const store = this.requireStore();
    const touched = store.undo();
    this.invalidate(touched);
    this.syncFlags();
    return touched;
  }

  redo(): Id[] {
    const store = this.requireStore();
    const touched = store.redo();
    this.invalidate(touched);
    this.syncFlags();
    return touched;
  }

  /** Replaces the transform of one object. */
  setTransform(id: Id, transform: Partial<Transform>): void {
    const store = this.requireStore();
    const object = store.object(id);
    if (!object) return;
    this.apply(replaceObjects([{ ...object, transform: { ...object.transform, ...transform } }]));
    this.mirrorTransform(id);
  }

  /** Sets every selected object's transform at once, for a gizmo drag. */
  setSelectedTransforms(patch: Partial<Transform>): Id[] {
    const store = this.requireStore();
    const objects = state.selection.ids
      .map((id) => store.object(id))
      .filter((o): o is DocObject => o !== null);
    if (objects.length === 0) return [];
    const touched = this.apply(replaceObjects(objects.map((o) => ({ ...o, transform: { ...o.transform, ...patch } }))));
    for (const id of touched) this.mirrorTransform(id);
    return touched;
  }

  /**
   * Starts a drag that edits the document many times but should undo as one
   * step: each `gestureApply` replaces the previous one instead of stacking.
   */
  /** True while a drag is being previewed; panels wait for it to end. */
  get gesturing(): boolean {
    return this.gesture !== null;
  }

  /** Starts a gesture over the selection, plus `extra` objects a proportional edit may carry along. */
  beginGesture(extra: Id[] = []): void {
    this.gesture = { steps: 0, start: new Map(), touched: new Set() };
    const store = this.store;
    if (!store) return;
    for (const id of [...state.selection.ids, ...extra]) {
      const o = store.object(id);
      if (o) this.gesture.start.set(id, { position: [...o.transform.position], rotation: [...o.transform.rotation], scale: [...o.transform.scale] } as Transform);
    }
  }

  /**
   * Applies the gesture's edit, replacing what the previous call applied: that
   * many steps are undone first, so the document never holds two previews.
   */
  gestureApply(apply: () => Id[]): Id[] {
    const g = this.gesture;
    if (!g) return apply();
    for (let i = 0; i < g.steps; i++) this.undo();
    const before = this.applied;
    const touched = apply();
    g.steps = this.applied - before;
    for (const id of touched) g.touched.add(id);
    return touched;
  }

  /**
   * Ends the gesture. With `collapse`, an edit that took several commands is
   * folded into one object replacement; only an edit that changes objects and
   * nothing else (no files) may ask for that.
   */
  endGesture(collapse = false): void {
    const g = this.gesture;
    this.gesture = null;
    this.syncFlags();
    if (!collapse || !g || g.steps <= 1 || !this.store) return;
    const store = this.store;
    const finals = [...g.touched].map((id) => store.object(id)).filter((o): o is DocObject => o !== null);
    for (let i = 0; i < g.steps; i++) this.undo();
    if (finals.length > 0) this.apply(replaceObjects(finals));
  }

  /**
   * Moves, turns or scales every selected object by the change the gizmo made
   * to the primary one, about the pivot, from where each one started. The
   * gizmo reports the primary's new absolute transform; handing that to every
   * object would stack a multi-selection on top of the primary.
   */
  dragSelection(delta: { position?: Vec3; rotation?: Quat; scale?: Vec3 }, pivot: Vec3): Id[] {
    const g = this.gesture;
    const store = this.requireStore();
    const primary = state.selection.primary;
    const from = primary ? g?.start.get(primary) : undefined;
    if (!g || !from) return [];
    const objects: DocObject[] = [];
    for (const [id, start] of g.start) {
      const o = store.object(id);
      if (!o) continue;
      let position = start.position;
      let rotation = start.rotation;
      let scaleV = start.scale;
      if (delta.position) position = add(start.position, sub(delta.position, from.position));
      if (delta.rotation) {
        const turn = quatMul(delta.rotation, quatConjugate(from.rotation));
        position = add(pivot, quatRotate(turn, sub(start.position, pivot)));
        rotation = quatNormalize(quatMul(turn, start.rotation));
      }
      if (delta.scale) {
        const k: Vec3 = [0, 1, 2].map((i) => (from.scale[i] !== 0 ? delta.scale![i] / from.scale[i] : 1)) as Vec3;
        position = add(pivot, mul(sub(start.position, pivot), k));
        scaleV = mul(start.scale, k);
      }
      objects.push({ ...o, transform: { position, rotation, scale: scaleV } });
    }
    if (objects.length === 0) return [];
    const touched = this.gestureApply(() => this.apply(replaceObjects(objects)));
    for (const id of touched) this.mirrorTransform(id);
    return touched;
  }

  /**
   * Applies a modal transform to the selection from where each object was
   * when the gesture began: a move, a turn about `pivot`, or a scale about it.
   * With `individual` each object turns and scales about its own origin.
   */
  transformSelection(t: { translate?: Vec3; turn?: Quat; scale?: Vec3 }, pivot: Vec3, individual = false, weights?: Map<Id, number>): Id[] {
    const g = this.gesture;
    const store = this.requireStore();
    if (!g || g.start.size === 0) return [];
    const objects: DocObject[] = [];
    for (const [id, start] of g.start) {
      const o = store.object(id);
      if (!o || o.locked) continue;
      const center = individual ? start.position : pivot;
      // Selected objects take the whole edit; a proportional neighbour a share of it.
      const w = weights && !state.selection.ids.includes(id) ? weights.get(id) ?? 0 : 1;
      let position = start.position;
      let rotation = start.rotation;
      let scaleV = start.scale;
      if (t.turn && w > 0) {
        const turn = w >= 1 ? t.turn : quatSlerp([0, 0, 0, 1], t.turn, w);
        position = add(center, quatRotate(turn, sub(position, center)));
        rotation = quatNormalize(quatMul(turn, start.rotation));
      }
      if (t.scale && w > 0) {
        const k: Vec3 = [0, 1, 2].map((i) => 1 + (t.scale![i] - 1) * w) as Vec3;
        position = add(center, mul(sub(position, center), k));
        scaleV = mul(start.scale, k);
      }
      if (t.translate && w > 0) position = add(position, [t.translate[0] * w, t.translate[1] * w, t.translate[2] * w]);
      objects.push({ ...o, transform: { position, rotation, scale: scaleV } });
    }
    if (objects.length === 0) return [];
    const touched = this.gestureApply(() => this.apply(replaceObjects(objects)));
    for (const id of touched) this.mirrorTransform(id);
    return touched;
  }

  /** The world position, rotation and scale an object ends up at through its parents. */
  worldTransform(id: Id): Transform | null {
    const store = this.requireStore();
    let o = store.object(id);
    if (!o) return null;
    let position: Vec3 = [...o.transform.position] as Vec3;
    let rotation: Quat = [...o.transform.rotation] as Quat;
    let scaleV: Vec3 = [...o.transform.scale] as Vec3;
    const seen = new Set<Id>([id]);
    while (o.parent && !seen.has(o.parent)) {
      seen.add(o.parent);
      const p = store.object(o.parent);
      if (!p) break;
      position = add(p.transform.position, quatRotate(p.transform.rotation, mul(p.transform.scale, position)));
      rotation = quatNormalize(quatMul(p.transform.rotation, rotation));
      scaleV = mul(p.transform.scale, scaleV);
      o = p;
    }
    return { position, rotation, scale: scaleV };
  }

  /** The local transform that puts an object at `world` under `parent`. */
  private localUnder(parent: Id | null, world: Transform): Transform {
    if (!parent) return world;
    const p = this.worldTransform(parent);
    if (!p) return world;
    const inv = quatConjugate(p.rotation);
    const div = (a: Vec3, b: Vec3): Vec3 => [0, 1, 2].map((i) => (b[i] !== 0 ? a[i] / b[i] : a[i])) as Vec3;
    return {
      position: div(quatRotate(inv, sub(world.position, p.position)), p.scale),
      rotation: quatNormalize(quatMul(inv, world.rotation)),
      scale: div(world.scale, p.scale),
    };
  }

  /** Blender's Ctrl+P, Object (Keep Transform): the selection goes under the active object and stays put. */
  parentToActive(): Id[] {
    const store = this.requireStore();
    const parent = state.selection.primary;
    if (!parent) return [];
    const objects: DocObject[] = [];
    for (const id of state.selection.ids) {
      if (id === parent || this.descendsFrom(parent, id)) continue;
      const o = store.object(id);
      const world = this.worldTransform(id);
      if (!o || !world) continue;
      objects.push({ ...o, parent, transform: this.localUnder(parent, world) });
    }
    return objects.length ? this.apply(replaceObjects(objects)) : [];
  }

  /** Blender's Alt+P, Clear and Keep Transformation. */
  clearParentKeep(): Id[] {
    const store = this.requireStore();
    const objects: DocObject[] = [];
    for (const id of state.selection.ids) {
      const o = store.object(id);
      const world = this.worldTransform(id);
      if (!o || !world || !o.parent) continue;
      objects.push({ ...o, parent: null, transform: world });
    }
    return objects.length ? this.apply(replaceObjects(objects)) : [];
  }

  /**
   * Mirrors the selection across the plane through `pivot` with normal
   * `normal`. Positions reflect; each rotation becomes its mirror image, so
   * nothing is given a negative scale the game cannot load.
   */
  mirrorSelection(normal: Vec3, pivot: Vec3, individual = false): Id[] {
    const store = this.requireStore();
    const n = normalize(normal, [1, 0, 0]);
    const objects: DocObject[] = [];
    for (const id of state.selection.ids) {
      const o = store.object(id);
      if (!o || o.locked) continue;
      const center = individual ? o.transform.position : pivot;
      const d = sub(o.transform.position, center);
      const position = sub(o.transform.position, scale(n, 2 * dot(d, n)));
      const [x, y, z, w] = o.transform.rotation;
      const v: Vec3 = [x, y, z];
      const r = sub(scale(n, 2 * dot(v, n)), v);
      objects.push({ ...o, transform: { ...o.transform, position, rotation: quatNormalize([r[0], r[1], r[2], w]) } });
    }
    return objects.length ? this.apply(replaceObjects(objects)) : [];
  }

  /**
   * Blender's Align Objects: lines the selection up on the chosen axes, by
   * bounds center or by the low or high side, relative to the active object,
   * the selection's median, the 3D cursor or the world origin.
   */
  alignObjects(axes: [boolean, boolean, boolean], mode: "center" | "min" | "max", relative: "active" | "selection" | "cursor" | "origin"): Id[] {
    const store = this.requireStore();
    const bounds = new Map(this.currentObjects().map((o) => [o.doc.id, o.worldBounds]));
    const anchorOf = (id: Id): Vec3 | null => {
      const b = bounds.get(id);
      const p = this.worldTransform(id)?.position ?? null;
      if (!b) return p;
      if (mode === "min") return b.min;
      if (mode === "max") return b.max;
      return [(b.min[0] + b.max[0]) / 2, (b.min[1] + b.max[1]) / 2, (b.min[2] + b.max[2]) / 2];
    };
    const ids = state.selection.ids.slice();
    let target: Vec3 | null = null;
    if (relative === "origin") target = [0, 0, 0];
    else if (relative === "cursor") target = state.cursorWorld ?? [0, 0, 0];
    else if (relative === "active") target = state.selection.primary ? anchorOf(state.selection.primary) : null;
    else {
      const all = ids.map(anchorOf).filter((p): p is Vec3 => !!p);
      if (all.length) target = scale(all.reduce((a, p) => add(a, p), [0, 0, 0] as Vec3), 1 / all.length);
    }
    if (!target) return [];
    const objects: DocObject[] = [];
    for (const id of ids) {
      if (relative === "active" && id === state.selection.primary) continue;
      const o = store.object(id);
      const anchor = anchorOf(id);
      const world = this.worldTransform(id);
      if (!o || o.locked || !anchor || !world) continue;
      const delta: Vec3 = [0, 1, 2].map((i) => (axes[i] ? target![i] - anchor[i] : 0)) as Vec3;
      const moved = { ...world, position: add(world.position, delta) };
      objects.push({ ...o, transform: this.localUnder(o.parent, moved) });
    }
    return objects.length ? this.apply(replaceObjects(objects)) : [];
  }

  /**
   * Blender's Randomize Transform: adds up to `location` units per axis, up to
   * `rotation` degrees per axis, and scales uniformly by up to `scaleBy`,
   * from a seed so the same numbers give the same result.
   */
  randomizeTransform(location: Vec3, rotation: Vec3, scaleBy: number, seed: number): Id[] {
    const store = this.requireStore();
    let s = (seed >>> 0) || 1;
    const rand = (): number => {
      s ^= s << 13;
      s ^= s >>> 17;
      s ^= s << 5;
      return ((s >>> 0) / 0xffffffff) * 2 - 1;
    };
    const objects: DocObject[] = [];
    for (const id of state.selection.ids) {
      const o = store.object(id);
      if (!o || o.locked) continue;
      const position = add(o.transform.position, [rand() * location[0], rand() * location[1], rand() * location[2]]);
      let r = o.transform.rotation as Quat;
      for (let i = 0; i < 3; i++) {
        const deg = rand() * rotation[i];
        if (deg !== 0) r = quatMul(quatFromAxisAngle(AXIS_UNITS[i], (deg * Math.PI) / 180), r);
      }
      const k = Math.max(0.01, 1 + rand() * scaleBy);
      objects.push({ ...o, transform: { position, rotation: quatNormalize(r), scale: scale(o.transform.scale, k) } });
    }
    return objects.length ? this.apply(replaceObjects(objects)) : [];
  }

  /** Blender's Alt+NumpadSlash: drops the selection out of the local view. */
  removeFromLocalView(): void {
    if (!state.selection.isolate) return;
    const drop = new Set(state.selection.ids);
    const keep = state.selection.isolated.filter((id) => !drop.has(id));
    setIsolate(keep.length > 0, keep);
  }

  /** Throws away what the running gesture applied and ends it. */
  cancelGesture(): void {
    if (!this.gesture) return;
    this.gestureApply(() => []);
    this.endGesture();
  }

  /** Resets one part of every selected object's transform. */
  clearTransform(part: "position" | "rotation" | "scale"): Id[] {
    const store = this.requireStore();
    const value: number[] = part === "position" ? [0, 0, 0] : part === "rotation" ? [0, 0, 0, 1] : [1, 1, 1];
    const objects: DocObject[] = [];
    for (const id of state.selection.ids) {
      const o = store.object(id);
      if (!o || o.locked) continue;
      objects.push({ ...o, transform: { ...o.transform, [part]: [...value] } });
    }
    if (objects.length === 0) return [];
    const touched = this.apply(replaceObjects(objects));
    for (const id of touched) this.mirrorTransform(id);
    return touched;
  }

  /** Hides or shows objects; a hidden object is drawn nowhere and cannot be picked. */
  setHidden(ids: Id[], hidden: boolean): Id[] {
    const store = this.requireStore();
    const objects = ids.map((id) => store.object(id)).filter((o): o is DocObject => !!o && o.hidden !== hidden);
    if (objects.length === 0) return [];
    return this.apply(replaceObjects(objects.map((o) => ({ ...o, hidden }))));
  }

  /** Shows every hidden object and selects what came back. */
  revealAll(): Id[] {
    const store = this.requireStore();
    const hidden = store.ids().filter((id) => store.object(id)?.hidden);
    this.setHidden(hidden, false);
    if (hidden.length > 0) select(hidden);
    return hidden;
  }

  invertSelection(): void {
    const store = this.requireStore();
    const current = new Set(state.selection.ids);
    select(store.ids().filter((id) => !current.has(id) && !store.object(id)?.locked && !store.object(id)?.hidden));
  }

  /** Selects everything sharing the active object's kind, prop model or entity class. */
  selectGrouped(): number {
    const store = this.requireStore();
    const active = state.selection.primary ? store.object(state.selection.primary) : null;
    if (!active) return 0;
    const key = (o: DocObject): string => {
      const k = o.kind;
      if (k.type === "prop") return `prop:${k.prop.model}`;
      if (k.type === "entity") return `entity:${k.entity.classname}`;
      return k.type;
    };
    const want = key(active);
    const ids = store.ids().filter((id) => {
      const o = store.object(id);
      return !!o && !o.hidden && !o.locked && key(o) === want;
    });
    select([...ids.filter((id) => id !== active.id), active.id]);
    return ids.length;
  }

  setMeta(meta: MapDoc["meta"]): void {
    this.apply(setMeta(meta));
  }

  setSettings(settings: Settings): void {
    this.apply(setSettings(settings));
  }

  setLayer(layer: Layer): void {
    this.apply({ op: "replace_layer", layer });
  }

  addLayer(layer: Layer): void {
    this.apply({ op: "add_layer", layer });
  }

  removeLayer(id: Id): void {
    this.apply({ op: "remove_layer", id });
  }

  removeObjects(ids: Id[]): void {
    if (ids.length === 0) return;
    for (const id of ids) this.live?.send({ op: "delete", id });
    this.apply(removeObjects(ids));
    if (this.meshEdit && ids.includes(this.meshEdit.object)) this.endMeshEdit();
    select([]);
  }

  addObjects(objects: DocObject[]): Id[] {
    const touched = this.apply(addObjects(objects));
    for (const id of touched) this.mirrorTransform(id);
    return touched;
  }

  /**
   * Builds one default object of a kind, positioned at `at` when the kind has a
   * place in the world. A brush and a mesh go through the core so their faces
   * and their glb are real.
   */
  createObject(kind: NewObjectKind, at: Vec3 = [0, 0, 0], name?: string): Id[] {
    const layer = this.requireStore().layers[0]?.id ?? "";
    const base = {
      layer,
      parent: null,
      transform: { position: [...at] as Vec3, rotation: [0, 0, 0, 1] as [number, number, number, number], scale: [1, 1, 1] as Vec3 },
      hidden: false,
      locked: false,
    };
    const object = (label: string, objectKind: DocObject["kind"]): DocObject => ({
      id: newUuid(),
      name: name ?? label,
      ...base,
      kind: objectKind,
    });
    switch (kind) {
      case "brush": {
        const brush = state.tool.brush;
        const bounds = centredBox(at, [brush.width, brush.depth, brush.height]);
        return this.brushBox(bounds.min, bounds.max, brush.material);
      }
      case "mesh":
        return this.meshCreate({ shape: "box", size: [64, 64, 64] }, { position: at, material: DEFAULT_MATERIAL, name });
      case "prop":
        return this.addObjects([object("prop", {
          type: "prop",
          prop: { model: "mdl/props/container.rmdl", skin: 0, static_prop: true, solid: true, cast_shadows: true, fade_distance: 0, keyvalues: {} },
        })]);
      case "entity":
        return this.addEntity("info_target", at, name);
      case "light":
        return this.addObjects([object("light", {
          type: "light",
          light: {
            kind: "point",
            color: [1, 1, 1],
            intensity: 1,
            range: 512,
            inner_angle: 30,
            outer_angle: 45,
            size: [32, 32],
            cast_shadows: true,
            baked: true,
            realtime: false,
            keyvalues: {},
          },
        })]);
      case "volume":
        return this.addObjects([object("volume", {
          type: "volume",
          volume: { kind: "trigger", classname: "trigger_multiple", brushes: [], keyvalues: {} },
        })]);
      case "path":
        return this.addObjects([object("path", {
          type: "path",
          path: { kind: "zipline", points: [at, [at[0] + 256, at[1], at[2] + 256]], keyvalues: {} },
        })]);
      case "terrain": {
        const heights = `terrain/${newUuid()}.f32`;
        const n = FLAT_TERRAIN_SAMPLES;
        this.apply(setFile(heights, new Uint8Array(new Float32Array(n * n).buffer)));
        return this.addObjects([object("terrain", {
          type: "terrain",
          terrain: {
            heights,
            resolution: [n, n],
            cell_size: 64,
            layers: [{ name: "base", material: "terrain/default", weights: "", uv_scale: 0.02, scatter: [] }],
            chunk_size: 64,
            collision: true,
            contents: 1,
            lightmap_scale: 16,
            recipe: null,
          },
        })]);
      }
      case "voxel_terrain":
        return this.addObjects([object("voxel_terrain", {
          type: "voxel_terrain",
          voxel_terrain: {
            // Object space: the transform already carries the position.
            bounds: [[-256, -256, -128], [256, 256, 128]],
            voxel_size: 16,
            // A slab to start from, so the new terrain shows and can be picked;
            // the first operation of a field has to be an add.
            ops: [{ op: "add", shape: { shape: "box", center: [0, 0, 0], half: [192, 192, 48], rotation: [0, 0, 0] } }],
            material: DEFAULT_MATERIAL,
            uv_scale: 0.05,
            collision: true,
            contents: 1,
            lightmap_scale: 0,
          },
        })]);
      case "group":
        return this.addObjects([object("group", { type: "group" })]);
      default:
        this.note(`a ${kind} cannot be created from the toolbar`);
        return [];
    }
  }

  /**
   * Adds a procedurally built mesh: the core writes `meshes/<id>.glb` and the
   * object that references it, so the whole thing is one undo step.
   */
  meshCreate(spec: MeshSpec, options: MeshOptions = {}): Id[] {
    return this.applyJson(
      this.requireCore().meshCreate(
        meshRequestJson(spec, {
          material: options.material ?? DEFAULT_MATERIAL,
          name: options.name,
          position: options.position,
          uvScale: options.uvScale,
          collision: options.collision,
        }),
      ),
    );
  }

  /** Every material path the document already uses, sorted and deduplicated. */
  projectMaterials(): string[] {
    const store = this.store;
    if (!store) return [];
    const seen = new Set<string>();
    for (const object of store.snapshot().objects.values()) {
      const kind = object.kind;
      if (kind.type === "brush" || kind.type === "volume") {
        const brushes = kind.type === "brush" ? [kind.brush] : kind.volume.brushes;
        for (const brush of brushes) for (const face of brush.faces) if (face.material) seen.add(face.material);
      } else if (kind.type === "mesh") {
        for (const value of Object.values(kind.mesh.material_map)) if (value) seen.add(value);
      } else if (kind.type === "terrain") {
        for (const layer of kind.terrain.layers) if (layer.material) seen.add(layer.material);
      } else if (kind.type === "voxel_terrain" && kind.voxel_terrain.material) {
        seen.add(kind.voxel_terrain.material);
      }
    }
    return [...seen].sort();
  }

  /** Adds an entity of a schema class at a world position, with a targetname. */
  addEntity(classname: string, at: Vec3 = [0, 0, 0], name?: string): Id[] {
    const short = classname.replace(/[^A-Za-z0-9_]/g, "_");
    return this.addObjects([
      {
        id: newUuid(),
        name: name ?? short,
        layer: this.requireStore().layers[0]?.id ?? "",
        parent: null,
        transform: { position: [...at] as Vec3, rotation: [0, 0, 0, 1] as [number, number, number, number], scale: [1, 1, 1] as Vec3 },
        hidden: false,
        locked: false,
        kind: {
          type: "entity",
          entity: {
            classname,
            keyvalues: { targetname: `${short}_${newUuid().slice(0, 8)}` },
            partition: null,
            links: {},
          },
        },
      },
    ]);
  }

  /** Runs a labelled batch so one undo step covers the whole gesture. */
  runBatch(label: string, commands: Command[]): Id[] {
    return this.apply(batch(label, commands));
  }

  rename(object: DocObject, name: string): void {
    if (object.name === name) return;
    this.apply(replaceObjects([{ ...object, name }]));
  }

  /** Moves an object under a new parent; null moves it to the root. */
  reparent(id: Id, parent: Id | null): void {
    const store = this.requireStore();
    const object = store.object(id);
    if (!object || object.id === parent) return;
    if (parent && this.descendsFrom(parent, id)) return;
    this.apply(replaceObjects([{ ...object, parent }]));
  }

  private descendsFrom(ancestor: Id, child: Id): boolean {
    const store = this.store;
    if (!store) return false;
    let current = store.object(child)?.parent ?? null;
    const seen = new Set<Id>();
    while (current && !seen.has(current)) {
      if (current === ancestor) return true;
      seen.add(current);
      current = store.object(current)?.parent ?? null;
    }
    return false;
  }

  /** Puts the ids under one new group object. */
  group(ids: Id[]): Id[] {
    const store = this.requireStore();
    if (ids.length === 0) return [];
    const first = store.object(ids[0]);
    const layer = first?.layer ?? store.layers[0]?.id ?? "";
    const groupId = newUuid();
    const group: DocObject = {
      id: groupId,
      name: "group",
      layer,
      parent: first?.parent ?? null,
      transform: identityTransform(),
      hidden: false,
      locked: false,
      kind: { type: "group" },
    };
    const moves: Command[] = [];
    for (const id of ids) {
      const o = store.object(id);
      if (o) moves.push(replaceObjects([{ ...o, parent: groupId }]));
    }
    return this.runBatch("group", [addObjects([group]), ...moves]);
  }

  /** Lifts the children of each group to the group's own parent. */
  ungroup(ids: Id[]): Id[] {
    const store = this.requireStore();
    const commands: Command[] = [];
    for (const id of ids) {
      const group = store.object(id);
      if (!group || group.kind.type !== "group") continue;
      for (const child of store.ids()) {
        const o = store.object(child);
        if (o && o.parent === id) commands.push(replaceObjects([{ ...o, parent: group.parent }]));
      }
      commands.push(removeObjects([id]));
    }
    if (commands.length === 0) return [];
    return this.runBatch("ungroup", commands);
  }

  /** Copies the ids, re-parented to the originals, with fresh ids. */
  duplicate(ids: Id[], offset = 16): Id[] {
    const store = this.requireStore();
    const copies: DocObject[] = [];
    for (const id of ids) {
      const original = store.object(id);
      if (!original) continue;
      const copy = reid(original);
      copy.transform = { ...copy.transform, position: addOffset(original.transform.position, offset) };
      copies.push(copy);
    }
    if (copies.length === 0) return [];
    return this.addObjects(copies);
  }

  /**
   * Writes the selection to `prefabs/<name>.r5prefab`, which is where a
   * project keeps prefabs, so the instance list finds it at compile time.
   */
  savePrefab(ids: Id[], name = "selection"): Id[] {
    const store = this.requireStore();
    const objects = ids
      .map((id) => store.object(id))
      .filter((o): o is DocObject => o !== null)
      .map((o) => ({ ...o, id: newUuid() }));
    if (objects.length === 0) return [];
    const path = `prefabs/${name.replace(/[^A-Za-z0-9_-]/g, "_")}.r5prefab`;
    return this.apply({ op: "set_file", path, bytes: Array.from(new TextEncoder().encode(JSON.stringify({ objects: objects.map(objectToWire) }, null, 2))) });
  }

  // --- brush tools, all of which the core answers as command JSON ---

  brushBox(min: Vec3, max: Vec3, material: string): Id[] {
    return this.applyJson(this.requireCore().brushBox(min, max, material));
  }

  brushClip(ids: Id[], normal: Vec3, dist: number, keep: "front" | "back" | "both"): Id[] {
    return this.applyJson(this.requireCore().brushClip(ids, normal, dist, keep));
  }

  brushHollow(id: Id, thickness: number): Id[] {
    return this.applyJson(this.requireCore().brushHollow(id, thickness));
  }

  brushCarve(carver: Id, targets: Id[]): Id[] {
    return this.applyJson(this.requireCore().brushCarve(carver, targets));
  }

  brushMerge(ids: Id[]): Id[] {
    return this.applyJson(this.requireCore().brushMergeConvex(ids));
  }

  brushResize(id: Id, size: Vec3): Id[] {
    return this.applyJson(this.requireCore().brushResize(id, size));
  }

  brushBounds(id: Id): number[] | null {
    try {
      return this.requireCore().brushBounds(id);
    } catch {
      return null;
    }
  }

  brushExtrudeFace(id: Id, faceIndex: number, distance: number): Id[] {
    return this.applyJson(this.requireCore().brushExtrudeFace(id, faceIndex, distance));
  }

  brushMoveVertex(id: Id, vertex: Vec3, to: Vec3): Id[] {
    return this.applyJson(this.requireCore().brushMoveVertex(id, vertex, to));
  }

  // --- mesh edit mode ---

  /** The open edit session, if Tab put one mesh under edit. */
  meshEdit: MeshEditState | null = null;
  /** Shading to restore when the edit session ends. */
  private savedShading: ShadingState | null = null;
  /** The last mesh op, for the redo panel and the control socket. */
  lastMeshOp: LastMeshOp | null = null;
  /** Bumped on every session change, so draw buffers refetch. */
  private sessionVersion = 0;
  private buffersCache: { version: number; buffers: EditBuffers | null } | null = null;
  private lastPreview: { request: LastMeshOp["request"]; result: MeshOpResult } | null = null;

  get meshEditing(): boolean {
    return this.meshEdit !== null;
  }

  /** Bumps on every session change, so draw buffers refetch. */
  get sessionStamp(): number {
    return this.sessionVersion;
  }

  private bumpSession(): void {
    this.sessionVersion++;
    this.buffersCache = null;
    touchMeshEdit();
  }

  private coreCounts(text: string): { verts: number; edges: number; faces: number } {
    const v = JSON.parse(text) as { verts: number; edges: number; faces: number };
    if (!Number.isInteger(v.verts) || !Number.isInteger(v.edges) || !Number.isInteger(v.faces)) {
      throw new EditorError("the core returned bad mesh counts");
    }
    return v;
  }

  beginMeshEdit(id: Id): void {
    const counts = this.coreCounts(this.requireCore().editBegin(id));
    this.meshEdit = { object: id, mode: "vertex", counts };
    // Edit mode draws solid with a wire overlay on the edited object; the
    // previous shading comes back when the session ends.
    this.savedShading = { ...state.ui.shading };
    setShading(editModeShading(state.ui.shading));
    this.lastPreview = null;
    this.bumpSession();
    this.syncFlags();
  }

  endMeshEdit(): void {
    if (!this.meshEdit) return;
    try {
      this.requireCore().editEnd();
    } catch {
      // The core already dropped the session (load, reset); the flag is what matters.
    }
    this.meshEdit = null;
    if (this.savedShading) setShading(this.savedShading);
    this.savedShading = null;
    this.lastPreview = null;
    this.bumpSession();
    this.syncFlags();
  }

  /** Tab: out of edit mode, or into it on the active mesh. */
  toggleMeshEdit(): void {
    if (this.meshEdit) {
      this.endMeshEdit();
      return;
    }
    const id = state.selection.primary;
    const object = id ? this.store?.object(id) : null;
    if (!id || !object) {
      this.note("select a mesh, then Tab to edit it");
      return;
    }
    if (object.kind.type === "brush") {
      this.note("brushes keep the vertex, edge and face tools; Convert to Mesh makes one editable");
      return;
    }
    if (object.kind.type !== "mesh") {
      this.note("edit mode works on meshes");
      return;
    }
    try {
      if (!object.kind.mesh.source.endsWith(".r5mesh")) {
        this.applyJson(this.requireCore().meshMakeEditable(id));
        this.note("made editable");
      }
      this.beginMeshEdit(id);
      this.note(`edit mode: ${object.name}`);
    } catch (e) {
      this.note(e instanceof Error ? e.message : String(e));
    }
  }

  meshEditSelect(sel: string): { mode: EditSelMode; ids: number[] } {
    const out = JSON.parse(this.requireCore().editSelect(sel)) as { mode: EditSelMode; ids: number[] };
    if (out.mode !== "vertex" && out.mode !== "edge" && out.mode !== "face") throw new EditorError("the core returned a bad selection mode");
    if (this.meshEdit) this.meshEdit = { ...this.meshEdit, mode: out.mode };
    this.bumpSession();
    return { mode: out.mode, ids: Array.isArray(out.ids) ? out.ids : [] };
  }

  meshSetMode(mode: EditSelMode): void {
    if (!this.meshEdit) return;
    this.meshEditSelect(selJson(mode));
  }

  /** The ids the session has selected, read back from its masks. */
  meshSessionIds(): { mode: EditSelMode; ids: number[] } {
    const mode = this.meshEdit?.mode ?? "vertex";
    const buffers = this.meshEditBuffers();
    const mask = !buffers ? null : mode === "vertex" ? buffers.vertSel : mode === "edge" ? buffers.edgeSel : buffers.faceSel;
    const ids: number[] = [];
    if (mask) for (let i = 0; i < mask.length; i++) if (mask[i]) ids.push(i);
    return { mode, ids };
  }

  meshEditBuffers(): EditBuffers | null {
    if (!this.meshEdit) return null;
    if (!this.buffersCache || this.buffersCache.version !== this.sessionVersion) {
      let buffers: EditBuffers | null = null;
      try {
        buffers = this.requireCore().editBuffers();
      } catch {
        buffers = null;
      }
      this.buffersCache = { version: this.sessionVersion, buffers };
    }
    return this.buffersCache.buffers;
  }

  meshEditPick(queryJson: string): PickReply {
    return parsePickReply(this.requireCore().editPick(queryJson));
  }

  /** Runs one mesh op through the core, applies it and records it for redo. */
  runMeshOp(requestJson: string): MeshOpResult {
    const { commandJson, result } = parseOpResult(this.requireCore().meshOp(requestJson));
    result.objects = this.applyJson(commandJson);
    const req = JSON.parse(requestJson) as { op: string; object: string; sel: unknown; params?: Record<string, unknown> };
    this.lastMeshOp = {
      request: { op: req.op, object: req.object, sel: req.sel, params: req.params ?? {} },
      report: result.report,
      depth: this.store?.depths().undo ?? 0,
    };
    if (this.meshEdit && req.object === this.meshEdit.object) {
      this.meshEdit = {
        ...this.meshEdit,
        mode: result.selection.mode,
        counts: { verts: result.report.verts, edges: result.report.edges, faces: result.report.faces },
      };
      this.bumpSession();
    }
    bumpPanels();
    return result;
  }

  meshOpRequest(op: string, sel: string, params: Record<string, unknown>): MeshOpResult {
    if (!this.meshEdit) throw new EditorError("no mesh is being edited");
    return this.runMeshOp(opRequest(this.meshEdit.object, op, JSON.parse(sel), params));
  }

  /** The redo panel's re-run: undo the last op, then apply it with new params. */
  adjustLastOp(params: Record<string, unknown>): MeshOpResult {
    const last = this.lastMeshOp;
    if (!last) throw new EditorError("no mesh operation to adjust");
    if ((this.store?.depths().undo ?? -1) !== last.depth) {
      throw new EditorError("the document changed since; the redo panel is stale");
    }
    this.undo();
    try {
      return this.runMeshOp(rerunRequest(last, params));
    } catch (e) {
      this.redo();
      throw e;
    }
  }

  /** A drag preview: mutates the session, redraws, produces no command. */
  meshEditPreview(requestJson: string): MeshOpResult {
    const v = JSON.parse(this.requireCore().editPreview(requestJson)) as { selection: MeshOpResult["selection"]; report: MeshOpResult["report"] };
    const result: MeshOpResult = { selection: v.selection, report: v.report };
    const req = JSON.parse(requestJson) as { op: string; object: string; sel: unknown; params?: Record<string, unknown> };
    this.lastPreview = { request: { op: req.op, object: req.object, sel: req.sel, params: req.params ?? {} }, result };
    if (this.meshEdit) {
      this.meshEdit = {
        ...this.meshEdit,
        mode: result.selection.mode,
        counts: { verts: result.report.verts, edges: result.report.edges, faces: result.report.faces },
      };
      this.invalidate([this.meshEdit.object]);
    }
    this.bumpSession();
    return result;
  }

  /** A drag release: the session becomes one `SetFile` undo step. */
  meshEditCommit(): Id[] {
    const touched = this.applyJson(this.requireCore().editCommit());
    if (this.lastPreview && this.meshEdit) {
      this.lastMeshOp = { request: this.lastPreview.request, report: this.lastPreview.result.report, depth: this.store?.depths().undo ?? 0 };
    }
    this.lastPreview = null;
    this.bumpSession();
    bumpPanels();
    return touched;
  }

  /** Drops uncommitted previews and reloads the session mesh, restoring `sel`. */
  resetMeshEdit(sel: { mode: EditSelMode; ids: number[] }): void {
    const id = this.meshEdit?.object;
    if (!id) return;
    try {
      this.requireCore().editEnd();
    } catch {
      // The core already dropped the session; beginning again restores it.
    }
    this.beginMeshEdit(id);
    try {
      this.meshEditSelect(selJson(sel.mode, sel.ids));
    } catch {
      // Ids the mesh no longer has stay dropped.
    }
  }

  meshHide(): void {
    if (!this.meshEdit) return;
    this.requireCore().editHide();
    this.bumpSession();
  }

  meshReveal(): void {
    if (!this.meshEdit) return;
    this.requireCore().editReveal();
    this.bumpSession();
  }

  brushToMesh(id: Id): Id[] {
    return this.applyJson(this.requireCore().brushToMesh(id));
  }

  meshMakeEditable(id: Id): Id[] {
    return this.applyJson(this.requireCore().meshMakeEditable(id));
  }

  // --- terrain ---

  terrainGenerate(recipeJson: string): Float32Array {
    return this.requireCore().terrainGenerate(recipeJson);
  }

  terrainSculpt(id: Id, strokeJson: string): Id[] {
    return this.applyJson(this.requireCore().terrainSculpt(id, strokeJson));
  }

  terrainAutoPaint(id: Id, rulesJson: string): Id[] {
    // The core writes each layer's weights to the path the layer names; a new
    // terrain has none yet, so each layer gets its own file first.
    const object = this.requireStore().object(id);
    if (object?.kind.type === "terrain" && object.kind.terrain.layers.some((l) => !l.weights)) {
      const terrain = object.kind.terrain;
      const layers = terrain.layers.map((l, i) => (l.weights ? l : { ...l, weights: `terrain/${id}_${i}.u8` }));
      this.apply(replaceObjects([{ ...object, kind: { type: "terrain", terrain: { ...terrain, layers } } }]));
    }
    return this.applyJson(this.requireCore().terrainAutoPaint(id, rulesJson));
  }

  terrainScatter(id: Id): Id[] {
    return this.applyJson(this.requireCore().terrainScatter(id));
  }

  // --- importers ---

  importObj(text: string, options: { scale: number; up: "y" | "z"; material_prefix: string }): Id[] {
    return this.applyJson(this.requireCore().importObj(text, JSON.stringify(options)));
  }

  importVmf(text: string): Id[] {
    return this.applyJson(this.requireCore().importVmf(text));
  }

  importQuakeMap(text: string): Id[] {
    return this.applyJson(this.requireCore().importQuakeMap(text));
  }

  importReMap(json: string): Id[] {
    return this.applyJson(this.requireCore().importReMap(json));
  }

  importGltf(path: string): Id[] {
    return this.applyJson(this.requireCore().importGltf(path));
  }

  /**
   * Stores a picked glb under `meshes/` and adds the mesh that references it.
   * The file goes in as a document command, so a save writes it to the project.
   */
  importGltfFile(fileName: string, bytes: Uint8Array): Id[] {
    const base = fileName.replace(/\.(glb|gltf)$/i, "").replace(/[^A-Za-z0-9_-]+/g, "_") || "mesh";
    let path = `meshes/${base}.glb`;
    for (let n = 1; this.requireCore().getFile(path); n++) path = `meshes/${base}_${n}.glb`;
    this.apply(setFile(path, bytes));
    return this.importGltf(path);
  }

  /** Compiles in the browser; the report is what the job panel shows. */
  compile(targets: string[], assetInfo: Record<string, unknown>): ReturnType<Core["compile"]> {
    return this.requireCore().compile(JSON.stringify(targets), JSON.stringify(assetInfo));
  }

  /** Low-quality lightmap preview for the viewport. */
  bakePreview(settings: unknown): Record<Id, { width: number; height: number; rgba: Uint8Array }> {
    return this.requireCore().bakePreview(JSON.stringify(settings));
  }

  /**
   * Runs a preview bake and packs the per-object lightmaps into one atlas the
   * renderer can sample. Returns the pack, or null when the bake was empty.
   */
  previewLightmaps(): PackedLightmap | null {
    const previews = this.bakePreview(this.document()?.settings.bake ?? {});
    const packed = packLightmaps(previews as LightmapPreviews);
    this.lightmaps = packed.rects.size > 0 ? packed : null;
    return this.lightmaps;
  }

  /**
   * Bumped by every command that changes the document, so a cache keyed on it
   * knows when its contents went stale.
   */
  get revision(): number {
    return this.revisionCounter;
  }

  /**
   * Inserts an asset at a world position. A model becomes a prop, a material a
   * 64 inch box brush; the caller supplies the surface point from `core.pick`.
   */
  placeAsset(kind: string, path: string, position: Vec3, layer?: Id): Id[] {
    const store = this.requireStore();
    const onLayer = layer ?? store.layers[0]?.id ?? "";
    const id = newUuid();
    const base: DocObject = {
      id,
      name: path.split("/").pop() ?? path,
      layer: onLayer,
      parent: null,
      transform: { position, rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
      hidden: false,
      locked: false,
      kind: { type: "group" },
    };
    if (kind === "model") {
      base.name = path;
      base.kind = {
        type: "prop",
        prop: { model: path, skin: 0, static_prop: true, solid: true, cast_shadows: true, fade_distance: 0, keyvalues: {} },
      };
      void this.loadModel(path);
      return this.addObjects([base]);
    }
    if (kind === "map") {
      // A shipped map is not geometry to drop in; it comes in as a reference.
      const stem = (path.split("/").pop() ?? path).replace(/\.bsp$/i, "");
      void this.startJob("import_map", { stem }).catch((e: unknown) => this.note(e instanceof Error ? e.message : String(e)));
      return [];
    }
    if (kind === "material") {
      base.kind = {
        type: "brush",
        brush: { faces: [], contents: 1, detail: false },
      };
      const brush = state.tool.brush;
      const bounds = centredBox(position, [brush.width, brush.depth, brush.height]);
      return this.brushBox(bounds.min, bounds.max, path);
    }
    if (kind === "entity" || kind === "sound") {
      base.kind = {
        type: "entity",
        entity: { classname: kind === "sound" ? "ambient_sound" : "info_target", keyvalues: kind === "sound" ? { "message": path } : {}, partition: null, links: {} },
      };
      return this.addObjects([base]);
    }
    this.note(`${kind} assets are not placeable in the viewport`);
    return [];
  }

  /** The world point under a screen position, from the core's own pick. */
  /**
   * The surface a ray hits, from the geometry the viewport draws. The core's
   * own pick walks every object and is far too slow for a map-sized document.
   * With x-ray, the farthest hit wins instead of the front one, so an
   * occluded object is picked; the reference props contribute their nearest
   * hit to the same contest.
   */
  pickSurface(origin: Vec3, dir: Vec3, maxDist = 1 << 22, accept?: (id: Id) => boolean, xray = 0): { id: Id; position: Vec3; normal: Vec3; distance: number } | null {
    const scene = this.scene;
    if (scene && (scene.instances.length > 0 || scene.refs)) {
      if (xray > 0) {
        const all = pickSceneAll(scene, origin, dir, maxDist, accept);
        const prop = scene.refs?.pick(scene.chunks, origin, dir, maxDist) ?? null;
        const hits = prop && (!accept || accept(refPropId(prop.record)))
          ? [...all, { id: refPropId(prop.record), position: prop.position, normal: prop.normal, distance: prop.distance }]
          : all;
        hits.sort((a, b) => a.distance - b.distance);
        return pickFarthest(hits);
      }
      const own = scene.instances.length > 0 ? pickScene(scene, origin, dir, maxDist, accept) : null;
      const prop = scene.refs?.pick(scene.chunks, origin, dir, own ? own.distance : maxDist) ?? null;
      if (prop && (!own || prop.distance < own.distance)) {
        return { id: refPropId(prop.record), position: prop.position, normal: prop.normal, distance: prop.distance };
      }
      return own;
    }
    const hit = this.core?.pick(origin, dir, maxDist);
    return hit ? { id: hit.id, position: hit.position as Vec3, normal: hit.normal as Vec3, distance: hit.distance } : null;
  }

  /**
   * Makes reference prop `record` an object of the document, hidden like a
   * decompiled one: the map's own record still compiles until the object is
   * edited. Returns the object that stands for the record.
   */
  promoteRefProp(record: number): Id | null {
    const store = this.requireStore();
    const table = this.sceneBuilder?.refProps?.table;
    if (!table || record >= table.count) return null;
    const reference = [...store.snapshot().objects.values()].find((o) => o.kind.type === "reference");
    if (!reference || reference.kind.type !== "reference") return null;
    const sources = reference.kind.reference.sources ?? {};
    for (const [id, r] of Object.entries(sources)) if (r.type === "prop" && r.index === record) return id;
    const angles: Vec3 = [table.angles[record * 3], table.angles[record * 3 + 1], table.angles[record * 3 + 2]];
    const scale = table.scale[record] > 0 ? table.scale[record] : 1;
    const id = newUuid();
    const prop: DocObject = {
      id,
      name: `prop${String(record).padStart(4, "0")}`,
      layer: reference.layer,
      parent: null,
      transform: {
        position: [table.origin[record * 3], table.origin[record * 3 + 1], table.origin[record * 3 + 2]],
        rotation: quatFromSourceAngles(angles),
        scale: [scale, scale, scale],
      },
      hidden: true,
      locked: false,
      kind: {
        type: "prop",
        prop: {
          model: table.models[table.model[record]] ?? "",
          skin: table.skin[record],
          static_prop: true,
          solid: (table.flags[record] & PROP_FLAG_SOLID) !== 0,
          cast_shadows: true,
          fade_distance: table.fade[record],
          keyvalues: {},
        },
      },
    };
    const withSource: DocObject = {
      ...reference,
      kind: { type: "reference", reference: { ...reference.kind.reference, sources: { ...sources, [id]: { type: "prop", index: record } } } },
    };
    this.apply(batch(`promote prop ${record}`, [addObjects([prop]), replaceObjects([withSource])]));
    return id;
  }

  pickWorld(origin: Vec3, dir: Vec3, maxDist = 1 << 22, xray = 0): Vec3 | null {
    return this.pickSurface(origin, dir, maxDist, undefined, xray)?.position ?? null;
  }


  validate(): ValidationIssue[] {
    const raw = this.requireCore().validate();
    try {
      const parsed: unknown = JSON.parse(raw);
      if (!Array.isArray(parsed)) return [];
      return parsed.filter((i): i is ValidationIssue => typeof i === "object" && i !== null && typeof (i as ValidationIssue).message === "string");
    } catch {
      return [];
    }
  }

  // --- projects ---

  async newProject(name: string): Promise<void> {
    const host = this.requireHost();
    await host.createProject(name);
    // A new project starts from an empty document, not from whatever was open.
    const core = this.requireCore();
    this.replaceStore(new DocumentStore(core, parseDoc(core.reset())), name);
    // The host seeds map.json with a placeholder the core cannot load, so the
    // real empty document is written straight away.
    await this.save();
    this.projects = await host.projects();
  }

  async openProject(name: string): Promise<void> {
    const host = this.requireHost();
    const text = new TextDecoder().decode(await host.readProjectFile(name, "map.json"));
    const core = this.requireCore();
    core.reset();
    // Meshes, heights, weights and prefabs are the document's own files; the
    // core has to hold them before the objects that reference them load.
    const saved = new Map<string, number>();
    const glbs: [string, Uint8Array][] = [];
    for (const entry of await host.projectFiles(name)) {
      if (!PROJECT_FILE_DIRS.some((dir) => entry.path.startsWith(`${dir}/`))) continue;
      const bytes = await host.readProjectFile(name, entry.path);
      core.putFile(entry.path, bytes);
      saved.set(entry.path, fileHash(bytes));
      if (entry.path.toLowerCase().endsWith(".glb")) glbs.push([entry.path, bytes]);
    }
    this.replaceStore(await DocumentStore.open(core, text), name);
    this.savedFiles = saved;
    // Mesh objects draw from the project's own glb files.
    for (const [path, bytes] of glbs) {
      try {
        this.modelCache.set(path, modelMesh(bytes.slice().buffer));
      } catch (e) {
        this.note(`mesh ${path}: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    await this.loadRefProps(name);
    this.modelVersion++;
    void this.loadDocumentModels();
  }

  /** The imported map's prop table, when the project keeps one. */
  private async loadRefProps(project: string): Promise<void> {
    const host = this.requireHost();
    if (!(await host.projectFiles(project)).some((e) => e.path === REF_PROP_TABLE)) return;
    try {
      const layer = new RefPropLayer(parseRefProps(await host.readProjectFile(project, REF_PROP_TABLE)));
      this.sceneBuilder?.setRefProps(layer);
      this.note(`reference: ${layer.table.count} static props, ${layer.table.models.length} models`);
    } catch (e) {
      this.note(`reference props: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async save(): Promise<void> {
    const store = this.requireStore();
    const text = store.save();
    if (!this.host) throw new EditorError("the host is not reachable from this page");
    if (!state.project) throw new EditorError("no project is open; use Save as to create one");
    const core = this.requireCore();
    // Every file the document references is written, not just map.json, or a
    // reopened project would have meshes and terrain with nothing behind them.
    for (const path of core.fileNames()) {
      const bytes = core.getFile(path);
      if (!bytes) continue;
      const hash = fileHash(bytes);
      if (this.savedFiles.get(path) === hash) continue;
      await this.host.writeProjectFile(state.project, path, bytes);
      this.savedFiles.set(path, hash);
    }
    await this.host.writeProjectFile(state.project, "map.json", new TextEncoder().encode(text));
    store.clearDirty();
    this.syncFlags();
    this.note(`saved ${state.project}`);
  }

  async saveAs(name: string): Promise<void> {
    const host = this.requireHost();
    await host.createProject(name);
    setProjectInfo(name, state.stem);
    // The new directory holds none of the files yet.
    this.savedFiles = new Map();
    await this.save();
    this.projects = await host.projects();
  }

  /** Swaps in another document and drops everything tied to the old one. */
  private replaceStore(store: DocumentStore, project: string): void {
    this.store = store;
    this.sceneBuilder = new SceneBuilder(this.source());
    this.markDirty();
    this.modelCache.clear();
    this.savedFiles = new Map();
    // A new document ends any edit session; the core dropped it with the old one.
    this.meshEdit = null;
    this.savedShading = null;
    this.lastPreview = null;
    this.bumpSession();
    clearSelection();
    setIsolate(false);
    this.syncFlags();
    setProjectInfo(project, store.meta.stem);
  }

  private requireHost(): HostClient {
    if (!this.host) throw new EditorError("the host is not reachable from this page");
    return this.host;
  }

  // --- assets ---

  async searchAssets(query: string, kind: string, offset = 0, limit = 40): Promise<void> {
    if (!this.host) return;
    const result = await this.host.assetSearch(query, ASSET_KIND_CODES[kind] ?? kind, offset, limit);
    this.assetTotal = result.total;
    // The index answers in type codes; the panels and placement speak names.
    this.assetResults = result.items.map((item) => ({ ...item, kind: assetKindName(item.kind) }));
    bumpPanels();
  }

  /** Downloads and parses a model preview, caching the parsed geometry. */
  async loadModel(path: string, quiet = false, failures?: string[]): Promise<ModelMesh | null> {
    if (this.modelCache.has(path)) return this.modelCache.get(path) ?? null;
    if (!this.host) {
      this.modelCache.set(path, null);
      return null;
    }
    let parsed: ModelMesh | null = null;
    try {
      const bytes = await this.host.assetModel(path);
      parsed = modelMesh(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer);
    } catch (e) {
      const why = `model ${path}: ${e instanceof Error ? e.message : String(e)}`;
      if (failures) failures.push(why);
      else this.note(why);
    }
    this.modelCache.set(path, parsed);
    // A batch load publishes its arrivals together; each bump rebuilds the scene.
    if (!quiet) this.modelVersion++;
    return parsed;
  }

  /**
   * Object-space wire edges of a cached asset model, through the core's
   * triangle-soup suppression. Reference props share these per model path.
   */
  wireOfModel(path: string): WireTable | null {
    const model = this.modelCache.get(path) ?? null;
    const core = this.core;
    if (!model || !core) return null;
    try {
      let vTotal = 0;
      let iTotal = 0;
      for (const p of model.parts) {
        vTotal += p.positions.length;
        iTotal += p.indices.length;
      }
      const positions = new Float32Array(vTotal);
      const indices = new Uint32Array(iTotal);
      let vOff = 0;
      let iOff = 0;
      for (const p of model.parts) {
        positions.set(p.positions, vOff);
        const base = vOff / 3;
        for (let i = 0; i < p.indices.length; i++) indices[iOff + i] = p.indices[i] + base;
        vOff += p.positions.length;
        iOff += p.indices.length;
      }
      const raw = core.soupEdges(positions, indices);
      return saneEdgeTable(raw) ? raw : null;
    } catch {
      return null;
    }
  }
  /**
   * Fetches every model the open document and the reference props name that
   * is not cached yet. The host exports them a batch at a time, one pass per
   * pak, while the models of the batch before download, so a shipped map's
   * props appear as they arrive; the most used models come first.
   */
  private async loadDocumentModels(): Promise<void> {
    const store = this.store;
    const host = this.host;
    if (!store || !host) return;
    const wanted = new Set<string>();
    for (const o of store.snapshot().objects.values()) {
      if (o.kind.type === "prop" && !this.modelCache.has(o.kind.prop.model)) wanted.add(o.kind.prop.model);
    }
    for (const model of this.sceneBuilder?.refProps?.pendingModels() ?? []) {
      if (model && !this.modelCache.has(model)) wanted.add(model);
    }
    const sky = store.meta.sky?.trim();
    if (sky && !this.modelCache.has(sky)) wanted.add(sky);
    const all = [...wanted];
    const ready: string[] = [];
    const missing: string[] = [];
    const failures: string[] = [];
    let exporting = true;
    let waiters: (() => void)[] = [];
    const wake = (): void => {
      const w = waiters;
      waiters = [];
      for (const f of w) f();
    };
    const exporter = async (): Promise<void> => {
      try {
        for (let i = 0; i < all.length && this.store === store; i += PREFETCH_BATCH) {
          const batch = all.slice(i, i + PREFETCH_BATCH);
          const unknown = new Set<string>();
          try {
            for (const m of (await host.prefetchModels(batch)).missing) unknown.add(m);
          } catch (e) {
            this.note(`model export: ${e instanceof Error ? e.message : String(e)}`);
          }
          for (const path of batch) {
            if (unknown.has(path)) {
              missing.push(path);
              this.modelCache.set(path, null);
            } else {
              ready.push(path);
            }
          }
          wake();
        }
      } finally {
        exporting = false;
        wake();
      }
    };
    let arrived = false;
    const worker = async (): Promise<void> => {
      for (;;) {
        if (this.store !== store) return;
        const path = ready.shift();
        if (path === undefined) {
          if (!exporting) return;
          await new Promise<void>((resolve) => waiters.push(resolve));
          continue;
        }
        await this.loadModel(path, true, failures);
        arrived = true;
      }
    };
    const publish = setInterval(() => {
      if (arrived) {
        arrived = false;
        this.modelVersion++;
      }
    }, MODEL_PUBLISH_MS);
    try {
      await Promise.all([exporter(), ...Array.from({ length: MODEL_FETCHES }, worker)]);
    } finally {
      clearInterval(publish);
      this.modelVersion++;
    }
    if (missing.length > 0) this.note(`${missing.length} models are not in the asset index, e.g. ${missing[0]}`);
    if (failures.length > 0) this.note(`${failures.length} models failed to load; first: ${failures[0]}`);
  }

  private materialRequests = new Map<string, Promise<MaterialAlbedo | null>>();
  /**
   * A browser opens six connections to the host and a texture export can wait
   * behind others; capping these keeps connections free for model downloads.
   */
  private materialSlots = new Slots(MATERIAL_FETCHES);

  /**
   * Resolves a document material name (`world/dev/dev_gray_512`) to its
   * albedo texture. Shipped materials carry a shader suffix on the pak asset,
   * so the common suffixes are tried in turn. Each name is looked up once, and a
   * name the indexed paks cannot answer is reported rather than dropped, so a
   * surface never renders as neutral grey without saying so.
   */
  resolveMaterialAlbedo(name: string): Promise<MaterialAlbedo | null> {
    const key = name.toLowerCase().split("\\").join("/");
    const hit = this.materialRequests.get(key);
    if (hit) return hit;
    const job = key
      ? new Promise<MaterialAlbedo | null>((resolve) => {
          this.materialQueue.push({ key, name, resolve });
          void this.pumpMaterials();
        })
      : Promise.resolve(null);
    this.materialRequests.set(key, job);
    return job;
  }

  private materialQueue: { key: string; name: string; resolve: (v: MaterialAlbedo | null) => void }[] = [];
  private materialPumping = false;

  /**
   * Sends queued material names to the host a batch at a time: one request
   * resolves, exports and caches a whole batch, one pass per pak, so the
   * texture downloads that follow are cache reads and run beside the next
   * batch.
   */
  private async pumpMaterials(): Promise<void> {
    if (this.materialPumping) return;
    this.materialPumping = true;
    try {
      while (this.materialQueue.length > 0) {
        // A frame asks for its materials one by one; let them gather.
        await new Promise((r) => setTimeout(r, MATERIAL_GATHER_MS));
        const batch = this.materialQueue.splice(0, MATERIAL_BATCH);
        const host = this.host;
        if (!host) {
          for (const b of batch) {
            this.warnMaterial(b.name, "the host is not reachable from this page");
            b.resolve(null);
          }
          continue;
        }
        let results: MaterialPrefetch[];
        try {
          results = await retried(() => host.prefetchMaterials(batch.map((b) => b.key), MATERIAL_SHADER_SUFFIXES));
        } catch (e) {
          for (const b of batch) {
            this.warnMaterial(b.name, `material export failed: ${e instanceof Error ? e.message : String(e)}`);
            b.resolve(null);
          }
          continue;
        }
        const byKey = new Map(results.map((r) => [r.name, r]));
        for (const b of batch) {
          const r = byKey.get(b.key);
          const albedo = r?.albedo;
          if (!r || !albedo) {
            this.warnMaterial(b.name, r?.error ?? "the host did not answer for it");
            b.resolve(null);
            continue;
          }
          void (async () => {
            await this.materialSlots.acquire();
            try {
              const bytes = await retried(() => host.assetTexture(albedo));
              b.resolve({ albedo, bytes, translucent: r.translucent });
            } catch (e) {
              this.warnMaterial(b.name, `albedo ${albedo}: ${e instanceof Error ? e.message : String(e)}`);
              b.resolve(null);
            } finally {
              this.materialSlots.release();
            }
          })();
        }
      }
    } finally {
      this.materialPumping = false;
    }
  }

  /** Records a material problem once, so the panels can list it. */
  warnMaterial(material: string, message: string, source: MaterialWarningSource = "host"): void {
    if (this.materialWarnings.some((w) => w.material === material && w.message === message)) return;
    this.materialWarnings = [...this.materialWarnings, { material, message, source }];
    bumpPanels();
  }

  /**
   * Replaces the renderer's own findings, which are recomputed from scratch
   * every time a material is compiled, and leaves the host's list alone.
   */
  setRendererMaterialWarnings(warnings: readonly { material: string; message: string }[]): void {
    const kept = this.materialWarnings.filter((w) => w.source !== "gpu");
    for (const w of warnings) kept.push({ material: w.material, message: w.message, source: "gpu" });
    // Called every frame, so the panels only hear about a real change.
    const same =
      kept.length === this.materialWarnings.length &&
      kept.every((w, i) => w.material === this.materialWarnings[i].material && w.message === this.materialWarnings[i].message && w.source === this.materialWarnings[i].source);
    if (same) return;
    this.materialWarnings = kept;
    bumpPanels();
  }

  /**
   * Asset, texture and job fetches outstanding, plus material batches still
   * queued behind the gather window. The control socket waits on this.
   */
  pendingLoads(): number {
    return (this.host?.pending() ?? 0) + this.materialQueue.length;
  }

  /** Downloads a texture by GUID; the caller registers it with the renderer. */
  async fetchTexture(guid: string): Promise<Uint8Array | null> {
    if (!this.host) return null;
    try {
      return await this.host.assetTexture(guid);
    } catch (e) {
      this.note(`texture ${guid}: ${e instanceof Error ? e.message : String(e)}`);
      return null;
    }
  }

  // --- jobs ---

  /** Starts a host job and polls it until it settles. */
  async startJob(kind: JobKind, params: Record<string, unknown>): Promise<string> {
    const host = this.requireHost();
    if (!state.project) throw new EditorError(`${kind}: open or create a project first`);
    // Every build step reads the project from disk, so unsaved edits go first.
    if (JOBS_READING_THE_PROJECT.has(kind) && this.store?.snapshot().dirty) await this.save();
    const { id } = await host.createJob(kind, state.project, await this.jobParams(kind, params));
    setState("jobs", "panelOpen", true);
    setState("jobs", "activeId", id);
    await this.pollJob(id);
    return id;
  }

  /** Fills in what the host requires of each job kind that the caller left out. */
  private async jobParams(kind: JobKind, params: Record<string, unknown>): Promise<Record<string, unknown>> {
    const out: Record<string, unknown> = { stem: this.store?.meta.stem ?? state.stem, ...params };
    if (kind === "compile" && !out.targets) out.targets = "client,dedi";
    if (kind === "deploy" && !Array.isArray(out.files)) out.files = await this.deployableFiles();
    return out;
  }

  /**
   * The build outputs a deploy may copy, relative to the build directory: the
   * host only places files under the directories the game reads from.
   */
  async deployableFiles(): Promise<string[]> {
    const files = await this.requireHost().projectFiles(state.project);
    const out = files
      .map((f) => f.path)
      .filter((p) => p.startsWith("build/"))
      .map((p) => p.slice("build/".length))
      .filter((p) => DEPLOY_DIRS.some((dir) => p.startsWith(`${dir}/`)));
    if (out.length === 0) throw new EditorError("deploy: the build has nothing to deploy yet; run Compile and Package first");
    return out;
  }

  /**
   * Imports a shipped map into a project of its own name and opens it: from a
   * picked client pak (its `_temp` partner comes along), or by stem from the
   * install. The project is created when it does not exist yet.
   */
  async importMap(req: { stem: string; pak?: string }): Promise<void> {
    const host = this.requireHost();
    const name = req.stem;
    if (!this.projects.some((p) => p.name === name)) {
      try {
        await host.createProject(name);
      } catch {
        // Already there: the import overwrites its document.
      }
    }
    const { id } = await host.createJob("import_map", name, req.pak ? { pak: req.pak } : { stem: name });
    setState("jobs", "panelOpen", true);
    setState("jobs", "activeId", id);
    this.note(`importing ${name}...`);
    for (;;) {
      await new Promise((r) => setTimeout(r, POLL_MS));
      const job = await host.job(id);
      upsertJob(job);
      if (job.state === "failed" || job.state === "cancelled") {
        const reason = job.log.trim().split("\n").pop() ?? "";
        throw new EditorError(`import ${name} ${job.state}${reason ? `: ${reason}` : ""}`);
      }
      if (job.state === "done") break;
    }
    this.projects = await host.projects();
    await this.openProject(name);
    this.note(`imported ${name}`);
  }

  private async pollJob(id: string): Promise<void> {
    if (!this.host) return;
    this.jobTimers.delete(id);
    let job: JobStatus;
    try {
      job = await this.host.job(id);
    } catch (e) {
      this.note(`job ${id}: ${e instanceof Error ? e.message : String(e)}`);
      return;
    }
    upsertJob(job);
    if (job.state === "done" || job.state === "failed" || job.state === "cancelled") {
      if (job.state === "failed") {
        const reason = job.log.trim().split("\n").pop() ?? "";
        this.note(`job ${job.kind} failed${reason ? `: ${reason}` : ""}`);
      } else this.note(`job ${job.kind} ${job.state}`);
      return;
    }
    this.schedulePoll(id);
  }

  private schedulePoll(id: string): void {
    const g = globalThis as unknown as { setTimeout?: (fn: () => void, ms: number) => number };
    if (!g.setTimeout || this.jobTimers.has(id)) return;
    this.jobTimers.set(id, g.setTimeout(() => void this.pollJob(id), POLL_MS));
  }

  async cancelJob(id: string): Promise<void> {
    if (!this.host) return;
    await this.host.cancelJob(id);
    // Polling carries on so the panel shows the job settle as cancelled.
  }

  /** Loads the host's recent job list into the panel. */
  async refreshJobs(): Promise<void> {
    if (!this.host) return;
    try {
      const jobs = await this.host.jobs();
      setState("jobs", "jobs", jobs);
      setState("jobs", "panelOpen", true);
      if (state.jobs.activeId === null && jobs.length > 0) setState("jobs", "activeId", jobs[0].id);
    } catch (e) {
      this.note(e instanceof Error ? e.message : String(e));
    }
  }

  // --- live link ---

  /** Connects the live socket; messages are applied to the in-game world. */
  connectLive(socketFactory?: (url: string) => SocketLike): void {
    if (!this.host) return;
    this.live = new LiveLink({
      url: liveUrl("", this.token ?? "", globalThis.location?.origin),
      socketFactory: socketFactory ?? ((u) => new WebSocket(u) as unknown as SocketLike),
      onStatus: (status: LiveStatusLabel, detail) => setLive({ status, detail: detail ?? "" }),
      onMessage: (message: LiveMessage) => this.onLiveMessage(message),
    });
    this.live.connect();
    setLive({ enabled: true });
  }

  disconnectLive(): void {
    this.live?.close();
    this.live = null;
    setLive({ enabled: false, status: "off", detail: "" });
  }

  /** Mirrors a committed prop transform to the running game. */
  private onLiveMessage(_message: LiveMessage): void {
    this.note("live link updated the running map");
  }

  /** Sends one object to the game, or removes it when it left the document. */
  mirrorTransform(id: Id): void {
    if (!this.live || this.live.status !== "open") return;
    const store = this.store;
    if (!store) return;
    const object = store.object(id);
    if (!object) {
      this.live.send({ op: "delete", id });
      this.liveSeen.delete(id);
      return;
    }
    if (object.kind.type !== "prop") return;
    const angles = sourceAnglesFromQuat(object.transform.rotation);
    const message: LiveMessage = {
      op: "prop",
      id,
      model: object.kind.prop.model,
      origin: object.transform.position,
      angles,
      scale: object.transform.scale,
    };
    if (this.liveSeen.has(id)) {
      this.live.send({ op: "move", id, origin: object.transform.position, angles });
    } else {
      this.live.send(message);
      this.liveSeen.add(id);
    }
  }

  // --- selection ---

  selectAll(): void {
    const store = this.requireStore();
    select(store.ids().filter((id) => !store.object(id)?.locked));
  }

  toggleSelect(id: Id): void {
    addToSelection(id);
  }

  clear(): void {
    clearSelection();
  }

  setHover(id: Id | null): void {
    setHover(id);
  }

  isolate(ids: Id[]): void {
    setIsolate(ids.length > 0, ids);
  }

  // --- scene ---

  private source(): SceneSource {
    if (this.sourceCache) return this.sourceCache;
    const core = this.core;
    const builder = this;
    const made: SceneSource = {
      objectMesh(id: Id): MeshBuffers | null {
        if (!core) return null;
        const hit = builder.meshCache.get(id);
        if (hit !== undefined) return hit;
        const mesh = core.objectMesh(id);
        builder.meshCache.set(id, mesh);
        return mesh;
      },
      objectEdges(id: Id): WireTable | null {
        if (!core) return null;
        const hit = builder.edgeCache.get(id);
        if (hit !== undefined) return hit;
        let wire: WireTable | null = null;
        try {
          const raw = core.objectEdges(id);
          wire = saneEdgeTable(raw) ? raw : null;
        } catch {
          wire = null;
        }
        builder.edgeCache.set(id, wire);
        return wire;
      },
      modelEdges(path: string): WireTable | null {
        if (!core) return null;
        const hit = builder.modelEdgeCache.get(path);
        if (hit !== undefined) return hit;
        const wire = builder.wireOfModel(path);
        builder.modelEdgeCache.set(path, wire);
        return wire;
      },
      model(path: string): ModelMesh | null {
        return builder.modelCache.get(path) ?? null;
      },
      bounds(object: DocObject, world: Mat4): Aabb | null {
        return builder.boundsOf(object, world);
      },
    };
    this.sourceCache = made;
    return made;
  }

  /** World bounds of an object, from its mesh, its model or its own points. */
  boundsOf(object: DocObject, world: Mat4): Aabb | null {
    const cached = this.boundsCache.get(object.id);
    if (cached) return cached;
    let result: Aabb | null = null;
    const kind = object.kind;
    if (kind.type === "prop" || (kind.type === "mesh" && !isEditableMesh(object))) {
      const model = this.modelCache.get(kind.type === "prop" ? kind.prop.model : kind.mesh.source);
      if (model) result = instanceBounds(model.bounds, world);
    } else if (kind.type === "light") {
      const r = Math.max(8, kind.light.range);
      const p: Vec3 = [world[12], world[13], world[14]];
      result = { min: [p[0] - r, p[1] - r, p[2] - r], max: [p[0] + r, p[1] + r, p[2] + r] };
    } else if (kind.type === "path") {
      let acc = emptyAabb();
      for (const point of kind.path.points) {
        const p: Vec3 = [
          world[0] * point[0] + world[4] * point[1] + world[8] * point[2] + world[12],
          world[1] * point[0] + world[5] * point[1] + world[9] * point[2] + world[13],
          world[2] * point[0] + world[6] * point[1] + world[10] * point[2] + world[14],
        ];
        acc = unionAabb(acc, { min: p, max: p });
      }
      result = isEmptyAabb(acc) ? null : acc;
    } else if (this.core) {
      const mesh = this.source().objectMesh(object.id);
      if (mesh && mesh.positions.length >= 3) {
        const local: Aabb = boundsOf(mesh.positions);
        const baked = kind.type === "brush" || kind.type === "volume" || kind.type === "terrain" || kind.type === "voxel_terrain" || kind.type === "mesh";
        result = baked ? local : instanceBounds(local, world);
      }
    }
    if (result) this.boundsCache.set(object.id, result);
    return result;
  }

  /** Snapshot of the document as scene objects, in draw order. */
  /** The shipped-map record an object was decompiled from, if any. */
  referenceRecord(id: Id): RefRecord | undefined {
    const store = this.store;
    if (!store) return undefined;
    return referenceRecord([...store.snapshot().objects.values()], id);
  }

  listSceneObjects(): SceneObject[] {
    const store = this.store;
    if (!store) return [];
    // Composing 10k+ transforms and bounds every frame is the viewport's cost;
    // the list only changes with the document, the loaded models or isolate.
    const key = [store.snapshot().doc, this.modelVersion, this.revisionCounter, state.selection.isolate, state.selection.isolated];
    if (this.sceneObjects.length > 0 && key.length === this.sceneListKey.length && key.every((v, i) => v === this.sceneListKey[i])) {
      return this.sceneObjects;
    }
    this.sceneListKey = key;
    const objects = [...store.snapshot().objects.values()];
    const byId = new Map(objects.map((o) => [o.id, o]));
    const layerVisible = new Map(store.layers.map((l) => [l.id, l.visible]));
    const isolated = state.selection.isolate ? isolatedSet(objects, state.selection.isolated) : null;
    const out: SceneObject[] = [];
    for (const doc of objects) {
      const chain: Transform[] = [];
      let parent = doc.parent;
      const guard = new Set<Id>();
      while (parent && !guard.has(parent)) {
        guard.add(parent);
        const p = byId.get(parent);
        if (!p) break;
        chain.unshift(p.transform);
        parent = p.parent;
      }
      const world = composeWorld(doc.transform, chain);
      const layer = layerVisible.get(doc.layer);
      out.push({
        doc,
        world,
        worldBounds: this.boundsOf(doc, world),
        // Isolate hides everything but the isolated objects and their children,
        // in the viewports as well as in the outliner.
        hidden: layer === false || (isolated !== null && !isolated.has(doc.id)),
        ancestors: chain,
      });
    }
    return out;
  }

  /** Rebuilds the scene; called once per frame when something changed. */
  rebuildScene(options: SceneOptions): Scene {
    if (!this.sceneBuilder) {
      this.scene = emptyScene();
      return this.scene;
    }
    this.sceneObjects = this.listSceneObjects();
    this.sceneBuilder.setSky(this.store?.meta.sky ?? null);
    this.scene = this.sceneBuilder.build(this.sceneObjects, options);
    return this.scene;
  }

  currentObjects(): SceneObject[] {
    return this.sceneObjects;
  }

  defaultSceneOptions(): SceneOptions {
    return { ...defaultSceneOptions(), invalidate: new Set(), selection: new Set(), hover: null };
  }

  /** The document as the UI sees it. */
  document(): MapDoc | null {
    return this.store ? this.store.snapshot().doc : null;
  }
}

/** The isolated ids plus every descendant of them. */
function isolatedSet(objects: readonly DocObject[], ids: readonly Id[]): Set<Id> {
  const out = new Set<Id>(ids);
  let grew = true;
  while (grew) {
    grew = false;
    for (const o of objects) {
      if (!out.has(o.id) && o.parent !== null && out.has(o.parent)) {
        out.add(o.id);
        grew = true;
      }
    }
  }
  return out;
}

/** A fresh id for an object or a layer: the core only takes uuids. */
export function newId(): string {
  return newUuid();
}

/** A fresh object id, matching the uuid shape the document stores. */
function newUuid(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (typeof g.crypto?.randomUUID === "function") return g.crypto.randomUUID();
  let out = "";
  for (let i = 0; i < 32; i++) out += Math.floor(Math.random() * 16).toString(16);
  return `${out.slice(0, 8)}-${out.slice(8, 12)}-4${out.slice(13, 16)}-8${out.slice(17, 20)}-${out.slice(20, 32)}`;
}

function identityTransform(): Transform {
  return { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] };
}

function addOffset(p: Vec3, by: number): Vec3 {
  return [p[0] + by, p[1] + by, p[2]];
}

/** A deep-enough copy of an object with a new id, for duplication. */
function reid(object: DocObject): DocObject {
  return {
    ...object,
    id: newUuid(),
    name: `${object.name}_copy`,
    parent: object.parent,
    transform: {
      position: [...object.transform.position] as Vec3,
      rotation: [...object.transform.rotation] as [number, number, number, number],
      scale: [...object.transform.scale] as Vec3,
    },
    kind: structuredCloneKind(object),
  };
}

function structuredCloneKind(object: DocObject): DocObject["kind"] {
  const json = JSON.stringify(object.kind);
  return JSON.parse(json) as DocObject["kind"];
}

function emptyScene(): Scene {  return {
    chunks: [],
    instances: [],
    byId: new Map(),
    bvh: { nodes: [], order: [], boxes: [] },
    bvhBoxes: [],
    grid: { positions: new Float32Array(0), weight: new Float32Array(0), count: 0 },
    overlay: { lines: { positions: new Float32Array(0), colors: new Float32Array(0), count: 0 }, boxes: [], billboards: [] },
    wire: [],
    refs: null,
    sky: null,
    version: 0,
  };
}

function transformPositions(positions: Float32Array, m: Float32Array): Float32Array {
  const out = new Float32Array(positions.length);
  for (let i = 0; i + 2 < positions.length; i += 3) {
    const x = positions[i];
    const y = positions[i + 1];
    const z = positions[i + 2];
    out[i] = m[0] * x + m[4] * y + m[8] * z + m[12];
    out[i + 1] = m[1] * x + m[5] * y + m[9] * z + m[13];
    out[i + 2] = m[2] * x + m[6] * y + m[10] * z + m[14];
  }
  return out;
}

/** How many model downloads an opened map keeps in flight at once. */
const MODEL_FETCHES = 4;
/** Models per host export pass; one pass decompresses each pak it touches once. */
const PREFETCH_BATCH = 256;
/** How many material lookups run at once; see `materialSlots`. */
const MATERIAL_FETCHES = 4;
/** Material names per host request; one request exports each pak it touches once. */
const MATERIAL_BATCH = 4096;
/** How long a new material waits for others of the same frame to join its batch. */
const MATERIAL_GATHER_MS = 50;

/** A counting semaphore for async work. */
class Slots {
  private waiting: (() => void)[] = [];
  constructor(private free: number) {}
  async acquire(): Promise<void> {
    if (this.free > 0) {
      this.free--;
      return;
    }
    await new Promise<void>((resolve) => this.waiting.push(resolve));
  }
  release(): void {
    const next = this.waiting.shift();
    if (next) next();
    else this.free++;
  }
}
/** How often a batch load lets the scene pick up the models that arrived. */
const MODEL_PUBLISH_MS = 400;

/** A core edge table with every pair indexing a real position. */
function saneEdgeTable(w: WireTable | null | undefined): w is WireTable {
  if (!w || !(w.positions instanceof Float32Array) || !(w.edges instanceof Uint32Array)) return false;
  if (w.edges.length === 0 || w.edges.length % 2 !== 0) return false;
  for (let i = 0; i < w.edges.length; i++) {
    if (w.edges[i] * 3 + 2 >= w.positions.length) return false;
  }
  return true;
}

/** A glb's primitives flattened into model parts, with their bounds. */
function modelMesh(buffer: ArrayBuffer): ModelMesh | null {
  const gltf = parseGltf(buffer);
  const parts: ModelPart[] = [];
  let min: Vec3 = [Infinity, Infinity, Infinity];
  let max: Vec3 = [-Infinity, -Infinity, -Infinity];
  for (const node of gltf.nodes) {
    if (node.mesh < 0 || node.mesh >= gltf.meshes.length) continue;
    for (const prim of gltf.meshes[node.mesh].primitives) {
      const material = gltf.materials[prim.material]?.name || DEFAULT_MATERIAL;
      const part: ModelPart = {
        positions: transformPositions(prim.positions, node.matrix),
        normals: prim.normals ?? new Float32Array(prim.positions.length),
        uvs: prim.uvs ?? new Float32Array((prim.positions.length / 3) * 2),
        indices: prim.indices,
        material,
      };
      parts.push(part);
      const b = boundsOf(part.positions);
      min = [Math.min(min[0], b.min[0]), Math.min(min[1], b.min[1]), Math.min(min[2], b.min[2])];
      max = [Math.max(max[0], b.max[0]), Math.max(max[1], b.max[1]), Math.max(max[2], b.max[2])];
    }
  }
  return parts.length > 0 && !(min[0] > max[0]) ? { parts, bounds: { min, max } } : null;
}
