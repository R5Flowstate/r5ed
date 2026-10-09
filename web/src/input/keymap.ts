export type CommandId =
  | "undo" | "redo" | "save" | "save_as" | "open" | "new"
  | "delete" | "duplicate" | "group" | "ungroup" | "isolate" | "frame" | "frame_all"
  | "select_all" | "deselect" | "select_invert" | "select_box" | "select_circle" | "select_grouped"
  | "hide" | "reveal" | "hide_unselected"
  | "grab" | "rotate" | "resize" | "clear_location" | "clear_rotation" | "clear_scale"
  | "tool_select" | "tool_move" | "tool_rotate" | "tool_scale"
  | "tool_brush" | "tool_clip" | "tool_vertex" | "tool_edge" | "tool_face"
  | "tool_sculpt" | "tool_paint" | "tool_voxel" | "tool_entity"
  | "gizmo_translate" | "gizmo_rotate" | "gizmo_scale" | "gizmo_space"
  | "snap_toggle" | "grid_toggle" | "gizmo_toggle" | "layout_one" | "layout_four" | "layout_toggle" | "maximize"
  | "view_front" | "view_back" | "view_right" | "view_left" | "view_top" | "view_bottom"
  | "view_persp_ortho" | "view_orbit_left" | "view_orbit_right" | "view_orbit_up" | "view_orbit_down" | "view_flip"
  | "view_pan_left" | "view_pan_right" | "view_pan_up" | "view_pan_down" | "view_zoom_in" | "view_zoom_out"
  | "view_pie" | "walk" | "cursor_to_selected" | "cursor_reset" | "selection_to_cursor" | "snap_pie" | "add_menu" | "search" | "rename"
  | "duplicate_linked" | "parent_set" | "parent_clear" | "mirror" | "align_objects" | "randomize_transform"
  | "proportional_toggle" | "proportional_falloff" | "orientation_pie" | "pivot_pie" | "snap_target_pie"
  | "view_center_cursor" | "zoom_region" | "dolly" | "local_view_remove" | "select_lasso"
  | "select_linked" | "select_more" | "select_less" | "select_by_type" | "select_random" | "select_pattern"
  | "cursor_to_grid" | "cursor_to_active" | "selection_to_grid" | "selection_to_active" | "selection_to_cursor_offset"
  | "toolbar_toggle" | "theme" | "shortcuts" | "project" | "import" | "map_settings" | "new_terrain"
  | "compile" | "bake" | "package" | "deploy" | "reload" | "jobs"
  | "live_toggle" | "focus_outliner" | "focus_inspector" | "focus_assets" | "help"
  | "mesh_mode_vertex" | "mesh_mode_edge" | "mesh_mode_face"
  | "mesh_extrude" | "mesh_fill" | "mesh_delete" | "mesh_merge_menu" | "mesh_normals_menu"
  | "mesh_recalc_outside" | "mesh_separate" | "mesh_select_linked" | "mesh_linked_expand"
  | "mesh_grow" | "mesh_shrink" | "mesh_hide" | "mesh_reveal" | "mesh_toggle_edit"
  | "mesh_loop_cut" | "mesh_edge_slide" | "mesh_inset" | "mesh_bevel" | "mesh_bevel_vertices"
  | "mesh_add_menu" | "mesh_join" | "mesh_uv_menu" | "mesh_edge_menu" | "mesh_face_menu" | "mesh_vertex_menu"
  | "shading_pie" | "shading_wire_toggle" | "shading_xray_toggle"
  | "adjust_last";

/**
 * Where a binding applies. Resolution walks the active contexts innermost
 * first, so a viewport key only fires with the pointer over a viewport, a
 * mesh key shadows the viewport while edit mode owns the selection, and a
 * modal operator shadows everything under it.
 */
export type KeyContext = "window" | "viewport" | "edit_mesh";

