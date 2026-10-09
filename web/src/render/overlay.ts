import type { Vec3 } from "../core/api";
import { add, cross, normalize, scale, sub } from "../math/vec3";
import type { Aabb } from "../math/vec3";
import { volumeColor } from "./material";

/** Line geometry: xyzrgb per vertex, ready for the line pipeline. */
export interface LineMesh {
  positions: Float32Array;
  colors: Float32Array;
  /** Vertex count; always a multiple of two. */
  count: number;
}

export interface BoxInstance {
  center: Vec3;
  half: Vec3;
  color: [number, number, number, number];
}

export interface BillboardInstance {
  center: Vec3;
  size: number;
  color: [number, number, number, number];
  /** UV rect within the glyph atlas. */
  rect: [number, number, number, number];
}

/** Accumulates line geometry without reallocating per call. */
export class LineBuilder {
  private pos: number[] = [];
  private col: number[] = [];

  get vertexCount(): number {
    return this.pos.length / 3;
  }

  segment(a: Vec3, b: Vec3, color: readonly [number, number, number, number]): void {
    this.pos.push(a[0], a[1], a[2], b[0], b[1], b[2]);
    for (let i = 0; i < 2; i++) this.col.push(color[0], color[1], color[2], color[3]);
  }

  polyline(points: readonly Vec3[], color: readonly [number, number, number, number], closed = false): void {
    for (let i = 0; i + 1 < points.length; i++) this.segment(points[i], points[i + 1], color);
    if (closed && points.length > 2) this.segment(points[points.length - 1], points[0], color);
  }

  /** A circle in the plane with the given normal, centred on `c`. */
  circle(c: Vec3, normal: Vec3, radius: number, color: readonly [number, number, number, number], segments = 24, arcFrom = 0, arcTo = Math.PI * 2): void {
    if (!(radius > 0) || segments < 3) return;
    const [a, b] = planeBasis(normal);
    const span = arcTo - arcFrom;
    let prev: Vec3 | null = null;
    for (let i = 0; i <= segments; i++) {
      const t = arcFrom + (span * i) / segments;
      const p = add(c, add(scale(a, Math.cos(t) * radius), scale(b, Math.sin(t) * radius)));
      if (prev) this.segment(prev, p, color);
      prev = p;
    }
  }

  build(): LineMesh {
    return {
      positions: new Float32Array(this.pos),
      colors: new Float32Array(this.col),
      count: this.pos.length / 3,
    };
  }

  /** Appends an already built mesh without recolouring it. */
  append(mesh: LineMesh): void {
    for (let i = 0; i < mesh.positions.length; i++) this.pos.push(mesh.positions[i]);
    for (let i = 0; i < mesh.colors.length; i++) this.col.push(mesh.colors[i]);
  }
}

/** An in-plane orthonormal basis whose cross product is `n`. */
export function planeBasis(n: Vec3): [Vec3, Vec3] {
  const helper: Vec3 = Math.abs(n[2]) < 0.9 ? [0, 0, 1] : [1, 0, 0];
  const a = normalize(cross(helper, n), [1, 0, 0]);
  return [a, normalize(cross(n, a), [0, 1, 0])];
}

const BOX_EDGES: [number, number][] = [
  [0, 1], [1, 3], [3, 2], [2, 0],
  [4, 5], [5, 7], [7, 6], [6, 4],
  [0, 4], [1, 5], [2, 6], [3, 7],
];

/** The eight corners of a box, indexed the way `BOX_EDGES` expects. */
export function boxCorners(min: Vec3, max: Vec3): Vec3[] {
  return [
    [min[0], min[1], min[2]],
    [max[0], min[1], min[2]],
    [min[0], max[1], min[2]],
    [max[0], max[1], min[2]],
    [min[0], min[1], max[2]],
    [max[0], min[1], max[2]],
    [min[0], max[1], max[2]],
    [max[0], max[1], max[2]],
  ];
}

/** The twelve edges of an axis-aligned box. */
export function boxWireframe(b: Aabb, color: readonly [number, number, number, number], into: LineBuilder): void {
  const corners = boxCorners(b.min, b.max);
  for (const [i, j] of BOX_EDGES) into.segment(corners[i], corners[j], color);
}

/**
 * A local box drawn under a transform: the twelve edges of the box as the
 * object is placed, turned and scaled with it rather than widened to the axes.
 */
export function orientedBoxWireframe(local: Aabb, m: ArrayLike<number>, color: readonly [number, number, number, number], into: LineBuilder): void {
  const corners = boxCorners(local.min, local.max).map((p): Vec3 => [
    m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
    m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
    m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
  ]);
  for (const [i, j] of BOX_EDGES) into.segment(corners[i], corners[j], color);
}

export const SELECTION_COLOR: readonly [number, number, number, number] = [1, 0.72, 0.15, 1];
export const HOVER_COLOR: readonly [number, number, number, number] = [0.4, 0.85, 1, 1];

/**
 * A sphere wireframe from three great circles. Spot and sky lights skip it and
 * get a cone instead.
 */
export function lightGizmo(
  into: LineBuilder,
  center: Vec3,
  kind: string,
  range: number,
  color: readonly [number, number, number],
  direction: Vec3,
  outerAngle: number,
): void {
  const c = [...color, 1] as [number, number, number, number];
  if (kind === "spot" || kind === "sun") {
    spotCone(into, center, direction, range, Math.max(1, Math.tan((outerAngle * Math.PI) / 360)), c);
    return;
  }
  const r = Math.max(1, range);
  into.circle(center, [0, 0, 1], r, c);
  into.circle(center, [1, 0, 0], r, c);
  into.circle(center, [0, 1, 0], r, c);
  if (kind === "sky") {
    // A vertical line marks the light as ambient-only.
    into.segment(center, add(center, [0, 0, r * 0.35]), c);
  }
}

