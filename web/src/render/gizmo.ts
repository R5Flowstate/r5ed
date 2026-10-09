import type { Quat, Vec3 } from "../core/api";
import { quatFromAxisAngle, quatMul, quatRotate } from "../math/quat";
import { rayAxis, rayPlane, type Ray } from "../math/ray";
import { add, cross, dot, length, normalize, scale, sub } from "../math/vec3";
import { type LineBuilder, planeBasis } from "./overlay";

export type GizmoMode = "translate" | "rotate" | "scale";
export type GizmoSpace = "world" | "local";
export type AxisHandle = "x" | "y" | "z";
export type PlaneHandle = "xy" | "yz" | "zx";
export type GizmoHandle = AxisHandle | PlaneHandle | "screen" | "view";

export const AXIS_VECTORS: Record<AxisHandle, Vec3> = {
  x: [1, 0, 0],
  y: [0, 1, 0],
  z: [0, 0, 1],
};

export const AXIS_COLORS: Record<AxisHandle, Vec3> = {
  x: [0.92, 0.27, 0.27],
  y: [0.55, 0.85, 0.3],
  z: [0.32, 0.55, 0.95],
};

export const HANDLE_ORDER: Record<GizmoMode, GizmoHandle[]> = {
  translate: ["x", "y", "z", "xy", "yz", "zx", "screen", "view"],
  rotate: ["screen", "view", "x", "y", "z"],
  scale: ["x", "y", "z", "xy", "yz", "zx", "screen", "view"],
};

export interface SnapSettings {
  enabled: boolean;
  /** Grid step in world units. */
  size: number;
  /** Angle step in degrees. */
  angle: number;
  /** Scale step as a fraction of 1. */
  scale: number;
  /** What a move snaps to: steps from where it began, the absolute grid, or the surface under the cursor. */
  target?: "increment" | "grid" | "face";
  /** With the face target, turn the object so its up axis follows the surface. */
  alignRotation?: boolean;
}

export function defaultSnap(): SnapSettings {
  return { enabled: true, size: 16, angle: 5, scale: 0.1, target: "increment", alignRotation: false };
}

/** Snaps to the nearest multiple of `step`; halves round away from zero. */
export function snapValue(value: number, step: number): number {
  if (!(step > 0) || !Number.isFinite(value)) return value;
  const q = value / step;
  return (q < 0 ? -1 : 1) * Math.round(Math.abs(q)) * step;
}

export function snapAngle(value: number, stepDeg: number): number {
  if (!(stepDeg > 0) || !Number.isFinite(value)) return value;
  return snapValue(value, stepDeg);
}

export function snapPosition(p: Vec3, snap: SnapSettings): Vec3 {
  if (!snap.enabled) return p;
  return [snapValue(p[0], snap.size), snapValue(p[1], snap.size), snapValue(p[2], snap.size)];
}

/** Axis direction in world space, honouring local/world space. */
export function handleAxis(handle: AxisHandle, rotation: Quat, space: GizmoSpace): Vec3 {
  const base = AXIS_VECTORS[handle];
  if (space === "world") return base;
  return normalize(quatRotate(rotation, base), base);
}

export function handlePlaneAxes(handle: PlaneHandle, rotation: Quat, space: GizmoSpace): [Vec3, Vec3] {
  const a = handle[0] as AxisHandle;
  const b = handle[1] as AxisHandle;
  return [handleAxis(a, rotation, space), handleAxis(b, rotation, space)];
}

export function handleNormal(handle: PlaneHandle, rotation: Quat, space: GizmoSpace): Vec3 {
  const [a, b] = handlePlaneAxes(handle, rotation, space);
  return normalize(cross(a, b), [0, 0, 1]);
}

/** The three axis handles as lines: a shaft, an arrow head and two ticks. */
function axisHandle(
  into: LineBuilder,
  pivot: Vec3,
  axis: Vec3,
  color: readonly [number, number, number, number],
  length: number,
  weight: number,
): void {
  const tip = add(pivot, scale(axis, length));
  into.segment(pivot, tip, color);
  const [u, v] = planeBasis(axis);
  for (const side of [u, v]) {
    const barb = add(scale(axis, -length * 0.18), scale(side, length * 0.12 * weight));
    into.segment(tip, add(tip, barb), color);
  }
  for (const t of [0.45, 0.65]) {
    into.segment(add(pivot, scale(axis, length * t)), add(pivot, scale(axis, length * (t + 0.08))), color);
  }
}