export interface Command {
  id: CommandId;
  label: string;
  group: "file" | "edit" | "select" | "transform" | "tool" | "view" | "build" | "panel";
}

export const COMMANDS: Command[] = [
  { id: "new", label: "New project", group: "file" },
  { id: "open", label: "Open project", group: "file" },
  { id: "save", label: "Save", group: "file" },
  { id: "save_as", label: "Save as", group: "file" },
  { id: "import", label: "Import", group: "file" },
  { id: "map_settings", label: "Map settings", group: "file" },
  { id: "new_terrain", label: "New terrain", group: "file" },
  { id: "project", label: "Project menu", group: "file" },
  { id: "undo", label: "Undo", group: "edit" },
  { id: "redo", label: "Redo", group: "edit" },
  { id: "delete", label: "Delete", group: "edit" },
  { id: "duplicate", label: "Duplicate", group: "edit" },
  { id: "group", label: "Group", group: "edit" },
  { id: "ungroup", label: "Ungroup", group: "edit" },
  { id: "rename", label: "Rename active", group: "edit" },
  { id: "add_menu", label: "Add menu", group: "edit" },
  { id: "search", label: "Search commands", group: "edit" },
  { id: "duplicate_linked", label: "Duplicate linked", group: "edit" },
  { id: "parent_set", label: "Parent to active (keep transform)", group: "edit" },
  { id: "parent_clear", label: "Clear parent (keep transform)", group: "edit" },
  { id: "mirror", label: "Mirror", group: "transform" },
  { id: "align_objects", label: "Align objects", group: "transform" },
  { id: "randomize_transform", label: "Randomize transform", group: "transform" },
  { id: "proportional_toggle", label: "Proportional editing", group: "transform" },
  { id: "proportional_falloff", label: "Proportional falloff", group: "transform" },
  { id: "orientation_pie", label: "Transform orientation", group: "transform" },
  { id: "pivot_pie", label: "Pivot point", group: "transform" },
  { id: "snap_target_pie", label: "Snap target", group: "transform" },
  { id: "cursor_to_grid", label: "Cursor to grid", group: "transform" },
  { id: "cursor_to_active", label: "Cursor to active", group: "transform" },
  { id: "selection_to_grid", label: "Selection to grid", group: "transform" },
  { id: "selection_to_active", label: "Selection to active", group: "transform" },
  { id: "selection_to_cursor_offset", label: "Selection to cursor (keep offset)", group: "transform" },
  { id: "view_center_cursor", label: "Center cursor and frame all", group: "view" },
  { id: "zoom_region", label: "Zoom region", group: "view" },
  { id: "dolly", label: "Dolly view", group: "view" },
  { id: "local_view_remove", label: "Remove from local view", group: "view" },
  { id: "select_lasso", label: "Lasso select", group: "select" },
  { id: "select_linked", label: "Select linked", group: "select" },
  { id: "select_more", label: "Select more (hierarchy)", group: "select" },
  { id: "select_less", label: "Select less (hierarchy)", group: "select" },
  { id: "select_by_type", label: "Select all by type", group: "select" },
  { id: "select_random", label: "Select random", group: "select" },
  { id: "select_pattern", label: "Select pattern", group: "select" },
  { id: "select_all", label: "Select all", group: "select" },
  { id: "deselect", label: "Deselect all", group: "select" },
  { id: "select_invert", label: "Invert selection", group: "select" },
  { id: "select_box", label: "Box select", group: "select" },
  { id: "select_circle", label: "Circle select", group: "select" },
  { id: "select_grouped", label: "Select grouped", group: "select" },
  { id: "hide", label: "Hide selected", group: "select" },
  { id: "reveal", label: "Reveal hidden", group: "select" },
  { id: "hide_unselected", label: "Hide unselected", group: "select" },
  { id: "grab", label: "Move", group: "transform" },
  { id: "rotate", label: "Rotate", group: "transform" },
  { id: "resize", label: "Scale", group: "transform" },
  { id: "clear_location", label: "Clear location", group: "transform" },
  { id: "clear_rotation", label: "Clear rotation", group: "transform" },
  { id: "clear_scale", label: "Clear scale", group: "transform" },
  { id: "snap_pie", label: "Snap menu", group: "transform" },
  { id: "cursor_to_selected", label: "Cursor to selected", group: "transform" },
  { id: "cursor_reset", label: "Cursor to world origin", group: "transform" },
  { id: "selection_to_cursor", label: "Selection to cursor", group: "transform" },
  { id: "snap_toggle", label: "Toggle snapping", group: "transform" },
  { id: "gizmo_space", label: "Toggle transform orientation", group: "transform" },
  { id: "tool_select", label: "Select tool", group: "tool" },
  { id: "tool_move", label: "Move tool", group: "tool" },
  { id: "tool_rotate", label: "Rotate tool", group: "tool" },
  { id: "tool_scale", label: "Scale tool", group: "tool" },
  { id: "tool_brush", label: "Brush tool", group: "tool" },
  { id: "tool_clip", label: "Clip tool", group: "tool" },
  { id: "tool_vertex", label: "Vertex edit", group: "tool" },
  { id: "tool_edge", label: "Edge edit", group: "tool" },
  { id: "tool_face", label: "Face edit", group: "tool" },
  { id: "tool_sculpt", label: "Terrain sculpt", group: "tool" },
  { id: "tool_paint", label: "Terrain paint", group: "tool" },
  { id: "tool_voxel", label: "Voxel terrain", group: "tool" },
  { id: "tool_entity", label: "Place entity", group: "tool" },
  { id: "gizmo_translate", label: "Move gizmo", group: "tool" },
  { id: "gizmo_rotate", label: "Rotate gizmo", group: "tool" },
  { id: "gizmo_scale", label: "Scale gizmo", group: "tool" },
  { id: "frame", label: "Frame selected", group: "view" },
  { id: "frame_all", label: "Frame all", group: "view" },
  { id: "isolate", label: "Local view", group: "view" },
  { id: "view_front", label: "Front view", group: "view" },
  { id: "view_back", label: "Back view", group: "view" },
  { id: "view_right", label: "Right view", group: "view" },
  { id: "view_left", label: "Left view", group: "view" },
  { id: "view_top", label: "Top view", group: "view" },
  { id: "view_bottom", label: "Bottom view", group: "view" },
  { id: "view_persp_ortho", label: "Perspective / orthographic", group: "view" },
  { id: "view_orbit_left", label: "Orbit left", group: "view" },
  { id: "view_orbit_right", label: "Orbit right", group: "view" },
  { id: "view_orbit_up", label: "Orbit up", group: "view" },
  { id: "view_orbit_down", label: "Orbit down", group: "view" },
  { id: "view_flip", label: "View opposite side", group: "view" },
  { id: "view_pan_left", label: "Pan left", group: "view" },
  { id: "view_pan_right", label: "Pan right", group: "view" },
  { id: "view_pan_up", label: "Pan up", group: "view" },
  { id: "view_pan_down", label: "Pan down", group: "view" },
  { id: "view_zoom_in", label: "Zoom in", group: "view" },
  { id: "view_zoom_out", label: "Zoom out", group: "view" },
  { id: "view_pie", label: "View menu", group: "view" },
  { id: "walk", label: "Walk navigation", group: "view" },
  { id: "grid_toggle", label: "Toggle grid", group: "view" },
  { id: "gizmo_toggle", label: "Toggle gizmos", group: "view" },
  { id: "layout_one", label: "Single viewport", group: "view" },
  { id: "layout_four", label: "Four viewports", group: "view" },
  { id: "layout_toggle", label: "Toggle quad view", group: "view" },
  { id: "maximize", label: "Maximize viewport", group: "view" },
  { id: "toolbar_toggle", label: "Toggle toolbar", group: "view" },
  { id: "theme", label: "Toggle theme", group: "view" },
  { id: "compile", label: "Compile", group: "build" },
  { id: "bake", label: "Bake", group: "build" },
  { id: "package", label: "Package", group: "build" },
  { id: "deploy", label: "Deploy", group: "build" },
  { id: "reload", label: "Reload map", group: "build" },
  { id: "jobs", label: "Job panel", group: "build" },
  { id: "live_toggle", label: "Toggle live link", group: "build" },
  { id: "shortcuts", label: "Keymap", group: "panel" },
  { id: "focus_outliner", label: "Focus outliner", group: "panel" },
  { id: "focus_inspector", label: "Focus properties", group: "panel" },
  { id: "focus_assets", label: "Focus asset browser", group: "panel" },
  { id: "help", label: "About", group: "panel" },
  { id: "mesh_mode_vertex", label: "Mesh select mode: vertex", group: "select" },
  { id: "mesh_mode_edge", label: "Mesh select mode: edge", group: "select" },
  { id: "mesh_mode_face", label: "Mesh select mode: face", group: "select" },
  { id: "mesh_select_linked", label: "Mesh select linked under cursor", group: "select" },
  { id: "mesh_linked_expand", label: "Mesh select linked from selection", group: "select" },
  { id: "mesh_grow", label: "Mesh grow selection", group: "select" },
  { id: "mesh_shrink", label: "Mesh shrink selection", group: "select" },
  { id: "mesh_hide", label: "Mesh hide elements", group: "select" },
  { id: "mesh_reveal", label: "Mesh reveal elements", group: "select" },
  { id: "mesh_extrude", label: "Mesh extrude", group: "edit" },
  { id: "mesh_fill", label: "Mesh fill", group: "edit" },
  { id: "mesh_delete", label: "Mesh delete", group: "edit" },
  { id: "mesh_merge_menu", label: "Mesh merge menu", group: "edit" },
  { id: "mesh_normals_menu", label: "Mesh normals menu", group: "edit" },
  { id: "mesh_recalc_outside", label: "Mesh recalculate normals outside", group: "edit" },
  { id: "mesh_separate", label: "Mesh separate", group: "edit" },
  { id: "mesh_toggle_edit", label: "Toggle mesh edit mode", group: "edit" },
  { id: "mesh_loop_cut", label: "Mesh loop cut and slide", group: "edit" },
  { id: "mesh_edge_slide", label: "Mesh edge slide", group: "edit" },
  { id: "mesh_inset", label: "Mesh inset faces", group: "edit" },
  { id: "mesh_bevel", label: "Mesh bevel edges", group: "edit" },
  { id: "mesh_bevel_vertices", label: "Mesh bevel vertices", group: "edit" },
  { id: "mesh_add_menu", label: "Mesh add in edit mode", group: "edit" },
  { id: "mesh_join", label: "Join meshes into the active one", group: "edit" },
  { id: "mesh_uv_menu", label: "Mesh UV menu", group: "edit" },
  { id: "mesh_edge_menu", label: "Mesh edge menu", group: "edit" },
  { id: "mesh_face_menu", label: "Mesh face menu", group: "edit" },
  { id: "mesh_vertex_menu", label: "Mesh vertex menu", group: "edit" },
  { id: "shading_pie", label: "Shading menu", group: "view" },
  { id: "shading_wire_toggle", label: "Toggle wireframe", group: "view" },
  { id: "shading_xray_toggle", label: "Toggle x-ray", group: "view" },
  { id: "adjust_last", label: "Adjust last operation", group: "edit" },
];

