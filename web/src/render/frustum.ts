import type { Vec3 } from "../core/api";
import type { Mat4 } from "../math/mat4";
import type { Aabb } from "../math/vec3";

/** Six planes as `nx*x + ny*y + nz*z + d >= 0` means inside. */
export type Plane = [number, number, number, number];

export interface Frustum {
  planes: Plane[];
}

/** Gribb/Hartmann plane extraction from a view-projection matrix. */
export function frustumFromViewProj(vp: Mat4): Frustum {
  const m = vp;
  const planes: Plane[] = [
    [m[3] + m[0], m[7] + m[4], m[11] + m[8], m[15] + m[12]],
    [m[3] - m[0], m[7] - m[4], m[11] - m[8], m[15] - m[12]],
    [m[3] + m[1], m[7] + m[5], m[11] + m[9], m[15] + m[13]],
    [m[3] - m[1], m[7] - m[5], m[11] - m[9], m[15] - m[13]],
    [m[2], m[6], m[10], m[14]],
    [m[3] - m[2], m[7] - m[6], m[11] - m[10], m[15] - m[14]],
  ];
  for (const p of planes) {
    const l = Math.hypot(p[0], p[1], p[2]);
    if (l > 1e-12) {
      p[0] /= l;
      p[1] /= l;
      p[2] /= l;
      p[3] /= l;
    }
  }
  return { planes };
}

export function aabbInFrustum(f: Frustum, b: Aabb): boolean {
  for (const [nx, ny, nz, d] of f.planes) {
    const px = nx >= 0 ? b.max[0] : b.min[0];
    const py = ny >= 0 ? b.max[1] : b.min[1];
    const pz = nz >= 0 ? b.max[2] : b.min[2];
    if (nx * px + ny * py + nz * pz + d < 0) return false;
  }
  return true;
}

export function sphereInFrustum(f: Frustum, center: Vec3, radius: number): boolean {
  for (const [nx, ny, nz, d] of f.planes) {
    if (nx * center[0] + ny * center[1] + nz * center[2] + d < -radius) return false;
  }
  return true;
}

/** 0 when a box is outside, 1 when it straddles a plane, 2 when fully inside. */
function boxClass(f: Frustum, min: readonly number[], max: readonly number[], margin: number): 0 | 1 | 2 {
  let inside = true;
  for (const [nx, ny, nz, d] of f.planes) {
    const pad = margin * (Math.abs(nx) + Math.abs(ny) + Math.abs(nz));
    const far = nx * (nx >= 0 ? max[0] : min[0]) + ny * (ny >= 0 ? max[1] : min[1]) + nz * (nz >= 0 ? max[2] : min[2]) + d;
    if (far < -pad) return 0;
    const near = nx * (nx >= 0 ? min[0] : max[0]) + ny * (ny >= 0 ? min[1] : max[1]) + nz * (nz >= 0 ? min[2] : max[2]) + d;
    if (near < -pad) inside = false;
  }
  return inside ? 2 : 1;
}

/**
 * The box indices a BVH holds that touch the frustum grown by `margin`. A node
 * wholly inside takes every box under it without testing them.
 */
export function cullBvh(
  f: Frustum,
  nodes: readonly { min: readonly number[]; max: readonly number[]; left: number; right: number; start: number; count: number }[],
  order: readonly number[],
  boxes: readonly Aabb[],
  margin: number,
): number[] {
  const out: number[] = [];
  if (nodes.length === 0) return out;
  const stack: [number, boolean][] = [[0, false]];
  while (stack.length > 0) {
    const [n, known] = stack.pop()!;
    const node = nodes[n];
    const cls = known ? 2 : boxClass(f, node.min, node.max, margin);
    if (cls === 0) continue;
    if (node.left < 0 && node.right < 0) {
      for (let i = node.start; i < node.start + node.count; i++) {
        const b = boxes[order[i]];
        if (cls === 2 || boxClass(f, b.min, b.max, margin) !== 0) out.push(order[i]);
      }
      continue;
    }
    if (node.left >= 0) stack.push([node.left, cls === 2]);
    if (node.right >= 0) stack.push([node.right, cls === 2]);
  }
  return out;
}
