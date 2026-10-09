import type { Vec3 } from "../core/api";
import { type Mat4, invert, lookAt, mat4, multiply, ortho, perspective } from "../math/mat4";
import { add, clamp, cross, length, normalize, scale, sub } from "../math/vec3";
import type { Ray } from "../math/ray";

/**
 * "persp" is a free (user) view; the rest are axis-aligned views. "side"
 * looks along +X, which is the left view.
 */
export type ViewKind = "persp" | "top" | "front" | "side" | "right" | "back" | "bottom";

export const VIEW_KINDS: ViewKind[] = ["persp", "top", "front", "side"];

export interface CameraState {
  kind: ViewKind;
  /** Orbit pivot, and the look-at point for the perspective view. */
  target: Vec3;
  /** Eye position for the perspective view. */
  eye: Vec3;
  /** Vertical field of view in degrees. */
  fov: number;
  near: number;
  far: number;
  /** Orthographic half-height in world units. */
  orthoHeight: number;
  /** Eye-to-target distance; also sets the ortho views' pullback. */
  distance: number;
  /** A free view drawn with an orthographic projection. */
  ortho?: boolean;
  /** Ortho came from snapping to an axis, so orbiting away turns it off again. */
  autoOrtho?: boolean;
}

/** A camera looking at the origin from the south, with a 60 inch grid. */
export function defaultCamera(): CameraState {
  return { kind: "persp", target: [0, 0, 0], eye: [512, -768, 384], fov: 60, near: 4, far: 1 << 22, orthoHeight: 512, distance: 1024 };
}

/** Whether a view kind is the free one; the axis views are orthographic. */
export function isPerspective(kind: ViewKind): boolean {
  return kind === "persp";
}

/** Whether a camera draws with a perspective projection. */
export function isPerspectiveState(state: CameraState): boolean {
  return state.kind === "persp" && !state.ortho;
}

/** The up vector a view uses; looking down -Z the screen up is +Y. */
export function viewUp(kind: ViewKind, forward?: Vec3): Vec3 {
  if (kind === "top") return [0, 1, 0];
  if (kind === "bottom") return [0, -1, 0];
  if (kind === "persp" && forward && Math.abs(forward[2]) > 0.9999) return [0, forward[2] < 0 ? 1 : -1, 0];
  return [0, 0, 1];
}

/** Fixed forward axis of an ortho view. */
export function viewAxis(kind: ViewKind): Vec3 | null {
  switch (kind) {
    case "top":
      return [0, 0, -1];
    case "front":
      return [0, 1, 0];
    case "side":
      return [1, 0, 0];
    case "right":
      return [-1, 0, 0];
    case "back":
      return [0, -1, 0];
    case "bottom":
      return [0, 0, 1];
    default:
      return null;
  }
}

/** Forward direction of a view; the perspective view depends on eye and target. */
export function viewForward(state: CameraState): Vec3 {
  const axis = viewAxis(state.kind);
  return axis ?? normalize(sub(state.target, state.eye), [1, 0, 0]);
}

/** Eye position for a view: the perspective view keeps its own, ortho pulls back. */
export function eyeFor(state: CameraState): Vec3 {
  const axis = viewAxis(state.kind);
  if (!axis) return state.eye;
  return add(state.target, scale(axis, -state.distance));
}


export class Camera {
  readonly state: CameraState;
  private view = mat4();
  private proj = mat4();
  private viewProj = mat4();
  private invViewProj = mat4();
  private aspect = 1;
  private dirty = true;

  constructor(state: CameraState = defaultCamera()) {
    this.state = state;
  }

  setAspect(aspect: number): void {
    const a = Number.isFinite(aspect) && aspect > 1e-4 ? aspect : 1;
    if (Math.abs(a - this.aspect) > 1e-6) {
      this.aspect = a;
      this.dirty = true;
    }
  }

  markDirty(): void {
    this.dirty = true;
  }

  matrices(): { view: Mat4; proj: Mat4; viewProj: Mat4; invViewProj: Mat4 } {
    if (this.dirty) this.recompute();
    return { view: this.view, proj: this.proj, viewProj: this.viewProj, invViewProj: this.invViewProj };
  }

  private recompute(): void {
    const s = this.state;
    const eye = eyeFor(s);
    lookAt(this.view, eye, s.target, viewUp(s.kind, viewForward(s)));
    if (isPerspectiveState(s)) {
      perspective(this.proj, (s.fov * Math.PI) / 180, this.aspect, Math.max(0.01, s.near), s.far);
    } else {
      const h = Math.max(0.01, s.orthoHeight);
      ortho(this.proj, h * this.aspect, h, -s.far, s.far);
    }
    multiply(this.viewProj, this.proj, this.view);
    invert(this.invViewProj, this.viewProj);
    this.dirty = false;
  }