const SCREEN_COLOR: readonly [number, number, number, number] = [0.85, 0.85, 0.85, 1];
const HIGHLIGHT: readonly [number, number, number, number] = [1, 1, 1, 1];
/** The rotate rings, as a fraction of the arm length. */
const RING_RADIUS = 0.85;
/** The side of a plane handle's corner square, as a fraction of the arm length. */
const PLANE_SQUARE = 0.35;

/**
 * The transform gizmo, drawn in world units so the handles are grabbed at the
 * same size the picker tests against.
 */
export function gizmoLines(
  into: LineBuilder,
  pivot: Vec3,
  rotation: Quat,
  mode: GizmoMode,
  space: GizmoSpace,
  size: number,
  active: GizmoHandle | null = null,
): void {
  const length = Math.max(1, size);
  const axes: AxisHandle[] = ["x", "y", "z"];
  for (const axis of axes) {
    const color = [...AXIS_COLORS[axis], 1] as [number, number, number, number];
    const hot = active === axis ? HIGHLIGHT : color;
    const dir = handleAxis(axis, rotation, space);
    if (mode === "rotate") {
      into.circle(pivot, dir, length * RING_RADIUS, hot, 48);
    } else {
      axisHandle(into, pivot, dir, hot, length, 1);
    }
    if (mode === "scale") {
      into.segment(add(pivot, scale(dir, length * 0.15)), add(pivot, scale(dir, length * 0.3)), hot);
    }
  }
  if (mode === "rotate") return;
  // The plane handles sit in the corner between two axes, with a square in them.
  const planes: PlaneHandle[] = ["xy", "yz", "zx"];
  for (const plane of planes) {
    const color = active === plane ? HIGHLIGHT : ([0.7, 0.7, 0.7, 0.8] as [number, number, number, number]);
    const [a, b] = handlePlaneAxes(plane, rotation, space);
    const corner = add(pivot, add(scale(a, length * PLANE_SQUARE), scale(b, length * PLANE_SQUARE)));
    into.polyline(
      [
        add(pivot, scale(a, length * PLANE_SQUARE)),
        corner,
        add(pivot, scale(b, length * PLANE_SQUARE)),
      ],
      color,
    );
  }
  // The centre handle moves in the screen plane, and the view handle along the
  // camera direction, both of which the picker offers last.
  const centre = active === "screen" ? HIGHLIGHT : SCREEN_COLOR;
  into.circle(pivot, [0, 0, 1], length * 0.12, centre, 16);
  if (active === "view") {
    into.circle(pivot, [0, 0, 1], length * 0.2, HIGHLIGHT, 16);
  }
}

export interface HandleHit {
  handle: GizmoHandle;
  /** World point on the handle where the ray met it. */
  point: Vec3;
  /** Signed distance from the pivot along the handle's primary axis. */
  t: number;
  /** Distance from the pivot projected onto each world axis, for plane handles. */
  plane: [number, number];
  /** Pixels from the cursor; smaller is a better match. */
  screenDist: number;
}

/** A world point expressed in pixels, or null when it is behind the camera. */
export type ProjectFn = (p: Vec3) => { x: number; y: number } | null;

function viewPlaneNormal(r: Ray): Vec3 {
  return normalize(cross(r.dir, [0, 0, 1]), [0, 1, 0]);
}

/**
 * In-plane basis for an axis: `a` reads as angle 0 and `b` is the direction of
 * positive (right-handed) rotation, so `b = cross(axis, a)`.
 */
function basisFromNormal(n: Vec3): [Vec3, Vec3] {
  const helper: Vec3 = Math.abs(n[0]) < 0.9 ? [1, 0, 0] : [0, 1, 0];
  const a = normalize(sub(helper, scale(n, dot(helper, n))), [1, 0, 0]);
  return [a, cross(n, a)];
}