export interface Binding {
  context: KeyContext;
  /** Canonical chord, e.g. "Ctrl+Shift+Z" or "Numpad7". */
  chord: string;
  command: CommandId;
}

export type PresetId = "blender" | "legacy";

export interface Keymap {
  preset: PresetId;
  bindings: Binding[];
}

const w = (chord: string, command: CommandId): Binding => ({ context: "window", chord, command });
const v = (chord: string, command: CommandId): Binding => ({ context: "viewport", chord, command });
const em = (chord: string, command: CommandId): Binding => ({ context: "edit_mesh", chord, command });

/**
 * The edit-mode chords both presets share: number keys pick the element mode
 * (object mode keeps them for tools), G/R/S start session drags, and E/X/M/F/
 * P/H drive the base mesh ops. Select-all, deselect and invert repeat the
 * viewport bindings so the shortcuts dialog shows them in this context.
 */
const EDIT_MESH: Binding[] = [
  em("1", "mesh_mode_vertex"),
  em("2", "mesh_mode_edge"),
  em("3", "mesh_mode_face"),
  em("A", "select_all"),
  em("Alt+A", "deselect"),
  em("Ctrl+I", "select_invert"),
  em("G", "grab"),
  em("R", "rotate"),
  em("S", "resize"),
  em("E", "mesh_extrude"),
  em("X", "mesh_delete"),
  em("M", "mesh_merge_menu"),
  em("F", "mesh_fill"),
  em("Shift+N", "mesh_recalc_outside"),
  em("Alt+N", "mesh_normals_menu"),
  em("P", "mesh_separate"),
  em("H", "mesh_hide"),
  em("Alt+H", "mesh_reveal"),
  em("L", "mesh_select_linked"),
  em("Ctrl+L", "mesh_linked_expand"),
  em("Ctrl+NumpadAdd", "mesh_grow"),
  em("Ctrl+NumpadSubtract", "mesh_shrink"),
  em("Tab", "mesh_toggle_edit"),
  em("Ctrl+R", "mesh_loop_cut"),
  em("I", "mesh_inset"),
  em("Ctrl+B", "mesh_bevel"),
  em("Ctrl+Shift+B", "mesh_bevel_vertices"),
  em("Shift+A", "mesh_add_menu"),
  em("U", "mesh_uv_menu"),
  em("Ctrl+E", "mesh_edge_menu"),
  em("Ctrl+F", "mesh_face_menu"),
  em("Ctrl+V", "mesh_vertex_menu"),
];

