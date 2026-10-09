import type { Id, Vec3 } from "../core/api";
import { invert, mat4, type Mat4 } from "../math/mat4";
import type { Aabb } from "../math/vec3";
import { type Scene, VERTEX_FLOATS } from "./scene";

export interface ScenePick {
  id: Id;
  position: Vec3;
  normal: Vec3;
  distance: number;
}

/** Entry distance of a ray into a box, or Infinity when it misses. */
function rayBox(o: Vec3, inv: Vec3, b: { min: readonly number[]; max: readonly number[] }, limit: number): number {
  let lo = 0;
  let hi = limit;
  for (let a = 0; a < 3; a++) {
    let t0 = (b.min[a] - o[a]) * inv[a];
    let t1 = (b.max[a] - o[a]) * inv[a];
    if (t0 > t1) [t0, t1] = [t1, t0];
    if (t0 > lo) lo = t0;
    if (t1 < hi) hi = t1;
    if (lo > hi) return Infinity;
  }
  return lo;
}

function transformPoint3(m: Mat4, p: Vec3): Vec3 {
  return [m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12], m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13], m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14]];
}

function transformVector3(m: Mat4, v: Vec3): Vec3 {
  return [m[0] * v[0] + m[4] * v[1] + m[8] * v[2], m[1] * v[0] + m[5] * v[1] + m[9] * v[2], m[2] * v[0] + m[6] * v[1] + m[10] * v[2]];
}

/**
 * The nearest surface a ray hits among the scene's drawn instances. The BVH
 * narrows the candidates to boxes the ray enters, nearest first, and only
 * those instances' triangles are tested, in their own space.
 */
export function pickScene(scene: Scene, origin: Vec3, dir: Vec3, maxDist = 1 << 22, accept?: (id: Id) => boolean): ScenePick | null {
  const hits = pickSceneAll(scene, origin, dir, maxDist, accept);
  return hits.length > 0 ? hits[0] : null;
}

/**
 * Every surface a ray hits, nearest first: the per-instance nearest hit of
 * each instance the ray enters. X-ray object pick resolves the farthest of
 * these, so an occluded object wins instead of the front one.
 */
export function pickSceneAll(scene: Scene, origin: Vec3, dir: Vec3, maxDist = 1 << 22, accept?: (id: Id) => boolean): ScenePick[] {
  const len = Math.hypot(dir[0], dir[1], dir[2]);
  if (!(len > 0)) return [];
  const d: Vec3 = [dir[0] / len, dir[1] / len, dir[2] / len];
  const inv: Vec3 = [1 / d[0], 1 / d[1], 1 / d[2]];
  const { nodes, order, boxes } = scene.bvh;
  if (nodes.length === 0 || order.length !== scene.instances.length) return [];

  const candidates: [number, number][] = [];
  const stack = [0];
  while (stack.length > 0) {
    const node = nodes[stack.pop()!];
    if (rayBox(origin, inv, node, maxDist) === Infinity) continue;
    if (node.left < 0 && node.right < 0) {
      for (let i = node.start; i < node.start + node.count; i++) {
        const t = rayBox(origin, inv, boxes[order[i]] as Aabb, maxDist);
        if (t !== Infinity) candidates.push([t, order[i]]);
      }
      continue;
    }
    if (node.left >= 0) stack.push(node.left);
    if (node.right >= 0) stack.push(node.right);
  }
  candidates.sort((a, b) => a[0] - b[0]);

  const hits: ScenePick[] = [];
  const local = mat4();
  for (const [entry, index] of candidates) {
    if (entry > maxDist) break;
    const instance = scene.instances[index];
    if (accept && !accept(instance.id)) continue;
    const chunk = scene.chunks[instance.chunk];
    if (!chunk || !invert(local, instance.transform)) continue;
    const lo = transformPoint3(local, origin);
    const ld = transformVector3(local, d);
    const v = chunk.vertices;
    const ix = chunk.indices;
    let hitT = Infinity;
    let hitTri = -1;
    for (let t = 0; t + 2 < ix.length; t += 3) {
      const a = ix[t] * VERTEX_FLOATS;
      const b = ix[t + 1] * VERTEX_FLOATS;
      const c = ix[t + 2] * VERTEX_FLOATS;
      const e1x = v[b] - v[a], e1y = v[b + 1] - v[a + 1], e1z = v[b + 2] - v[a + 2];
      const e2x = v[c] - v[a], e2y = v[c + 1] - v[a + 1], e2z = v[c + 2] - v[a + 2];
      const px = ld[1] * e2z - ld[2] * e2y, py = ld[2] * e2x - ld[0] * e2z, pz = ld[0] * e2y - ld[1] * e2x;
      const det = e1x * px + e1y * py + e1z * pz;
      if (Math.abs(det) < 1e-12) continue;
      const f = 1 / det;
      const sx = lo[0] - v[a], sy = lo[1] - v[a + 1], sz = lo[2] - v[a + 2];
      const u = f * (sx * px + sy * py + sz * pz);
      if (u < 0 || u > 1) continue;
      const qx = sy * e1z - sz * e1y, qy = sz * e1x - sx * e1z, qz = sx * e1y - sy * e1x;
      const w = f * (ld[0] * qx + ld[1] * qy + ld[2] * qz);
      if (w < 0 || u + w > 1) continue;
      const tt = f * (e2x * qx + e2y * qy + e2z * qz);
      if (tt > 1e-6 && tt < hitT) {
        hitT = tt;
        hitTri = t;
      }
    }
    if (hitTri < 0) continue;
    const localHit: Vec3 = [lo[0] + ld[0] * hitT, lo[1] + ld[1] * hitT, lo[2] + ld[2] * hitT];
    const world = transformPoint3(instance.transform, localHit);
    const distance = Math.hypot(world[0] - origin[0], world[1] - origin[1], world[2] - origin[2]);
    if (distance > maxDist) continue;
    const a = ix[hitTri] * VERTEX_FLOATS, b = ix[hitTri + 1] * VERTEX_FLOATS, c = ix[hitTri + 2] * VERTEX_FLOATS;
    const e1: Vec3 = [v[b] - v[a], v[b + 1] - v[a + 1], v[b + 2] - v[a + 2]];
    const e2: Vec3 = [v[c] - v[a], v[c + 1] - v[a + 1], v[c + 2] - v[a + 2]];
    let n = transformVector3(instance.transform, [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]]);
    const nl = Math.hypot(n[0], n[1], n[2]) || 1;
    n = [n[0] / nl, n[1] / nl, n[2] / nl];
    if (n[0] * d[0] + n[1] * d[1] + n[2] * d[2] > 0) n = [-n[0], -n[1], -n[2]];
    hits.push({ id: instance.id, position: world, normal: n, distance });
  }
  hits.sort((a, b) => a.distance - b.distance);
  return hits;
}

/**
 * The x-ray object pick: the farthest of the ray's hits, so the occluded
 * object wins. This is what the GPU pick's farthest-wins pass agrees with.
 */
export function pickFarthest(hits: readonly ScenePick[]): ScenePick | null {
  return hits.length > 0 ? hits[hits.length - 1] : null;
}