function ringHit(
  r: Ray,
  pivot: Vec3,
  normal: Vec3,
  radius: number,
  project: ProjectFn,
  cursor: { x: number; y: number },
  tolerance: number,
): number | null {
  const t = rayPlane(r, pivot, normal);
  if (t === null || t <= 0) return null;
  const world = add(r.origin, scale(r.dir, t));
  const off = sub(world, pivot);
  if (length(off) < 1e-9) return null;
  // The tolerance is in pixels, so the miss is measured on screen: from the
  // cursor to the ring point nearest the ray's hit.
  const onRing = add(pivot, scale(normalize(off, [1, 0, 0]), radius));
  const p = project(onRing);
  if (!p) return null;
  if (Math.hypot(p.x - cursor.x, p.y - cursor.y) > tolerance) return null;
  return t;
}

/**
 * Picks the gizmo handle under the cursor. Handles are ranked by their pixel
 * distance to the cursor, so the closest one wins regardless of pick order.
 */
export function pickHandle(
  r: Ray,
  pivot: Vec3,
  rotation: Quat,
  space: GizmoSpace,
  mode: GizmoMode,
  project: ProjectFn,
  cursor: { x: number; y: number },
  size: number,
  tolerance: number,
): HandleHit | null {
  let best: HandleHit | null = null;
  for (const handle of HANDLE_ORDER[mode]) {
    const hit = testHandle(r, pivot, rotation, space, mode, handle, project, cursor, size, tolerance);
    // The quarter-pixel bias breaks near-ties towards the earlier handle, so an
    // axis wins over the plane it lies in.
    if (hit && (!best || hit.screenDist < best.screenDist - 0.25)) best = hit;
  }
  return best;
}

function testHandle(
  r: Ray,
  pivot: Vec3,
  rotation: Quat,
  space: GizmoSpace,
  mode: GizmoMode,
  handle: GizmoHandle,
  project: ProjectFn,
  cursor: { x: number; y: number },
  size: number,
  tolerance: number,
): HandleHit | null {
  if (handle === "x" || handle === "y" || handle === "z") {
    if (mode === "rotate") {
      const axis = handleAxis(handle, rotation, space);
      // The ring is drawn at RING_RADIUS of the arm length; it is grabbed there.
      const t = ringHit(r, pivot, axis, size * RING_RADIUS, project, cursor, tolerance);
      if (t === null) return null;
      return { handle, point: add(pivot, scale(axis, size)), t, plane: [0, 0], screenDist: 0 };
    }
    const axis = handleAxis(handle, rotation, space);
    const t = rayAxis(r, pivot, axis);
    // Along the drawn arm only, not the whole infinite axis.
    if (t === null || t <= 0 || t > size * 1.15) return null;
    const world = add(pivot, scale(axis, t));
    const p = project(world);
    if (!p) return null;
    const d = Math.hypot(p.x - cursor.x, p.y - cursor.y);
    if (d > tolerance) return null;
    const plane = planeComponents(sub(world, pivot), axis);
    return { handle, point: world, t, plane, screenDist: d };
  }
  if (handle === "xy" || handle === "yz" || handle === "zx") {
    if (mode === "rotate") return null;
    const n = handleNormal(handle, rotation, space);
    const t = rayPlane(r, pivot, n);
    if (t === null || t <= 0) return null;
    const world = add(r.origin, scale(r.dir, t));
    const p = project(world);
    if (!p) return null;
    const d = Math.hypot(p.x - cursor.x, p.y - cursor.y);
    if (d > tolerance) return null;
    const [a, b] = handlePlaneAxes(handle, rotation, space);
    const rel = sub(world, pivot);
    const u = dot(rel, a);
    const v = dot(rel, b);
    // Inside the drawn corner square only.
    const edge = size * PLANE_SQUARE * 1.3;
    if (u < 0 || v < 0 || u > edge || v > edge) return null;
    return { handle, point: world, t, plane: [u, v], screenDist: d };
  }
  const n = viewPlaneNormal(r);
  const t = rayPlane(r, pivot, n);
  if (t === null) return null;
  const world = add(r.origin, scale(r.dir, t));
  const p = project(world);
  if (!p) return null;
  const d = Math.hypot(p.x - cursor.x, p.y - cursor.y);
  if (d > tolerance) return null;
  const rel = sub(world, pivot);
  const primary = handle === "screen" ? scale(cross(n, [0, 0, 1]), -1) : normalize(sub(pivot, r.origin), [1, 0, 0]);
  return { handle, point: world, t, plane: [dot(rel, primary), dot(rel, cross(primary, n))], screenDist: d };
}

