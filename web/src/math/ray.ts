import type { Vec3 } from "../core/api";
import { add, cross, dot, length, normalize, scale, sub } from "./vec3";

export interface Ray {
  origin: Vec3;
  dir: Vec3;
}

export function ray(origin: Vec3, dir: Vec3): Ray {
  return { origin, dir: normalize(dir, [0, 0, -1]) };
}

export function rayAt(r: Ray, t: number): Vec3 {
  return add(r.origin, scale(r.dir, t));
}

/** Slab test. Returns the entry/exit distances, or null when the ray misses. */
export function rayAabb(r: Ray, min: Vec3, max: Vec3, maxDist = Infinity): { tmin: number; tmax: number } | null {
  let tmin = 0;
  let tmax = maxDist;
  for (let i = 0; i < 3; i++) {
    const d = r.dir[i];
    const o = r.origin[i];
    if (Math.abs(d) < 1e-12) {
      if (o < min[i] || o > max[i]) return null;
      continue;
    }
    const inv = 1 / d;
    let t1 = (min[i] - o) * inv;
    let t2 = (max[i] - o) * inv;
    if (t1 > t2) {
      const tmp = t1;
      t1 = t2;
      t2 = tmp;
    }
    if (t1 > tmin) tmin = t1;
    if (t2 < tmax) tmax = t2;
    if (tmin > tmax) return null;
  }
  return { tmin, tmax };
}

/** Ray/plane intersection. `null` when the ray is parallel to the plane. */
export function rayPlane(r: Ray, planePoint: Vec3, planeNormal: Vec3): number | null {
  const denom = dot(r.dir, planeNormal);
  if (Math.abs(denom) < 1e-12) return null;
  return dot(sub(planePoint, r.origin), planeNormal) / denom;
}

/**
 * The plane through three points, as a unit normal and a plane distance. Three
 * collinear points span no plane, so +Z is returned for them.
 */
export function planeFromThree(a: Vec3, b: Vec3, c: Vec3): [Vec3, number] {
  const u: Vec3 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
  const v: Vec3 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
  const raw: Vec3 = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
  const len = Math.hypot(raw[0], raw[1], raw[2]);
  const n: Vec3 = len > 1e-9 ? [raw[0] / len, raw[1] / len, raw[2] / len] : [0, 0, 1];
  return [n, n[0] * a[0] + n[1] * a[1] + n[2] * a[2]];
}

/**
 * Parameter of the point of closest approach measured along `axisDir` from
 * `axisOrigin`; null when the ray is parallel to the axis or the axis is
 * degenerate. The matching world point is `axisOrigin + axisDir * t`.
 */
export function rayAxis(r: Ray, axisOrigin: Vec3, axisDir: Vec3): number | null {
  const w0 = sub(axisOrigin, r.origin);
  const a = dot(axisDir, axisDir);
  if (a < 1e-12) return null;
  const b = dot(axisDir, r.dir);
  const c = dot(r.dir, r.dir);
  const d = dot(axisDir, w0);
  const e = dot(r.dir, w0);
  const den = a * c - b * b;
  if (Math.abs(den) < 1e-9) return null;
  return (b * e - c * d) / den;
}

/** Distance from `p` to the infinite line through `axisOrigin` along `axisDir`. */
export function distanceToAxis(p: Vec3, axisOrigin: Vec3, axisDir: Vec3): number {
  const w = sub(p, axisOrigin);
  const proj = scale(axisDir, dot(w, axisDir));
  return length(sub(w, proj));
}

/** Ray/triangle (Moller-Trumbore). Backface culling when `cull` is set. */
export function rayTriangle(
  r: Ray,
  a: Vec3,
  b: Vec3,
  c: Vec3,
  cull = false,
): number | null {
  const e1 = sub(b, a);
  const e2 = sub(c, a);
  const p = cross(r.dir, e2);
  const det = dot(e1, p);
  if (cull ? det <= 1e-12 : Math.abs(det) < 1e-12) return null;
  const inv = 1 / det;
  const tv = sub(r.origin, a);
  const u = dot(tv, p) * inv;
  if (u < -1e-7 || u > 1 + 1e-7) return null;
  const q = cross(tv, e1);
  const v = dot(r.dir, q) * inv;
  if (v < -1e-7 || u + v > 1 + 1e-7) return null;
  const t = dot(e2, q) * inv;
  return t > 1e-6 ? t : null;
}

export function reflectDir(dir: Vec3, n: Vec3): Vec3 {
  const d = dot(dir, n);
  return normalize(sub(dir, scale(n, 2 * d)));
}