  /** Ray through a viewport pixel. */
  pickRay(px: number, py: number, width: number, height: number): Ray {
    if (this.dirty) this.recompute();
    const ndcX = (px / Math.max(1, width)) * 2 - 1;
    const ndcY = 1 - (py / Math.max(1, height)) * 2;
    const near = unprojectPoint(this.invViewProj, ndcX, ndcY, 0);
    const far = unprojectPoint(this.invViewProj, ndcX, ndcY, 1);
    if (!near || !far) return { origin: eyeFor(this.state), dir: viewForward(this.state) };
    const dir = normalize(sub(far, near), viewForward(this.state));
    if (isPerspectiveState(this.state)) return { origin: eyeFor(this.state), dir };
    // Ortho rays are parallel; start them at the mid-point of the ray span.
    return { origin: add(near, scale(dir, length(sub(far, near)) * 0.5)), dir };
  }
}

function unprojectPoint(inv: Mat4, x: number, y: number, z: number): Vec3 | null {
  const w = inv[3] * x + inv[7] * y + inv[11] * z + inv[15];
  if (Math.abs(w) < 1e-12) return null;
  return [
    (inv[0] * x + inv[4] * y + inv[8] * z + inv[12]) / w,
    (inv[1] * x + inv[5] * y + inv[9] * z + inv[13]) / w,
    (inv[2] * x + inv[6] * y + inv[10] * z + inv[14]) / w,
  ];
}

/** Orbits the eye about the target. Deltas are radians; pitch is clamped. */
export function orbit(state: CameraState, dYaw: number, dPitch: number): void {
  toUserView(state);
  const offset = sub(eyeFor(state), state.target);
  const r = length(offset);
  if (r < 1e-6) return;
  const curYaw = Math.atan2(offset[0], offset[1]);
  const curPitch = Math.asin(clamp(offset[2] / r, -1, 1));
  const yaw = curYaw + dYaw;
  const pitch = clamp(curPitch + dPitch, -1.5533, 1.5533);
  const cp = Math.cos(pitch);
  state.eye = [
    state.target[0] + r * cp * Math.sin(yaw),
    state.target[1] + r * cp * Math.cos(yaw),
    state.target[2] + r * Math.sin(pitch),
  ];
  state.distance = r;
}

/** Slides eye and target in the view plane; offsets are in world units. */
export function pan(state: CameraState, rightAmount: number, upAmount: number): void {
  const f = normalize(sub(state.target, eyeFor(state)), viewForward(state));
  let r = cross(f, viewUp(state.kind, f));
  if (length(r) < 1e-6) r = cross(f, [0, 1, 0]);
  r = normalize(r, [1, 0, 0]);
  const u = normalize(cross(r, f), viewUp(state.kind, f));
  const delta = add(scale(r, -rightAmount), scale(u, upAmount));
  state.eye = add(state.eye, delta);
  state.target = add(state.target, delta);
}

/** Moves the eye along its forward/right/up basis; the target rides along. */
export function fly(state: CameraState, forward: number, strafe: number, rise: number, speed: number): void {
  const f = normalize(sub(state.target, state.eye), viewForward(state));
  let r = cross(f, viewUp(state.kind, f));
  if (length(r) < 1e-6) r = cross(f, [0, 1, 0]);
  r = normalize(r, [1, 0, 0]);
  const u = normalize(cross(r, f), viewUp(state.kind, f));
  const delta = add(add(scale(f, forward * speed), scale(r, strafe * speed)), scale(u, rise * speed));
  state.eye = add(state.eye, delta);
  state.target = add(state.target, delta);
}

/** Dollies along the view axis without changing direction. */
export function dolly(state: CameraState, amount: number): void {
  state.distance = Math.min(200000, Math.max(1, state.distance + amount));
  if (isPerspective(state.kind)) {
    state.eye = add(state.target, scale(viewForward(state), -state.distance));
  }
}

/**
 * Moves the whole camera, eye and orbit target, `distance` units along a
 * direction, through whatever is in the way.
 */
export function slide(state: CameraState, direction: Vec3, distance: number): void {
  const move = scale(normalize(direction, viewForward(state)), distance);
  state.eye = add(eyeFor(state), move);
  state.target = add(state.target, move);
}

/**
 * Scales the whole camera about a world point: the eye and the orbit target
 * both move toward it by `ratio` (below 1 closes in, above 1 backs off). The
 * point stays where it is on screen, the view keeps its direction, and the
 * orbit distance shrinks with the zoom, so orbiting afterwards turns about
 * what was zoomed into. The steps shrink as the point nears; a wheel never
 * passes the surface.
 */
export function zoomAbout(state: CameraState, point: Vec3, ratio: number): void {
  const eye = eyeFor(state);
  const minRatio = 1 / Math.max(1, state.distance);
  const r = Math.max(minRatio, Math.min(8, ratio));
  state.eye = add(point, scale(sub(eye, point), r));
  state.target = add(point, scale(sub(state.target, point), r));
  state.distance = Math.min(200000, Math.max(1, state.distance * r));
}