function planeComponents(rel: Vec3, axis: Vec3): [number, number] {
  const helper: Vec3 = Math.abs(axis[2]) < 0.9 ? [0, 0, 1] : [1, 0, 0];
  const a = normalize(cross(helper, axis), [1, 0, 0]);
  const b = cross(axis, a);
  return [dot(rel, a), dot(rel, b)];
}


export interface TransformDelta {
  position?: Vec3;
  rotation?: Quat;
  scale?: Vec3;
}

export interface DragState {
  mode: GizmoMode;
  handle: GizmoHandle;
  space: GizmoSpace;
  pivot: Vec3;
  position: Vec3;
  rotation: Quat;
  scale: Vec3;
  /** Handle geometry captured at grab time. */
  hit: HandleHit;
  /** Rotation/drag axis; in translate and scale it is the primary axis. */
  axis: Vec3;
  startAngle: number;
  snap: SnapSettings;
  /** Accumulated rotation in degrees, updated on every move. */
  totalAngle: number;
}

export interface DragParams {
  r: Ray;
  pivot: Vec3;
  rotation: Quat;
  position: Vec3;
  objectScale: Vec3;
  mode: GizmoMode;
  space: GizmoSpace;
  hit: HandleHit;
  snap: SnapSettings;
  /** Camera forward, used by the view-aligned rotate handle. */
  cameraDir: Vec3;
}

function dragAxis(p: DragParams): Vec3 {
  const h = p.hit.handle;
  if (h === "x" || h === "y" || h === "z") return handleAxis(h, p.rotation, p.space);
  if (h === "xy" || h === "yz" || h === "zx") return handleNormal(h, p.rotation, p.space);
  if (h === "view") return normalize(p.cameraDir, [0, 0, 1]);
  return viewPlaneNormal(p.r);
}

/** Captures the gizmo state at the moment a drag starts. */
export function beginDrag(p: DragParams): DragState {
  const axis = dragAxis(p);
  const world = add(p.r.origin, scale(p.r.dir, p.hit.t));
  return {
    mode: p.mode,
    handle: p.hit.handle,
    space: p.space,
    pivot: p.pivot,
    position: p.position,
    rotation: p.rotation,
    scale: p.objectScale,
    hit: p.hit,
    axis,
    startAngle: axisAngle(world, p.pivot, axis),
    snap: p.snap,
    totalAngle: 0,
  };
}

/** Applies a drag to the transform, returning the absolute new values. */
export function applyDrag(state: DragState, r: Ray): TransformDelta {
  if (state.mode === "rotate") return rotateDrag(state, r);
  if (state.mode === "scale") return scaleDrag(state, r);
  return translateDrag(state, r);
}

function translateDrag(state: DragState, r: Ray): TransformDelta {
  const h = state.handle;
  let next: Vec3;
  if (h === "x" || h === "y" || h === "z") {
    const t = rayAxis(r, state.pivot, state.axis);
    if (t === null) return { position: state.position };
    let travel = t - state.hit.t;
    if (state.snap.enabled) travel = snapValue(travel, state.snap.size);
    next = add(state.position, scale(state.axis, travel));
  } else {
    const n = h === "xy" || h === "yz" || h === "zx" ? state.axis : viewPlaneNormal(r);
    const t = rayPlane(r, state.hit.point, n);
    if (t === null) return { position: state.position };
    const world = add(r.origin, scale(r.dir, t));
    const delta = sub(world, state.hit.point);
    if (n[0] !== 0 || n[1] !== 0 || n[2] !== 0) {
      // Axis and plane handles keep their component off the drag plane fixed.
      const keep = projectOut(delta, n);
      next = add(state.position, keep);
    } else {
      next = add(state.position, delta);
    }
    if (state.snap.enabled) next = snapPosition(next, state.snap);
  }
  return { position: next };
}

