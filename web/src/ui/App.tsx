import { refPropIndex } from "../render/refprops";
import { type JSX, For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { Id, Vec3 } from "../core/api";
import { Editor, NEW_OBJECT_KINDS, newId, type NewObjectKind } from "../state/editor";
import {
  clearSelection,
  openDialog,
  select,
  setCursorWorld,
  setIsolate,
  setLayout,
  setSnap,
  setState,
  type PivotMode,
  type Falloff,
  setTool,
  setUi,
  state,
  toggleTheme,
  type ToolId,
} from "../state/stores";
import { COMMANDS, chordFor, isTextTarget, resolve, type CommandId, type KeyContext } from "../input/keymap";
import { Menu, type MenuItem, type MenuSpec } from "./menus";
import { targetView, anyViewBusy } from "./viewRegistry";
import type { PickMode } from "./Viewport";
import { TopBar } from "./TopBar";
import { Outliner } from "./Outliner";
import { Inspector } from "./Inspector";
import { AssetBrowser, JobPanel } from "./AssetBrowser";
import { MessageLog, StatusBar } from "./StatusBar";
import { Viewport } from "./Viewport";
import { Toolbar } from "./Toolbar";
import {
  AboutDialog,
  ImportDialog,
  MapSettingsDialog,
  NewMeshDialog,
  NewTerrainDialog,
  ProjectDialog,
  ShortcutsDialog,
} from "./dialogs";
import type { DocObject } from "../doc/model";
import type { SceneObject } from "../render/scene";
import { snapValue } from "../render/gizmo";
import { quatConjugate, quatRotate } from "../math/quat";
import { opRequest, selJson } from "../state/meshEdit";
import { EDIT_PRIMITIVES, MESH_OPS, type MeshOpKey, type OpSpec } from "../state/meshOps";
import { setShading } from "../state/stores";
import { setMode, toggleWireframe, toggleXray, type ShadingMode } from "../render/shading";

const GIZMO_BY_COMMAND: Partial<Record<CommandId, ToolId>> = {
  gizmo_translate: "move",
  gizmo_rotate: "rotate",
  gizmo_scale: "scale",
};

const TOOL_BY_COMMAND: Partial<Record<CommandId, ToolId>> = {
  tool_select: "select",
  tool_move: "move",
  tool_rotate: "rotate",
  tool_scale: "scale",
  tool_brush: "brush",
  tool_clip: "clip",
  tool_vertex: "vertex",
  tool_edge: "edge",
  tool_face: "face",
  tool_sculpt: "terrain_sculpt",
  tool_paint: "terrain_paint",
  tool_voxel: "terrain_voxel",
  tool_entity: "entity",
};

export interface AppProps {
  editor: Editor;
}

/** The editor shell: toolbar, panels, viewports, dialogs and the key map. */
/** How long validation waits after the last edit. */
const VALIDATE_DELAY_MS = 400;

export function App(props: AppProps): JSX.Element {
  const [projectMode, setProjectMode] = createSignal<"new" | "open" | "save_as" | null>(null);
  const [objects, setObjects] = createSignal<DocObject[]>([]);
  const [layers, setLayers] = createSignal(props.editor.store?.layers.slice() ?? []);
  const [issues, setIssues] = createSignal<{ id: string | null; message: string }[]>([]);
  const [assetDrag, setAssetDrag] = createSignal(false);
  const [menu, setMenu] = createSignal<MenuSpec | null>(null);
  let mouse = { x: 0, y: 0 };
  let hoverTarget: Element | null = null;

  let refreshedDoc: unknown = null;
  let validateTimer: ReturnType<typeof setTimeout> | undefined;
  const refresh = (): void => {
    const store = props.editor.store;
    if (!store || props.editor.gesturing) return;
    const snap = store.snapshot();
    // Every edit replaces the document, so an unchanged one needs no rework.
    if (snap.doc === refreshedDoc) return;
    refreshedDoc = snap.doc;
    setObjects([...snap.objects.values()]);
    setLayers(store.layers.slice());
    // Validation walks the whole document; it runs once edits settle rather
    // than after each one.
    clearTimeout(validateTimer);
    validateTimer = setTimeout(() => {
      try {
        setIssues(props.editor.validate());
      } catch {
        setIssues([]);
      }
    }, VALIDATE_DELAY_MS);
  };

  createEffect(() => {
    void state.docRevision;
    refresh();
  });

  const selected = (): DocObject | null => {
    // The store is not reactive, so the revision is what makes the inspector
    // see an edit; without it the next field commits over a stale object.
    void state.docRevision;
    const id = state.selection.primary;
    return id ? props.editor.store?.object(id) ?? null : null;
  };

  const suggestions = createMemo<Record<string, string[]>>(() => {
    const models: string[] = [];
    const materials: string[] = [];
    const sounds: string[] = [];
    for (const o of objects()) {
      if (o.kind.type === "prop") models.push(o.kind.prop.model);
      if (o.kind.type === "brush") for (const f of o.kind.brush.faces) if (!materials.includes(f.material)) materials.push(f.material);
      if (o.kind.type === "entity") {
        for (const [k, v] of Object.entries(o.kind.entity.keyvalues)) {
          if (k.startsWith("sounds") && !sounds.includes(v)) sounds.push(v);
        }
      }
    }
    return { model: models, material: materials, sound: sounds, string: [], target: [], enum: [], flags: [] };
  });

  const run = (id: CommandId): void => {
    const tool = TOOL_BY_COMMAND[id];
    if (tool) {
      setTool(tool);
      return;
    }
    const gizmo = GIZMO_BY_COMMAND[id];
    if (gizmo) {
      // The gizmo modes are the move, rotate and scale tools.
      setTool(gizmo);
      return;
    }
    const view = targetView();
    if (view?.run(id)) return;
    const ids = (): Id[] => [...state.selection.ids];
    switch (id) {
      case "frame_all":
        frameObjects(false);
        break;
      case "select_invert":
        if (props.editor.meshEditing) meshSelectInvert();
        else props.editor.invertSelection();
        break;
      case "select_grouped":
        openList("Select grouped", groupedItems());
        break;
      case "hide":
        props.editor.setHidden(ids(), true);
        clearSelection();
        break;
      case "hide_unselected": {
        const keep = new Set(state.selection.ids);
        props.editor.setHidden(props.editor.store?.ids().filter((i) => !keep.has(i)) ?? [], true);
        break;
      }
      case "reveal":
        props.editor.revealAll();
        break;
      case "clear_location":
        props.editor.clearTransform("position");
        break;
      case "clear_rotation":
        props.editor.clearTransform("rotation");
        break;
      case "clear_scale":
        props.editor.clearTransform("scale");
        break;
      case "toolbar_toggle":
        setUi({ showToolbar: !state.ui.showToolbar });
        break;
      case "layout_toggle":
        setLayout(state.ui.layout === "four" ? "one" : "four");
        break;
      case "maximize":
        maximize();
        break;
      case "add_menu":
        openList("Add", addItems());
        break;
      case "search":
        setMenu({ type: "search", title: "Search", x: mouse.x, y: mouse.y, items: searchItems() });
        break;
      case "rename": {
        const object = selected();
        if (!object) break;
        setMenu({ type: "prompt", title: "Rename", x: mouse.x, y: mouse.y, value: object.name, submit: (v) => v.trim() && props.editor.rename(object, v.trim()) });
        break;
      }
      case "view_pie":
        setMenu({ type: "pie", title: "View", x: mouse.x, y: mouse.y, items: viewPie() });
        break;
      case "snap_pie":
        setMenu({ type: "pie", title: "Snap", x: mouse.x, y: mouse.y, items: snapPie() });
        break;
      case "cursor_to_selected":
      case "cursor_reset":
      case "selection_to_cursor":
        snapOp(id);
        break;
      case "cursor_to_grid": {
        const c = state.cursorWorld ?? [0, 0, 0];
        const g = state.snap.size;
        setCursorWorld([snapValue(c[0], g), snapValue(c[1], g), snapValue(c[2], g)]);
        break;
      }
      case "cursor_to_active":
        cursorToActive();
        break;
      case "selection_to_grid":
        selectionToGrid();
        break;
      case "selection_to_active": {
        const a = state.selection.primary ? props.editor.worldTransform(state.selection.primary)?.position : null;
        if (a) moveSelected(() => [...a] as Vec3);
        break;
      }
      case "selection_to_cursor_offset": {
        const ps = selectedPositions();
        if (ps.length === 0) break;
        const mid = ps.reduce((acc, q) => [acc[0] + q[0] / ps.length, acc[1] + q[1] / ps.length, acc[2] + q[2] / ps.length], [0, 0, 0] as Vec3);
        const c = state.cursorWorld ?? [0, 0, 0];
        moveSelected((q) => [q[0] + c[0] - mid[0], q[1] + c[1] - mid[1], q[2] + c[2] - mid[2]]);
        break;
      }
      case "duplicate_linked":
        run("duplicate");
        break;
      case "parent_set":
        props.editor.note(`parented ${props.editor.parentToActive().length} to the active object`);
        break;
      case "parent_clear":
        props.editor.note(`cleared the parent of ${props.editor.clearParentKeep().length}`);
        break;
      case "mirror":
        openList("Mirror", mirrorItems());
        break;
      case "align_objects":
        alignStep1();
        break;
      case "randomize_transform":
        setMenu({
          type: "prompt",
          title: "Randomize: location x y z, rotation x y z (deg), scale, seed",
          x: mouse.x,
          y: mouse.y,
          value: "64 64 0, 0 0 180, 0.1, 1",
          submit: randomize,
        });
        break;
      case "proportional_toggle":
        setState("proportional", "enabled", (on) => !on);
        props.editor.note(`proportional editing ${state.proportional.enabled ? "on" : "off"}`);
        break;
      case "proportional_falloff":
        setMenu({ type: "pie", title: "Falloff", x: mouse.x, y: mouse.y, items: falloffPie() });
        break;
      case "orientation_pie":
        setMenu({ type: "pie", title: "Orientation", x: mouse.x, y: mouse.y, items: orientationPie() });
        break;
      case "pivot_pie":
        setMenu({ type: "pie", title: "Pivot", x: mouse.x, y: mouse.y, items: pivotPie() });
        break;
      case "snap_target_pie":
        setMenu({ type: "pie", title: "Snap to", x: mouse.x, y: mouse.y, items: snapTargetPie() });
        break;
      case "view_center_cursor":
        setCursorWorld([0, 0, 0]);
        frameObjects(false);
        break;
      case "local_view_remove":
        props.editor.removeFromLocalView();
        break;
      case "select_linked":
        openList("Select linked", linkedItems());
        break;
      case "select_more":
      case "select_less":
        selectHierarchy(id === "select_more");
        break;
      case "select_by_type":
        openList("Select all by type", typeItems());
        break;
      case "select_random":
        setMenu({ type: "prompt", title: "Select random: percent of unselected", x: mouse.x, y: mouse.y, value: "50", submit: selectRandom });
        break;
      case "select_pattern":
        setMenu({ type: "prompt", title: "Select pattern (* and ? wildcards)", x: mouse.x, y: mouse.y, value: "*", submit: selectPattern });
        break;
      case "undo":
        props.editor.undo();
        break;
      case "redo":
        props.editor.redo();
        break;
      case "save":
        void props.editor.save().catch((e: unknown) => props.editor.note(e instanceof Error ? e.message : String(e)));
        break;
      case "new":
        setProjectMode("new");
        break;
      case "open":
        setProjectMode("open");
        break;
      case "save_as":
        setProjectMode("save_as");
        break;
      case "delete":
        props.editor.removeObjects([...state.selection.ids]);
        break;
      case "duplicate": {
        // Blender duplicates in place and grabs, so the copy follows the pointer.
        const blender = props.editor.keymap.preset === "blender";
        const copies = props.editor.duplicate(ids(), blender ? 0 : 16);
        if (copies.length > 0) select(copies);
        if (blender && copies.length > 0) targetView()?.run("grab");
        break;
      }
      case "group":
        props.editor.group([...state.selection.ids]);
        break;
      case "ungroup":
        props.editor.ungroup([...state.selection.ids]);
        break;
      case "isolate":
        setIsolate(!state.selection.isolate, [...state.selection.ids]);
        break;
      case "select_all":
        if (props.editor.meshEditing) meshSelectAll();
        else props.editor.selectAll();
        break;
      case "deselect":
        if (props.editor.meshEditing) meshDeselect();
        else props.editor.clear();
        break;
      case "frame":
        frameObjects(true);
        break;
      case "snap_toggle":
        setSnap({ enabled: !state.snap.enabled });
        break;
      case "grid_toggle":
        setUi({ showGrid: !state.ui.showGrid });
        break;
      case "gizmo_toggle":
        setUi({ showGizmo: !state.ui.showGizmo });
        break;
      case "gizmo_space":
        setOrientation(state.orientation === "global" ? "local" : "global");
        break;
      case "layout_one":
        setLayout("one");
        break;
      case "layout_four":
        setLayout("four");
        break;
      case "theme":
        toggleTheme();
        break;
      case "project":
        openDialog("project");
        break;
      case "import":
        openDialog("import");
        break;
      case "map_settings":
        openDialog("map-settings");
        break;
      case "new_terrain":
        openDialog("new-terrain");
        break;
      case "shortcuts":
        openDialog("shortcuts");
        break;
      case "help":
        openDialog("about");
        break;
      case "live_toggle":
        if (props.editor.live) props.editor.disconnectLive();
        else props.editor.connectLive();
        break;
      case "jobs":
        if (state.jobs.panelOpen) setState("jobs", "panelOpen", false);
        else void props.editor.refreshJobs().catch((e: unknown) => props.editor.note(e instanceof Error ? e.message : String(e)));
        break;
      case "compile":
      case "bake":
      case "package":
      case "deploy":
      case "reload":
        void props.editor.startJob(id === "compile" ? "compile" : id, {}).catch((e: unknown) => props.editor.note(e instanceof Error ? e.message : String(e)));
        break;
      case "focus_outliner":
      case "focus_inspector":
      case "focus_assets":
        (document.querySelector(`[aria-label="${labelFor(id)}"]`) as HTMLElement | null)?.focus({ preventScroll: true });
        break;
      case "mesh_toggle_edit":
        props.editor.toggleMeshEdit();
        break;
      case "shading_pie":
        setMenu({ type: "pie", title: "Shading", x: mouse.x, y: mouse.y, items: shadingPie() });
        break;
      case "shading_wire_toggle":
        setShading(toggleWireframe(state.ui.shading));
        break;
      case "shading_xray_toggle":
        setShading(toggleXray(state.ui.shading));
        break;
      case "adjust_last": {
        const panel = document.querySelector("[data-redo-panel] input, [data-redo-panel] select") as HTMLElement | null;
        if (panel) panel.focus({ preventScroll: true });
        props.editor.note(props.editor.lastMeshOp ? "adjust the last operation below" : "no mesh operation to adjust yet");
        break;
      }
      case "mesh_delete":
        openList("Delete mesh", meshDeleteItems());
        break;
      case "mesh_merge_menu":
        openList("Merge", meshMergeItems());
        break;
      case "mesh_normals_menu":
        openList("Normals", meshNormalsItems());
        break;
      case "mesh_edge_menu":
        openList("Edge", meshEdgeItems());
        break;
      case "mesh_face_menu":
        openList("Face", meshFaceItems());
        break;
      case "mesh_vertex_menu":
        openList("Vertex", meshVertexItems());
        break;
      case "mesh_uv_menu":
        openList("UV", meshUvItems());
        break;
      case "mesh_add_menu":
        openList("Add mesh", meshAddItems());
        break;
      case "mesh_join":
        meshJoin();
        break;
      default:
        break;
    }
  };

  /** Points the view under the pointer at the selection, or at everything. */
  const frameObjects = (selectionOnly: boolean): void => {
    const chosen = new Set(state.selection.ids);
    const objects: SceneObject[] = props.editor.currentObjects().filter((o) => !o.hidden && !o.doc.hidden);
    const picked = selectionOnly && chosen.size > 0 ? objects.filter((o) => chosen.has(o.doc.id)) : objects;
    const boxes = picked.map((o) => o.worldBounds).filter((b): b is { min: Vec3; max: Vec3 } => !!b);
    if (boxes.length === 0) {
      props.editor.note("nothing to frame");
      return;
    }
    const min: Vec3 = [Infinity, Infinity, Infinity];
    const max: Vec3 = [-Infinity, -Infinity, -Infinity];
    for (const b of boxes) {
      for (let i = 0; i < 3; i++) {
        min[i] = Math.min(min[i], b.min[i]);
        max[i] = Math.max(max[i], b.max[i]);
      }
    }
    targetView()?.frame(min, max);
  };

  const maximize = (): void => {
    if (state.ui.layout === "four") {
      const index = state.ui.hoverView ?? 0;
      setState("ui", "viewports", (vps) => vps.map((v, i) => ({ ...v, active: i === index })));
      setState("ui", "layout", "one");
      setState("ui", "maximized", index);
    } else if (state.ui.maximized !== null) {
      setState("ui", "maximized", null);
      setLayout("four");
    }
  };

  const hint = (id: CommandId): string => chordFor(props.editor.keymap, id);

  const openList = (title: string, items: MenuItem[]): void => {
    setMenu({ type: "list", title, x: mouse.x, y: mouse.y, items });
  };

  const addItems = (): MenuItem[] => NEW_OBJECT_KINDS.map((k) => ({ label: k.label, run: () => create(k.id) }));

  const searchItems = (): MenuItem[] => [
    ...COMMANDS.filter((c) => c.id !== "search").map((c) => ({ label: c.label, hint: hint(c.id), run: () => run(c.id) })),
    ...NEW_OBJECT_KINDS.map((k) => ({ label: `Add ${k.label}`, run: () => create(k.id) })),
  ];

  const item = (label: string, id: CommandId): MenuItem => ({ label, hint: hint(id), run: () => run(id) });

  const viewPie = (): (MenuItem | null)[] => [
    item("Left", "view_left"),
    item("Right", "view_right"),
    item("Bottom", "view_bottom"),
    item("Top", "view_top"),
    item("Front", "view_front"),
    item("Back", "view_back"),
    item("View Selected", "frame"),
    item("Frame All", "frame_all"),
  ];

  /** Z pie: the shading mode, then the x-ray and overlay toggles. */
  const shadingPie = (): (MenuItem | null)[] => {
    const s = state.ui.shading;
    const mode = (m: ShadingMode): MenuItem => ({ label: mark(s.mode === m, m[0].toUpperCase() + m.slice(1)), run: () => setShading(setMode(s, m)) });
    return [
      mode("textured"),
      mode("solid"),
      mode("wireframe"),
      { label: mark(s.xray > 0, "X-ray"), run: () => setShading(toggleXray(s)) },
      { label: mark(s.wireOverlay, "Wire overlay"), run: () => setShading({ wireOverlay: !s.wireOverlay }) },
      { label: mark(state.ui.showLightmaps, "Lightmaps"), run: () => setUi({ showLightmaps: !state.ui.showLightmaps }) },
      null,
      null,
    ];
  };

  // Slot order: left, right, bottom, top, top-left, top-right, bottom-left, bottom-right, as Blender's snap pie.
  const snapPie = (): (MenuItem | null)[] => [
    item("Selection to Cursor", "selection_to_cursor"),
    item("Cursor to Selected", "cursor_to_selected"),
    item("Cursor to World Origin", "cursor_reset"),
    item("Selection to Grid", "selection_to_grid"),
    item("Cursor to Grid", "cursor_to_grid"),
    item("Selection to Active", "selection_to_active"),
    item("Selection to Cursor (Offset)", "selection_to_cursor_offset"),
    item("Cursor to Active", "cursor_to_active"),
  ];

  const PIVOT_LABELS: Record<PivotMode, string> = {
    median: "Median Point",
    bbox: "Bounding Box Center",
    active: "Active Element",
    individual: "Individual Origins",
    cursor: "3D Cursor",
  };

  const mark = (on: boolean, label: string): string => (on ? `* ${label}` : label);

  const pivotPie = (): (MenuItem | null)[] =>
    (["bbox", "cursor", "individual", "median", "active"] as PivotMode[]).map((pv) => ({
      label: mark(state.pivot === pv, PIVOT_LABELS[pv]),
      run: () => setState("pivot", pv),
    }));

  const setOrientation = (o: "global" | "local" | "view"): void => {
    setState("orientation", o);
    setState("gizmoSpace", o === "local" ? "local" : "world");
  };

  const orientationPie = (): (MenuItem | null)[] =>
    (["global", "local", "view"] as const).map((o) => ({ label: mark(state.orientation === o, o[0].toUpperCase() + o.slice(1)), run: () => setOrientation(o) }));

  const falloffPie = (): (MenuItem | null)[] =>
    (["smooth", "sphere", "root", "sharp", "linear", "constant"] as Falloff[]).map((f) => ({
      label: mark(state.proportional.falloff === f, f[0].toUpperCase() + f.slice(1)),
      run: () => setState("proportional", "falloff", f),
    }));

  const snapTargetPie = (): (MenuItem | null)[] => [
    { label: mark(state.snap.target === "increment", "Increment"), run: () => setSnap({ target: "increment" }) },
    { label: mark(state.snap.target === "grid", "Grid"), run: () => setSnap({ target: "grid" }) },
    { label: mark(state.snap.target === "face", "Face"), run: () => setSnap({ target: "face" }) },
    { label: mark(!!state.snap.alignRotation, "Align Rotation to Target"), run: () => setSnap({ alignRotation: !state.snap.alignRotation }) },
  ];

  const mirrorAxis = (axis: number, local: boolean): void => {
    const store = props.editor.store;
    const active = state.selection.primary ? store?.object(state.selection.primary) : null;
    const unit: Vec3 = [axis === 0 ? 1 : 0, axis === 1 ? 1 : 0, axis === 2 ? 1 : 0];
    const normal = local && active ? quatRotate(active.transform.rotation, unit) : unit;
    const ps = selectedPositions();
    if (ps.length === 0) return;
    const median = ps.reduce((acc, q) => [acc[0] + q[0] / ps.length, acc[1] + q[1] / ps.length, acc[2] + q[2] / ps.length], [0, 0, 0] as Vec3);
    const pivot = state.pivot === "cursor" && state.cursorWorld ? state.cursorWorld : state.pivot === "active" && active ? active.transform.position : median;
    props.editor.mirrorSelection(normal, pivot, state.pivot === "individual");
  };

  const mirrorItems = (): MenuItem[] => [
    { label: "X Global", run: () => mirrorAxis(0, false) },
    { label: "Y Global", run: () => mirrorAxis(1, false) },
    { label: "Z Global", run: () => mirrorAxis(2, false) },
    { label: "Local X", run: () => mirrorAxis(0, true) },
    { label: "Local Y", run: () => mirrorAxis(1, true) },
    { label: "Local Z", run: () => mirrorAxis(2, true) },
  ];

  /** Runs a mesh op on the session selection, noting failures instead of throwing. */
  const meshOp = (op: string, params: Record<string, unknown>): void => {
    const session = props.editor.meshEdit;
    if (!session) {
      props.editor.note("no mesh is being edited");
      return;
    }
    try {
      const cur = props.editor.meshSessionIds();
      props.editor.runMeshOp(opRequest(session.object, op, JSON.parse(selJson(cur.mode, cur.ids)), params));
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };

  const meshDeleteItems = (): MenuItem[] =>
    (["verts", "edges", "faces", "only_faces", "dissolve"] as const).map((mode) => ({
      label: mode,
      run: () => meshOp("delete", { mode }),
    }));

  const meshMergeItems = (): MenuItem[] => [
    { label: "At center", run: () => meshOp("merge", { mode: "center" }) },
    { label: "At cursor", run: () => meshOp("merge", { mode: "cursor", cursor: meshCursor() }) },
    { label: "Collapse", run: () => meshOp("merge", { mode: "collapse" }) },
    { label: "By distance", run: () => meshOp("merge", { mode: "by_distance" }) },
  ];

  /** The 3D cursor in the edited object's space, for cursor-pivot ops. */
  const meshCursor = (): Vec3 => {
    const world = state.cursorWorld ?? [0, 0, 0];
    const id = props.editor.meshEdit?.object;
    const t = id ? props.editor.store?.object(id)?.transform : null;
    if (!t) return world;
    const rel: Vec3 = [world[0] - t.position[0], world[1] - t.position[1], world[2] - t.position[2]];
    const s = t.scale;
    const unscaled: Vec3 = [rel[0] / (s[0] || 1), rel[1] / (s[1] || 1), rel[2] / (s[2] || 1)];
    return quatRotate(quatConjugate(t.rotation), unscaled);
  };

  const meshNormalsItems = (): MenuItem[] => [
    { label: "Recalculate outside", run: () => meshOp("recalc_normals", { mode: "outside" }) },
    { label: "Recalculate inside", run: () => meshOp("recalc_normals", { mode: "inside" }) },
    { label: "Flip", run: () => meshOp("flip", {}) },
    { label: "Shade smooth", run: () => meshOp("shade", { mode: "smooth" }) },
    { label: "Shade flat", run: () => meshOp("shade", { mode: "flat" }) },
  ];

  /** A catalog op run straight away with its defaults; the redo panel tunes it after. */
  const catalogItem = (key: MeshOpKey): MenuItem => {
    const spec = MESH_OPS[key] as OpSpec;
    return { label: spec.label, run: () => meshOp(spec.op, { ...spec.params }) };
  };

  const meshEdgeItems = (): MenuItem[] => [
    item("Loop cut and slide", "mesh_loop_cut"),
    item("Edge slide", "mesh_edge_slide"),
    item("Bevel edges", "mesh_bevel"),
    catalogItem("subdivide"),
    catalogItem("unsubdivide"),
    catalogItem("bridge"),
    catalogItem("dissolve_edges"),
    catalogItem("mark_seam"),
    catalogItem("clear_seam"),
    catalogItem("mark_sharp"),
    catalogItem("clear_sharp"),
  ];

  const meshFaceItems = (): MenuItem[] => [
    item("Inset faces", "mesh_inset"),
    catalogItem("subdivide"),
    catalogItem("dissolve_limited"),
    catalogItem("boolean_difference"),
    catalogItem("boolean_union"),
    catalogItem("boolean_intersect"),
  ];

  const meshVertexItems = (): MenuItem[] => [
    item("Bevel vertices", "mesh_bevel_vertices"),
    catalogItem("spin"),
    catalogItem("screw"),
    catalogItem("dissolve_verts"),
  ];

  const meshUvItems = (): MenuItem[] => [
    catalogItem("uv_box"),
    catalogItem("uv_cylinder"),
    catalogItem("uv_planar"),
    catalogItem("uv_unwrap"),
    catalogItem("uv_pack"),
    catalogItem("uv_reset"),
    catalogItem("mark_seam"),
    catalogItem("clear_seam"),
  ];

  /** Shapes added into the edited mesh at the 3D cursor, as new selected elements. */
  const meshAddItems = (): MenuItem[] =>
    EDIT_PRIMITIVES.map((p) => ({
      label: p.label,
      run: () => meshOp("add_primitive", { spec: p.spec, transform: { position: meshCursor(), rotation: [0, 0, 0, 1], scale: [1, 1, 1] } }),
    }));

  /** Ctrl+J: the other selected meshes and brushes join the active one. */
  const meshJoin = (): void => {
    const active = state.selection.primary;
    const others = state.selection.ids.filter((id) => id !== active);
    if (!active || others.length === 0) {
      props.editor.note("select the meshes to join, then the one they join into last");
      return;
    }
    try {
      props.editor.runMeshOp(opRequest(active, "join", { mode: "face", ids: [] }, { with: others }));
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };

  /** Element count of the session's mode, so select-all and invert need no core round trip. */
  const meshModeCount = (): number | null => {
    const session = props.editor.meshEdit;
    if (!session) return null;
    return session.mode === "vertex" ? session.counts.verts : session.mode === "edge" ? session.counts.edges : session.counts.faces;
  };

  const meshSelectIds = (ids: number[]): void => {
    const session = props.editor.meshEdit;
    if (!session) return;
    try {
      props.editor.meshEditSelect(selJson(session.mode, ids));
    } catch (e) {
      props.editor.note(e instanceof Error ? e.message : String(e));
    }
  };

  const meshSelectAll = (): void => {
    const n = meshModeCount();
    if (n === null) return;
    meshSelectIds(Array.from({ length: n }, (_, i) => i));
  };

  const meshDeselect = (): void => {
    meshSelectIds([]);
  };

  const meshSelectInvert = (): void => {
    const n = meshModeCount();
    if (n === null) return;
    const kept = new Set(props.editor.meshSessionIds().ids);
    const ids: number[] = [];
    for (let i = 0; i < n; i++) if (!kept.has(i)) ids.push(i);
    meshSelectIds(ids);
  };

  let alignAxes: [boolean, boolean, boolean] = [true, false, false];
  let alignMode: "center" | "min" | "max" = "center";
  const alignStep1 = (): void =>
    openList(
      "Align objects: axes",
      (["X", "Y", "Z", "XY", "XYZ"] as const).map((a) => ({
        label: a,
        run: () => {
          alignAxes = [a.includes("X"), a.includes("Y"), a.includes("Z")];
          setTimeout(alignStep2, 0);
        },
      })),
    );
  const alignStep2 = (): void =>
    openList("Align objects: by", [
      { label: "Centers", run: () => ((alignMode = "center"), setTimeout(alignStep3, 0)) },
      { label: "Negative sides", run: () => ((alignMode = "min"), setTimeout(alignStep3, 0)) },
      { label: "Positive sides", run: () => ((alignMode = "max"), setTimeout(alignStep3, 0)) },
    ]);
  const alignStep3 = (): void =>
    openList(
      "Align objects: relative to",
      (["active", "selection", "cursor", "origin"] as const).map((r) => ({
        label: { active: "Active", selection: "Selection", cursor: "3D Cursor", origin: "Scene Origin" }[r],
        run: () => props.editor.note(`aligned ${props.editor.alignObjects(alignAxes, alignMode, r).length} objects`),
      })),
    );

  const randomize = (text: string): void => {
    const parts = text.split(",").map((g) => g.trim().split(/\s+/).map(Number));
    const loc = parts[0] ?? [];
    const rot = parts[1] ?? [];
    const v3 = (a: number[]): Vec3 => [0, 1, 2].map((i) => (Number.isFinite(a[i]) ? a[i] : 0)) as Vec3;
    const sc = Number.isFinite(parts[2]?.[0]) ? parts[2][0] : 0;
    const seed = Number.isFinite(parts[3]?.[0]) ? parts[3][0] : 1;
    props.editor.note(`randomized ${props.editor.randomizeTransform(v3(loc), v3(rot), sc, seed).length} objects`);
  };

  const allObjects = (): DocObject[] => {
    const store = props.editor.store;
    return store ? store.ids().map((i) => store.object(i)).filter((o): o is DocObject => !!o && !o.hidden && !o.locked) : [];
  };

  const selectWhere = (keep: (o: DocObject) => boolean, extend = false): void => {
    const picked = allObjects().filter(keep).map((o) => o.id);
    const active = state.selection.primary;
    const base = extend ? state.selection.ids.filter((i) => !picked.includes(i)) : [];
    const ordered = [...base, ...picked.filter((i) => i !== active), ...(active && picked.includes(active) ? [active] : [])];
    select(ordered);
    props.editor.note(`${picked.length} selected`);
  };

  const modelOf = (o: DocObject): string | null => (o.kind.type === "prop" ? o.kind.prop.model : null);
  const classOf = (o: DocObject): string | null => (o.kind.type === "entity" ? o.kind.entity.classname : null);
  const materialsOf = (o: DocObject): string[] => (o.kind.type === "brush" ? o.kind.brush.faces.map((f) => f.material) : []);

  const groupedItems = (): MenuItem[] => {
    const active = state.selection.primary ? props.editor.store?.object(state.selection.primary) : null;
    if (!active) return [{ label: "No active object", heading: true, run: () => undefined }];
    const descendants = (id: string): Set<string> => {
      const out = new Set<string>();
      const walk = (pid: string): void => {
        for (const o of allObjects()) if (o.parent === pid && !out.has(o.id)) (out.add(o.id), walk(o.id));
      };
      walk(id);
      return out;
    };
    return [
      { label: "Children", run: () => selectWhere((o) => descendants(active.id).has(o.id), true) },
      { label: "Immediate Children", run: () => selectWhere((o) => o.parent === active.id, true) },
      { label: "Parent", run: () => selectWhere((o) => o.id === active.parent) },
      { label: "Siblings", run: () => selectWhere((o) => o.parent === active.parent) },
      { label: "Type", run: () => selectWhere((o) => o.kind.type === active.kind.type) },
      { label: "Layer", run: () => selectWhere((o) => o.layer === active.layer) },
      { label: "Model", run: () => selectWhere((o) => modelOf(o) !== null && modelOf(o) === modelOf(active)) },
      { label: "Entity Class", run: () => selectWhere((o) => classOf(o) !== null && classOf(o) === classOf(active)) },
    ];
  };

  const linkedItems = (): MenuItem[] => {
    const active = state.selection.primary ? props.editor.store?.object(state.selection.primary) : null;
    if (!active) return [{ label: "No active object", heading: true, run: () => undefined }];
    const mats = new Set(materialsOf(active));
    return [
      { label: "Object Data (same model)", run: () => selectWhere((o) => modelOf(o) !== null && modelOf(o) === modelOf(active)) },
      { label: "Material", run: () => selectWhere((o) => materialsOf(o).some((m) => mats.has(m))) },
      { label: "Entity Class", run: () => selectWhere((o) => classOf(o) !== null && classOf(o) === classOf(active)) },
    ];
  };

  const typeItems = (): MenuItem[] => {
    const kinds = [...new Set(allObjects().map((o) => o.kind.type))].sort();
    return kinds.map((k) => ({ label: k, run: () => selectWhere((o) => o.kind.type === k) }));
  };

  /** Blender's Select More/Less in object mode walks the parent/child hierarchy. */
  const selectHierarchy = (more: boolean): void => {
    const chosen = new Set(state.selection.ids);
    const objects = allObjects();
    if (more) {
      for (const o of objects) {
        if (chosen.has(o.id) && o.parent) chosen.add(o.parent);
        if (o.parent && state.selection.ids.includes(o.parent)) chosen.add(o.id);
      }
    } else {
      for (const o of objects) {
        if (!chosen.has(o.id)) continue;
        const kids = objects.filter((k) => k.parent === o.id);
        const edge = (o.parent && !state.selection.ids.includes(o.parent)) || kids.some((k) => !state.selection.ids.includes(k.id));
        if (edge) chosen.delete(o.id);
      }
    }
    select([...chosen]);
  };

  const selectRandom = (text: string): void => {
    const pct = Math.max(0, Math.min(100, Number(text) || 0)) / 100;
    const current = new Set(state.selection.ids);
    selectWhere((o) => current.has(o.id) || Math.random() < pct);
  };

  const selectPattern = (text: string): void => {
    const re = new RegExp(`^${text.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".")}$`, "i");
    selectWhere((o) => re.test(o.name));
  };

  const selectedPositions = (): Vec3[] =>
    state.selection.ids.map((i) => props.editor.store?.object(i)?.transform.position).filter((p): p is Vec3 => !!p);

  const snapOp = (id: CommandId): void => {
    if (id === "cursor_reset") {
      setCursorWorld([0, 0, 0]);
      return;
    }
    if (id === "cursor_to_selected") {
      const ps = selectedPositions();
      if (ps.length === 0) return;
      setCursorWorld(ps.reduce((a, p) => [a[0] + p[0] / ps.length, a[1] + p[1] / ps.length, a[2] + p[2] / ps.length], [0, 0, 0] as Vec3));
      return;
    }
    const cursor = state.cursorWorld ?? [0, 0, 0];
    moveSelected(() => [...cursor] as Vec3);
  };

  const moveSelected = (to: (p: Vec3) => Vec3): void => {
    const store = props.editor.store;
    if (!store) return;
    const moved = state.selection.ids
      .map((i) => store.object(i))
      .filter((o): o is DocObject => !!o && !o.locked)
      .map((o) => ({ ...o, transform: { ...o.transform, position: to(o.transform.position) } }));
    if (moved.length > 0) props.editor.apply({ op: "replace_objects", objects: moved });
  };

  const selectionToGrid = (): void => {
    const g = state.snap.size;
    moveSelected((p) => [snapValue(p[0], g), snapValue(p[1], g), snapValue(p[2], g)]);
  };

  const cursorToActive = (): void => {
    const id = state.selection.primary;
    const p = id ? props.editor.store?.object(id)?.transform.position : null;
    if (p) setCursorWorld([...p] as Vec3);
  };

  /** Applies a click or box result the way Blender's select modes do. */
  const applyPick = (picked: Id[], mode: PickMode): void => {
    if (mode === "set") {
      if (picked.length > 0) select(picked);
      else clearSelection();
      return;
    }
    const current = state.selection.ids.slice();
    if (mode === "subtract") {
      const drop = new Set(picked);
      select(current.filter((i) => !drop.has(i)));
      return;
    }
    // Extend: a lone click on the active object deselects it, on a selected
    // one makes it active, on anything else adds it as the new active.
    if (picked.length === 1) {
      const id = picked[0];
      if (state.selection.primary === id) select(current.filter((i) => i !== id));
      else select([...current.filter((i) => i !== id), id]);
      return;
    }
    const have = new Set(current);
    select([...current, ...picked.filter((i) => !have.has(i))]);
  };

  const dialogOpen = (): boolean => projectMode() !== null || state.ui.dialog !== null;

  const closeDialogs = (): void => {
    setProjectMode(null);
    openDialog(null);
  };

  // On the document, so a shortcut works wherever focus is: after a click on
  // a panel header, the page background or a button, not only in a viewport.
  const onKeyDown = (e: KeyboardEvent): void => {
    if (e.defaultPrevented) return;
    if (e.key === "Escape" && dialogOpen()) {
      e.preventDefault();
      closeDialogs();
      return;
    }
    if (isTextTarget(e.target)) return;
    // Under a modal only Escape acts; the rest would edit the hidden document.
    if (dialogOpen() || menu() || anyViewBusy()) return;
    // Viewport keys act on the view under the pointer, or from the outliner, as in Blender.
    // What is under the pointer now: a closed popup leaves the last hover target stale.
    const under = document.elementFromPoint?.(mouse.x, mouse.y) ?? hoverTarget;
    const overView = !!under?.closest?.(".viewport, .outliner");
    const view = under?.closest?.(".viewport");
    if (view) {
      const index = [...document.querySelectorAll(".viewport")].indexOf(view);
      if (index >= 0) setState("ui", "hoverView", state.ui.viewports.findIndex((v, i) => i >= 0 && (state.ui.layout === "four" ? i === index : v.active)));
    }
    const contexts: KeyContext[] = overView
      ? props.editor.meshEditing
        ? ["edit_mesh", "viewport", "window"]
        : ["viewport", "window"]
      : ["window"];
    const id = resolve(props.editor.keymap, contexts, e, state.input);
    if (!id) return;
    e.preventDefault();
    run(id);
  };

  onMount(() => {
    document.addEventListener("keydown", onKeyDown);
    const onMove = (e: PointerEvent): void => {
      mouse = { x: e.clientX, y: e.clientY };
      hoverTarget = e.target as Element | null;
    };
    document.addEventListener("pointermove", onMove, true);
    onCleanup(() => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointermove", onMove, true);
    });
    refresh();
    const g = globalThis as unknown as { setInterval?: (fn: () => void, ms: number) => number };
    const timer = g.setInterval?.(refresh, 500);
    onCleanup(() => {
      if (timer !== undefined) (globalThis as unknown as { clearInterval?: (id: number) => void }).clearInterval?.(timer);
    });
  });

  createEffect(() => {
    document.documentElement.dataset.theme = state.ui.theme;
  });

  const viewports = createMemo(() => state.ui.viewports.filter((v) => state.ui.layout === "four" || v.active));

  const targets = createMemo(() => objects().map((o) => ({ id: o.id, name: o.name })));

  const createAt = (): Vec3 => state.cursorWorld ?? [0, 0, 0];

  const create = (kind: NewObjectKind): void => {
    // A mesh needs a shape, so it opens the picker rather than guessing one.
    if (kind === "mesh") {
      openDialog("new-mesh");
      return;
    }
    let added: Id[] = [];
    try {
      added = props.editor.createObject(kind, createAt());
    } catch (e) {
      props.editor.note(`${kind}: ${e instanceof Error ? e.message : String(e)}`);
      return;
    }
    if (added.length > 0) select(added);
    else props.editor.note(`${kind} could not be created`);
  };

  /** The drop hint, so the panels show a target while an asset is dragged. */
  const onDragStart = (e: DragEvent): void => {
    const types = e.dataTransfer?.types;
    if (types?.includes("text/r5ed-asset")) setAssetDrag(true);
  };

  return (
    <div class="app" onDragStart={onDragStart} onDragEnd={() => setAssetDrag(false)}>
      <TopBar editor={props.editor} onCommand={run} />
      <div class="body">
        <Outliner
          editor={props.editor}
          objects={objects()}
          layers={layers()}
          onSelect={(id: string, additive: boolean) => {
            if (additive) props.editor.toggleSelect(id);
            else select([id]);
          }}
          onLayer={(layer) => props.editor.setLayer(layer)}
          onCreate={(kind) => create(kind)}
          onAddLayer={() => props.editor.addLayer({ id: newId(), name: `layer ${layers().length + 1}`, visible: true, locked: false })}
          onRemoveLayer={(id) => props.editor.removeLayer(id)}
          onRename={(object, name) => props.editor.rename(object, name)}
        />
        <main class="center">
          <div class="viewport-area">
          <Show when={state.ui.showToolbar}>
            <Toolbar keymap={props.editor.keymap} />
          </Show>
          <div class="viewports" classList={{ four: state.ui.layout === "four" }}>
            <For each={viewports()}>
              {(view) => (
                <Viewport
                  editor={props.editor}
                  kind={view.kind}
                  index={state.ui.viewports.indexOf(view)}
                  active={view.active}
                  onPick={(picked: Id | null, mode: PickMode) => {
                    const record = picked === null ? null : refPropIndex(picked);
                    const id = record === null ? picked : props.editor.promoteRefProp(record);
                    // A click on empty space only clears in the plain select mode.
                    if (!id && mode !== "set") return;
                    applyPick(id ? [id] : [], mode);
                    refresh();
                  }}
                  onBoxSelect={(ids: Id[], mode: PickMode) => {
                    applyPick(ids, mode);
                    refresh();
                  }}
                />
              )}
            </For>
          </div>
          </div>
          <Show when={state.jobs.panelOpen}>
            <JobPanel editor={props.editor} />
          </Show>
        </main>
        <div class="right-col">
          <AssetBrowser
            editor={props.editor}
            onPlace={(asset) => {
              const added = props.editor.placeAsset(asset.kind, asset.path, createAt());
              if (added.length > 0) select(added);
            }}
          />
          <Inspector
            editor={props.editor}
            object={selected()}
            issues={issues()}
            suggestions={suggestions()}
            targets={targets()}
          />
          <MessageLog editor={props.editor} />
        </div>
      </div>
      <StatusBar editor={props.editor} />
      <Show when={assetDrag()}>
        <div class="drop-hint">drop an asset to place it</div>
      </Show>
      <Show when={projectMode()}>
        {(mode) => (
          <ProjectDialog
            editor={props.editor}
            mode={mode()}
            onClose={() => {
              setProjectMode(null);
              refresh();
            }}
          />
        )}
      </Show>
      <Show when={state.ui.dialog === "map-settings"}>
        <MapSettingsDialog editor={props.editor} onClose={() => openDialog(null)} />
      </Show>
      <Show when={state.ui.dialog === "import"}>
        <ImportDialog editor={props.editor} onClose={() => openDialog(null)} />
      </Show>
      <Show when={state.ui.dialog === "new-terrain"}>
        <NewTerrainDialog editor={props.editor} onClose={() => openDialog(null)} />
      </Show>
      <Show when={state.ui.dialog === "new-mesh"}>
        <NewMeshDialog
          editor={props.editor}
          onClose={() => {
            openDialog(null);
            refresh();
          }}
        />
      </Show>
      <Show when={state.ui.dialog === "shortcuts"}>
        <ShortcutsDialog editor={props.editor} onClose={() => openDialog(null)} />
      </Show>
      <Show when={menu()}>{(spec) => <Menu spec={spec()} onClose={() => setMenu(null)} />}</Show>
      <Show when={state.ui.dialog === "about"}>
        <AboutDialog editor={props.editor} onClose={() => openDialog(null)} />
      </Show>
    </div>
  );

}

function labelFor(id: CommandId): string {
  switch (id) {
    case "focus_outliner":
      return "Outliner";
    case "focus_inspector":
      return "Inspector";
    default:
      return "Asset browser";
  }
}