/** Blender 4.x default keymap, left-click select, restricted to what r5ed has. */
const BLENDER: Binding[] = [
  w("Ctrl+N", "new"),
  w("Ctrl+O", "open"),
  w("Ctrl+S", "save"),
  w("Ctrl+Shift+S", "save_as"),
  w("Ctrl+Shift+I", "import"),
  w("Ctrl+Shift+M", "map_settings"),
  w("Ctrl+Shift+T", "new_terrain"),
  w("Ctrl+Z", "undo"),
  w("Ctrl+Shift+Z", "redo"),
  w("Ctrl+Y", "redo"),
  w("F1", "help"),
  w("F2", "rename"),
  w("F3", "search"),
  w("F6", "reload"),
  w("F7", "jobs"),
  w("F9", "adjust_last"),
  w("F12", "compile"),
  w("Ctrl+Enter", "compile"),
  w("Ctrl+Shift+B", "bake"),
  w("Ctrl+Shift+P", "package"),
  w("Ctrl+Shift+D", "deploy"),
  w("Ctrl+Shift+K", "live_toggle"),
  w("Ctrl+Shift+L", "theme"),
  w("Ctrl+Space", "maximize"),
  w("Ctrl+Alt+Q", "layout_toggle"),
  v("A", "select_all"),
  v("Alt+A", "deselect"),
  v("Ctrl+I", "select_invert"),
  v("B", "select_box"),
  v("C", "select_circle"),
  v("Shift+G", "select_grouped"),
  v("H", "hide"),
  v("Alt+H", "reveal"),
  v("Shift+H", "hide_unselected"),
  v("X", "delete"),
  v("Delete", "delete"),
  v("Shift+D", "duplicate"),
  v("Ctrl+G", "group"),
  v("Ctrl+Alt+G", "ungroup"),
  v("Shift+A", "add_menu"),
  v("Ctrl+J", "mesh_join"),
  v("G", "grab"),
  v("R", "rotate"),
  v("S", "resize"),
  v("Alt+G", "clear_location"),
  v("Alt+R", "clear_rotation"),
  v("Alt+S", "clear_scale"),
  v("Shift+S", "snap_pie"),
  v("Shift+Tab", "snap_toggle"),
  v("Comma", "orientation_pie"),
  v("Period", "pivot_pie"),
  v("O", "proportional_toggle"),
  v("Shift+O", "proportional_falloff"),
  v("Ctrl+Shift+Tab", "snap_target_pie"),
  v("Alt+D", "duplicate_linked"),
  v("Ctrl+P", "parent_set"),
  v("Alt+P", "parent_clear"),
  v("Ctrl+M", "mirror"),
  v("Shift+C", "view_center_cursor"),
  v("Shift+B", "zoom_region"),
  v("Alt+NumpadDivide", "local_view_remove"),
  v("Shift+L", "select_linked"),
  v("Ctrl+NumpadAdd", "select_more"),
  v("Ctrl+NumpadSubtract", "select_less"),
  v("Ctrl+Backquote", "gizmo_toggle"),
  v("Alt+Quote", "grid_toggle"),
  v("NumpadDecimal", "frame"),
  v("Home", "frame_all"),
  v("NumpadDivide", "isolate"),
  v("Numpad1", "view_front"),
  v("Ctrl+Numpad1", "view_back"),
  v("Numpad3", "view_right"),
  v("Ctrl+Numpad3", "view_left"),
  v("Numpad7", "view_top"),
  v("Ctrl+Numpad7", "view_bottom"),
  v("Numpad5", "view_persp_ortho"),
  v("Numpad4", "view_orbit_left"),
  v("Numpad6", "view_orbit_right"),
  v("Numpad8", "view_orbit_up"),
  v("Numpad2", "view_orbit_down"),
  v("Numpad9", "view_flip"),
  v("Ctrl+Numpad4", "view_pan_left"),
  v("Ctrl+Numpad6", "view_pan_right"),
  v("Ctrl+Numpad8", "view_pan_up"),
  v("Ctrl+Numpad2", "view_pan_down"),
  v("NumpadAdd", "view_zoom_in"),
  v("NumpadSubtract", "view_zoom_out"),
  v("Backquote", "view_pie"),
  v("Shift+Backquote", "walk"),
  v("Z", "shading_pie"),
  v("Shift+Z", "shading_wire_toggle"),
  v("Alt+Z", "shading_xray_toggle"),
  v("T", "toolbar_toggle"),
  v("Tab", "mesh_toggle_edit"),
  ...EDIT_MESH,
];

