import type { Vec3 } from "../core/api";
import type { BrushFace } from "../doc/model";
import { cross, dot } from "../math/vec3";

/** How far off a plane a point may sit and still be on it, in inches. */
const ON_PLANE = 0.01;

/**
 * The corners of a convex brush: every point where three of its face planes
 * meet that no other face cuts away. The faces are world-space planes
 * (`normal . p = dist`, normal pointing out), as the document stores them.
 */
export function brushVertices(faces: readonly BrushFace[]): Vec3[] {
  const out: Vec3[] = [];
  for (let i = 0; i < faces.length; i++) {
    for (let j = i + 1; j < faces.length; j++) {
      for (let k = j + 1; k < faces.length; k++) {
        const p = intersect3(faces[i], faces[j], faces[k]);
        if (!p) continue;
        if (!faces.every((f) => dot(f.normal, p) - f.dist <= ON_PLANE)) continue;
        if (out.some((q) => Math.hypot(q[0] - p[0], q[1] - p[1], q[2] - p[2]) < ON_PLANE * 10)) continue;
        out.push(p);
      }
    }
  }
  return out;
}

/** The faces a corner lies on, by index. */
function facesOf(faces: readonly BrushFace[], p: Vec3): number[] {
  const out: number[] = [];
  faces.forEach((f, i) => {
    if (Math.abs(dot(f.normal, p) - f.dist) <= ON_PLANE * 10) out.push(i);
  });
  return out;
}

/** The edges of a convex brush: pairs of corners that share two faces. */
export function brushEdges(faces: readonly BrushFace[], vertices: readonly Vec3[] = brushVertices(faces)): [Vec3, Vec3][] {
  const on = vertices.map((v) => facesOf(faces, v));
  const out: [Vec3, Vec3][] = [];
  for (let a = 0; a < vertices.length; a++) {
    for (let b = a + 1; b < vertices.length; b++) {
      const shared = on[a].filter((f) => on[b].includes(f));
      if (shared.length >= 2) out.push([vertices[a], vertices[b]]);
    }
  }
  return out;
}

/** The face a surface point sits on: the plane it is nearest. */
export function faceAt(faces: readonly BrushFace[], p: Vec3): number {
  let best = -1;
  let bestDist = Infinity;
  faces.forEach((f, i) => {
    const d = Math.abs(dot(f.normal, p) - f.dist);
    if (d < bestDist) {
      bestDist = d;
      best = i;
    }
  });
  return best;
}

/** The index of the screen point nearest the cursor within `tolerance` pixels, or -1. */
export function nearestOnScreen(points: readonly ({ x: number; y: number } | null)[], cursor: { x: number; y: number }, tolerance: number): number {
  let best = -1;
  let bestDist = tolerance;
  points.forEach((p, i) => {
    if (!p) return;
    const d = Math.hypot(p.x - cursor.x, p.y - cursor.y);
    if (d <= bestDist) {
      bestDist = d;
      best = i;
    }
  });
  return best;
}

/** Pixel distance from a cursor to a screen segment. */
export function segmentDistance(a: { x: number; y: number }, b: { x: number; y: number }, c: { x: number; y: number }): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const len = dx * dx + dy * dy;
  const t = len > 0 ? Math.max(0, Math.min(1, ((c.x - a.x) * dx + (c.y - a.y) * dy) / len)) : 0;
  return Math.hypot(a.x + dx * t - c.x, a.y + dy * t - c.y);
}

function intersect3(a: BrushFace, b: BrushFace, c: BrushFace): Vec3 | null {
  const bc = cross(b.normal, c.normal);
  const det = dot(a.normal, bc);
  if (Math.abs(det) < 1e-9) return null;
  const ca = cross(c.normal, a.normal);
  const ab = cross(a.normal, b.normal);
  // `+ 0` folds a -0 into 0, so a corner compares equal wherever it came from.
  return [
    (a.dist * bc[0] + b.dist * ca[0] + c.dist * ab[0]) / det + 0,
    (a.dist * bc[1] + b.dist * ca[1] + c.dist * ab[1]) / det + 0,
    (a.dist * bc[2] + b.dist * ca[2] + c.dist * ab[2]) / det + 0,
  ];
}
