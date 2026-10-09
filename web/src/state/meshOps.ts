/**
 * The modelling ops edit mode exposes beyond select/transform: what each is
 * called in menus, its default params (so the Adjust Last Operation panel
 * shows every field from the start) and, for the modal ones, which param the
 * mouse drags and which the wheel steps.
 */

export type FieldKind = "number" | "int" | "bool" | "enum";

export interface OpField {
  key: string;
  kind: FieldKind;
  min?: number;
  max?: number;
  options?: readonly string[];
}

export interface OpModal {
  /** The param mouse distance sets, in world units per screen pixel times `dragScale`. */
  drag?: string;
  dragScale?: number;
  /** A param the drag sets as a 0..1 fraction across the viewport instead of a distance. */
  fraction?: boolean;
  /** The integer param the wheel steps. */
  wheel?: string;
}

export interface OpSpec {
  op: string;
  label: string;
  params: Record<string, unknown>;
  fields: OpField[];
  modal?: OpModal;
}

const num = (key: string, min?: number, max?: number): OpField => ({ key, kind: "number", min, max });
const int = (key: string, min?: number, max?: number): OpField => ({ key, kind: "int", min, max });
const bool = (key: string): OpField => ({ key, kind: "bool" });
const choice = (key: string, options: readonly string[]): OpField => ({ key, kind: "enum", options });

const DISSOLVE = ["verts", "edges", "limited"] as const;
const UV_SCALE = ["world", "fit"] as const;
const UV_SPACE = ["object", "world"] as const;