/** The keymap r5ed shipped before the Blender preset. */
const LEGACY: Binding[] = [
  w("Ctrl+N", "new"),
  w("Ctrl+O", "open"),
  w("Ctrl+S", "save"),
  w("Ctrl+Shift+S", "save_as"),
  w("Ctrl+Z", "undo"),
  w("Ctrl+Y", "redo"),
  w("Delete", "delete"),
  w("Ctrl+D", "duplicate"),
  w("Ctrl+G", "group"),
  w("Ctrl+Shift+G", "ungroup"),
  w("Ctrl+I", "isolate"),
  w("Ctrl+A", "select_all"),
  w("Escape", "deselect"),
  w("F", "frame"),
  w("1", "tool_select"),
  w("2", "tool_move"),
  w("3", "tool_rotate"),
  w("4", "tool_scale"),
  w("B", "tool_brush"),
  w("C", "tool_clip"),
  w("V", "tool_vertex"),
  w("E", "tool_edge"),
  w("R", "tool_face"),
  w("T", "tool_sculpt"),
  w("P", "tool_paint"),
  w("X", "tool_voxel"),
  w("H", "tool_entity"),
  w("Ctrl+1", "gizmo_translate"),
  w("Ctrl+2", "gizmo_rotate"),
  w("Ctrl+3", "gizmo_scale"),
  w("Space", "gizmo_space"),
  w("Ctrl+Shift+X", "snap_toggle"),
  w("Quote", "grid_toggle"),
  w("Ctrl+Quote", "gizmo_toggle"),
  w("Tab", "mesh_toggle_edit"),
  w("Ctrl+5", "layout_one"),
  w("Ctrl+4", "layout_four"),
  w("Ctrl+Shift+L", "theme"),
  w("Ctrl+U", "import"),
  w("Ctrl+Shift+M", "map_settings"),
  w("Ctrl+Shift+T", "new_terrain"),
  w("F2", "shortcuts"),
  w("F3", "focus_outliner"),
  w("F4", "focus_inspector"),
  w("F5", "focus_assets"),
  w("Ctrl+Enter", "compile"),
  w("Ctrl+Shift+B", "bake"),
  w("Ctrl+Shift+P", "package"),
  w("Ctrl+Shift+D", "deploy"),
  w("F6", "reload"),
  w("F7", "jobs"),
  w("Ctrl+Shift+K", "live_toggle"),
  w("F1", "help"),
  w("F9", "adjust_last"),
  w("Z", "shading_pie"),
  w("Shift+Z", "shading_wire_toggle"),
  w("Alt+Z", "shading_xray_toggle"),
  ...EDIT_MESH,
];