/** Frames a bounding box, keeping the current view direction. */
export function frameBounds(state: CameraState, min: Vec3, max: Vec3, fovY: number): void {
  const center = scale(add(min, max), 0.5);
  const radius = Math.max(1, length(sub(max, min)) * 0.5);
  const dist = radius / Math.max(0.05, Math.sin((fovY * Math.PI) / 360));
  const dir = normalize(sub(eyeFor(state), state.target), [-0.57, -0.76, 0.45]);
  state.target = center;
  state.eye = add(center, scale(dir, dist));
  state.distance = dist;
  // The 2D views zoom to the box as well; orthoHeight is half the view height.
  state.orthoHeight = Math.min(262144, Math.max(8, radius * 1.1));
}

/** World-units-per-pixel at the target plane, used to scale drags. */
export function panScale(state: CameraState, viewportHeight: number): number {
  if (isPerspectiveState(state)) {
    return (2 * state.distance * Math.tan((state.fov * Math.PI) / 360)) / Math.max(1, viewportHeight);
  }
  return (2 * state.orthoHeight) / Math.max(1, viewportHeight);
}

/** The label a view shows, e.g. "Top Orthographic" or "User Perspective". */
export function viewLabel(state: CameraState): string {
  const names: Record<ViewKind, string> = { persp: "User", top: "Top", bottom: "Bottom", front: "Front", back: "Back", right: "Right", side: "Left" };
  return `${names[state.kind]} ${isPerspectiveState(state) ? "Perspective" : "Orthographic"}`;
}

/**
 * Turns an axis view into the free view it shows so it can orbit. Leaving an
 * axis that snapped to ortho returns to perspective.
 */
export function toUserView(state: CameraState): void {
  if (state.kind !== "persp") {
    const axis = viewAxis(state.kind) ?? [0, 1, 0];
    // Straight down has no yaw; lean toward +Y so an orbit continues into the front view.
    const dir = Math.abs(axis[2]) > 0.5 ? normalize([0, 1e-4, axis[2]], axis) : axis;
    state.eye = add(state.target, scale(dir, -state.distance));
    state.kind = "persp";
    state.ortho = true;
  }
  if (state.autoOrtho) setOrtho(state, false);
}

/** Switches projection, keeping the size of what is at the target on screen. */
export function setOrtho(state: CameraState, ortho: boolean): void {
  const half = Math.tan((state.fov * Math.PI) / 360);
  if (ortho && isPerspectiveState(state)) state.orthoHeight = Math.min(262144, Math.max(8, state.distance * half));
  if (!ortho && !isPerspectiveState(state)) {
    const f = viewForward(state);
    state.kind = "persp";
    state.distance = Math.min(200000, Math.max(1, state.orthoHeight / Math.max(1e-4, half)));
    state.eye = add(state.target, scale(f, -state.distance));
  }
  state.ortho = ortho;
  state.autoOrtho = false;
}

/** Snaps to an axis view about the current target, going ortho the way auto perspective does. */
export function setAxisView(state: CameraState, kind: Exclude<ViewKind, "persp">): void {
  const wasPersp = isPerspectiveState(state);
  if (wasPersp) state.orthoHeight = Math.min(262144, Math.max(8, state.distance * Math.tan((state.fov * Math.PI) / 360)));
  state.kind = kind;
  state.ortho = true;
  state.autoOrtho = wasPersp || state.autoOrtho === true;
  state.eye = eyeFor(state);
}

const OPPOSITE: Record<Exclude<ViewKind, "persp">, Exclude<ViewKind, "persp">> = {
  top: "bottom", bottom: "top", front: "back", back: "front", right: "side", side: "right",
};

/** Looks at the target from the other side. */
export function flipView(state: CameraState): void {
  if (state.kind !== "persp") {
    const auto = state.autoOrtho;
    setAxisView(state, OPPOSITE[state.kind]);
    state.autoOrtho = auto;
    return;
  }
  state.eye = add(state.target, sub(state.target, state.eye));
}

/** Zooms an orthographic view by `ratio`, keeping `point` where it is on screen. */
export function zoomOrtho(state: CameraState, ratio: number, point?: Vec3): void {
  const next = Math.min(262144, Math.max(8, state.orthoHeight * ratio));
  const k = next / state.orthoHeight;
  if (point) {
    const f = viewForward(state);
    const d = sub(state.target, point);
    const along = f[0] * d[0] + f[1] * d[1] + f[2] * d[2];
    const move = scale(sub(d, scale(f, along)), k - 1);
    state.eye = add(eyeFor(state), move);
    state.target = add(state.target, move);
  }
  state.orthoHeight = next;
}