export const MESH_OPS = {
  loop_cut: {
    op: "loop_cut",
    label: "Loop cut and slide",
    params: { cuts: 1, smoothness: 0, factor: 0, even: false, flipped: false },
    fields: [int("cuts", 1, 64), num("smoothness", -1, 1), num("factor", -1, 1), bool("even"), bool("flipped")],
    modal: { drag: "factor", fraction: true, wheel: "cuts" },
  },
  edge_slide: {
    op: "edge_slide",
    label: "Edge slide",
    params: { factor: 0, even: false, flipped: false, clamp: true },
    fields: [num("factor", -1, 1), bool("even"), bool("flipped"), bool("clamp")],
    modal: { drag: "factor", fraction: true },
  },
  subdivide: {
    op: "subdivide",
    label: "Subdivide",
    params: { cuts: 1, smoothness: 0, fractal: 0, seed: 0 },
    fields: [int("cuts", 1, 64), num("smoothness", 0, 1), num("fractal", 0), int("seed")],
  },
  unsubdivide: {
    op: "unsubdivide",
    label: "Un-subdivide",
    params: { iterations: 2 },
    fields: [int("iterations", 1, 16)],
  },
  dissolve_verts: { op: "dissolve", label: "Dissolve vertices", params: { mode: "verts" }, fields: [choice("mode", DISSOLVE)] },
  dissolve_edges: { op: "dissolve", label: "Dissolve edges", params: { mode: "edges" }, fields: [choice("mode", DISSOLVE)] },
  dissolve_limited: {
    op: "dissolve",
    label: "Limited dissolve",
    params: { mode: "limited", angle: 5 },
    fields: [choice("mode", DISSOLVE), num("angle", 0, 180)],
  },
  inset: {
    op: "inset",
    label: "Inset faces",
    params: { thickness: 8, depth: 0, even_offset: true, relative_offset: false, boundary: true, outset: false, individual: false, edge_rail: false },
    fields: [
      num("thickness", 0),
      num("depth"),
      bool("even_offset"),
      bool("relative_offset"),
      bool("boundary"),
      bool("outset"),
      bool("individual"),
      bool("edge_rail"),
    ],
    modal: { drag: "thickness", dragScale: 1 },
  },
  bridge: {
    op: "bridge_edge_loops",
    label: "Bridge edge loops",
    params: { cuts: 0, twist: 0, reverse: false, interpolation: "linear", smoothness: 1, merge: false },
    fields: [int("cuts", 0, 64), int("twist"), bool("reverse"), choice("interpolation", ["linear", "path", "surface"]), num("smoothness", 0, 2), bool("merge")],
  },
  bevel_edges: {
    op: "bevel",
    label: "Bevel edges",
    params: {
      affect: "edges",
      offset_type: "offset",
      amount: 4,
      segments: 1,
      profile: 0.5,
      clamp_overlap: true,
      loop_slide: true,
      material: -1,
      harden_normals: false,
      mark_seam: false,
      mark_sharp: false,
    },
    fields: [
      choice("affect", ["edges", "vertices"]),
      choice("offset_type", ["offset", "width", "depth", "percent"]),
      num("amount", 0),
      int("segments", 1, 64),
      num("profile", 0, 1),
      bool("clamp_overlap"),
      bool("loop_slide"),
      int("material", -1),
      bool("harden_normals"),
      bool("mark_seam"),
      bool("mark_sharp"),
    ],
    modal: { drag: "amount", dragScale: 1, wheel: "segments" },
  },
  bevel_vertices: {
    op: "bevel",
    label: "Bevel vertices",
    params: { affect: "vertices", offset_type: "offset", amount: 4, segments: 1, profile: 0.5, clamp_overlap: true },
    fields: [choice("affect", ["edges", "vertices"]), choice("offset_type", ["offset", "width", "depth", "percent"]), num("amount", 0), int("segments", 1, 64), num("profile", 0, 1), bool("clamp_overlap")],
    modal: { drag: "amount", dragScale: 1, wheel: "segments" },
  },
  spin: {
    op: "spin",
    label: "Spin",
    params: { axis: [0, 0, 1], center: [0, 0, 0], angle_deg: 360, steps: 12, dupli: false, merge_ends: true },
    fields: [num("angle_deg", -3600, 3600), int("steps", 1, 512), bool("dupli"), bool("merge_ends")],
  },
  screw: {
    op: "screw",
    label: "Screw",
    params: { axis: [0, 0, 1], center: [0, 0, 0], angle_deg: 360, turns: 1, height: 64, steps: 12 },
    fields: [num("angle_deg", -3600, 3600), int("turns", 1, 64), num("height"), int("steps", 1, 512)],
  },
  mark_seam: { op: "mark", label: "Mark seam", params: { kind: "seam", clear: false }, fields: [choice("kind", ["seam", "sharp"]), bool("clear")] },
  clear_seam: { op: "mark", label: "Clear seam", params: { kind: "seam", clear: true }, fields: [choice("kind", ["seam", "sharp"]), bool("clear")] },
  mark_sharp: { op: "mark", label: "Mark sharp", params: { kind: "sharp", clear: false }, fields: [choice("kind", ["seam", "sharp"]), bool("clear")] },
  clear_sharp: { op: "mark", label: "Clear sharp", params: { kind: "sharp", clear: true }, fields: [choice("kind", ["seam", "sharp"]), bool("clear")] },
  uv_box: {
    op: "uv_box",
    label: "Box project",
    params: { scale: "world", texels_per_tile: 128, offset: [0, 0], rotation: 0, space: "object" },
    fields: [choice("scale", UV_SCALE), num("texels_per_tile", 1, 65536), num("rotation", -360, 360), choice("space", UV_SPACE)],
  },
  uv_cylinder: {
    op: "uv_cylinder",
    label: "Cylinder project",
    params: { axis: "object_z", seam_angle_deg: 180, scale: "world", texels_per_tile: 128, cap: "planar" },
    fields: [
      choice("axis", ["object_z", "world_z", "fit"]),
      num("seam_angle_deg", -360, 360),
      choice("scale", UV_SCALE),
      num("texels_per_tile", 1, 65536),
      choice("cap", ["planar", "none"]),
    ],
  },
  uv_planar: {
    op: "uv_planar",
    label: "Planar project",
    params: { axis: "z", scale: "world", texels_per_tile: 128, offset: [0, 0], rotation: 0, space: "object" },
    fields: [
      choice("axis", ["x", "y", "z", "neg_x", "neg_y", "neg_z", "view"]),
      choice("scale", UV_SCALE),
      num("texels_per_tile", 1, 65536),
      num("rotation", -360, 360),
      choice("space", UV_SPACE),
    ],
  },
  uv_unwrap: {
    op: "uv_unwrap",
    label: "Unwrap",
    params: { method: "conformal", fill_holes: true, margin: 0.001, scale: "world", texels_per_tile: 128 },
    fields: [choice("method", ["conformal"]), bool("fill_holes"), num("margin", 0, 0.45), choice("scale", UV_SCALE), num("texels_per_tile", 1, 65536)],
  },
  uv_pack: { op: "uv_pack", label: "Pack islands", params: { margin: 0.001, rotate: true }, fields: [num("margin", 0, 0.45), bool("rotate")] },
  uv_reset: { op: "uv_reset", label: "Reset UVs", params: {}, fields: [] },
  boolean_difference: {
    op: "boolean",
    label: "Boolean: difference (selected cuts the rest)",
    params: { operation: "difference", operand: "unselected", keep_material: false },
    fields: [choice("operation", ["difference", "union", "intersect"]), bool("keep_material")],
  },
  boolean_union: {
    op: "boolean",
    label: "Boolean: union",
    params: { operation: "union", operand: "unselected", keep_material: false },
    fields: [choice("operation", ["difference", "union", "intersect"]), bool("keep_material")],
  },
  boolean_intersect: {
    op: "boolean",
    label: "Boolean: intersect",
    params: { operation: "intersect", operand: "unselected", keep_material: false },
    fields: [choice("operation", ["difference", "union", "intersect"]), bool("keep_material")],
  },
} satisfies Record<string, OpSpec>;