export function presetBindings(preset: PresetId): Binding[] {
  return (preset === "legacy" ? LEGACY : BLENDER).map((b) => ({ ...b }));
}

export function defaultKeymap(preset: PresetId = "blender"): Keymap {
  return { preset, bindings: presetBindings(preset) };
}

export function commandLabel(id: CommandId): string {
  return COMMANDS.find((c) => c.id === id)?.label ?? id;
}

const ALIASES: Record<string, string> = {
  ctrl: "Ctrl",
  control: "Ctrl",
  cmd: "Ctrl",
  command: "Ctrl",
  meta: "Ctrl",
  shift: "Shift",
  alt: "Alt",
  option: "Alt",
  esc: "Escape",
  escape: "Escape",
  del: "Delete",
  delete: "Delete",
  return: "Enter",
  enter: "Enter",
  space: "Space",
  "'": "Quote",
  "`": "Backquote",
  ",": "Comma",
  ".": "Period",
  "/": "Slash",
  "-": "Minus",
  "=": "Equal",
};

const MODIFIERS = ["Ctrl", "Alt", "Shift"];

function normalizePart(part: string): string {
  const alias = ALIASES[part.toLowerCase()];
  if (alias) return alias;
  if (part.length === 1) return part.toUpperCase();
  const numpad = /^numpad(.+)$/i.exec(part);
  if (numpad) return `Numpad${numpad[1].length === 1 ? numpad[1] : numpad[1][0].toUpperCase() + numpad[1].slice(1)}`;
  return part[0].toUpperCase() + part.slice(1);
}