/** Two cone outlines plus the base circle, from `center` along `dir`. */
export function spotCone(
  into: LineBuilder,
  center: Vec3,
  dir: Vec3,
  length: number,
  tanAngle: number,
  color: readonly [number, number, number, number],
): void {
  const d = normalize(dir, [0, 0, -1]);
  const tip = add(center, scale(d, length));
  const radius = Math.max(0.5, length * Math.min(8, tanAngle));
  const [a, b] = planeBasis(d);
  const rim: Vec3[] = [];
  for (let i = 0; i < 12; i++) {
    const t = (i / 12) * Math.PI * 2;
    rim.push(add(tip, add(scale(a, Math.cos(t) * radius), scale(b, Math.sin(t) * radius))));
  }
  for (let i = 0; i < rim.length; i++) into.segment(center, rim[i], color);
  into.polyline(rim, color, true);
}

export const SPLINE_SEGMENTS = 16;

/** Uniform Catmull-Rom sample count for a path, at least one per segment. */
export function splineSampleCount(points: readonly Vec3[], perSegment = SPLINE_SEGMENTS): number {
  if (points.length < 2) return points.length;
  return (points.length - 1) * perSegment + 1;
}

function catmull(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: number): Vec3 {
  const t2 = t * t;
  const t3 = t2 * t;
  const out: Vec3 = [0, 0, 0];
  for (let i = 0; i < 3; i++) {
    out[i] = 0.5 * ((2 * p1[i]) + (-p0[i] + p2[i]) * t + (2 * p0[i] - 5 * p1[i] + 4 * p2[i] - p3[i]) * t2 + (-p0[i] + 3 * p1[i] - 3 * p2[i] + p3[i]) * t3);
  }
  return out;
}

/**
 * Samples a control polygon as a Catmull-Rom curve. Endpoints are duplicated
 * so the curve starts and ends exactly on the first and last control point.
 */
export function sampleSpline(points: readonly Vec3[], perSegment = SPLINE_SEGMENTS): Vec3[] {
  if (points.length === 0) return [];
  if (points.length === 1) return [[...points[0] as Vec3]];
  if (points.length === 2) return [[...points[0] as Vec3], [...points[1] as Vec3]];
  const out: Vec3[] = [];
  for (let i = 0; i + 1 < points.length; i++) {
    const p0 = points[Math.max(0, i - 1)];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[Math.min(points.length - 1, i + 2)];
    for (let k = 0; k < perSegment; k++) out.push(catmull(p0, p1, p2, p3, k / perSegment));
  }
  out.push([...points[points.length - 1] as Vec3]);
  return out;
}

/** A path drawn as a curve with square end caps. */
export function pathGizmo(into: LineBuilder, points: readonly Vec3[], color: readonly [number, number, number, number], halfSize = 6): void {
  const curve = sampleSpline(points);
  into.polyline(curve, color);
  if (curve.length < 2) return;
  for (const end of [curve[0], curve[curve.length - 1]] as [Vec3, Vec3]) {
    const dir = normalize(sub(end, end === curve[0] ? curve[1] : curve[curve.length - 2]), [0, 0, 1]);
    const [a, b] = planeBasis(dir);
    const corners: Vec3[] = [
      add(add(end, scale(a, halfSize)), scale(b, halfSize)),
      add(add(end, scale(a, -halfSize)), scale(b, halfSize)),
      add(add(end, scale(a, -halfSize)), scale(b, -halfSize)),
      add(add(end, scale(a, halfSize)), scale(b, -halfSize)),
    ];
    into.polyline(corners, color, true);
  }
}

/** A translucent box instance for one volume, in world space. */
export function volumeBox(b: Aabb, kind: string): BoxInstance {
  const half = scale(sub(b.max, b.min), 0.5);
  return { center: scale(add(b.min, b.max), 0.5), half, color: volumeColor(kind) };
}

/** AABB of a triangle soup given as flat positions. */
export function boundsOf(positions: Float32Array, stride = 3, offset = 0): Aabb {
  const min: Vec3 = [Infinity, Infinity, Infinity];
  const max: Vec3 = [-Infinity, -Infinity, -Infinity];
  const verts = Math.floor(positions.length / stride);
  for (let i = offset; i < verts; i++) {
    for (let c = 0; c < 3; c++) {
      const v = positions[i * stride + c];
      if (!Number.isFinite(v)) continue;
      if (v < min[c]) min[c] = v;
      if (v > max[c]) max[c] = v;
    }
  }
  if (min[0] > max[0]) return { min: [0, 0, 0], max: [0, 0, 0] };
  return { min, max };
}

/** Eight transformed corners of a box, used to bound a rotated instance. */
export function transformedBounds(b: Aabb, m: Float32Array): Aabb {
  let min: Vec3 = [Infinity, Infinity, Infinity];
  let max: Vec3 = [-Infinity, -Infinity, -Infinity];
  for (const c of boxCorners(b.min, b.max)) {
    const p: Vec3 = [
      m[0] * c[0] + m[4] * c[1] + m[8] * c[2] + m[12],
      m[1] * c[0] + m[5] * c[1] + m[9] * c[2] + m[13],
      m[2] * c[0] + m[6] * c[1] + m[10] * c[2] + m[14],
    ];
    min = [Math.min(min[0], p[0]), Math.min(min[1], p[1]), Math.min(min[2], p[2])];
    max = [Math.max(max[0], p[0]), Math.max(max[1], p[1]), Math.max(max[2], p[2])];
  }
  return { min, max };
}