function projectOut(v: Vec3, n: Vec3): Vec3 {
  const d = dot(v, n);
  if (Math.abs(d) < 1e-9) return v;
  return sub(v, scale(normalize(n, [0, 0, 1]), d));
}

function scaleDrag(state: DragState, r: Ray): TransformDelta {
  const h = state.handle;
  if (h === "screen" || h === "view") {
    const n = viewPlaneNormal(r);
    const t = rayPlane(r, state.hit.point, n);
    const d0 = length(sub(state.hit.point, state.pivot));
    const d1 = t === null ? d0 : length(sub(add(r.origin, scale(r.dir, t)), state.pivot));
    const raw = d0 > 1e-6 ? d1 / d0 : 1;
    const k = Math.max(1e-3, state.snap.enabled ? snapValue(raw, state.snap.scale) : raw);
    return { scale: h === "view" ? scale(state.scale, k) : [state.scale[0] * k, state.scale[1] * k, state.scale[2]] };
  }
  const plane = h === "xy" || h === "yz" || h === "zx";
  const first = (h === "xy" ? "x" : h === "yz" ? "y" : "z") as AxisHandle;
  const axisList: AxisHandle[] = plane ? [first, h === "xy" ? "y" : h === "yz" ? "z" : "x"] : [h as AxisHandle];
  const out: Vec3 = [...state.scale];
  for (const a of axisList) {
    const idx = a === "x" ? 0 : a === "y" ? 1 : 2;
    const axis = handleAxis(a, state.rotation, state.space);
    const t = plane ? planeAxisT(r, state, axis) : rayAxis(r, state.pivot, axis);
    if (t === null) continue;
    const ref = plane ? state.hit.plane[a === first ? 0 : 1] : state.hit.t;
    const raw = Math.abs(ref) > 1e-4 ? t / ref : 1;
    const factor = state.snap.enabled ? snapValue(raw, state.snap.scale) : raw;
    out[idx] = state.scale[idx] * Math.max(1e-3, Math.abs(factor));
  }
  return { scale: out };
}

function planeAxisT(r: Ray, state: DragState, axis: Vec3): number | null {
  const t = rayPlane(r, state.pivot, state.axis);
  if (t === null) return null;
  return dot(sub(add(r.origin, scale(r.dir, t)), state.pivot), axis);
}

function rotateDrag(state: DragState, r: Ray): TransformDelta {
  const t = rayPlane(r, state.pivot, state.axis);
  if (t === null) return { rotation: state.rotation };
  const world = add(r.origin, scale(r.dir, t));
  let deg = axisAngle(world, state.pivot, state.axis) - state.startAngle;
  if (state.snap.enabled) {
    // Snap the new total, then feed back the difference so the handle never drifts.
    const total = state.totalAngle + deg;
    const snapped = snapAngle(total, state.snap.angle);
    deg = snapped - state.totalAngle;
  }
  // The total stays unwrapped: the quaternion repeats every 360 degrees but the
  // snap feedback needs to know which turn the cursor is in.
  state.totalAngle += deg;
  const local = quatFromAxisAngle(state.axis, (state.totalAngle * Math.PI) / 180);
  const rotation = state.space === "local" ? quatMul(local, state.rotation) : quatMul(state.rotation, local);
  return { rotation };
}

/** Signed angle of `p` about `axis` through `pivot`, in degrees. */
export function axisAngle(p: Vec3, pivot: Vec3, axis: Vec3): number {
  const rel = sub(p, pivot);
  const inPlane = sub(rel, scale(axis, dot(rel, axis)));
  if (length(inPlane) < 1e-9) return 0;
  const [a, b] = basisFromNormal(axis);
  return (Math.atan2(dot(inPlane, b), dot(inPlane, a)) * 180) / Math.PI;
}

/** Wraps an angle in degrees into (-180, 180]. */
export function wrapDegrees(a: number): number {
  const x = ((((a + 180) % 360) + 360) % 360) - 180;
  return x === -180 ? 180 : x;
}