/** Canonical form of a written chord: modifiers in Ctrl, Alt, Shift order. */
export function chordOf(binding: string): string {
  const parts = binding
    .split("+")
    .map((p) => p.trim())
    .filter((p) => p.length > 0)
    .map(normalizePart);
  const mods = MODIFIERS.filter((m) => parts.includes(m));
  const keys = parts.filter((p) => !MODIFIERS.includes(p));
  if (keys.length !== 1) return "";
  return [...mods, keys[0]].join("+");
}

/** The physical key of an event, so Numpad1 and the top-row 1 stay distinct. */
export function keyName(event: Pick<KeyboardEvent, "code" | "key">): string {
  const code = event.code ?? "";
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (code.startsWith("Numpad")) return code;
  if (code === "Space") return "Space";
  if (code) {
    if (["ControlLeft", "ControlRight", "ShiftLeft", "ShiftRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight"].includes(code)) return "";
    return code;
  }
  // Synthetic events carry only `key`.
  const key = event.key ?? "";
  if (["Control", "Shift", "Alt", "Meta"].includes(key)) return "";
  if (key === " ") return "Space";
  return normalizePart(key);
}

export interface InputPrefs {
  /** Top-row digits act as the numpad, for keyboards without one. */
  emulateNumpad: boolean;
  /** Alt+left drag acts as the middle button. */
  emulate3Button: boolean;
  /** Wheel and Ctrl+MMB zoom toward the point under the cursor. */
  zoomToMouse: boolean;
  invertZoom: boolean;
  /** Radians per pixel of orbit drag. */
  orbitSpeed: number;
}

