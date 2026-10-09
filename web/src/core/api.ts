// Contract between the editor UI and the wasm core (crates/r5ed-wasm).
// JSON shapes mirror crates/rdoc (serde, snake_case).

export type Id = string;
export type Vec3 = [number, number, number];
export type Quat = [number, number, number, number];

export interface MeshBuffers {
  positions: Float32Array; // xyz
  normals: Float32Array; // xyz
  uvs: Float32Array; // uv
  indices: Uint32Array;
  /** One draw range per material. */
  ranges: { material: string; start: number; count: number }[];
}

export interface CompileResult {
  ok: boolean;
  /** Per target: sidecar files (name -> bytes) plus .ent partitions. */
  client?: Record<string, Uint8Array>;
  dedi?: Record<string, Uint8Array>;
  report: { warnings: string[]; errors: string[]; stats: Record<string, number>; timings_ms: Record<string, number> };
}

export interface PickHit {
  id: Id;
  position: Vec3;
  normal: Vec3;
  distance: number;
}

/** Buffers the edit session draws from (world space; mirrors rdoc Buffers). */
export interface EditBuffers {
  verts: Float32Array;
  positions: Float32Array;
  normals: Float32Array;
  uvs: Float32Array;
  tris: Uint32Array;
  /** Polygon edges (never triangulation diagonals) as vertex-table pairs. */
  polyEdges: Uint32Array;
  faceCentres: Float32Array;
  vertSel: Uint8Array;
  edgeSel: Uint8Array;
  faceSel: Uint8Array;
  /** One byte per polygon edge: bit 0 sharp, bit 1 seam. */
  edgeFlags: Uint8Array;
  counts: [number, number, number];
  /** Changes exactly when the drawn topology does. */
  topoHash: string;
}

export interface Core {
  /** Loads a document (map.json text). Returns the normalized document JSON. */
  load(docJson: string): string;
  /** Serializes the current document. */
  save(): string;

  /** Applies an rdoc Command (JSON). Returns ids touched. */
  apply(commandJson: string): Id[];
  undo(): Id[];
  redo(): Id[];
  canUndo(): boolean;
  canRedo(): boolean;

  /** Object JSON by id; a missing object is `undefined` from wasm-bindgen, `null` from a fake. */
  object(id: Id): string | null | undefined;
  /** All object ids in draw order. */
  objectIds(): Id[];

  /** Triangulated render geometry of a brush, volume, path or terrain chunk, in world space. */
  objectMesh(id: Id): MeshBuffers | null;
  /**
   * Wireframe edges of a brush, volume or mesh in object space: polygon
   * outlines for brushes, polygon edges for editable meshes, and triangle
   * edges with coplanar diagonals removed for anything else. Null when the
   * object has no line drawing (terrains, entities, groups).
   */
  objectEdges(id: Id): { positions: Float32Array; edges: Uint32Array } | null;
  /**
   * Triangle soup to wireframe edges, as `objectEdges` draws them: welded
   * positions plus edge pairs with coplanar diagonals removed. Used for
   * reference and prop models, cached per mesh by the caller.
   */
  soupEdges(positions: Float32Array, indices: Uint32Array): { positions: Float32Array; edges: Uint32Array } | null;
  /** Ray pick against brushes, meshes, terrain and prop bounds. */
  pick(origin: Vec3, dir: Vec3, maxDist: number): PickHit | null;

  /** Brush tools; each returns the Command JSON to apply (so undo stays uniform). */
  brushBox(min: Vec3, max: Vec3, material: string): string;
  brushClip(ids: Id[], planeNormal: Vec3, planeDist: number, keep: "front" | "back" | "both"): string;
  brushHollow(id: Id, thickness: number): string;
  brushCarve(carver: Id, targets: Id[]): string;
  brushMergeConvex(ids: Id[]): string;
  brushExtrudeFace(id: Id, faceIndex: number, distance: number): string;
  brushMoveVertex(id: Id, vertex: Vec3, to: Vec3): string;
  /** Resizes a brush to `size` (W, D, H), keeping its min corner; returns the Command JSON. */
  brushResize(id: Id, size: Vec3): string;
  /** Object-space bounds `[minx, miny, minz, maxx, maxy, maxz]` of a brush. */
  brushBounds(id: Id): number[];

  /** Authored mesh: `requestJson` is `{spec: {shape, ...}, material?, name?, transform?, uv_scale?, collision?, editable?}`; returns the Command JSON. */
  meshCreate(requestJson: string): string;

  /** Runs one mesh op (`{op, object, sel, params}`); returns `{command, selection, report}` JSON to apply. */
  meshOp(requestJson: string): string;
  /** Makes a glb-sourced mesh editable; returns the Command JSON to apply. */
  meshMakeEditable(id: Id): string;
  /** Converts a brush to an editable mesh; returns the Command JSON to apply. */
  brushToMesh(id: Id): string;

  /** Loads the mesh into an edit session; returns its element counts JSON. */
  editBegin(objectId: Id): string;
  /** Leaves edit mode, committing nothing new. */
  editEnd(): void;
  /** Replaces the session selection (`{mode, ids?, query?}`); returns it. */
  editSelect(selJson: string): string;
  /** Draw buffers over the session mesh, in world space; null without a session. */
  editBuffers(): EditBuffers | null;
  /** CPU pick against the session mesh; `PickQuery` in, `PickReply` JSON out. */
  editPick(queryJson: string): string;
  /** Mutates the session for a drag without a command; `{selection, report}` out. */
  editPreview(opRequestJson: string): string;
  /** The one `SetFile` for the `.r5mesh` a drag release applies. */
  editCommit(): string;
  /** Hides the selected elements until revealed; session view state only. */
  editHide(): void;
  /** Reveals every hidden element. */
  editReveal(): void;

  /** Terrain: generate heights from a recipe (rterrain JSON). */
  terrainGenerate(recipeJson: string): Float32Array;
  /** Applies one sculpt stroke to a terrain object's heights; returns the Command JSON. */
  terrainSculpt(id: Id, strokeJson: string): string;
  /** Recomputes splat weights from auto-paint rules; returns the Command JSON. */
  terrainAutoPaint(id: Id, rulesJson: string): string;
  /** Evaluates scatter rules; returns the Command JSON adding the props. */
  terrainScatter(id: Id): string;
  /** Registers a project file the core may read (heights, weights, glb). */
  putFile(path: string, bytes: Uint8Array): void;
  getFile(path: string): Uint8Array | null | undefined;
  /** Every project file the core holds (meshes, heights, weights, prefabs). */
  fileNames(): string[];
  /** Drops the document, its history and every file; returns the empty document JSON. */
  reset(): string;

  /** Entity schema JSON (schema/entities.json). */
  schema(): string;
  /** Validates the document against the schema and compiler rules. */
  validate(): string; // JSON [{severity, id, message}]

  /** Full compile in the browser. assetInfoJson is /api/assets tables fetched by the UI. */
  compile(targetsJson: string, assetInfoJson: string): CompileResult;
  /** Low-quality lightmap preview for the viewport (per object lightmap RGBA8). */
  bakePreview(settingsJson: string): Record<Id, { width: number; height: number; rgba: Uint8Array }>;

  /** Imports */
  importVmf(text: string): string; // Command JSON
  importObj(text: string, optionsJson: string): string; // Command JSON
  importQuakeMap(text: string): string; // Command JSON
  importReMap(json: string): string; // Command JSON
  importGltf(path: string): string; // Command JSON (adds a Mesh object referencing the stored file)
}

export type CoreFactory = () => Promise<Core>;
