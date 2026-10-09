import { createStore, produce } from "solid-js/store";
import type { Id } from "../core/api";
import { type CameraState, defaultCamera } from "../render/camera";
import { type GizmoMode, type GizmoSpace, type SnapSettings, defaultSnap } from "../render/gizmo";
import { DEFAULT_BRUSH_HEIGHT, brushHeight } from "../render/boxdrag";
import { DEFAULT_MATERIAL } from "../doc/model";
import type { LiveStatusLabel } from "../host/live";
import { defaultStroke, type SculptTool } from "../terrain/sculpt";
import type { JobStatus } from "../host/client";
import type { InputPrefs } from "../input/keymap";
import { loadInputPrefs } from "../input/prefs";
import { defaultShading, type ShadingState } from "../render/shading";

export type ToolId =
  | "select" | "move" | "rotate" | "scale"
  | "brush" | "clip" | "vertex" | "edge" | "face"
  | "terrain_sculpt" | "terrain_paint" | "terrain_voxel" | "entity";

export interface ToolState {
  id: ToolId;
  gizmo: GizmoMode;
  /** The brush the viewport and the terrain inspector both read. */
  sculpt: {
    brush: SculptTool;
    radius: number;
    strength: number;
    /** 0 is a flat disc, 1 is a smoothstep cone. */
    falloff: number;
    pressure: number;
    /** Splat layer the paint tool writes. */
    layer: number;
  };
  /** Dimensions and material the brush tool gives a new box. */
  brush: BrushOptions;
  /** The class a click with the entity tool places. */
  entityClass: string;
}

export type DialogId =
  | "project" | "map-settings" | "import" | "new-terrain" | "new-mesh" | "prefab" | "shortcuts" | "about";

export interface BrushOptions {
  /** Thickness along the axis a drag cannot reach. */
  height: number;
  /** Plan size a click without a drag falls back to. */
  width: number;
  depth: number;
  material: string;
}

export function defaultBrushOptions(): BrushOptions {
  return { height: DEFAULT_BRUSH_HEIGHT, width: 64, depth: 64, material: DEFAULT_MATERIAL };
}

export interface UiState {
  theme: "dark" | "light";
  layout: "one" | "four";
  viewports: { kind: "persp" | "top" | "front" | "side" | "right" | "back" | "bottom"; active: boolean }[];
  showGrid: boolean;
  showGizmo: boolean;
  showLightGizmos: boolean;
  showEntityBillboards: boolean;
  showVolumes: boolean;
  showPaths: boolean;
  showLightmaps: boolean;
  showToolbar: boolean;
  /** Viewport shading: textured, solid or wireframe, plus x-ray and overlay. */
  shading: ShadingState;
  dialog: DialogId | null;
  status: string;
  /** The viewport under the pointer, which receives viewport keys. */
  hoverView: number | null;
  /** A viewport maximized out of the quad layout, restored by the same key. */
  maximized: number | null;
}

export type PivotMode = "median" | "bbox" | "active" | "individual" | "cursor";

export type Falloff = "smooth" | "sphere" | "root" | "sharp" | "linear" | "constant";

export interface ProportionalState {
  enabled: boolean;
  /** Influence radius in world units. */
  radius: number;
  falloff: Falloff;
}

/** A modal operator in progress: it owns the keys and the status bar until it ends. */
export interface ModalState {
  name: string;
  /** Live readout, e.g. "Dx 64  Dy 0  Dz 0 along global X". */
  header: string;
  hints: [string, string][];
}

export interface SelectionState {
  ids: Id[];
  primary: Id | null;
  isolated: Id[];
  hover: Id | null;
  isolate: boolean;
}

export interface JobState {
  jobs: JobStatus[];
  activeId: string | null;
  panelOpen: boolean;
}

export interface LiveState {
  enabled: boolean;
  status: LiveStatusLabel;
  detail: string;
}

export interface EditorState {
  camera: CameraState;
  tool: ToolState;
  ui: UiState;
  selection: SelectionState;
  jobs: JobState;
  live: LiveState;
  snap: SnapSettings;
  gizmoSpace: GizmoSpace;
  input: InputPrefs;
  modal: ModalState | null;
  /** What rotate and scale turn about. */
  pivot: PivotMode;
  /** The transform orientation axis keys lock to first. */
  orientation: "global" | "local" | "view";
  proportional: ProportionalState;
  dirty: boolean;
  /** Bumped after every document change so views that mirror the core refresh. */
  docRevision: number;
  canUndo: boolean;
  canRedo: boolean;
  fps: number;
  cursorWorld: [number, number, number] | null;
  project: string;
  stem: string;
  /**
   * Bumped when an editor list the panels read changes: the search results,
   * the message log and the material warnings live on the editor, outside
   * the store, so this is what makes those panels re-render.
   */
  assets: { revision: number };
}