export function defaultInputPrefs(): InputPrefs {
  return { emulateNumpad: false, emulate3Button: false, zoomToMouse: true, invertZoom: false, orbitSpeed: 0.008 };
}

/** Canonical chord of a key event, or "" for a bare modifier press. */
export function keyChord(event: KeyboardEvent, prefs?: Pick<InputPrefs, "emulateNumpad">): string {
  let key = keyName(event);
  if (!key) return "";
  if (prefs?.emulateNumpad && /^[0-9]$/.test(key)) key = `Numpad${key}`;
  const parts: string[] = [];
  if (event.ctrlKey || event.metaKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  parts.push(key);
  return parts.join("+");
}

/**
 * The command a key event maps to, looking in `contexts` innermost first.
 */
export function resolve(keymap: Keymap, contexts: readonly KeyContext[], event: KeyboardEvent, prefs?: Pick<InputPrefs, "emulateNumpad">): CommandId | null {
  const chord = keyChord(event, prefs);
  if (!chord) return null;
  for (const context of contexts) {
    for (const b of keymap.bindings) {
      if (b.context === context && chordOf(b.chord) === chord) return b.command;
    }
  }
  return null;
}

/** The first chord bound to a command, for menus and tooltips. */
export function chordFor(keymap: Keymap, command: CommandId): string {
  return keymap.bindings.find((b) => b.command === command)?.chord ?? "";
}

/**
 * Another command the chord already fires in the same context. A window
 * binding also collides with a viewport one, since the window keymap is
 * always under the viewport's.
 */
export function conflictFor(keymap: Keymap, command: CommandId, context: KeyContext, candidate: string): CommandId | null {
  const chord = chordOf(candidate);
  if (!chord) return null;
  for (const b of keymap.bindings) {
    if (b.command === command) continue;
    const overlaps = b.context === context || b.context === "window" || context === "window";
    if (overlaps && chordOf(b.chord) === chord) return b.command;
  }
  return null;
}

/** Rebinds a command's first binding, refusing a chord something else owns. */
export function rebind(keymap: Keymap, command: CommandId, candidate: string, context?: KeyContext): Keymap | string {
  const chord = chordOf(candidate);
  if (!chord) return "a binding needs at least one key";
  const existing = keymap.bindings.findIndex((b) => b.command === command);
  const ctx = context ?? (existing >= 0 ? keymap.bindings[existing].context : "window");
  const other = conflictFor(keymap, command, ctx, chord);
  if (other) return `${chord} is already bound to ${commandLabel(other)}`;
  const bindings = keymap.bindings.slice();
  if (existing >= 0) bindings[existing] = { ...bindings[existing], chord, context: ctx };
  else bindings.push({ context: ctx, chord, command });
  return { ...keymap, bindings };
}

/** True when the event targets a text field, so typing never fires a command. */
export function isTextTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || !el.tagName) return false;
  const tag = el.tagName.toUpperCase();
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  if (el.isContentEditable === true) return true;
  const attr = el.getAttribute?.("contenteditable");
  return attr !== null && attr !== undefined && attr !== "false";
}