export type MeshOpKey = keyof typeof MESH_OPS;

/** Shapes the add-in-edit menu offers, with the params `add_primitive` takes. */
export const EDIT_PRIMITIVES: { label: string; spec: Record<string, unknown> }[] = [
  { label: "Box", spec: { shape: "box", size: [64, 64, 64] } },
  { label: "Plane", spec: { shape: "plane", size: [64, 64], divisions: [1, 1] } },
  { label: "Cylinder", spec: { shape: "cylinder", radius: 32, height: 64, segments: 12 } },
  { label: "Cone", spec: { shape: "cone", radius: 32, height: 64, segments: 12 } },
  { label: "Sphere", spec: { shape: "sphere", radius: 32, segments: 12, rings: 8 } },
  { label: "Torus", spec: { shape: "torus", major: 64, minor: 16, segments: 16, sides: 8 } },
  { label: "Stairs", spec: { shape: "stairs", steps: 4, run: 128, rise: 64, width: 64 } },
  { label: "Ramp", spec: { shape: "ramp", length: 128, width: 64, height: 32 } },
  { label: "Arch", spec: { shape: "arch", width: 128, height: 160, thickness: 16, depth: 32, segments: 8 } },
];

/** The spec behind a run op request, so the redo panel can type its fields. */
export function specForRequest(op: string, params: Record<string, unknown>): OpSpec | null {
  const candidates = (Object.values(MESH_OPS) as OpSpec[]).filter((s) => s.op === op);
  if (candidates.length <= 1) return candidates[0] ?? null;
  // Ops shared by several entries (bevel, dissolve, mark, boolean) are told
  // apart by the param their entries pin.
  return (
    candidates.find((s) =>
      Object.entries(s.params).every(([k, v]) => typeof v !== "string" && typeof v !== "boolean" ? true : params[k] === undefined || params[k] === v),
    ) ?? candidates[0]
  );
}

/** Keeps a field's value inside its declared bounds; ints round. */
export function clampField(field: OpField, value: number): number {
  let v = field.kind === "int" ? Math.round(value) : value;
  if (field.min !== undefined) v = Math.max(field.min, v);
  if (field.max !== undefined) v = Math.min(field.max, v);
  return v;
}

/**
 * The params a modal op runs with: `base` with the drag param set from the
 * pointer's travel and the wheel param stepped. `travelPx` is the distance the
 * pointer moved from where the modal began (signed for fractions: right is
 * positive), `widthPx` the viewport width, `worldPerPx` the world units one
 * pixel spans at the selection.
 */
export function modalParams(
  spec: OpSpec,
  base: Record<string, unknown>,
  travelPx: number,
  widthPx: number,
  worldPerPx: number,
  wheelSteps: number,
): Record<string, unknown> {
  const out: Record<string, unknown> = { ...base };
  const m = spec.modal;
  if (!m) return out;
  if (m.drag) {
    const field = spec.fields.find((f) => f.key === m.drag);
    const raw = m.fraction ? (2 * travelPx) / Math.max(1, widthPx) : Math.abs(travelPx) * worldPerPx * (m.dragScale ?? 1);
    out[m.drag] = field ? clampField(field, raw) : raw;
  }
  if (m.wheel) {
    const field = spec.fields.find((f) => f.key === m.wheel);
    const start = typeof base[m.wheel] === "number" ? (base[m.wheel] as number) : 1;
    out[m.wheel] = field ? clampField(field, start + wheelSteps) : start + wheelSteps;
  }
  return out;
}
