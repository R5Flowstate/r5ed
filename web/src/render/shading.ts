import type { Vec3 } from "../core/api";

/** Viewport shading mode: textured, studio-lit solid, or wireframe. */
export type ShadingMode = "textured" | "solid" | "wireframe";

/** What a solid surface is coloured by. */
export type SolidColor = "material" | "object" | "single" | "uv_checker";

export interface ShadingState {
  mode: ShadingMode;
  /** Solid colour source; textured mode ignores it. */
  color: SolidColor;
  /** RGB for `single`, each 0..1. */
  single: [number, number, number];
  /** X-ray ghost amount, 0 is off. */
  xray: number;
  /** Drawn wire edges over solid surfaces. */
  wireOverlay: boolean;
  /** Cull back faces in solid mode. */
  cullBackfaces: boolean;
  /** Last non-wireframe mode, for the Shift+Z toggle back. */
  prev: ShadingMode;
}

/** Global cap on wireframe segments drawn in one frame. */
export const WIRE_LINE_BUDGET = 200000;
/** X-ray amount the Alt+Z toggle turns on. */
export const XRAY_DEFAULT = 0.5;
/** Wireframe line colour over the dark viewport. */
export const WIRE_COLOR: readonly [number, number, number, number] = [0.72, 0.76, 0.84, 1];

export function defaultShading(): ShadingState {
  return { mode: "textured", color: "material", single: [0.8, 0.8, 0.8], xray: 0, wireOverlay: false, cullBackfaces: false, prev: "textured" };
}

/** Shading with `mode`, remembering the last non-wireframe one for Shift+Z. */
export function setMode(s: ShadingState, mode: ShadingMode): ShadingState {
  if (mode === "wireframe") return s.mode === "wireframe" ? s : { ...s, mode, prev: s.mode };
  return { ...s, mode };
}

/** Shift+Z: wireframe, or back to the mode before it. */
export function toggleWireframe(s: ShadingState): ShadingState {
  return s.mode === "wireframe" ? { ...s, mode: s.prev } : { ...s, mode: "wireframe", prev: s.mode };
}

/** Alt+Z: x-ray on at the default amount, or off. */
export function toggleXray(s: ShadingState, amount = XRAY_DEFAULT): ShadingState {
  return { ...s, xray: s.xray > 0 ? 0 : amount };
}

/** Edit mode draws solid with a wire overlay on the edited object. */
export function editModeShading(s: ShadingState): ShadingState {
  return { ...s, mode: "solid", wireOverlay: true };
}

/** True when the frame draws wire buffers: always in wireframe, in solid only with the overlay toggle. */
export function drawsWire(s: ShadingState): boolean {
  return s.mode === "wireframe" || (s.mode === "solid" && s.wireOverlay);
}

/** True when the mode names a shading the page draws. */
export function isShadingMode(v: unknown): v is ShadingMode {
  return v === "textured" || v === "solid" || v === "wireframe";
}

/** True when the value names a solid colour source. */
export function isSolidColor(v: unknown): v is SolidColor {
  return v === "material" || v === "object" || v === "single" || v === "uv_checker";
}

/** A finite x-ray amount 0..1, or null. */
export function clampXray(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) && v >= 0 && v <= 1 ? v : null;
}

/** An RGB triple of finite 0..1 channels, or null. */
export function clampSingle(v: unknown): [number, number, number] | null {
  if (!Array.isArray(v) || v.length !== 3) return null;
  const out: number[] = [];
  for (const n of v) {
    if (typeof n !== "number" || !Number.isFinite(n) || n < 0 || n > 1) return null;
    out.push(n);
  }
  return [out[0], out[1], out[2]];
}

/** Index of a solid colour source in the frame's shade vector. */
export function solidColorIndex(color: SolidColor): number {
  return color === "object" ? 1 : color === "single" ? 2 : color === "uv_checker" ? 3 : 0;
}

/** Stable pastel for an object id, so solid mode tells objects apart. */
export function hashObjectColor(id: string): [number, number, number] {
  let h = 0x811c9dc5;
  for (let i = 0; i < id.length; i++) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  const pick = (salt: number): number => {
    let x = (h ^ salt) >>> 0;
    x = Math.imul(x ^ (x >>> 15), 0x2c1b3c6d);
    x = Math.imul(x ^ (x >>> 12), 0x297a2d39);
    x ^= x >>> 15;
    return 0.35 + ((x >>> 0) % 1000) / 1000 * 0.55;
  };
  return [pick(0x9e37), pick(0x51f3), pick(0x2b11)];
}

/** Indices into `dists` nearest first, ties by index; non-finite sorts last. */
export function sortNearest(dists: readonly number[]): number[] {
  const order = dists.map((_, i) => i);
  order.sort((a, b) => {
    const da = dists[a];
    const db = dists[b];
    const fa = Number.isFinite(da);
    const fb = Number.isFinite(db);
    if (fa && fb) return da !== db ? (da as number) - (db as number) : a - b;
    if (fa) return -1;
    if (fb) return 1;
    return a - b;
  });
  return order;
}

/**
 * How many leading items of `counts` (already nearest first) fit in `budget`
 * segments. The render expands whole items only, so a remainder is left for
 * nothing rather than drawing half an object.
 */
export function takeUntilBudget(counts: readonly number[], budget: number): number {
  let used = 0;
  let n = 0;
  for (const c of counts) {
    if (c < 0 || !Number.isFinite(c)) continue;
    if (used + c > budget) break;
    used += c;
    n++;
  }
  return n;
}

/** Segment indices nearest first under the budget; mirrors the core's `select_lines`. */
export function selectSegmentBudget(dists: readonly number[], budget: number): number[] {
  return sortNearest(dists).slice(0, Math.max(0, Math.min(budget, dists.length)));
}

/** Midpoint distance of a world-space segment to the eye. */
export function segmentDistance(ax: number, ay: number, az: number, bx: number, by: number, bz: number, eye: Vec3): number {
  const mx = (ax + bx) / 2 - eye[0];
  const my = (ay + by) / 2 - eye[1];
  const mz = (az + bz) / 2 - eye[2];
  return Math.hypot(mx, my, mz);
}