export const [state, setState] = createStore<EditorState>({
  camera: defaultCamera(),
  tool: {
    id: "select",
    gizmo: "translate",
    sculpt: {
      brush: "raise",
      radius: defaultStroke().radius,
      strength: defaultStroke().strength,
      falloff: defaultStroke().falloff,
      pressure: 1,
      layer: 0,
    },
    brush: defaultBrushOptions(),
    entityClass: "info_target",
  },
  ui: {
    theme: "dark",
    layout: "one",
    viewports: [
      { kind: "persp", active: true },
      { kind: "top", active: false },
      { kind: "front", active: false },
      { kind: "side", active: false },
    ],
    showGrid: true,
    showGizmo: true,
    showLightGizmos: true,
    showEntityBillboards: true,
    showVolumes: true,
    showPaths: true,
    showLightmaps: false,
    showToolbar: true,
    shading: defaultShading(),
    dialog: null,
    status: "",
    hoverView: null,
    maximized: null,
  },
  selection: { ids: [], primary: null, isolated: [], hover: null, isolate: false },
  jobs: { jobs: [], activeId: null, panelOpen: false },
  live: { enabled: false, status: "off", detail: "" },
  snap: defaultSnap(),
  gizmoSpace: "world",
  input: loadInputPrefs(),
  modal: null,
  pivot: "median",
  orientation: "global",
  proportional: { enabled: false, radius: 512, falloff: "smooth" },
  dirty: false,
  docRevision: 0,
  canUndo: false,
  canRedo: false,
  fps: 0,
  cursorWorld: null,
  project: "",
  stem: "mp_rr_untitled",
  assets: { revision: 0 },
});

/** Tells the panels that read editor-held lists to re-render. */
export function bumpPanels(): void {
  setState("assets", "revision", (r) => r + 1);
}

/** Refreshes views of the edit session without dirtying the document. */
export function touchMeshEdit(): void {
  setState("docRevision", (r) => r + 1);
}

export function setCamera(patch: Partial<CameraState>): void {
  setState("camera", produce((c) => Object.assign(c, patch)));
}

export function setTool(id: ToolId): void {
  setState("tool", "id", id);
  if (id === "move") setState("tool", "gizmo", "translate");
  if (id === "rotate") setState("tool", "gizmo", "rotate");
  if (id === "scale") setState("tool", "gizmo", "scale");
}

export function setUi(patch: Partial<UiState>): void {
  setState("ui", produce((u) => Object.assign(u, patch)));
}

/** Merges a patch into the viewport shading state. */
export function setShading(patch: Partial<ShadingState>): void {
  setState("ui", "shading", produce((s) => Object.assign(s, patch)));
}

export function toggleTheme(): void {
  setState("ui", "theme", (t) => (t === "dark" ? "light" : "dark"));
}

export function select(ids: Id[]): void {
  setState("selection", "ids", [...ids]);
  setState("selection", "primary", ids.length ? ids[ids.length - 1] : null);
}

export function addToSelection(id: Id): void {
  if (state.selection.ids.includes(id)) {
    setState("selection", "ids", state.selection.ids.filter((x) => x !== id));
    if (state.selection.primary === id) setState("selection", "primary", null);
    return;
  }
  setState("selection", "ids", [...state.selection.ids, id]);
  setState("selection", "primary", id);
}

export function clearSelection(): void {
  setState("selection", "ids", []);
  setState("selection", "primary", null);
}

export function setHover(id: Id | null): void {
  setState("selection", "hover", id);
}

export function setIsolate(enabled: boolean, ids: Id[] = []): void {
  setState("selection", "isolate", enabled);
  setState("selection", "isolated", ids);
}

export function upsertJob(job: JobStatus): void {
  setState("jobs", "jobs", (jobs) => [...jobs.filter((j) => j.id !== job.id), job]);
}

export function setLive(patch: Partial<LiveState>): void {
  setState("live", produce((l) => Object.assign(l, patch)));
}

export function setSnap(patch: Partial<SnapSettings>): void {
  setState("snap", produce((s) => Object.assign(s, patch)));
}

export function setSculpt(patch: Partial<EditorState["tool"]["sculpt"]>): void {
  setState("tool", "sculpt", produce((s) => Object.assign(s, patch)));
}

export function setBrush(patch: Partial<BrushOptions>): void {
  const next: Partial<BrushOptions> = { ...patch };
  if (patch.height !== undefined) next.height = brushHeight(patch.height, state.tool.brush.height);
  setState("tool", "brush", produce((b) => Object.assign(b, next)));
}

export function setLayout(layout: "one" | "four"): void {
  setState("ui", "layout", layout);
  setState("ui", "viewports", (vps) => vps.map((v, i) => ({ ...v, active: layout === "four" || i === 0 })));
}

export function setViewportKind(index: number, kind: EditorState["ui"]["viewports"][number]["kind"]): void {
  setState("ui", "viewports", index, "kind", kind);
}

export function openDialog(id: DialogId | null): void {
  setState("ui", "dialog", id);
}

export function setDocumentFlags(dirty: boolean, canUndo: boolean, canRedo: boolean): void {
  setState("docRevision", (r) => r + 1);
  setState("dirty", dirty);
  setState("canUndo", canUndo);
  setState("canRedo", canRedo);
}

export function setFps(fps: number): void {
  setState("fps", Math.max(0, Math.round(fps)));
}

export function setCursorWorld(p: [number, number, number] | null): void {
  setState("cursorWorld", p);
}

export function setProjectInfo(project: string, stem: string): void {
  setState("project", project);
  setState("stem", stem);
}

export function setModal(modal: ModalState | null): void {
  setState("modal", modal);
}

export function setInputPrefs(patch: Partial<InputPrefs>): void {
  setState("input", produce((p) => Object.assign(p, patch)));
}
