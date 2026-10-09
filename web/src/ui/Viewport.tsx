import { markTranslucent } from "../render/scene";
import { defaultSlot } from "../render/material";
import { type JSX, For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import type { Id, Vec3 } from "../core/api";
import type { BrushFace } from "../doc/model";
import {
  Camera,
  type CameraState,
  defaultCamera,
  eyeFor,
  flipView,
  fly,
  frameBounds,
  isPerspectiveState,
  orbit,
  pan,
  panScale,
  setAxisView,
  setOrtho,
  slide,
  viewAxis,
  viewForward,
  viewLabel,
  viewUp,
  VIEW_KINDS,
  type ViewKind,
  zoomAbout,
  zoomOrtho,
} from "../render/camera";
import { GpuError, initGpu, type GpuContext, unsupportedMessage } from "../render/device";
import { Renderer, type Lighting, defaultLighting, sceneBounds } from "../render/renderer";
import { type Scene, defaultSceneOptions, type SceneOptions, instancesInBox } from "../render/scene";
import { buildGlyphAtlas } from "../render/glyphs";
import type { GlyphAtlas } from "../render/atlas";
import { type GridSize, adaptiveGrid, snapToGrid } from "../render/grid";
import { dragBox } from "../render/boxdrag";
import { applyDrag, beginDrag, pickHandle, snapValue, type DragState, type GizmoHandle } from "../render/gizmo";
import { rayAxis, rayPlane, planeFromThree, type Ray } from "../math/ray";
import { brushEdges, brushVertices, faceAt, nearestOnScreen, segmentDistance } from "../render/brushedit";
import { invert, mat4, transformPoint, type Mat4 } from "../math/mat4";
import { add, cross, dot, normalize, scale, sub } from "../math/vec3";
import { type Quat, quatConjugate, quatFromAxisAngle, quatMul, quatRotate } from "../math/quat";
import { defaultStroke, strokeJson, type Stroke, type StrokePoint } from "../terrain/sculpt";
import { defaultLayerRule, rulesJson, type LayerRule } from "../terrain/splat";
import { setCamera, setCursorWorld, setFps, setModal, setState, state } from "../state/stores";
import { encodePng } from "../render/png";
import type { Editor } from "../state/editor";
import type { ToolId } from "../state/stores";
import type { CommandId } from "../input/keymap";
import { TRANSFORM_HINTS, type Orientation, type TransformMode, type TransformOp, axisMask, beginOp, describeOp, isTyping, pressAxis, typeKey, typedValues } from "../input/transform";
import { markPresented, presentedFrames, views } from "./viewRegistry";
import { EditOverlayCache } from "../render/editOverlay";
import { WIRE_LINE_BUDGET } from "../render/shading";
import { opRequest, pickQueryJson, selJson, type EditSelMode, type MeshEditState, type PickReply } from "../state/meshEdit";
import { MESH_OPS, type MeshOpKey, type OpSpec, modalParams } from "../state/meshOps";

/** Material paths whose albedo alpha is a cutout mask in game. */
const ALPHA_TESTED = /foliage|grass|leaf|leaves|tree|bush|plant|fern|ivy|vine|moss|weed|shrub|branch|cactus|decal|fence|chainlink|net/i;

export type PickMode = "set" | "extend" | "subtract";

export interface ViewportProps {
  editor: Editor;
  kind: ViewKind;
  index: number;
  active: boolean;
  onPick: (id: Id | null, mode: PickMode) => void;
  onBoxSelect: (ids: Id[], mode: PickMode) => void;
}

type Point = { x: number; y: number };

type Drag =
  | { mode: "orbit" | "pan" | "zoom" | "dolly" | "gizmo" | "box" | "select"; x: number; y: number; sx: number; sy: number; gizmo?: DragState; center?: boolean; pick?: PickMode }
  | { mode: "sculpt"; x: number; y: number; target: Id; points: StrokePoint[]; painted: Set<Id> }
  /** Corners of a brush dragged on the view plane through the first of them. */
  | { mode: "vertex"; x: number; y: number; target: Id; corners: Vec3[]; grab: Vec3; normal: Vec3 }
  /** A brush face pushed or pulled along its own normal. */
  | { mode: "face"; x: number; y: number; target: Id; face: number; origin: Vec3; normal: Vec3; start: number }
  /** Components of the edited mesh dragged on the view plane, previewed in the session. */
  | {
      mode: "mesh"; x: number; y: number; sx: number; sy: number; kind: "translate" | "rotate" | "scale";
      grab: Vec3; normal: Vec3; applied: Vec3; lastAngle: number; factor: number; startDist: number;
      base: { mode: EditSelMode; ids: number[] }; moved: boolean;
    };

type Modal =
  | {
      kind: "transform";
      op: TransformOp;
      pivot: Vec3;
      /** Where the pointer was when the op began, and the virtual pointer now. */
      start: Point;
      at: Point;
      /** Unwrapped screen angle for rotate. */
      angle: number;
      lastAngle: number;
      trackball: [number, number];
      ctrl: boolean;
      shift: boolean;
      /** The rotation of the active object, for local axes. */
      local: Quat;
      readout: Vec3 | number;
      /** Where a middle press began, to pick an axis by the drag direction. */
      mmbFrom: Point | null;
      /** Unselected objects a proportional edit carries, by distance from the selection. */
      neighbours: { id: Id; distance: number }[];
    }
  | { kind: "walk"; saved: CameraState; keys: Set<string>; speed: number; last: number; fast: boolean; slow: boolean; gravity: boolean; vz: number; grounded: boolean }
  | { kind: "box"; mode: PickMode; start: Point | null; at: Point }
  | { kind: "zoom"; start: Point | null; at: Point; out: boolean }
  | { kind: "lasso"; mode: PickMode; path: Point[] }
  | { kind: "circle"; radius: number; at: Point; painting: PickMode | null }
  /**
   * A modelling op whose params follow the pointer and wheel: it runs once on
   * the first change and re-runs through the redo path after that.
   */
  | {
      kind: "meshop";
      key: MeshOpKey;
      start: Point;
      at: Point;
      wheel: number;
      worldPerPx: number;
      widthPx: number;
      params: Record<string, unknown>;
      ran: boolean;
    };

/** The on-screen length of a gizmo arm. */
const GIZMO_PIXELS = 90;
/** How far one Alt+wheel notch moves the camera, in units. */
const WHEEL_STEP_UNITS = 64;
/** Pixels a press may travel and still count as a click. */
const CLICK_SLOP = 4;
/** One numpad orbit step. */
const ORBIT_STEP = Math.PI / 12;

/** The tools that edit a selected brush's corners, edges or faces. */
const BRUSH_EDIT_TOOLS: ReadonlySet<ToolId> = new Set<ToolId>(["vertex", "edge", "face"]);

/** The tools that draw and drag the transform gizmo. */
const GIZMO_TOOLS: ReadonlySet<ToolId> = new Set<ToolId>(["move", "rotate", "scale"]);

const AXIS_COLORS = ["#ff3352", "#8bdc00", "#2890ff"];
const AXES: Vec3[] = [
  [1, 0, 0],
  [0, 1, 0],
  [0, 0, 1],
];

/** Snaps every axis, for points picked on an ortho view plane. */
function snapAll(p: Vec3, size: number): Vec3 {
  const s = Math.max(1, size);
  return [Math.round(p[0] / s) * s, Math.round(p[1] / s) * s, Math.round(p[2] / s) * s];
}

function cameraFor(kind: ViewKind, shared: CameraState): CameraState {
  const c: CameraState = { ...shared, target: [...shared.target] as Vec3, eye: [...shared.eye] as Vec3, kind: "persp", ortho: false, autoOrtho: false };
  if (kind !== "persp") {
    setAxisView(c, kind);
    c.autoOrtho = false;
  }
  return c;
}

/** One viewport: canvas, Blender-style navigation, modal operators, brush tools and picking. */
export function Viewport(props: ViewportProps): JSX.Element {
  let canvas!: HTMLCanvasElement;
  let renderer: Renderer | null = null;
  let context: GpuContext | null = null;
  const camera = new Camera(cameraFor(props.kind, state.camera));
  const [error, setError] = createSignal<string | null>(null);
  const [drag, setDrag] = createSignal<Drag | null>(null);
  const [modal, setModalLocal] = createSignal<Modal | null>(null);
  const [loading, setLoading] = createSignal(true);
  const [clipPoints, setClipPoints] = createSignal<Vec3[]>([]);
  const [pointer, setPointer] = createSignal<Point | null>(null);
  // Bumped whenever the camera moves, so the SVG overlay re-projects.
  const [viewTick, setViewTick] = createSignal(0);
  const lighting = defaultLighting();
  let frameHandle = 0;
  let frames = 0;
  let fpsClock = 0;
  let lost = false;
  let lightmapRevision = -1;
  let atlas: GlyphAtlas | null = null;

  /** The quad layout's axis views keep their direction, as Blender's locked quad view does. */
  const locked = (): boolean => props.kind !== "persp" && state.ui.layout === "four";

  createEffect(
    on(
      () => props.kind,
      (kind) => {
        const next = cameraFor(kind, camera.state);
        Object.assign(camera.state, next);
        cameraMoved();
      },
      { defer: true },
    ),
  );

  const cameraMoved = (): void => {
    camera.markDirty();
    setViewTick((t) => t + 1);
  };

  /** How far the view reaches: the real eye distance in perspective, the visible height in ortho. */
  const viewDistance = (): number => {
    if (!isPerspectiveState(camera.state)) return camera.state.orthoHeight;
    const e = eyeFor(camera.state);
    const t = camera.state.target;
    return Math.hypot(e[0] - t[0], e[1] - t[1], e[2] - t[2]);
  };

  const sceneOptions = (): SceneOptions => {
    const base = defaultSceneOptions();
    const k = camera.state.kind;
    const shading = state.ui.shading;
    const edited = props.editor.meshEdit?.object ?? null;
    return {
      ...base,
      showGrid: state.ui.showGrid && (k === "persp" || k === "top" || k === "bottom"),
      showLightGizmos: state.ui.showLightGizmos,
      showEntityBillboards: state.ui.showEntityBillboards,
      showVolumes: state.ui.showVolumes,
      showPaths: state.ui.showPaths,
      showOutlines: state.ui.showGizmo,
      grid: adaptiveGrid(state.snap.size, viewDistance(), camera.state.target),
      atlas,
      lightmaps: lighting.showLightmaps ? props.editor.lightmaps : null,
      selection: new Set(state.selection.ids),
      hover: state.selection.hover,
      billboardScale: 64,
      gizmo: gizmoSpec(),
      markers: editMarkers(),
      editOverlay: meshOverlay(),
      wire: shading.mode === "wireframe"
        ? { eye: eyeFor(camera.state), budget: WIRE_LINE_BUDGET, only: null }
        : shading.mode === "solid" && shading.wireOverlay && edited !== null
          ? { eye: eyeFor(camera.state), budget: WIRE_LINE_BUDGET, only: edited }
          : null,
    };
  };

  /**
   * The gizmo's arm length in world units: about 90 pixels on screen at any
   * zoom. Drawing and picking share it, so a handle is grabbed where it shows.
   */
  const gizmoSize = (): number => panScale(camera.state, canvas.clientHeight) * GIZMO_PIXELS;

  /** The gizmo to draw this frame, sized so the handles keep their screen size. */
  const gizmoSpec = (): SceneOptions["gizmo"] => {
    if (!state.ui.showGizmo || !GIZMO_TOOLS.has(state.tool.id) || modal()) return null;
    const pivot = gizmoPivot();
    if (!pivot) return null;
    return { mode: state.tool.gizmo, space: state.gizmoSpace, pivot: pivot.pivot, rotation: pivot.rotation, size: gizmoSize(), active: null };
  };

  onMount(async () => {
    views.set(props.index, controller);
    try {
      const init = await initGpu(canvas);
      context = init.context;
      lost = false;
      void init.lost.then((message) => {
        lost = true;
        setError(unsupportedMessage(message));
      });
      renderer = new Renderer(context);
      const glyphs = document.createElement("canvas");
      atlas = buildGlyphAtlas(glyphs, state.ui.theme);
      renderer.setAtlas(atlas);
      setLoading(false);
      requestFrame();
    } catch (e) {
      setError(e instanceof GpuError ? e.message : unsupportedMessage(e instanceof Error ? e.message : String(e)));
      setLoading(false);
    }
  });

  onCleanup(() => {
    if (views.get(props.index) === controller) views.delete(props.index);
    if (frameHandle) cancelAnimationFrame(frameHandle);
    endModalListeners();
    renderer?.destroy();
  });

  const requestFrame = (): void => {
    frameHandle = requestAnimationFrame(frame);
  };

  // The perspective view's camera is kept in the store so it survives a
  // layout change; the axis views start from its target.
  const publishCamera = (): void => {
    cameraMoved();
    if (camera.state.kind !== "persp" && props.kind !== "persp") return;
    setCamera({
      target: [...camera.state.target] as Vec3,
      eye: [...eyeFor(camera.state)] as Vec3,
      distance: camera.state.distance,
      orthoHeight: camera.state.orthoHeight,
    });
  };

  /** The plane a pointer drag works on, which works in ortho views too. */
  const workPlane = (): { point: Vec3; normal: Vec3 } => {
    const forward = viewForward(camera.state);
    return { point: camera.state.target, normal: forward };
  };

  const onPlane = (ray: Ray, point: Vec3, normal: Vec3): Vec3 | null => {
    const t = rayPlane(ray, point, normal);
    if (t === null) return null;
    return [ray.origin[0] + ray.dir[0] * t, ray.origin[1] + ray.dir[1] * t, ray.origin[2] + ray.dir[2] * t];
  };

  /** The world point under a screen position, on the work plane. */
  const workPoint = (p: Point): Vec3 | null => {
    const plane = workPlane();
    return onPlane(rayAt(p), plane.point, plane.normal);
  };

  /** Copies the ui toggles onto the frame's lighting block. */
  const syncLighting = (): void => {
    lighting.showLightmaps = state.ui.showLightmaps;
  };

  /**
   * Bakes a preview once per document revision, so the atlas the shader
   * samples always matches the document the frame is drawing.
   */
  const syncLightmaps = (): void => {
    if (!renderer) return;
    if (!state.ui.showLightmaps) {
      if (lightmapRevision !== -1) {
        lightmapRevision = -1;
        renderer.setLightmapAtlas(null);
        props.editor.lightmaps = null;
      }
      return;
    }
    if (lightmapRevision === props.editor.revision) return;
    lightmapRevision = props.editor.revision;
    try {
      const packed = props.editor.previewLightmaps();
      renderer.setLightmapAtlas(packed);
      props.editor.note(packed ? `lightmap preview: ${packed.rects.size} objects` : "lightmap preview is empty");
    } catch (e) {
      lightmapRevision = -1;
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };

  const loadedMaterials = new Set<string>();

  /** Fetches the albedo texture of every material the scene draws, once each. */
  const requestMaterials = (scene: { chunks: { subRanges: { material: string }[] }[] }): void => {
    for (const chunk of scene.chunks) {
      for (const r of chunk.subRanges) {
        const name = r.material;
        if (!name || loadedMaterials.has(name)) continue;
        loadedMaterials.add(name);
        void props.editor.resolveMaterialAlbedo(name).then((hit) => {
          if (!hit || !renderer) return;
          try {
            renderer.addDdsTexture(hit.albedo, hit.bytes);
            // A blended effect (light shafts, glows) is shown as a faint tint.
            if (hit.translucent) markTranslucent(name);
            renderer.setMaterial({
              ...defaultSlot(name),
              baseColor: hit.translucent ? [1, 1, 1, 0.12] : [1, 1, 1, 1],
              albedoGuid: hit.albedo,
              alphaTest: ALPHA_TESTED.test(name),
            });
          } catch (e) {
            props.editor.note(`material ${name}: ${e instanceof Error ? e.message : String(e)}`);
          }
        });
      }
    }
  };

  const frame = (time: number): void => {
    requestFrame();
    if (lost) return;
    const m = modal();
    if (m?.kind === "walk") walkStep(m, time);
    syncLighting();
    const dpr = Math.min(2, globalThis.devicePixelRatio || 1);
    const width = Math.max(1, Math.floor(canvas.clientWidth * dpr));
    const height = Math.max(1, Math.floor(canvas.clientHeight * dpr));
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
      setViewTick((t) => t + 1);
    }
    camera.setAspect(width / height);
    const matrices = camera.matrices();
    syncLightmaps();
    const options = sceneOptions();
    lighting.gridFade = options.grid.fadeDistance;
    const scene = props.editor.rebuildScene(options);
    requestMaterials(scene);
    renderer?.render(scene, { ...matrices, eye: eyeFor(camera.state), width, height }, lighting, state.ui.shading);
    if (renderer) markPresented();
    if (renderer) props.editor.setRendererMaterialWarnings(renderer.materials.warnings);
    frames++;
    if (time - fpsClock > 500) {
      setFps((frames * 1000) / Math.max(1, time - fpsClock));
      frames = 0;
      fpsClock = time;
    }
  };

  const localPoint = (e: MouseEvent | PointerEvent): Point => {
    const rect = canvas.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  };

  const rayAt = (p: Point) => camera.pickRay(p.x, p.y, canvas.clientWidth, canvas.clientHeight);

  /** X-ray amount for surface picks, so picking sees through what the shading shows through. */
  const xrayPick = (): number => state.ui.shading.xray;

  const gizmoPivot = (): { pivot: Vec3; rotation: [number, number, number, number] } | null => {
    const id = state.selection.primary;
    if (!id) return null;
    const object = props.editor.store?.object(id);
    if (!object) return null;
    return { pivot: object.transform.position, rotation: object.transform.rotation };
  };

  /** The faces of the selected brush, when the tool edits brush geometry. */
  const editedBrush = (): { id: Id; faces: BrushFace[] } | null => {
    if (!BRUSH_EDIT_TOOLS.has(state.tool.id)) return null;
    const id = state.selection.primary;
    const object = id ? props.editor.store?.object(id) : null;
    if (!object || object.kind.type !== "brush") return null;
    return { id: object.id, faces: object.kind.brush.faces };
  };

  /** Corner boxes and edge lines for the vertex, edge and face tools. */
  const editMarkers = (): SceneOptions["markers"] => {
    const brush = editedBrush();
    if (!brush) return null;
    const corners = brushVertices(brush.faces);
    const size = panScale(camera.state, canvas.clientHeight) * 4;
    return { points: state.tool.id === "vertex" ? corners : [], edges: brushEdges(brush.faces, corners), size };
  };

  /** Pixel position of a world point in this view. */
  const toScreen = (world: Vec3): Point | null => {
    const m = camera.matrices().viewProj;
    const cx = m[0] * world[0] + m[4] * world[1] + m[8] * world[2] + m[12];
    const cy = m[1] * world[0] + m[5] * world[1] + m[9] * world[2] + m[13];
    const cw = m[3] * world[0] + m[7] * world[1] + m[11] * world[2] + m[15];
    if (cw <= 1e-6) return null;
    return { x: ((cx / cw) * 0.5 + 0.5) * canvas.clientWidth, y: (0.5 - (cy / cw) * 0.5) * canvas.clientHeight };
  };

  // --- brush edit tools ---

  /**
   * Starts a vertex, edge or face drag on the selected brush, if the press is
   * on one of its handles. Each move re-applies the edit from the brush as it
   * was at the press, so the whole drag is one undo step.
   */
  const beginBrushEdit = (p: Point): boolean => {
    const brush = editedBrush();
    if (!brush) return false;
    const corners = brushVertices(brush.faces);
    const forward = viewForward(camera.state);
    if (state.tool.id === "vertex") {
      const i = nearestOnScreen(corners.map(toScreen), p, 12);
      if (i < 0) return false;
      props.editor.beginGesture();
      setDrag({ mode: "vertex", x: p.x, y: p.y, target: brush.id, corners: [corners[i]], grab: corners[i], normal: forward });
      return true;
    }
    if (state.tool.id === "edge") {
      let best = -1;
      let bestDist = 10;
      const edges = brushEdges(brush.faces, corners);
      edges.forEach(([a, b], i) => {
        const sa = toScreen(a);
        const sb = toScreen(b);
        if (!sa || !sb) return;
        const d = segmentDistance(sa, sb, p);
        if (d <= bestDist) {
          bestDist = d;
          best = i;
        }
      });
      if (best < 0) return false;
      const [a, b] = edges[best];
      props.editor.beginGesture();
      setDrag({ mode: "vertex", x: p.x, y: p.y, target: brush.id, corners: [a, b], grab: [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2, (a[2] + b[2]) / 2], normal: forward });
      return true;
    }
    // Face: the surface under the cursor, on the selected brush only.
    const ray = rayAt(p);
    const hit = props.editor.pickSurface(ray.origin, ray.dir, undefined, undefined, xrayPick());
    if (!hit || hit.id !== brush.id) return false;
    const face = faceAt(brush.faces, hit.position);
    if (face < 0) return false;
    const normal = brush.faces[face].normal;
    const start = rayAxis(ray, hit.position, normal) ?? 0;
    props.editor.beginGesture();
    setDrag({ mode: "face", x: p.x, y: p.y, target: brush.id, face, origin: hit.position, normal, start });
    props.editor.note(`face ${face}: drag to push or pull it`);
    return true;
  };

  const moveBrushEdit = (current: Extract<Drag, { mode: "vertex" | "face" }>, p: Point): void => {
    const ray = rayAt(p);
    const grid = state.snap.enabled ? state.snap.size : 0;
    const snap = (v: number): number => (grid > 0 ? Math.round(v / grid) * grid : v);
    try {
      if (current.mode === "face") {
        const t = rayAxis(ray, current.origin, current.normal);
        if (t === null) return;
        const distance = snap(t - current.start);
        if (Math.abs(distance) < 0.5) return;
        props.editor.gestureApply(() => props.editor.brushExtrudeFace(current.target, current.face, distance));
        return;
      }
      const at = onPlane(ray, current.grab, current.normal);
      if (!at) return;
      const delta: Vec3 = [snap(at[0] - current.grab[0]), snap(at[1] - current.grab[1]), snap(at[2] - current.grab[2])];
      if (delta.every((d) => Math.abs(d) < 0.5)) return;
      props.editor.gestureApply(() => {
        // The corners are moved one after another; each move starts from the
        // brush the previous one left, which still has the next corner where
        // it was, because a move only re-derives the faces that corner is on.
        const touched: Id[] = [];
        for (const c of current.corners) {
          touched.push(...props.editor.brushMoveVertex(current.target, c, [c[0] + delta[0], c[1] + delta[1], c[2] + delta[2]]));
        }
        return touched;
      });
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };

  /** The voxel tool: a click on a voxel terrain adds a sphere there; with Shift it carves one. */
  const voxelClick = (p: Point, carve: boolean): boolean => {
    const ray = rayAt(p);
    const hit = props.editor.pickSurface(ray.origin, ray.dir, undefined, undefined, xrayPick());
    if (!hit) return false;
    const found = props.editor.currentObjects().find((o) => o.doc.id === hit.id);
    if (!found || found.doc.kind.type !== "voxel_terrain") return false;
    const local = toTerrainLocal(found.world, hit.position);
    if (!local) return false;
    const voxel = found.doc.kind.voxel_terrain;
    const ops = Array.isArray(voxel.ops) ? voxel.ops : [];
    const shape = { shape: "sphere", center: local, radius: Math.max(voxel.voxel_size * 2, state.tool.sculpt.radius) };
    const next = [...ops, { op: carve ? "subtract" : "union", shape, k: 0 }];
    props.editor.apply({ op: "replace_objects", objects: [{ ...found.doc, kind: { type: "voxel_terrain", voxel_terrain: { ...voxel, ops: next } } }] });
    props.editor.note(`voxel: ${carve ? "carved" : "added"} a sphere`);
    return true;
  };

  // --- terrain sculpt and paint ---

  /** The terrain object a sculpt brush is over, if any. */
  const sculptTarget = (p: Point): { id: Id; world: Mat4; point: Vec3 } | null => {
    const scene = props.editor.scene;
    if (!scene) return null;
    const ray = rayAt(p);
    const hit = props.editor.pickSurface(ray.origin, ray.dir, undefined, undefined, xrayPick());
    if (hit) {
      const found = props.editor.currentObjects().find((o) => o.doc.id === hit.id);
      if (found && (found.doc.kind.type === "terrain" || found.doc.kind.type === "voxel_terrain")) {
        return { id: hit.id, world: found.world, point: hit.position };
      }
    }
    const point = workPoint(p);
    if (!point) return null;
    const radius = state.tool.sculpt.radius;
    const min: Vec3 = [point[0] - radius, point[1] - radius, point[2] - 1];
    const max: Vec3 = [point[0] + radius, point[1] + radius, point[2] + 1];
    for (const instance of instancesInBox(scene, min, max)) {
      const found = props.editor.currentObjects().find((o) => o.doc.id === instance.id);
      if (found && (found.doc.kind.type === "terrain" || found.doc.kind.type === "voxel_terrain")) {
        return { id: instance.id, world: instance.transform, point };
      }
    }
    return null;
  };

  /** Terrain-local coordinates of a world point, in inches. */
  const toTerrainLocal = (world: Mat4, p: Vec3): Vec3 | null => {
    const inv = invert(mat4(), world);
    if (!inv) return null;
    const local = transformPoint(inv, p);
    if (local[3] === 0) return null;
    return [local[0] / local[3], local[1] / local[3], local[2] / local[3]];
  };

  const beginSculpt = (p: Point): boolean => {
    const target = sculptTarget(p);
    if (!target) return false;
    const local = toTerrainLocal(target.world, target.point);
    if (!local) return false;
    const point: StrokePoint = { x: local[0], y: local[1], pressure: state.tool.sculpt.pressure };
    // A stroke is one undo step however many moves it takes.
    props.editor.beginGesture();
    setDrag({ mode: "sculpt", x: p.x, y: p.y, target: target.id, points: [point], painted: new Set([target.id]) });
    if (state.tool.id === "terrain_paint") paintLayers(target.id, local);
    else applySculpt(target.id, [point]);
    return true;
  };

  /** Adds a point to the live stroke and sends the whole stroke to the core. */
  const extendSculpt = (current: Extract<Drag, { mode: "sculpt" }>, p: Point): void => {
    const target = sculptTarget(p);
    if (!target) return;
    const local = toTerrainLocal(target.world, target.point);
    if (!local) return;
    const spacing = Math.max(1, state.tool.sculpt.radius / 4);
    const last = current.points[current.points.length - 1];
    if (last && Math.hypot(local[0] - last.x, local[1] - last.y) < spacing) return;
    const points = [...current.points, { x: local[0], y: local[1], pressure: state.tool.sculpt.pressure }];
    setDrag({ ...current, points, x: p.x, y: p.y });
    if (state.tool.id === "terrain_paint") paintLayers(target.id, local);
    else applySculpt(target.id, points);
  };

  /**
   * Applies the whole stroke so far. It replaces the previous application
   * rather than adding to it, so the terrain moves once per stroke however
   * many pointer moves it took.
   */
  const applySculpt = (id: Id, points: StrokePoint[]): void => {
    const brush = state.tool.sculpt;
    const stroke: Stroke = {
      ...defaultStroke(brush.brush),
      points,
      radius: brush.radius,
      strength: brush.strength,
      falloff: brush.falloff,
    };
    try {
      props.editor.gestureApply(() => props.editor.terrainSculpt(id, strokeJson(stroke)));
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };

  /**
   * The core exposes splat weights only through the auto-paint rules, so a
   * paint stroke re-evaluates the rules of the object it touched, with the
   * brushed layer keyed off the height under the cursor.
   */
  const paintLayers = (id: Id, at: Vec3): void => {
    const terrain = props.editor.store?.object(id);
    if (!terrain || terrain.kind.type !== "terrain") return;
    const layer = Math.min(state.tool.sculpt.layer, Math.max(0, terrain.kind.terrain.layers.length - 1));
    const stored = (terrain.kind.terrain.recipe as { autopaint?: LayerRule[] } | null)?.autopaint;
    const rules = terrain.kind.terrain.layers.map((l, index) => {
      const rule = stored?.[index] ?? defaultLayerRule(l.name, "constant");
      if (index !== layer) return rule;
      return { ...rule, driver: "height" as const, range_lo: at[2] - 64, range_hi: at[2] + 64, weight: 1 };
    });
    try {
      props.editor.gestureApply(() => props.editor.terrainAutoPaint(id, rulesJson(rules)));
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };

  const finishSculpt = (current: Extract<Drag, { mode: "sculpt" }>): void => {
    // Every move already applied the stroke so far; nothing is left to add.
    props.editor.endGesture();
    if (state.tool.id === "terrain_paint") props.editor.note(`paint: ${current.painted.size} objects repainted`);
    else props.editor.note(`sculpt: ${current.points.length} stroke points`);
  };

  // --- selection ---

  /** Objects whose origin projects inside a screen rectangle. */
  const objectsInRect = (a: Point, b: Point): Id[] => {
    const x0 = Math.min(a.x, b.x);
    const x1 = Math.max(a.x, b.x);
    const y0 = Math.min(a.y, b.y);
    const y1 = Math.max(a.y, b.y);
    return selectableScreenPoints()
      .filter(({ at }) => at.x >= x0 && at.x <= x1 && at.y >= y0 && at.y <= y1)
      .map(({ id }) => id);
  };

  const objectsInCircle = (c: Point, r: number): Id[] =>
    selectableScreenPoints()
      .filter(({ at }) => Math.hypot(at.x - c.x, at.y - c.y) <= r)
      .map(({ id }) => id);

  const selectableScreenPoints = (): { id: Id; at: Point }[] => {
    const out: { id: Id; at: Point }[] = [];
    for (const o of props.editor.currentObjects()) {
      if (o.hidden || o.doc.hidden || o.doc.locked) continue;
      const b = o.worldBounds;
      const center: Vec3 = b ? [(b.min[0] + b.max[0]) / 2, (b.min[1] + b.max[1]) / 2, (b.min[2] + b.max[2]) / 2] : o.doc.transform.position;
      const at = toScreen(center);
      if (at) out.push({ id: o.doc.id, at });
    }
    return out;
  };

  const pickAt = async (p: Point, mode: PickMode): Promise<void> => {
    if (!renderer || !context) return;
    const dpr = Math.min(2, globalThis.devicePixelRatio || 1);
    const matrices = camera.matrices();
    const scene = props.editor.rebuildScene(sceneOptions());
    const id = await renderer.pick(
      scene,
      { ...matrices, eye: eyeFor(camera.state), width: canvas.width, height: canvas.height },
      lighting,
      p.x * dpr,
      p.y * dpr,
      state.ui.shading.xray,
    );
    props.onPick(id, mode);
  };

  // --- modal operators ---

  let virtual: Point = { x: 0, y: 0 };
  let listening = false;

  const beginModalListeners = (): void => {
    if (listening) return;
    listening = true;
    document.addEventListener("keydown", onModalKeyDown, true);
    document.addEventListener("keyup", onModalKeyUp, true);
    document.addEventListener("pointermove", onModalPointerMove, true);
    document.addEventListener("pointerdown", onModalPointerDown, true);
    document.addEventListener("pointerup", onModalPointerUp, true);
    document.addEventListener("wheel", onModalWheel, { capture: true, passive: false });
    document.addEventListener("contextmenu", preventDefault, true);
  };

  function endModalListeners(): void {
    if (!listening) return;
    listening = false;
    document.removeEventListener("keydown", onModalKeyDown, true);
    document.removeEventListener("keyup", onModalKeyUp, true);
    document.removeEventListener("pointermove", onModalPointerMove, true);
    document.removeEventListener("pointerdown", onModalPointerDown, true);
    document.removeEventListener("pointerup", onModalPointerUp, true);
    document.removeEventListener("wheel", onModalWheel, true);
    document.removeEventListener("contextmenu", preventDefault, true);
  }

  function preventDefault(e: Event): void {
    e.preventDefault();
  }

  const startModal = (m: Modal): void => {
    setModalLocal(m);
    beginModalListeners();
    publishModal(m);
  };

  const endModal = (): void => {
    setModalLocal(null);
    endModalListeners();
    setModal(null);
    if (document.pointerLockElement === canvas) document.exitPointerLock?.();
    cameraMoved();
  };

  const publishModal = (m: Modal): void => {
    switch (m.kind) {
      case "transform":
        setModal({
          name: opName(m.op.mode),
          header: `${describeOp(m.op, m.readout)}${m.neighbours.length ? `  proportional ${Math.round(state.proportional.radius)}` : ""}`,
          hints: TRANSFORM_HINTS,
        });
        break;
      case "walk":
        setModal({
          name: "Walk",
          header: `speed ${Math.round(m.speed)} u/s${m.gravity ? "  gravity" : ""}`,
          hints: [
            ["LMB / Enter", "Confirm"],
            ["RMB / Esc", "Cancel"],
            ["W A S D", "Move"],
            ["Q E / R F", "Down / Up"],
            ["Space", "Teleport"],
            ["Tab", "Gravity"],
            ["V", "Jump"],
            ["Shift / Alt", "Fast / Slow"],
            ["Wheel", "Speed"],
          ],
        });
        break;
      case "zoom":
        setModal({ name: "Zoom region", header: "", hints: [["LMB drag", "Zoom in"], ["MMB drag", "Zoom out"], ["RMB / Esc", "Cancel"]] });
        break;
      case "lasso":
        setModal({ name: "Lasso select", header: "", hints: [["LMB drag", "Select"], ["Ctrl drag", "Extend"], ["Shift drag", "Deselect"], ["RMB / Esc", "Cancel"]] });
        break;
      case "box":
        setModal({ name: "Box select", header: "", hints: [["LMB drag", "Select"], ["Shift LMB drag", "Deselect"], ["RMB / Esc", "Cancel"]] });
        break;
      case "circle":
        setModal({ name: "Circle select", header: `radius ${Math.round(m.radius)}`, hints: [["LMB", "Select"], ["Shift LMB / MMB", "Deselect"], ["Wheel", "Radius"], ["RMB / Esc / Enter", "Done"]] });
        break;
      case "meshop": {
        const spec = MESH_OPS[m.key] as OpSpec;
        const shown = props.editor.lastMeshOp?.request.params ?? spec.params;
        const keys = [spec.modal?.drag, spec.modal?.wheel].filter((k): k is string => !!k);
        const header = keys
          .map((k) => {
            const v = shown[k];
            return `${k} ${typeof v === "number" ? (Number.isInteger(v) ? v : v.toFixed(3)) : String(v)}`;
          })
          .join("  ");
        const hints: [string, string][] = [];
        if (spec.modal?.drag) hints.push(["Move", spec.modal.drag]);
        if (spec.modal?.wheel) hints.push(["Wheel", spec.modal.wheel]);
        hints.push(["LMB / Enter", "Confirm"], ["RMB / Esc", "Cancel"]);
        setModal({ name: spec.label, header, hints });
        break;
      }
    }
  };

  const opName = (mode: TransformMode): string => (mode === "grab" ? "Move" : mode === "rotate" ? "Rotate" : "Scale");

  const startTransform = (mode: TransformMode): boolean => {
    const activeObject = state.selection.primary ? props.editor.store?.object(state.selection.primary) : null;
    const pivot = pivotPoint(activeObject?.transform.position ?? null);
    if (!pivot) {
      toast(`${opName(mode).toLowerCase()}: nothing selected`);
      return false;
    }
    const at = pointer() ?? { x: canvas.clientWidth / 2, y: canvas.clientHeight / 2 };
    virtual = { ...at };
    lastReal = { ...at };
    const neighbours = state.proportional.enabled ? proportionalCandidates() : [];
    props.editor.beginGesture(neighbours.map((n) => n.id));
    const pivotScreen = toScreen(pivot) ?? at;
    const a0 = Math.atan2(-(at.y - pivotScreen.y), at.x - pivotScreen.x);
    startModal({
      kind: "transform",
      op: beginOp(mode),
      pivot,
      start: { ...at },
      at: { ...at },
      angle: 0,
      lastAngle: a0,
      trackball: [0, 0],
      ctrl: false,
      shift: false,
      local: activeObject?.transform.rotation ?? [0, 0, 0, 1],
      readout: mode === "rotate" ? 0 : mode === "grab" ? [0, 0, 0] : [1, 1, 1],
      mmbFrom: null,
      neighbours,
    });
    return true;
  };

  /** Where rotate and scale turn about, from the header's pivot point. */
  const pivotPoint = (active: Vec3 | null): Vec3 | null => {
    const store = props.editor.store;
    if (!store) return null;
    const ps = state.selection.ids.map((id) => store.object(id)?.transform.position).filter((p): p is Vec3 => !!p);
    if (ps.length === 0) return null;
    if (state.pivot === "cursor" && state.cursorWorld) return state.cursorWorld;
    if (state.pivot === "active" && active) return active;
    if (state.pivot === "bbox") {
      const lo: Vec3 = [Infinity, Infinity, Infinity];
      const hi: Vec3 = [-Infinity, -Infinity, -Infinity];
      for (const p of ps) for (let i = 0; i < 3; i++) (lo[i] = Math.min(lo[i], p[i])), (hi[i] = Math.max(hi[i], p[i]));
      return [(lo[0] + hi[0]) / 2, (lo[1] + hi[1]) / 2, (lo[2] + hi[2]) / 2];
    }
    return scale(ps.reduce((a, p) => add(a, p), [0, 0, 0] as Vec3), 1 / ps.length);
  };

  /** Unselected objects near the selection that a proportional edit can carry, with their starting distance. */
  const proportionalCandidates = (): { id: Id; distance: number }[] => {
    const store = props.editor.store;
    if (!store) return [];
    const chosen = new Set(state.selection.ids);
    const origins = state.selection.ids.map((id) => store.object(id)?.transform.position).filter((p): p is Vec3 => !!p);
    // The radius can grow during the drag; four times the start radius bounds what is gathered.
    const reach = state.proportional.radius * 4;
    const out: { id: Id; distance: number }[] = [];
    for (const o of props.editor.currentObjects()) {
      if (chosen.has(o.doc.id) || o.hidden || o.doc.hidden || o.doc.locked || o.doc.parent) continue;
      const p = o.doc.transform.position;
      let best = Infinity;
      for (const q of origins) best = Math.min(best, Math.hypot(p[0] - q[0], p[1] - q[1], p[2] - q[2]));
      if (best <= reach) out.push({ id: o.doc.id, distance: best });
    }
    return out;
  };

  const falloffWeight = (t: number): number => {
    const x = Math.max(0, Math.min(1, 1 - t));
    switch (state.proportional.falloff) {
      case "constant":
        return t < 1 ? 1 : 0;
      case "linear":
        return x;
      case "sharp":
        return x * x;
      case "root":
        return Math.sqrt(x);
      case "sphere":
        return Math.sqrt(Math.max(0, 1 - (1 - x) * (1 - x)));
      default:
        return x * x * (3 - 2 * x);
    }
  };

  const proportionalWeights = (m: Extract<Modal, { kind: "transform" }>): Map<Id, number> | undefined => {
    if (m.neighbours.length === 0) return undefined;
    const r = Math.max(1, state.proportional.radius);
    return new Map(m.neighbours.map((n) => [n.id, falloffWeight(n.distance / r)]));
  };

  /** The three axes of the orientation a constraint uses. */
  const orientationAxes = (m: Extract<Modal, { kind: "transform" }>, orientation: Orientation): Vec3[] => {
    if (orientation === "local") return AXES.map((a) => normalize(quatRotate(m.local, a), a));
    if (orientation === "view") {
      const f = viewForward(camera.state);
      const up0 = normalize(viewUp(camera.state.kind, f), [0, 0, 1]);
      const right = normalize(cross(f, up0), [1, 0, 0]);
      const up = normalize(cross(right, f), up0);
      return [right, up, scale(f, -1)];
    }
    return AXES;
  };

  const axisVector = (m: Extract<Modal, { kind: "transform" }>, axis: number): Vec3 => orientationAxes(m, m.op.constraint?.orientation ?? "global")[axis];

  /** Blender's MMB axis pointing: the axis whose screen direction best matches the drag. */
  const pickAxisFromDrag = (m: Extract<Modal, { kind: "transform" }>, from: Point, to: Point): number => {
    const dx = to.x - from.x;
    const dy = to.y - from.y;
    const len = Math.hypot(dx, dy);
    if (len < 2) return -1;
    const base = toScreen(m.pivot);
    if (!base) return -1;
    const size = panScale(camera.state, canvas.clientHeight) * 100;
    let best = -1;
    let bestDot = 0;
    orientationAxes(m, state.orientation).forEach((a, i) => {
      const tip = toScreen(add(m.pivot, scale(a, size)));
      if (!tip) return;
      const sx = tip.x - base.x;
      const sy = tip.y - base.y;
      const sl = Math.hypot(sx, sy);
      if (sl < 1e-3) return;
      const d = Math.abs((sx * dx + sy * dy) / (sl * len));
      if (d > bestDot) {
        bestDot = d;
        best = i;
      }
    });
    return best;
  };

  /** Rotation that turns +Z onto `n`. */
  const alignZTo = (n: Vec3): Quat => {
    const z: Vec3 = [0, 0, 1];
    const d = Math.max(-1, Math.min(1, dot(z, n)));
    if (d > 0.9999) return [0, 0, 0, 1];
    if (d < -0.9999) return [1, 0, 0, 0];
    return quatFromAxisAngle(normalize(cross(z, n), [1, 0, 0]), Math.acos(d));
  };

  /** Recomputes and applies the transform for the modal's current input. */
  const applyTransform = (m: Extract<Modal, { kind: "transform" }>): void => {
    const op = m.op;
    const snapOn = state.snap.enabled !== m.ctrl;
    // Ctrl+Shift snaps in finer steps: a tenth of the grid, 1 degree, 0.01 scale.
    const fine = snapOn && m.shift;
    const typed = typedValues(op);
    const typing = isTyping(op);
    const c = op.constraint;
    const individual = state.pivot === "individual";
    const forward = viewForward(camera.state);
    const toViewer = scale(forward, -1);
    const weights = proportionalWeights(m);
    try {
      if (op.mode === "grab") {
        let d: Vec3 = [0, 0, 0];
        let turn: Quat | undefined;
        const step = fine ? state.snap.size / 10 : state.snap.size;
        if (typing) {
          if (c && !c.plane) d = scale(axisVector(m, c.axis), typed[c.axis] ?? typed[op.typingAxis] ?? 0);
          else {
            const raw: Vec3 = [typed[0] ?? 0, typed[1] ?? 0, typed[2] ?? 0];
            if (c?.plane) raw[c.axis] = 0;
            const axes = orientationAxes(m, c?.orientation ?? "global");
            d = add(add(scale(axes[0], raw[0]), scale(axes[1], raw[1])), scale(axes[2], raw[2]));
          }
        } else if (snapOn && state.snap.target === "face") {
          // Face snap: the pivot lands on the surface under the cursor, ignoring what is being moved.
          const moving = new Set([...state.selection.ids, ...m.neighbours.map((n) => n.id)]);
          const r = rayAt(m.at);
          const hit = props.editor.pickSurface(r.origin, r.dir, undefined, (id) => !moving.has(id), xrayPick());
          if (hit) {
            d = sub(hit.position, m.pivot);
            if (c && !c.plane) {
              const axis = axisVector(m, c.axis);
              d = scale(axis, dot(d, axis));
            }
            if (state.snap.alignRotation) turn = alignZTo(hit.normal);
          }
        } else {
          const r0 = rayAt(m.start);
          const r1 = rayAt(m.at);
          if (c && !c.plane) {
            const axis = axisVector(m, c.axis);
            const t0 = rayAxis(r0, m.pivot, axis);
            const t1 = rayAxis(r1, m.pivot, axis);
            let t = t0 !== null && t1 !== null ? t1 - t0 : 0;
            if (snapOn) {
              // Grid snaps the pivot to absolute grid lines; increment snaps the distance moved.
              const base = state.snap.target === "grid" ? dot(m.pivot, axis) : 0;
              t = snapValue(base + t, step) - base;
            }
            d = scale(axis, t);
          } else {
            const normal = c ? axisVector(m, c.axis) : forward;
            const p0 = onPlane(r0, m.pivot, normal);
            const p1 = onPlane(r1, m.pivot, normal);
            d = p0 && p1 ? sub(p1, p0) : [0, 0, 0];
            if (snapOn) {
              const base: Vec3 = state.snap.target === "grid" ? m.pivot : [0, 0, 0];
              d = [0, 1, 2].map((i) => snapValue(base[i] + d[i], step) - base[i]) as Vec3;
            }
            if (c) d = sub(d, scale(normal, dot(d, normal)));
          }
        }
        m.readout = d;
        props.editor.transformSelection({ translate: d, turn }, m.pivot, individual || !!turn, weights);
      } else if (op.mode === "rotate") {
        let turn: Quat;
        let degrees: number;
        if (op.trackball && !typing) {
          const up = normalize(viewUp(camera.state.kind, forward), [0, 0, 1]);
          const right = normalize(cross(forward, up), [1, 0, 0]);
          const trueUp = normalize(cross(right, forward), up);
          turn = quatMul(quatFromAxisAngle(trueUp, m.trackball[0] * 0.01), quatFromAxisAngle(right, m.trackball[1] * 0.01));
          degrees = 0;
        } else {
          const axis = c ? axisVector(m, c.axis) : toViewer;
          if (typing) degrees = typed[c ? c.axis : 0] ?? typed[0] ?? 0;
          else {
            const sign = dot(axis, toViewer) >= 0 ? 1 : -1;
            degrees = ((m.angle * 180) / Math.PI) * sign;
            if (snapOn) degrees = snapValue(degrees, fine ? 1 : state.snap.angle);
          }
          turn = quatFromAxisAngle(axis, (degrees * Math.PI) / 180);
        }
        m.readout = degrees;
        props.editor.transformSelection({ turn }, m.pivot, individual, weights);
      } else {
        const pivotScreen = toScreen(m.pivot) ?? m.start;
        let k = Math.hypot(m.at.x - pivotScreen.x, m.at.y - pivotScreen.y) / Math.max(1, Math.hypot(m.start.x - pivotScreen.x, m.start.y - pivotScreen.y));
        if (snapOn && !typing) {
          const step = fine ? 0.01 : state.snap.scale;
          k = snapValue(k, step) || step;
        }
        const mask = axisMask(op);
        const s: Vec3 = [0, 1, 2].map((i) => {
          if (mask[i] === 0) return 1;
          if (typing) return typed[c && !c.plane ? c.axis : i] ?? typed[op.typingAxis] ?? 1;
          return k;
        }) as Vec3;
        m.readout = s;
        props.editor.transformSelection({ scale: s }, m.pivot, individual, weights);
      }
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
    publishModal(m);
    setModalLocal({ ...m });
  };

  const confirmModal = (): void => {
    const m = modal();
    if (!m) return;
    if (m.kind === "transform") props.editor.endGesture(true);
    if (m.kind === "walk") publishCamera();
    endModal();
  };

  /** World units one screen pixel spans at the mesh selection, for distance-driven ops. */
  const worldPerPixelAtSelection = (): number => {
    const at = selectionMedian();
    if (!at) return 1;
    const fwd = viewForward(camera.state);
    let right = cross(fwd, [0, 0, 1]);
    if (Math.hypot(right[0], right[1], right[2]) < 1e-6) right = [1, 0, 0];
    const probe = 64;
    const a = toScreen(at);
    const b = toScreen(add(at, scale(normalize(right), probe)));
    if (!a || !b) return 1;
    const px = Math.hypot(b.x - a.x, b.y - a.y);
    return px > 1e-6 ? probe / px : 1;
  };

  const beginMeshOpModal = (key: MeshOpKey): boolean => {
    const session = props.editor.meshEdit;
    if (!session) return false;
    if (props.editor.meshSessionIds().ids.length === 0) {
      props.editor.note("nothing selected");
      return true;
    }
    const spec = MESH_OPS[key] as OpSpec;
    const at = pointer() ?? { x: canvas.clientWidth / 2, y: canvas.clientHeight / 2 };
    startModal({
      kind: "meshop",
      key,
      start: at,
      at,
      wheel: 0,
      worldPerPx: worldPerPixelAtSelection(),
      widthPx: canvas.clientWidth,
      params: { ...spec.params },
      ran: false,
    });
    return true;
  };

  /** Runs the modal op with the params the pointer and wheel now give. */
  const applyMeshOpModal = (m: Extract<Modal, { kind: "meshop" }>): void => {
    const session = props.editor.meshEdit;
    if (!session) return;
    const spec = MESH_OPS[m.key] as OpSpec;
    const travel = spec.modal?.fraction ? m.at.x - m.start.x : Math.hypot(m.at.x - m.start.x, m.at.y - m.start.y);
    const next = modalParams(spec, m.params, travel, m.widthPx, m.worldPerPx, m.wheel);
    try {
      if (m.ran) {
        props.editor.adjustLastOp(next);
      } else {
        const cur = props.editor.meshSessionIds();
        props.editor.runMeshOp(opRequest(session.object, spec.op, JSON.parse(selJson(cur.mode, cur.ids)), next));
        m.ran = true;
      }
    } catch (e) {
      // A value the op refuses (a zero slide, an overlapping bevel) waits for the next move.
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
    publishModal(m);
  };

  const cancelModal = (): void => {
    const m = modal();
    if (!m) return;
    if (m.kind === "transform") props.editor.cancelGesture();
    if (m.kind === "meshop" && m.ran) props.editor.undo();
    if (m.kind === "walk") {
      Object.assign(camera.state, m.saved);
      publishCamera();
    }
    endModal();
  };

  const resizeProportional = (grow: boolean): void => {
    setState("proportional", "radius", (r) => Math.max(8, Math.min(65536, r * (grow ? 1.15 : 1 / 1.15))));
  };

  function onModalKeyDown(e: KeyboardEvent): void {
    const m = modal();
    if (!m) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") return cancelModal();
    if (e.key === "Enter" || e.key === "NumpadEnter") return confirmModal();
    if (m.kind === "transform") {
      if (e.key === "Control" || e.key === "Shift") {
        if (e.key === "Control") m.ctrl = true;
        else m.shift = true;
        return applyTransform(m);
      }
      if (m.neighbours.length > 0 && (e.key === "PageUp" || e.key === "PageDown")) {
        resizeProportional(e.key === "PageUp");
        return applyTransform(m);
      }
      const letter = e.code.startsWith("Key") ? e.code.slice(3) : "";
      const axis = letter === "X" ? 0 : letter === "Y" ? 1 : letter === "Z" ? 2 : -1;
      if (axis >= 0) {
        m.op = pressAxis(m.op, axis, e.shiftKey, state.orientation);
        return applyTransform(m);
      }
      if (m.op.mode === "rotate" && letter === "R") {
        m.op = { ...m.op, trackball: !m.op.trackball };
        return applyTransform(m);
      }
      // The top row and the numpad both type numbers.
      const numpad: Record<string, string> = { NumpadDecimal: ".", NumpadMultiply: "*", NumpadSubtract: "-", NumpadAdd: "+", NumpadDivide: "/" };
      let key = e.code.startsWith("Numpad") && /^[0-9]$/.test(e.code.slice(6)) ? e.code.slice(6) : numpad[e.code] ?? e.key;
      if (key === "Tab" && (e.ctrlKey || e.shiftKey)) key = "ShiftTab";
      const next = typeKey(m.op, key, e.ctrlKey);
      if (next) {
        m.op = next;
        applyTransform(m);
      }
      return;
    }
    if (m.kind === "walk") {
      if (e.code === "Tab") {
        m.gravity = !m.gravity;
        m.vz = 0;
        publishModal(m);
        return;
      }
      if (e.code === "Space") return teleport();
      if (e.code === "KeyV" && m.gravity && m.grounded) {
        m.vz = WALK_JUMP;
        m.grounded = false;
        return;
      }
      if (e.code === "NumpadAdd" || e.code === "NumpadSubtract") {
        m.speed = Math.min(20000, Math.max(16, m.speed * (e.code === "NumpadAdd" ? 1.25 : 0.8)));
        publishModal(m);
        return;
      }
      m.keys.add(e.code);
      m.fast = e.shiftKey;
      m.slow = e.altKey;
      return;
    }
    if (m.kind === "circle" && (e.key === "+" || e.key === "=" || e.code === "NumpadAdd" || e.key === "-" || e.code === "NumpadSubtract")) {
      const grow = e.key === "+" || e.key === "=" || e.code === "NumpadAdd";
      setModalLocal({ ...m, radius: Math.max(4, m.radius * (grow ? 1.2 : 1 / 1.2)) });
      publishModal(modal()!);
    }
  }

  function onModalKeyUp(e: KeyboardEvent): void {
    const m = modal();
    if (!m) return;
    e.stopPropagation();
    if (m.kind === "transform" && (e.key === "Control" || e.key === "Shift")) {
      if (e.key === "Control") m.ctrl = false;
      else m.shift = false;
      applyTransform(m);
    }
    if (m.kind === "walk") {
      m.keys.delete(e.code);
      m.fast = e.shiftKey;
      m.slow = e.altKey;
    }
  }

  let lastReal: Point = { x: 0, y: 0 };

  function onModalPointerMove(e: PointerEvent): void {
    const m = modal();
    if (!m) return;
    if (m.kind === "transform") {
      // Shift slows the pointer to a tenth, for fine placement.
      const precision = e.shiftKey && !m.ctrl ? 0.1 : 1;
      const real = localPoint(e);
      const dx = document.pointerLockElement === canvas ? e.movementX : real.x - lastReal.x;
      const dy = document.pointerLockElement === canvas ? e.movementY : real.y - lastReal.y;
      lastReal = real;
      virtual = { x: virtual.x + dx * precision, y: virtual.y + dy * precision };
      const prev = m.at;
      m.at = { ...virtual };
      if (m.op.mode === "rotate") {
        if (m.op.trackball) m.trackball = [m.trackball[0] + (m.at.x - prev.x), m.trackball[1] + (m.at.y - prev.y)];
        const pivotScreen = toScreen(m.pivot) ?? m.start;
        const a = Math.atan2(-(m.at.y - pivotScreen.y), m.at.x - pivotScreen.x);
        let da = a - m.lastAngle;
        if (da > Math.PI) da -= 2 * Math.PI;
        if (da < -Math.PI) da += 2 * Math.PI;
        m.angle += da;
        m.lastAngle = a;
      }
      applyTransform(m);
      return;
    }
    if (m.kind === "walk") {
      turnHead(-e.movementX * 0.0025, -e.movementY * 0.0025);
      return;
    }
    if (m.kind === "meshop") {
      m.at = localPoint(e);
      applyMeshOpModal(m);
      return;
    }
    if (m.kind === "box" || m.kind === "zoom") {
      setModalLocal({ ...m, at: localPoint(e) });
      return;
    }
    if (m.kind === "lasso") {
      if (m.path.length > 0) setModalLocal({ ...m, path: [...m.path, localPoint(e)] });
      return;
    }
    if (m.kind === "circle") {
      const at = localPoint(e);
      setModalLocal({ ...m, at });
      if (m.painting) props.onBoxSelect(objectsInCircle(at, m.radius), m.painting === "subtract" ? "subtract" : "extend");
    }
  }

  function onModalPointerDown(e: PointerEvent): void {
    const m = modal();
    if (!m) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.button === 2) return m.kind === "circle" ? endModal() : cancelModal();
    if (m.kind === "transform") {
      if (e.button === 0) confirmModal();
      if (e.button === 1) setModalLocal({ ...m, mmbFrom: { ...m.at } });
      return;
    }
    if (m.kind === "walk" || m.kind === "meshop") {
      if (e.button === 0) confirmModal();
      return;
    }
    if (m.kind === "box" || m.kind === "zoom") {
      const p = localPoint(e);
      if (m.kind === "box") setModalLocal({ ...m, mode: e.shiftKey || e.button === 1 ? "subtract" : "extend", start: p, at: p });
      else setModalLocal({ ...m, out: e.button === 1, start: p, at: p });
      return;
    }
    if (m.kind === "lasso") {
      setModalLocal({ ...m, mode: e.shiftKey ? "subtract" : e.ctrlKey ? "extend" : m.mode, path: [localPoint(e)] });
      return;
    }
    if (m.kind === "circle") {
      const painting: PickMode = e.shiftKey || e.button === 1 ? "subtract" : "extend";
      setModalLocal({ ...m, painting });
      props.onBoxSelect(objectsInCircle(localPoint(e), m.radius), painting);
    }
  }

  function onModalPointerUp(e: PointerEvent): void {
    const m = modal();
    if (!m) return;
    e.stopPropagation();
    if (m.kind === "transform" && e.button === 1 && m.mmbFrom) {
      let axis = pickAxisFromDrag(m, m.mmbFrom, m.at);
      if (axis < 0) axis = pickAxisFromDrag(m, m.start, m.at);
      m.mmbFrom = null;
      if (axis >= 0) m.op = { ...m.op, constraint: { axis, plane: e.shiftKey, orientation: state.orientation } };
      return applyTransform(m);
    }
    if (m.kind === "box" && m.start) {
      props.onBoxSelect(objectsInRect(m.start, localPoint(e)), m.mode);
      endModal();
    }
    if (m.kind === "zoom" && m.start) {
      zoomToRect(m.start, localPoint(e), m.out);
      endModal();
    }
    if (m.kind === "lasso" && m.path.length > 2) {
      props.onBoxSelect(objectsInPolygon(m.path), m.mode);
      endModal();
    }
    if (m.kind === "circle") setModalLocal({ ...m, painting: null });
  }

  function onModalWheel(e: WheelEvent): void {
    const m = modal();
    if (!m) return;
    e.preventDefault();
    e.stopPropagation();
    if (m.kind === "transform" && m.neighbours.length > 0) {
      resizeProportional(e.deltaY < 0);
      applyTransform(m);
    }
    if (m.kind === "walk") {
      m.speed = Math.min(20000, Math.max(16, m.speed * (e.deltaY < 0 ? 1.25 : 0.8)));
      publishModal(m);
    }
    if (m.kind === "circle") {
      setModalLocal({ ...m, radius: Math.max(4, m.radius * (e.deltaY < 0 ? 1.15 : 1 / 1.15)) });
      publishModal(modal()!);
    }
    if (m.kind === "meshop" && (MESH_OPS[m.key] as OpSpec).modal?.wheel) {
      m.wheel += e.deltaY < 0 ? 1 : -1;
      applyMeshOpModal(m);
    }
  }

  /** Objects whose origin projects inside a screen polygon. */
  const objectsInPolygon = (poly: Point[]): Id[] =>
    selectableScreenPoints()
      .filter(({ at }) => {
        let inside = false;
        for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
          const a = poly[i];
          const b = poly[j];
          if (a.y > at.y !== b.y > at.y && at.x < ((b.x - a.x) * (at.y - a.y)) / (b.y - a.y) + a.x) inside = !inside;
        }
        return inside;
      })
      .map(({ id }) => id);

  /** Shift+B: frames the dragged rectangle, or with the middle button backs out by it. */
  const zoomToRect = (a: Point, b: Point, out: boolean): void => {
    const w = Math.abs(b.x - a.x);
    const h = Math.abs(b.y - a.y);
    if (w < 4 || h < 4) return;
    const center = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
    const ratio = Math.max(w / canvas.clientWidth, h / canvas.clientHeight);
    const factor = out ? 1 / ratio : ratio;
    const s = camera.state;
    const ray = rayAt(center);
    const hit = isPerspectiveState(s) ? props.editor.pickSurface(ray.origin, ray.dir, undefined, undefined, xrayPick())?.position ?? null : null;
    const focus = hit ?? workPoint(center);
    if (focus) {
      const move = sub(focus, s.target);
      const f = viewForward(s);
      const lateral = isPerspectiveState(s) ? move : sub(move, scale(f, dot(move, f)));
      s.target = add(s.target, lateral);
      s.eye = add(eyeFor(s), lateral);
    }
    if (isPerspectiveState(s)) zoomAbout(s, s.target, factor);
    else s.orthoHeight = Math.min(262144, Math.max(8, s.orthoHeight * factor));
    publishCamera();
  };

  // --- walk navigation ---

  const WALK_EYE_HEIGHT = 60;
  const WALK_GRAVITY = 800;
  const WALK_JUMP = 330;

  const startWalk = (): void => {
    const saved: CameraState = { ...camera.state, target: [...camera.state.target] as Vec3, eye: [...eyeFor(camera.state)] as Vec3 };
    if (camera.state.kind !== "persp" || !isPerspectiveState(camera.state)) setOrtho(camera.state, false);
    canvas.requestPointerLock?.();
    startModal({ kind: "walk", saved, keys: new Set(), speed: 512, last: performance.now(), fast: false, slow: false, gravity: false, vz: 0, grounded: false });
  };

  /** Space in walk mode: jumps to the surface under the crosshair, at eye height. */
  const teleport = (): void => {
    const s = camera.state;
    const eye = eyeFor(s);
    const f = viewForward(s);
    const hit = props.editor.pickSurface(eye, f, undefined, undefined, xrayPick());
    if (!hit) return;
    const to = add(hit.position, [0, 0, WALK_EYE_HEIGHT]);
    const move = sub(to, eye);
    s.eye = to;
    s.target = add(s.target, move);
    cameraMoved();
  };

  /** Turns the eye in place: yaw about world Z, pitch about the view's right. */
  const turnHead = (yaw: number, pitch: number): void => {
    const s = camera.state;
    const eye = eyeFor(s);
    let f = normalize(sub(s.target, eye), [0, 1, 0]);
    f = quatRotate(quatFromAxisAngle([0, 0, 1], yaw), f);
    const right = normalize(cross(f, [0, 0, 1]), [1, 0, 0]);
    const pitched = quatRotate(quatFromAxisAngle(right, pitch), f);
    if (Math.abs(pitched[2]) < 0.995) f = pitched;
    s.target = add(eye, scale(f, s.distance));
    cameraMoved();
  };

  const walkStep = (m: Extract<Modal, { kind: "walk" }>, now: number): void => {
    const dt = Math.min(0.1, (now - m.last) / 1000);
    m.last = now;
    const k = m.keys;
    const forward = (k.has("KeyW") || k.has("ArrowUp") ? 1 : 0) - (k.has("KeyS") || k.has("ArrowDown") ? 1 : 0);
    const strafe = (k.has("KeyD") || k.has("ArrowRight") ? 1 : 0) - (k.has("KeyA") || k.has("ArrowLeft") ? 1 : 0);
    // E/Q rise and fall in world space, R/F along the view's own up; both only without gravity.
    const rise = m.gravity ? 0 : (k.has("KeyE") ? 1 : 0) - (k.has("KeyQ") ? 1 : 0);
    const lift = m.gravity ? 0 : (k.has("KeyR") ? 1 : 0) - (k.has("KeyF") ? 1 : 0);
    const speed = m.speed * (m.fast ? 4 : 1) * (m.slow ? 0.25 : 1) * dt;
    const s = camera.state;
    let moved = false;
    if (forward !== 0 || strafe !== 0 || lift !== 0) {
      if (m.gravity) {
        // Walking keeps to the ground plane: forward is the view's heading.
        const f = viewForward(s);
        const heading = normalize([f[0], f[1], 0], [0, 1, 0]);
        const right = normalize(cross(heading, [0, 0, 1]), [1, 0, 0]);
        const d = add(scale(heading, forward * speed), scale(right, strafe * speed));
        s.eye = add(eyeFor(s), d);
        s.target = add(s.target, d);
      } else fly(s, forward * speed, strafe * speed, lift * speed, 1);
      moved = true;
    }
    if (rise !== 0) {
      s.eye = add(s.eye, [0, 0, rise * speed]);
      s.target = add(s.target, [0, 0, rise * speed]);
      moved = true;
    }
    if (m.gravity) {
      const eye = eyeFor(s);
      const below = props.editor.pickSurface(eye, [0, 0, -1], undefined, undefined, xrayPick());
      const floor = below ? below.position[2] + WALK_EYE_HEIGHT : -Infinity;
      m.vz -= WALK_GRAVITY * dt;
      let z = eye[2] + m.vz * dt;
      m.grounded = z <= floor;
      if (m.grounded) {
        z = floor;
        m.vz = 0;
      }
      const dz = z - eye[2];
      if (dz !== 0) {
        s.eye = add(eye, [0, 0, dz]);
        s.target = add(s.target, [0, 0, dz]);
        moved = true;
      }
    }
    if (moved) cameraMoved();
  };

  // --- pointer input ---

  const onPointerDown = (e: PointerEvent): void => {
    if (modal()) return;
    canvas.focus();
    // Captured, so a drag that leaves the canvas still ends in pointerup here.
    canvas.setPointerCapture?.(e.pointerId);
    const p = localPoint(e);
    const middle = e.button === 1 || (e.button === 0 && e.altKey && state.input.emulate3Button);
    if (middle) {
      // Stops the browser's autoscroll on a middle press.
      e.preventDefault();
      const mode = e.shiftKey && e.ctrlKey ? "dolly" : e.shiftKey ? "pan" : e.ctrlKey ? "zoom" : locked() ? "pan" : "orbit";
      orbitFrom = { ...camera.state, target: [...camera.state.target] as Vec3, eye: [...eyeFor(camera.state)] as Vec3 };
      setDrag({ mode, x: p.x, y: p.y, sx: p.x, sy: p.y, center: e.altKey && !state.input.emulate3Button });
      return;
    }
    if (e.button === 2) {
      if (e.shiftKey) placeCursor(p);
      return;
    }
    if (e.button !== 0) return;
    if (props.editor.meshEditing) {
      const meshPick: PickMode = e.shiftKey ? "extend" : e.ctrlKey ? "subtract" : "set";
      if (e.altKey && e.ctrlKey) {
        meshLoopAt(p, true, meshPick);
        return;
      }
      if (e.altKey) {
        meshLoopAt(p, false, meshPick);
        return;
      }
      const hit = meshSelectAt(p, meshPick);
      if (hit && meshPick !== "subtract") beginMeshDrag(p, "translate", hit);
      else setDrag({ mode: "select", x: p.x, y: p.y, sx: p.x, sy: p.y, pick: meshPick });
      return;
    }
    if (e.ctrlKey && e.altKey) {
      startModal({ kind: "lasso", mode: e.shiftKey ? "subtract" : "set", path: [p] });
      return;
    }
    const toolId = state.tool.id;
    if (toolId === "clip") {
      if (selectedBrushes().length > 0) {
        addClipPoint(p);
        return;
      }
      toast("Clip cuts the selected brushes: click a brush to select it, then click the cut points");
    }
    if (!e.altKey && BRUSH_EDIT_TOOLS.has(toolId)) {
      if (beginBrushEdit(p)) return;
      toast(
        editedBrush()
          ? `drag a ${toolId === "vertex" ? "corner" : toolId === "edge" ? "edge" : "face"} of the selected brush`
          : "vertex, edge and face editing works on brushes: click a brush to select it (a mesh edits with Tab)",
      );
    }
    if (!e.altKey && toolId === "terrain_voxel") {
      if (!voxelClick(p, e.shiftKey)) toast(hasKind("voxel_terrain") ? "click on a voxel terrain: add a sphere, Shift+click carves" : "no voxel terrain yet: Shift+A > Voxel terrain");
      return;
    }
    const r = rayAt(p);
    const pivot = gizmoPivot();
    // Only the transform tools own a gizmo; a sculpt, brush or clip drag that
    // starts on the pivot must not move the object instead.
    if (pivot && state.ui.showGizmo && GIZMO_TOOLS.has(state.tool.id)) {
      const hit = pickHandle(r, pivot.pivot, pivot.rotation, state.gizmoSpace, state.tool.gizmo, toScreen, p, gizmoSize(), 14);
      if (hit) {
        const object = props.editor.store?.object(state.selection.primary ?? "");
        if (object) {
          const dragState = beginDrag({
            r,
            pivot: pivot.pivot,
            rotation: pivot.rotation,
            position: object.transform.position,
            objectScale: object.transform.scale,
            mode: state.tool.gizmo,
            space: state.gizmoSpace,
            hit,
            snap: state.snap,
            cameraDir: viewForward(camera.state),
          });
          props.editor.beginGesture();
          setDrag({ mode: "gizmo", x: p.x, y: p.y, sx: p.x, sy: p.y, gizmo: dragState });
          return;
        }
      }
    }
    if (state.tool.id === "brush") {
      boxSurface = surfaceUnder(p);
      boxStart = brushPoint(p);
      boxEnd = boxStart;
      setDrag({ mode: "box", x: p.x, y: p.y, sx: p.x, sy: p.y });
      return;
    }
    if (state.tool.id === "terrain_sculpt" || state.tool.id === "terrain_paint") {
      if (!beginSculpt(p)) toast(hasKind("terrain") ? "no terrain under the cursor" : "sculpt and paint work on terrain: Shift+A > Terrain adds one");
      return;
    }
    if (state.tool.id === "entity" && !e.shiftKey && !e.ctrlKey) {
      placeEntity(p);
      return;
    }
    // The select tool: a click picks, a drag box-selects.
    const pick: PickMode = e.shiftKey ? "extend" : e.ctrlKey ? "subtract" : "set";
    setDrag({ mode: "select", x: p.x, y: p.y, sx: p.x, sy: p.y, pick });
  };

  const onPointerMove = (e: PointerEvent): void => {
    const p = localPoint(e);
    setPointer(p);
    if (modal()) return;
    const current = drag();
    if (!current) return;
    const dx = p.x - current.x;
    const dy = p.y - current.y;
    switch (current.mode) {
      case "orbit": {
        const speed = state.input.orbitSpeed;
        if (e.altKey && orbitFrom) snapOrbit(p.x - current.sx, p.y - current.sy);
        else orbit(camera.state, -dx * speed, dy * speed);
        break;
      }
      case "dolly": {
        // Dolly moves the point of interest too, so it can go past what zoom stops at.
        if (!isPerspectiveState(camera.state)) setOrtho(camera.state, false);
        const step = -dy * panScale(camera.state, canvas.clientHeight) * 2;
        slide(camera.state, viewForward(camera.state), step);
        break;
      }
      case "pan":
        pan(camera.state, dx * panScale(camera.state, canvas.clientHeight), -dy * panScale(camera.state, canvas.clientHeight));
        break;
      case "zoom": {
        const factor = Math.exp(dy * 0.005 * (state.input.invertZoom ? -1 : 1));
        zoomBy(factor, null);
        break;
      }
      case "box":
        if (state.tool.id === "brush" && (dx !== 0 || dy !== 0)) updateBoxDrag(p);
        break;
      case "sculpt":
        extendSculpt(current, p);
        break;
      case "vertex":
      case "face":
        moveBrushEdit(current, p);
        break;
      case "mesh":
        moveMeshDrag(current, p);
        break;
      case "gizmo": {
        if (!current.gizmo) break;
        const delta = applyDrag(current.gizmo, rayAt(p));
        props.editor.dragSelection(delta, current.gizmo.pivot);
        break;
      }
      default:
        break;
    }
    if (current.mode !== "sculpt" && current.mode !== "vertex" && current.mode !== "face" && current.mode !== "mesh") {
      setDrag({ ...current, x: p.x, y: p.y });
    }
    if (current.mode === "orbit" || current.mode === "pan" || current.mode === "zoom" || current.mode === "dolly") publishCamera();
  };

  const onPointerUp = (e: PointerEvent): void => {
    const p = localPoint(e);
    const wasDrag = drag();
    setDrag(null);
    if (!wasDrag) return;
    if (wasDrag.mode === "gizmo" || wasDrag.mode === "vertex" || wasDrag.mode === "face") {
      // An edge drag moves two corners, two commands; they undo as one.
      props.editor.endGesture(wasDrag.mode === "vertex");
      return;
    }
    if (wasDrag.mode === "mesh") {
      // A session drag previews without commands; the release is one SetFile.
      props.editor.endGesture(false);
      if (wasDrag.moved) {
        try {
          props.editor.meshEditCommit();
        } catch (e) {
          props.editor.note(e instanceof Error ? e.message : String(e));
        }
      }
      return;
    }
    if (wasDrag.mode === "sculpt") {
      finishSculpt(wasDrag);
      return;
    }
    const moved = Math.hypot(p.x - wasDrag.sx, p.y - wasDrag.sy) >= CLICK_SLOP;
    if ((wasDrag.mode === "orbit" || wasDrag.mode === "pan") && wasDrag.center && !moved) {
      centerOn(p);
      return;
    }
    if (wasDrag.mode === "box") {
      finishBoxDrag();
      return;
    }
    if (wasDrag.mode === "select") {
      if (moved) {
        if (props.editor.meshEditing) meshBoxSelect({ x: wasDrag.sx, y: wasDrag.sy }, p, wasDrag.pick ?? "set");
        else props.onBoxSelect(objectsInRect({ x: wasDrag.sx, y: wasDrag.sy }, p), wasDrag.pick ?? "set");
      } else if (!props.editor.meshEditing) {
        void pickAt(p, wasDrag.pick ?? "set");
      }
    }
  };

  // --- camera commands ---

  let orbitFrom: CameraState | null = null;

  /**
   * Alt while orbiting: the view snaps to 45 degree steps from where the drag
   * began, and lands in an ortho axis view when it lines up with one.
   */
  const snapOrbit = (dx: number, dy: number): void => {
    if (!orbitFrom) return;
    const speed = state.input.orbitSpeed;
    Object.assign(camera.state, { ...orbitFrom, target: [...orbitFrom.target], eye: [...orbitFrom.eye] });
    const off = sub(eyeFor(camera.state), camera.state.target);
    const r = Math.hypot(off[0], off[1], off[2]);
    const yaw0 = Math.atan2(off[0], off[1]);
    const pitch0 = Math.asin(Math.max(-1, Math.min(1, off[2] / Math.max(1e-6, r))));
    const q = Math.PI / 4;
    const yaw = Math.round((yaw0 - dx * speed) / q) * q;
    const pitch = Math.max(-Math.PI / 2, Math.min(Math.PI / 2, Math.round((pitch0 + dy * speed) / q) * q));
    const axisPitch = Math.abs(Math.abs(pitch) - Math.PI / 2) < 1e-6;
    const axisYaw = Math.abs(Math.round(yaw / (Math.PI / 2)) * (Math.PI / 2) - yaw) < 1e-6;
    if (axisPitch) return setAxisView(camera.state, pitch > 0 ? "top" : "bottom");
    if (pitch === 0 && axisYaw) {
      const quarter = ((Math.round(yaw / (Math.PI / 2)) % 4) + 4) % 4;
      // Eye bearings: 0 is +Y (back), 1 is +X (right), 2 is -Y (front), 3 is -X (left).
      return setAxisView(camera.state, (["back", "right", "front", "side"] as const)[quarter]);
    }
    const cp = Math.cos(pitch);
    camera.state.kind = "persp";
    camera.state.eye = add(camera.state.target, [r * cp * Math.sin(yaw), r * cp * Math.cos(yaw), r * Math.sin(pitch)]);
    camera.state.distance = r;
  };

  /** Zooms by `factor` (below 1 closes in) about a point, or the view center. */
  const zoomBy = (factor: number, at: Point | null): void => {
    if (!isPerspectiveState(camera.state)) {
      zoomOrtho(camera.state, factor, at && state.input.zoomToMouse ? workPoint(at) ?? undefined : undefined);
    } else {
      let point = camera.state.target;
      if (at && state.input.zoomToMouse) {
        const ray = rayAt(at);
        const hit = props.editor.pickSurface(ray.origin, ray.dir, undefined, undefined, xrayPick());
        if (hit) point = hit.position;
      }
      zoomAbout(camera.state, point, factor);
    }
    camera.markDirty();
  };

  /** Alt+middle click: the view turns to look at the surface under the cursor. */
  const centerOn = (p: Point): void => {
    const ray = rayAt(p);
    const hit = props.editor.pickSurface(ray.origin, ray.dir, undefined, undefined, xrayPick());
    if (!hit) return;
    const move = sub(hit.position, camera.state.target);
    camera.state.target = hit.position;
    camera.state.eye = add(eyeFor(camera.state), move);
    publishCamera();
  };

  const placeEntity = (p: Point): void => {
    const ray = rayAt(p);
    const hit = props.editor.pickWorld(ray.origin, ray.dir, undefined, xrayPick()) ?? workPoint(p);
    if (!hit) return;
    const at = state.snap.enabled ? snapToGrid(hit, state.snap.size as GridSize) : hit;
    try {
      const added = props.editor.addEntity(state.tool.entityClass, at);
      if (added.length > 0) props.onBoxSelect(added, "set");
      props.editor.note(`placed ${state.tool.entityClass}`);
    } catch (err) {
      props.editor.note(err instanceof Error ? err.message : String(err));
    }
  };

  const [toastText, setToastText] = createSignal<string | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  /** A short message over the view, for a tool that cannot act where it was used. */
  const toast = (text: string): void => {
    setToastText(text);
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => setToastText(null), 3500);
  };

  const hasKind = (type: string): boolean => props.editor.currentObjects().some((o) => o.doc.kind.type === type && !o.hidden);

  const selectedBrushes = (): Id[] => state.selection.ids.filter((id) => props.editor.store?.object(id)?.kind.type === "brush");

  /** Shift+right click: puts the 3D cursor on the surface under the pointer. */
  const placeCursor = (p: Point): void => {
    const ray = rayAt(p);
    const hit = props.editor.pickWorld(ray.origin, ray.dir, undefined, xrayPick()) ?? workPoint(p);
    if (!hit) return;
    setCursorWorld(state.snap.enabled ? snapToGrid(hit, state.snap.size as GridSize) : hit);
  };

  const frameBox = (min: Vec3, max: Vec3): void => {
    frameBounds(camera.state, min, max, camera.state.fov);
    if (camera.state.kind !== "persp") camera.state.eye = eyeFor(camera.state);
    publishCamera();
  };

  const controller = {
    busy: (): boolean => modal() !== null || drag() !== null,
    camera: (): CameraState => camera.state,
    cameraChanged: (): void => {
      camera.markDirty();
      setViewTick((t) => t + 1);
      publishCamera();
    },
    frameSeq: (): number => presentedFrames,
    capture: async (width: number, height: number, overlays: boolean): Promise<Uint8Array> => {
      if (!renderer) throw new Error("the viewport has no renderer yet");
      const base = sceneOptions();
      const options = overlays ? base : { ...base, selection: new Set<Id>(), hover: null, gizmo: null, markers: null, editOverlay: null };
      const scene = props.editor.rebuildScene(options);
      const shot = new Camera({ ...camera.state, target: [...camera.state.target] as Vec3, eye: [...eyeFor(camera.state)] as Vec3 });
      shot.setAspect(width / height);
      const matrices = shot.matrices();
      syncLighting();
      const rgba = await renderer.capture(scene, { ...matrices, eye: eyeFor(shot.state), width, height }, { ...lighting }, overlays, state.ui.shading);
      return encodePng({ width, height, rgba, bitDepth: 8 });
    },
    ray: (x: number, y: number) => rayAt({ x, y }),
    frame: frameBox,
    run: (command: CommandId): boolean => {
      if (modal()) return false;
      const s = camera.state;
      const step = panScale(s, canvas.clientHeight) * canvas.clientHeight * 0.1;
      const axisView = (kind: Exclude<ViewKind, "persp">): void => {
        if (locked()) return;
        setAxisView(s, kind);
      };
      switch (command) {
        case "view_front": axisView("front"); break;
        case "view_back": axisView("back"); break;
        case "view_right": axisView("right"); break;
        case "view_left": axisView("side"); break;
        case "view_top": axisView("top"); break;
        case "view_bottom": axisView("bottom"); break;
        case "view_persp_ortho":
          if (locked()) return true;
          setOrtho(s, isPerspectiveState(s));
          break;
        case "view_orbit_left": if (!locked()) orbit(s, -ORBIT_STEP, 0); break;
        case "view_orbit_right": if (!locked()) orbit(s, ORBIT_STEP, 0); break;
        case "view_orbit_up": if (!locked()) orbit(s, 0, ORBIT_STEP); break;
        case "view_orbit_down": if (!locked()) orbit(s, 0, -ORBIT_STEP); break;
        case "view_flip": if (!locked()) flipView(s); break;
        case "view_pan_left": pan(s, step, 0); break;
        case "view_pan_right": pan(s, -step, 0); break;
        case "view_pan_up": pan(s, 0, -step); break;
        case "view_pan_down": pan(s, 0, step); break;
        case "view_zoom_in": zoomBy(1 / 1.25, null); break;
        case "view_zoom_out": zoomBy(1.25, null); break;
        case "walk": startWalk(); return true;
        case "grab":
          if (props.editor.meshEditing) return armMeshDrag("translate");
          return startTransform("grab") || true;
        case "rotate":
          if (props.editor.meshEditing) return armMeshDrag("rotate");
          return startTransform("rotate") || true;
        case "resize":
          if (props.editor.meshEditing) return armMeshDrag("scale");
          return startTransform("resize") || true;
        case "mesh_toggle_edit":
          props.editor.toggleMeshEdit();
          return true;
        case "mesh_mode_vertex":
          props.editor.meshSetMode("vertex");
          return true;
        case "mesh_mode_edge":
          props.editor.meshSetMode("edge");
          return true;
        case "mesh_mode_face":
          props.editor.meshSetMode("face");
          return true;
        case "mesh_select_linked":
          meshLinkedAt();
          return true;
        case "mesh_linked_expand":
          meshLinkedExpand();
          return true;
        case "mesh_grow":
          meshGrowShrink(true);
          return true;
        case "mesh_shrink":
          meshGrowShrink(false);
          return true;
        case "mesh_hide":
          props.editor.meshHide();
          return true;
        case "mesh_reveal":
          props.editor.meshReveal();
          return true;
        case "mesh_extrude":
          return meshExtrude();
        case "mesh_fill":
          meshOpDirect("fill", {});
          return true;
        case "mesh_loop_cut":
          return beginMeshOpModal("loop_cut");
        case "mesh_edge_slide":
          return beginMeshOpModal("edge_slide");
        case "mesh_inset":
          return beginMeshOpModal("inset");
        case "mesh_bevel":
          return beginMeshOpModal("bevel_edges");
        case "mesh_bevel_vertices":
          return beginMeshOpModal("bevel_vertices");
        case "mesh_separate":
          meshOpDirect("separate", { mode: "selected" });
          return true;
        case "mesh_recalc_outside":
          meshOpDirect("recalc_normals", { mode: "outside" });
          return true;
        case "select_box":
          startModal({ kind: "box", mode: "extend", start: null, at: pointer() ?? { x: 0, y: 0 } });
          return true;
        case "zoom_region":
          startModal({ kind: "zoom", start: null, at: pointer() ?? { x: 0, y: 0 }, out: false });
          return true;
        case "select_lasso":
          startModal({ kind: "lasso", mode: "set", path: [] });
          return true;
        case "select_circle":
          startModal({ kind: "circle", radius: 40, at: pointer() ?? { x: canvas.clientWidth / 2, y: canvas.clientHeight / 2 }, painting: null });
          return true;
        default:
          return false;
      }
      publishCamera();
      return true;
    },
  };

  const onWheel = (e: WheelEvent): void => {
    e.preventDefault();
    if (modal()) return;
    const lines = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 800 : 1;
    const delta = e.deltaY * lines * (state.input.invertZoom ? -1 : 1);
    const at = localPoint(e);
    if (e.ctrlKey || e.shiftKey) {
      // Blender: Ctrl+wheel pans across, Shift+wheel pans up and down.
      const amount = Math.sign(delta) * panScale(camera.state, canvas.clientHeight) * 40;
      if (e.ctrlKey) pan(camera.state, amount, 0);
      else pan(camera.state, 0, -amount);
    } else if (e.altKey && isPerspectiveState(camera.state)) {
      // A fixed step along the cursor ray that goes through walls, for
      // getting inside a room or a box.
      const ray = rayAt(at);
      const notches = -Math.max(-6, Math.min(6, delta / 100));
      slide(camera.state, ray.dir, notches * WHEEL_STEP_UNITS);
    } else {
      zoomBy(Math.exp(Math.max(-600, Math.min(600, delta)) * 0.0015), at);
    }
    camera.markDirty();
    publishCamera();
  };

  // --- mesh edit mode ---

  const editOverlayCache = new EditOverlayCache();
  let overlayStamp = -1;
  let overlayZoom = 0;
  let overlayLines: SceneOptions["editOverlay"] = null;

  /** The session overlay, rebuilt on session changes and zoom steps only. */
  const meshOverlay = (): SceneOptions["editOverlay"] => {
    const session = props.editor.meshEdit;
    const buffers = session ? props.editor.meshEditBuffers() : null;
    if (!session || !buffers) {
      overlayStamp = -1;
      overlayLines = null;
      editOverlayCache.clear();
      return null;
    }
    const zoom = Math.round(Math.log(Math.max(1, viewDistance())) * 8);
    if (overlayStamp !== props.editor.sessionStamp || zoom !== overlayZoom) {
      overlayStamp = props.editor.sessionStamp;
      overlayZoom = zoom;
      overlayLines = editOverlayCache.update(buffers, session.mode, panScale(camera.state, canvas.clientHeight) * 4).lines;
    }
    return overlayLines;
  };

  /** Header readout; docRevision ticks on every session change. */
  const editInfo = (): MeshEditState | null => (void state.docRevision, props.editor.meshEdit);

  const meshNote = (e: unknown): void => props.editor.note(e instanceof Error ? e.message : String(e));

  const meshPickQuery = (mode: EditSelMode, p: Point, rect?: [number, number, number, number]): string => {
    const ray = rayAt(p);
    return pickQueryJson({
      mode,
      x: p.x,
      y: p.y,
      eye: eyeFor(camera.state),
      viewProj: camera.matrices().viewProj,
      width: canvas.clientWidth,
      height: canvas.clientHeight,
      origin: ray.origin,
      dir: ray.dir,
      xray: state.ui.shading.xray > 0,
      rect,
    });
  };

  /** Click select in the session; returns the pick for a drag to start from. */
  const meshSelectAt = (p: Point, pick: PickMode): PickReply | null => {
    const session = props.editor.meshEdit;
    if (!session) return null;
    try {
      const hit = props.editor.meshEditPick(meshPickQuery(session.mode, p));
      if (hit.kind === "vertex" || hit.kind === "edge" || hit.kind === "face") {
        const cur = props.editor.meshSessionIds();
        const ids = pick === "extend" ? [...new Set([...cur.ids, hit.id])] : pick === "subtract" ? cur.ids.filter((i) => i !== hit.id) : [hit.id];
        props.editor.meshEditSelect(selJson(session.mode, ids));
        return hit;
      }
      if (pick === "set") props.editor.meshEditSelect(selJson(session.mode, []));
      return null;
    } catch (e) {
      meshNote(e);
      return null;
    }
  };

  const meshLoopAt = (p: Point, ring: boolean, pick: PickMode): void => {
    const session = props.editor.meshEdit;
    if (!session) return;
    try {
      const hit = props.editor.meshEditPick(meshPickQuery("edge", p));
      if (hit.kind !== "edge") return;
      const query = { kind: ring ? "ring" : "loop", edge: hit.id };
      const cur = session.mode === "edge" && pick !== "set" ? props.editor.meshSessionIds().ids : [];
      props.editor.meshEditSelect(selJson("edge", cur, query));
    } catch (e) {
      meshNote(e);
    }
  };

  const meshBoxSelect = (a: Point, b: Point, pick: PickMode): void => {
    const session = props.editor.meshEdit;
    if (!session) return;
    try {
      const rect: [number, number, number, number] = [Math.min(a.x, b.x), Math.min(a.y, b.y), Math.max(a.x, b.x), Math.max(a.y, b.y)];
      const hit = props.editor.meshEditPick(meshPickQuery(session.mode, { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }, rect));
      if (hit.kind !== "elements") return;
      const cur = props.editor.meshSessionIds();
      const ids = pick === "extend" ? [...new Set([...cur.ids, ...hit.ids])] : pick === "subtract" ? cur.ids.filter((i) => !hit.ids.includes(i)) : hit.ids;
      props.editor.meshEditSelect(selJson(session.mode, ids));
    } catch (e) {
      meshNote(e);
    }
  };

  /** Median of the session selection in world space, for drag pivots. */
  const selectionMedian = (): Vec3 | null => {
    const session = props.editor.meshEdit;
    const buffers = props.editor.meshEditBuffers();
    if (!session || !buffers) return null;
    const { mode, ids } = props.editor.meshSessionIds();
    if (ids.length === 0) return null;
    const pts: Vec3[] = [];
    if (mode === "vertex") {
      for (const i of ids) if (i * 3 + 2 < buffers.verts.length) pts.push([buffers.verts[i * 3], buffers.verts[i * 3 + 1], buffers.verts[i * 3 + 2]]);
    } else if (mode === "edge") {
      for (const i of ids) {
        if (i * 2 + 1 >= buffers.polyEdges.length) continue;
        for (const v of [buffers.polyEdges[i * 2], buffers.polyEdges[i * 2 + 1]]) {
          if (v * 3 + 2 < buffers.verts.length) pts.push([buffers.verts[v * 3], buffers.verts[v * 3 + 1], buffers.verts[v * 3 + 2]]);
        }
      }
    } else {
      for (const i of ids) {
        if (i * 3 + 2 < buffers.faceCentres.length) pts.push([buffers.faceCentres[i * 3], buffers.faceCentres[i * 3 + 1], buffers.faceCentres[i * 3 + 2]]);
      }
    }
    if (pts.length === 0) return null;
    return scale(pts.reduce((a, p) => add(a, p), [0, 0, 0] as Vec3), 1 / pts.length);
  };

  /** A world delta into the edited object's space, for session translate ops. */
  const meshObjectDelta = (d: Vec3): Vec3 => {
    const session = props.editor.meshEdit;
    const t = session ? props.editor.store?.object(session.object)?.transform : null;
    if (!t) return d;
    const s = t.scale;
    const unscaled: Vec3 = [d[0] / (s[0] || 1), d[1] / (s[1] || 1), d[2] / (s[2] || 1)];
    return quatRotate(quatConjugate(t.rotation), unscaled);
  };

  /** Starts a session drag about the pick point, or the selection median. */
  const beginMeshDrag = (p: Point, kind: "translate" | "rotate" | "scale", hit: PickReply | null): boolean => {
    const session = props.editor.meshEdit;
    if (!session) return false;
    const base = props.editor.meshSessionIds();
    if (base.ids.length === 0) {
      props.editor.note("nothing selected");
      return false;
    }
    const ray = rayAt(p);
    const grab =
      hit && hit.kind !== "elements" && hit.kind !== "none"
        ? add(ray.origin, scale(ray.dir, hit.distance))
        : selectionMedian() ?? ray.origin;
    const gs = toScreen(grab) ?? p;
    props.editor.beginGesture();
    setDrag({
      mode: "mesh",
      x: p.x,
      y: p.y,
      sx: p.x,
      sy: p.y,
      kind,
      grab,
      normal: viewForward(camera.state),
      applied: [0, 0, 0],
      lastAngle: Math.atan2(-(p.y - gs.y), p.x - gs.x),
      factor: 1,
      startDist: Math.hypot(p.x - gs.x, p.y - gs.y),
      base,
      moved: false,
    });
    return true;
  };

  const armMeshDrag = (kind: "translate" | "rotate" | "scale"): boolean => {
    const at = pointer() ?? { x: canvas.clientWidth / 2, y: canvas.clientHeight / 2 };
    return beginMeshDrag(at, kind, null);
  };

  const moveMeshDrag = (current: Extract<Drag, { mode: "mesh" }>, p: Point): void => {
    const session = props.editor.meshEdit;
    if (!session) return;
    const sel = props.editor.meshSessionIds();
    if (sel.ids.length === 0) return;
    const selJsonText = selJson(sel.mode, sel.ids);
    const grid = state.snap.enabled ? state.snap.size : 0;
    const snapV = (v: number): number => (grid > 0 ? Math.round(v / grid) * grid : v);
    try {
      if (current.kind === "translate") {
        const at = onPlane(rayAt(p), current.grab, current.normal);
        if (!at) return;
        const total = meshObjectDelta([snapV(at[0] - current.grab[0]), snapV(at[1] - current.grab[1]), snapV(at[2] - current.grab[2])]);
        const step: Vec3 = [total[0] - current.applied[0], total[1] - current.applied[1], total[2] - current.applied[2]];
        if (Math.hypot(step[0], step[1], step[2]) < 1e-6) return;
        props.editor.meshEditPreview(opRequest(session.object, "translate", JSON.parse(selJsonText), { translate: step }));
        setDrag({ ...current, x: p.x, y: p.y, applied: total, moved: true });
        return;
      }
      if (current.kind === "rotate") {
        const gs = toScreen(current.grab);
        if (!gs) return;
        const a = Math.atan2(-(p.y - gs.y), p.x - gs.x);
        let da = a - current.lastAngle;
        if (da > Math.PI) da -= 2 * Math.PI;
        if (da < -Math.PI) da += 2 * Math.PI;
        const deg = (da * 180) / Math.PI;
        if (Math.abs(deg) < 1e-3) return;
        const axis = meshObjectDelta(scale(viewForward(camera.state), -1));
        const len = Math.hypot(axis[0], axis[1], axis[2]);
        if (len < 1e-9) return;
        const norm: Vec3 = [axis[0] / len, axis[1] / len, axis[2] / len];
        props.editor.meshEditPreview(opRequest(session.object, "rotate", JSON.parse(selJsonText), { axis: norm, angle_deg: deg }));
        setDrag({ ...current, x: p.x, y: p.y, lastAngle: a, moved: true });
        return;
      }
      const gs = toScreen(current.grab);
      if (!gs) return;
      const total = Math.max(0.01, 1 + (Math.hypot(p.x - gs.x, p.y - gs.y) - current.startDist) * 0.005);
      const step = total / current.factor;
      if (Math.abs(step - 1) < 1e-4) return;
      props.editor.meshEditPreview(opRequest(session.object, "scale", JSON.parse(selJsonText), { scale: [step, step, step] }));
      setDrag({ ...current, x: p.x, y: p.y, factor: total, moved: true });
    } catch (e) {
      meshNote(e);
    }
  };

  const cancelMeshDrag = (): void => {
    const d = drag();
    if (!d || d.mode !== "mesh") return;
    const base = d.base;
    setDrag(null);
    props.editor.endGesture(false);
    props.editor.resetMeshEdit(base);
  };

  createEffect(() => {
    const d = drag();
    if (!d || d.mode !== "mesh") return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        cancelMeshDrag();
        return;
      }
      if ((e.key === "g" || e.key === "G") && d.kind === "translate" && !e.ctrlKey && !e.altKey) {
        // G G, as in Blender: the grab becomes an edge slide of the same selection.
        e.preventDefault();
        e.stopPropagation();
        cancelMeshDrag();
        beginMeshOpModal("edge_slide");
      }
    };
    document.addEventListener("keydown", onKey, true);
    onCleanup(() => document.removeEventListener("keydown", onKey, true));
  });

  const meshLinkedAt = (): void => {
    const session = props.editor.meshEdit;
    const at = pointer() ?? { x: canvas.clientWidth / 2, y: canvas.clientHeight / 2 };
    if (!session) return;
    try {
      const hit = props.editor.meshEditPick(meshPickQuery(session.mode, at));
      if (hit.kind !== "vertex" && hit.kind !== "edge" && hit.kind !== "face") return;
      props.editor.meshEditSelect(selJson(session.mode, [], { kind: "linked", from: hit.id }));
    } catch (e) {
      meshNote(e);
    }
  };

  const meshLinkedExpand = (): void => {
    const session = props.editor.meshEdit;
    if (!session) return;
    try {
      const cur = props.editor.meshSessionIds();
      if (cur.ids.length === 0) return;
      props.editor.meshEditSelect(selJson(cur.mode, cur.ids, { kind: "linked_sel" }));
    } catch (e) {
      meshNote(e);
    }
  };

  const meshGrowShrink = (grow: boolean): void => {
    const session = props.editor.meshEdit;
    if (!session) return;
    try {
      const cur = props.editor.meshSessionIds();
      if (cur.ids.length === 0) return;
      props.editor.meshEditSelect(selJson(cur.mode, cur.ids, { kind: grow ? "grow" : "shrink" }));
    } catch (e) {
      meshNote(e);
    }
  };

  const meshOpDirect = (op: string, params: Record<string, unknown>): void => {
    const session = props.editor.meshEdit;
    if (!session) return;
    try {
      const cur = props.editor.meshSessionIds();
      props.editor.runMeshOp(opRequest(session.object, op, JSON.parse(selJson(cur.mode, cur.ids)), params));
    } catch (e) {
      meshNote(e);
    }
  };

  const meshExtrude = (): boolean => {
    const session = props.editor.meshEdit;
    if (!session) return false;
    try {
      const cur = props.editor.meshSessionIds();
      props.editor.runMeshOp(opRequest(session.object, "extrude_region", JSON.parse(selJson(cur.mode, cur.ids)), {}));
      const at = pointer() ?? { x: canvas.clientWidth / 2, y: canvas.clientHeight / 2 };
      beginMeshDrag(at, "translate", null);
      return true;
    } catch (e) {
      meshNote(e);
      return true;
    }
  };

  // --- brush box and clip tools ---

  let boxStart: Vec3 | null = null;
  let boxEnd: Vec3 | null = null;
  /** Height of the surface the brush drag began on, in a perspective view. */
  let boxSurface: Vec3 | null = null;

  const surfaceUnder = (p: Point): Vec3 | null => {
    if (!isPerspectiveState(camera.state)) return null;
    const ray = rayAt(p);
    return props.editor.pickSurface(ray.origin, ray.dir, undefined, undefined, xrayPick())?.position ?? null;
  };

  /** The plane brushes are drawn on: the ground in perspective and top, the view plane in front and side. */
  const brushPlane = (): { point: Vec3; normal: Vec3 } => {
    const t = camera.state.target;
    const k = camera.state.kind;
    if (k === "front" || k === "back") return { point: [t[0], t[1], t[2]], normal: [0, 1, 0] };
    if (k === "side" || k === "right") return { point: [t[0], t[1], t[2]], normal: [1, 0, 0] };
    return { point: [0, 0, boxSurface ? boxSurface[2] : 0], normal: [0, 0, 1] };
  };

  const brushPoint = (p: Point): Vec3 | null => {
    const ray = rayAt(p);
    const plane = brushPlane();
    const t = rayPlane(ray, plane.point, plane.normal);
    if (t === null || t < 0) return null;
    return [ray.origin[0] + ray.dir[0] * t, ray.origin[1] + ray.dir[1] * t, ray.origin[2] + ray.dir[2] * t];
  };

  /** Box corners for the current drag; the axis along the plane normal gets the tool's height. */
  const boxBounds = (): { min: Vec3; max: Vec3 } | null => {
    if (!boxStart || !boxEnd) return null;
    return dragBox(boxStart, boxEnd, brushPlane().normal, state.snap.size as GridSize, state.tool.brush.height);
  };

  const updateBoxDrag = (p: Point): void => {
    boxEnd = brushPoint(p) ?? boxEnd;
    const b = boxBounds();
    if (b) props.editor.note(`box ${b.max[0] - b.min[0]} x ${b.max[1] - b.min[1]} x ${b.max[2] - b.min[2]}`);
  };

  const finishBoxDrag = (): void => {
    const b = boxBounds();
    boxStart = null;
    boxEnd = null;
    boxSurface = null;
    if (!b) {
      props.editor.note("brush: point at the grid to draw a box");
      return;
    }
    try {
      props.editor.brushBox(b.min, b.max, state.tool.brush.material);
    } catch (err) {
      props.editor.note(err instanceof Error ? err.message : String(err));
    }
  };

  /**
   * Collects the points of a clip plane. In a perspective view that is three
   * points on surfaces; in an ortho view it is two, and the plane holds the
   * line between them and the view axis, the way a 2D view draws a cut.
   */
  const addClipPoint = (p: Point): void => {
    const ray = rayAt(p);
    const ortho = !isPerspectiveState(camera.state);
    const point = ortho ? workPoint(p) : props.editor.pickWorld(ray.origin, ray.dir, undefined, xrayPick()) ?? workPoint(p);
    if (!point) {
      props.editor.note("clip: no point under the cursor");
      return;
    }
    const snapped = ortho ? snapAll(point, state.snap.size) : snapToGrid(point, state.snap.size as GridSize);
    const next = [...clipPoints(), snapped];
    const needed = ortho ? 2 : 3;
    if (next.length < needed) {
      setClipPoints(next);
      props.editor.note(`clip: point ${next.length} of ${needed}`);
      return;
    }
    if (ortho) {
      const axis = viewForward(camera.state);
      next.push([next[0][0] + axis[0] * 64, next[0][1] + axis[1] * 64, next[0][2] + axis[2] * 64]);
    }
    setClipPoints([]);
    const targets = state.selection.ids.filter((id) => props.editor.store?.object(id)?.kind.type === "brush");
    if (targets.length === 0) {
      props.editor.note("clip: select a brush first");
      return;
    }
    const [normal, dist] = planeFromThree(next[0], next[1], next[2]);
    try {
      const touched = props.editor.brushClip(targets, normal, dist, "front");
      props.editor.note(`clip: ${touched.length} brushes changed`);
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };

  const onContextMenu = (e: MouseEvent): void => e.preventDefault();

  /** Places a dropped asset on the surface under the cursor. */
  const onDrop = (e: DragEvent): void => {
    e.preventDefault();
    e.stopPropagation();
    const raw = e.dataTransfer?.getData("text/r5ed-asset");
    if (!raw) return;
    let asset: { kind: string; path: string };
    try {
      asset = JSON.parse(raw) as { kind: string; path: string };
    } catch {
      props.editor.note("the dropped payload was not an asset");
      return;
    }
    const point = localPoint(e as unknown as MouseEvent);
    const ray = rayAt(point);
    const at = props.editor.pickWorld(ray.origin, ray.dir, undefined, xrayPick()) ?? workPoint(point) ?? [ray.origin[0] + ray.dir[0] * 256, ray.origin[1] + ray.dir[1] * 256, ray.origin[2] + ray.dir[2] * 256];
    const placed = props.editor.placeAsset(asset.kind, asset.path, snapToGrid(at, state.snap.size as GridSize));
    if (placed.length > 0) props.editor.note(`placed ${placed.length} of ${asset.path}`);
  };

  const onDragOver = (e: DragEvent): void => {
    if (e.dataTransfer?.types.includes("text/r5ed-asset")) e.preventDefault();
  };

  const frameAll = (): void => {
    const b = props.editor.scene ? sceneBounds(props.editor.scene) : null;
    if (b) frameBox(b.min, b.max);
  };

  createMemo(() => {
    if (state.ui.theme) {
      if (renderer && document) {
        const glyphs = document.createElement("canvas");
        atlas = buildGlyphAtlas(glyphs, state.ui.theme);
        renderer.setAtlas(atlas);
      }
    }
    return null;
  });

  // --- overlay: 3D cursor, navigation widget, modal feedback ---

  const overlay = createMemo(() => {
    void viewTick();
    void state.docRevision;
    const m = modal();
    const cursor = state.cursorWorld ? toScreen(state.cursorWorld) : null;
    let pivotLine: { a: Point; b: Point } | null = null;
    let axisLine: { a: Point; b: Point; color: string } | null = null;
    if (m?.kind === "transform") {
      const ps = toScreen(m.pivot);
      if (ps && m.op.mode !== "grab") pivotLine = { a: ps, b: m.at };
      const c = m.op.constraint;
      if (c && !c.plane) {
        const dir = axisVector(m, c.axis);
        const far = panScale(camera.state, canvas.clientHeight) * 4000;
        const a = toScreen(add(m.pivot, scale(dir, -far)));
        const b = toScreen(add(m.pivot, scale(dir, far)));
        if (a && b) axisLine = { a, b, color: AXIS_COLORS[c.axis] };
      }
    }
    let brush: { at: Point; r: number } | null = null;
    const p = pointer();
    if (p && !m && (state.tool.id === "terrain_sculpt" || state.tool.id === "terrain_paint" || state.tool.id === "terrain_voxel")) {
      brush = { at: p, r: Math.max(4, state.tool.sculpt.radius / Math.max(1e-6, panScale(camera.state, canvas.clientHeight))) };
    }
    let influence: { at: Point; r: number } | null = null;
    if (m?.kind === "transform" && m.neighbours.length > 0) {
      const at = toScreen(m.pivot);
      if (at) influence = { at, r: state.proportional.radius / Math.max(1e-6, panScale(camera.state, canvas.clientHeight)) };
    }
    return { cursor, pivotLine, axisLine, brush, influence };
  });

  /** The axis ball: each world axis turned into this view, for the top-right widget. */
  const navAxes = createMemo(() => {
    void viewTick();
    const f = viewForward(camera.state);
    const up0 = normalize(viewUp(camera.state.kind, f), [0, 0, 1]);
    const right = normalize(cross(f, up0), [1, 0, 0]);
    const up = normalize(cross(right, f), up0);
    const out = [0, 1, 2].flatMap((i) => {
      const a = AXES[i];
      const x = dot(a, right);
      const y = -dot(a, up);
      const depth = dot(a, f);
      return [
        { axis: i, positive: true, x, y, depth, label: "XYZ"[i] },
        { axis: i, positive: false, x: -x, y: -y, depth: -depth, label: "" },
      ];
    });
    // Farther balls first, so the near ones draw on top.
    return out.sort((a, b) => b.depth - a.depth);
  });

  const NAV_KINDS: Exclude<ViewKind, "persp">[][] = [
    ["right", "side"],
    ["back", "front"],
    ["top", "bottom"],
  ];

  let navDrag: Point | null = null;
  const onNavDown = (e: PointerEvent): void => {
    e.stopPropagation();
    e.preventDefault();
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    navDrag = { x: e.clientX, y: e.clientY };
  };
  const onNavMove = (e: PointerEvent): void => {
    if (!navDrag || locked()) return;
    const dx = e.clientX - navDrag.x;
    const dy = e.clientY - navDrag.y;
    navDrag = { x: e.clientX, y: e.clientY };
    orbit(camera.state, -dx * state.input.orbitSpeed, dy * state.input.orbitSpeed);
    publishCamera();
  };
  const onNavUp = (): void => {
    navDrag = null;
  };

  const snapToAxisBall = (axis: number, positive: boolean): void => {
    // Clicking the +X ball looks from +X toward the center, which is the right view.
    const kind = NAV_KINDS[axis][positive ? 0 : 1];
    if (locked()) return;
    // The ball of the view already shown flips to the opposite side, as in Blender.
    if (camera.state.kind === kind) flipView(camera.state);
    else setAxisView(camera.state, kind);
    publishCamera();
  };

  const activeName = createMemo(() => {
    void state.docRevision;
    const id = state.selection.primary;
    return id ? props.editor.store?.object(id)?.name ?? "" : "";
  });

  return (
    <div
      class="viewport"
      classList={{ active: props.active, persp: props.kind === "persp", operating: modal() !== null }}
      onPointerEnter={() => setState("ui", "hoverView", props.index)}
      onPointerLeave={() => {
        if (!drag() && !modal()) setState("ui", "hoverView", null);
      }}
    >
      <canvas
        ref={canvas}
        tabindex={0}
        aria-label={`${props.kind} viewport`}
        data-view={props.kind}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerLeave={() => {
          if (!drag()) setPointer(null);
        }}
        onWheel={onWheel}
        onContextMenu={onContextMenu}
        onDrop={onDrop}
        onDragOver={onDragOver}
      />
      <svg class="view-overlay-svg" aria-hidden="true">
        <Show when={overlay().axisLine}>
          {(l) => <line x1={l().a.x} y1={l().a.y} x2={l().b.x} y2={l().b.y} stroke={l().color} stroke-width="1.5" />}
        </Show>
        <Show when={overlay().pivotLine}>
          {(l) => <line class="pivot-line" x1={l().a.x} y1={l().a.y} x2={l().b.x} y2={l().b.y} />}
        </Show>
        <Show when={overlay().influence}>
          {(c) => <circle class="influence-circle" cx={c().at.x} cy={c().at.y} r={c().r} />}
        </Show>
        <Show when={(() => {
          const m = modal();
          return m?.kind === "lasso" && m.path.length > 1 ? m.path : null;
        })()}>
          {(path) => <polygon class="select-box" points={path().map((q) => `${q.x},${q.y}`).join(" ")} />}
        </Show>
        <Show when={(() => {
          const m = modal();
          return m?.kind === "zoom" && m.start ? m : null;
        })()}>
          {(m) => (
            <rect
              class="select-box"
              x={Math.min(m().start!.x, m().at.x)}
              y={Math.min(m().start!.y, m().at.y)}
              width={Math.abs(m().at.x - m().start!.x)}
              height={Math.abs(m().at.y - m().start!.y)}
            />
          )}
        </Show>
        <Show when={modal()?.kind === "walk"}>
          <g class="walk-crosshair" transform={`translate(${(void viewTick(), canvas.clientWidth / 2)} ${canvas.clientHeight / 2})`}>
            <line x1="-10" y1="0" x2="-4" y2="0" />
            <line x1="4" y1="0" x2="10" y2="0" />
            <line x1="0" y1="-10" x2="0" y2="-4" />
            <line x1="0" y1="4" x2="0" y2="10" />
          </g>
        </Show>
        <Show when={overlay().brush}>
          {(b) => <circle class="brush-circle" cx={b().at.x} cy={b().at.y} r={b().r} />}
        </Show>
        <Show when={overlay().cursor}>
          {(c) => (
            <g class="cursor3d" transform={`translate(${c().x} ${c().y})`}>
              <circle r="10" />
              <line x1="-18" y1="0" x2="-6" y2="0" />
              <line x1="6" y1="0" x2="18" y2="0" />
              <line x1="0" y1="-18" x2="0" y2="-6" />
              <line x1="0" y1="6" x2="0" y2="18" />
            </g>
          )}
        </Show>
        <Show when={(() => {
          const m = modal();
          return m?.kind === "box" && m.start ? m : null;
        })()}>
          {(m) => (
            <rect
              class="select-box"
              x={Math.min(m().start!.x, m().at.x)}
              y={Math.min(m().start!.y, m().at.y)}
              width={Math.abs(m().at.x - m().start!.x)}
              height={Math.abs(m().at.y - m().start!.y)}
            />
          )}
        </Show>
        <Show when={(() => {
          const d = drag();
          const p = pointer();
          return d?.mode === "select" && p && Math.hypot(p.x - d.sx, p.y - d.sy) >= CLICK_SLOP ? { d, p } : null;
        })()}>
          {(v) => (
            <rect
              class="select-box"
              x={Math.min(v().d.sx, v().p.x)}
              y={Math.min(v().d.sy, v().p.y)}
              width={Math.abs(v().p.x - v().d.sx)}
              height={Math.abs(v().p.y - v().d.sy)}
            />
          )}
        </Show>
        <Show when={(() => {
          const m = modal();
          return m?.kind === "box" && !m.start ? pointer() : null;
        })()}>
          {(p) => (
            <g class="box-crosshair">
              <line x1={0} y1={p().y} x2={9999} y2={p().y} />
              <line x1={p().x} y1={0} x2={p().x} y2={9999} />
            </g>
          )}
        </Show>
        <Show when={(() => {
          const m = modal();
          return m?.kind === "circle" ? m : null;
        })()}>
          {(m) => <circle class="select-circle" cx={m().at.x} cy={m().at.y} r={m().radius} />}
        </Show>
      </svg>
      <div class="view-label">
        <span class="view-name">{(void viewTick(), viewLabel(camera.state))}</span>
        <Show when={editInfo()}>
          {(info) => (
            <span class="view-edit-mode">
              edit {info().mode} V{info().counts.verts} E{info().counts.edges} F{info().counts.faces}
              {(drag()?.mode === "mesh" ? " (drag, Esc cancels)" : "")}
            </span>
          )}
        </Show>
        <Show when={activeName()}>
          <span class="view-active">{activeName()}</span>
        </Show>
        <Show when={clipPoints().length > 0}>
          <span class="clip-count">clip {clipPoints().length}/{isPerspectiveState(camera.state) ? 3 : 2}</span>
        </Show>
      </div>
      <div class="nav-widget">
        <svg class="nav-ball" viewBox="-50 -50 100 100" onPointerDown={onNavDown} onPointerMove={onNavMove} onPointerUp={onNavUp} aria-label="View orientation">
          <circle class="nav-bg" r="48" />
          <For each={navAxes()}>
            {(a) => (
              <g
                class="nav-axis"
                classList={{ negative: !a.positive }}
                onClick={(e) => {
                  e.stopPropagation();
                  snapToAxisBall(a.axis, a.positive);
                }}
              >
                <Show when={a.positive}>
                  <line x1="0" y1="0" x2={a.x * 34} y2={a.y * 34} stroke={AXIS_COLORS[a.axis]} stroke-width="2" />
                </Show>
                <circle cx={a.x * 34} cy={a.y * 34} r={a.positive ? 9 : 7} fill={AXIS_COLORS[a.axis]} fill-opacity={a.positive ? 1 : 0.35} />
                <Show when={a.label}>
                  <text x={a.x * 34} y={a.y * 34 + 3.5}>
                    {a.label}
                  </text>
                </Show>
              </g>
            )}
          </For>
        </svg>
        <div class="nav-buttons">
          <button type="button" title="Zoom (drag)" onPointerDown={(e) => navButtonDrag(e, "zoom")}>
            <svg viewBox="0 0 16 16"><circle cx="7" cy="7" r="4.5" /><line x1="10.5" y1="10.5" x2="14" y2="14" /></svg>
          </button>
          <button type="button" title="Pan (drag)" onPointerDown={(e) => navButtonDrag(e, "pan")}>
            <svg viewBox="0 0 16 16"><path d="M8 1v14M1 8h14M8 1l-2 2M8 1l2 2M8 15l-2-2M8 15l2-2M1 8l2-2M1 8l2 2M15 8l-2-2M15 8l-2 2" /></svg>
          </button>
          <button type="button" title="Perspective / orthographic (Numpad 5)" onClick={() => controller.run("view_persp_ortho")}>
            <svg viewBox="0 0 16 16"><rect x="2" y="2" width="12" height="12" /><path d="M2 2l3 3h6l3-3M5 5v6l-3 3M11 5v6l3 3M5 11h6" /></svg>
          </button>
          <button type="button" title="Frame all (Home)" onClick={frameAll}>
            <svg viewBox="0 0 16 16"><path d="M1 5V1h4M11 1h4v4M15 11v4h-4M5 15H1v-4" /></svg>
          </button>
        </div>
      </div>
      <Show when={toastText()}>
        <div class="view-toast" role="status">
          {toastText()}
        </div>
      </Show>
      <Show when={loading()}>
        <div class="view-overlay">starting WebGPU</div>
      </Show>
      <Show when={error()}>
        {(message) => (
          <div class="view-overlay fatal" role="alert">
            <h2>WebGPU unavailable</h2>
            <p>{message()}</p>
          </div>
        )}
      </Show>
    </div>
  );

  /** The zoom and pan buttons drag the view, for mice and pads without a middle button. */
  function navButtonDrag(e: PointerEvent, mode: "zoom" | "pan"): void {
    e.preventDefault();
    e.stopPropagation();
    let last = { x: e.clientX, y: e.clientY };
    const move = (ev: PointerEvent): void => {
      const dx = ev.clientX - last.x;
      const dy = ev.clientY - last.y;
      last = { x: ev.clientX, y: ev.clientY };
      if (mode === "zoom") zoomBy(Math.exp(dy * 0.005), null);
      else pan(camera.state, dx * panScale(camera.state, canvas.clientHeight), -dy * panScale(camera.state, canvas.clientHeight));
      publishCamera();
    };
    const up = (): void => {
      document.removeEventListener("pointermove", move);
      document.removeEventListener("pointerup", up);
    };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", up);
  }
}

export { VIEW_KINDS, defaultCamera, viewAxis, cameraFor };
export type { Scene, ToolId, GizmoHandle, Lighting };
