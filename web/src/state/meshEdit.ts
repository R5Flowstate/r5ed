import type { Core, Id, Vec3 } from "../core/api";

/** The element mode an edit session selects in. */
export type EditSelMode = "vertex" | "edge" | "face";

/** What the editor holds while a mesh is being edited. */
export interface MeshEditState {
  object: Id;
  mode: EditSelMode;
  counts: { verts: number; edges: number; faces: number };
}

/** The last mesh op, for the redo panel and the control socket. */
export interface LastMeshOp {
  request: { op: string; object: string; sel: unknown; params: Record<string, unknown> };
  report: { verts: number; edges: number; faces: number; warnings: string[] };
  /** Undo depth right after the op applied; adjust refuses a stale panel. */
  depth: number;
}

export interface MeshOpResult {
  selection: { mode: EditSelMode; ids: number[] };
  report: { verts: number; edges: number; faces: number; warnings: string[] };
  /** Objects the op's command touched: the edited mesh, plus any it created (separate). */
  objects?: string[];
}

/** `{mode, ids?, query?}` as the session's `editSelect` takes it. */
export function selJson(mode: EditSelMode, ids: number[] = [], query?: unknown): string {
  return JSON.stringify(query === undefined ? { mode, ids } : { mode, ids, query });
}

/** `{op, object, sel, params}` as `meshOp` and `editPreview` take it. */
export function opRequest(object: Id, op: string, sel: unknown, params: Record<string, unknown>): string {
  return JSON.stringify({ op, object, sel, params });
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** Parses and shape-checks a `{command, selection, report}` op result. */
export function parseOpResult(text: string): { commandJson: string; result: MeshOpResult } {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    throw new Error("the core returned malformed op JSON");
  }
  if (!isRecord(v) || !isRecord(v.selection) || !isRecord(v.report)) throw new Error("the core returned a malformed op result");
  const mode = v.selection.mode;
  if (mode !== "vertex" && mode !== "edge" && mode !== "face") throw new Error("the core returned a bad selection mode");
  if (!Array.isArray(v.selection.ids) || !v.selection.ids.every((i) => Number.isInteger(i))) {
    throw new Error("the core returned bad selection ids");
  }
  for (const k of ["verts", "edges", "faces"]) {
    if (!Number.isInteger((v.report as Record<string, unknown>)[k])) throw new Error("the core returned a bad report");
  }
  return {
    commandJson: JSON.stringify(v.command),
    result: {
      selection: { mode, ids: v.selection.ids as number[] },
      report: {
        verts: v.report.verts as number,
        edges: v.report.edges as number,
        faces: v.report.faces as number,
        warnings: Array.isArray(v.report.warnings) ? (v.report.warnings as string[]) : [],
      },
    },
  };
}

/** A validated op request the redo panel re-runs with edited params. */
export function rerunRequest(last: LastMeshOp, params: Record<string, unknown>): string {
  return JSON.stringify({ op: last.request.op, object: last.request.object, sel: last.request.sel, params });
}

/** Runs the re-run request against the core; the caller undoes first. */
export function rerunMeshOp(core: Core, last: LastMeshOp, params: Record<string, unknown>): { commandJson: string; result: MeshOpResult } {
  return parseOpResult(core.meshOp(rerunRequest(last, params)));
}

export interface PickQueryInput {
  mode: EditSelMode;
  x: number;
  y: number;
  eye: Vec3;
  /** Column-major view-projection matrix. */
  viewProj: ArrayLike<number>;
  width: number;
  height: number;
  origin: Vec3;
  dir: Vec3;
  xray?: boolean;
  thresholdPx?: number;
  rect?: [number, number, number, number];
}

export function pickQueryJson(q: PickQueryInput): string {
  return JSON.stringify({
    mode: q.mode,
    x: q.x,
    y: q.y,
    eye: q.eye,
    view_proj: Array.from(q.viewProj),
    width: q.width,
    height: q.height,
    ray_origin: q.origin,
    ray_dir: q.dir,
    xray: q.xray ?? false,
    threshold_px: q.thresholdPx ?? 12,
    ...(q.rect ? { rect: q.rect } : {}),
  });
}

export type PickReply =
  | { kind: "vertex" | "edge" | "face"; id: number; distance: number }
  | { kind: "elements"; mode: EditSelMode; ids: number[] }
  | { kind: "none" };

/** Parses and shape-checks a session `PickReply`. */
export function parsePickReply(text: string): PickReply {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    throw new Error("the core returned malformed pick JSON");
  }
  if (!isRecord(v) || typeof v.kind !== "string") throw new Error("the core returned a malformed pick reply");
  if (v.kind === "none") return { kind: "none" };
  if (v.kind === "elements") {
    if (!Array.isArray(v.ids) || !v.ids.every((i) => Number.isInteger(i))) throw new Error("the core returned bad pick ids");
    const mode = v.mode;
    if (mode !== "vertex" && mode !== "edge" && mode !== "face") throw new Error("the core returned a bad pick mode");
    return { kind: "elements", mode, ids: v.ids as number[] };
  }
  if ((v.kind === "vertex" || v.kind === "edge" || v.kind === "face") && Number.isInteger(v.id)) {
    return { kind: v.kind, id: v.id as number, distance: typeof v.distance === "number" ? v.distance : 0 };
  }
  throw new Error("the core returned a malformed pick reply");
}
