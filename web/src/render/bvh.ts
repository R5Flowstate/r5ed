import { type Aabb, type Vec3, emptyAabb, isEmptyAabb, unionAabb } from "../math/vec3";

/** Axis-aligned bounding volume hierarchy over object bounds. */
export interface BvhNode {
  min: [number, number, number];
  max: [number, number, number];
  /** Index of the first child, or -1 for a leaf. */
  left: number;
  right: number;
  /** First item index and count for leaves. */
  start: number;
  count: number;
}

export interface Bvh {
  nodes: BvhNode[];
  /** Item indices reordered so each leaf's range is contiguous. */
  order: number[];
  /** Item boxes, copied in so a query can filter leaves without extra input. */
  boxes: Aabb[];
}

const LEAF_SIZE = 4;

function centroidX(b: Aabb): number {
  return (b.min[0] + b.max[0]) * 0.5;
}
function centroidY(b: Aabb): number {
  return (b.min[1] + b.max[1]) * 0.5;
}
function centroidZ(b: Aabb): number {
  return (b.min[2] + b.max[2]) * 0.5;
}

const AXIS_GETTERS = [centroidX, centroidY, centroidZ] as const;

/** Builds a BVH over the given boxes. Empty input yields an empty tree. */
export function buildBvh(boxes: readonly Aabb[]): Bvh {
  const order = boxes.map((_, i) => i);
  const bvh: Bvh = { nodes: [], order, boxes: boxes.map((b) => ({ min: [...b.min] as Vec3, max: [...b.max] as Vec3 })) };
  if (boxes.length === 0) return bvh;
  buildRange(bvh, boxes, order, 0, boxes.length);
  return bvh;
}

function buildRange(bvh: Bvh, boxes: readonly Aabb[], order: number[], start: number, end: number): number {
  const index = bvh.nodes.length;
  bvh.nodes.push({ min: [0, 0, 0], max: [0, 0, 0], left: -1, right: -1, start, count: end - start });
  let bounds = emptyAabb();
  for (let i = start; i < end; i++) bounds = unionAabb(bounds, boxes[order[i]]);
  if (isEmptyAabb(bounds)) {
    bvh.nodes[index].min = [0, 0, 0];
    bvh.nodes[index].max = [0, 0, 0];
    return index;
  }
  bvh.nodes[index].min = bounds.min.slice() as [number, number, number];
  bvh.nodes[index].max = bounds.max.slice() as [number, number, number];
  const count = end - start;
  if (count <= LEAF_SIZE) return index;
  // Split on the widest centroid extent, falling back to the median on ties.
  let axis = 0;
  let best = -1;
  for (let a = 0; a < 3; a++) {
    let lo = Infinity;
    let hi = -Infinity;
    const get = AXIS_GETTERS[a];
    for (let i = start; i < end; i++) {
      const c = get(boxes[order[i]]);
      if (c < lo) lo = c;
      if (c > hi) hi = c;
    }
    if (hi - lo > best) {
      best = hi - lo;
      axis = a;
    }
  }
  if (!(best > 1e-9)) return index;
  const get = AXIS_GETTERS[axis];
  const slice = order.slice(start, end);
  slice.sort((p, q) => get(boxes[p]) - get(boxes[q]));
  for (let i = 0; i < slice.length; i++) order[start + i] = slice[i];
  const mid = start + (count >> 1);
  const left = buildRange(bvh, boxes, order, start, mid);
  const right = buildRange(bvh, boxes, order, mid, end);
  bvh.nodes[index].left = left;
  bvh.nodes[index].right = right;
  bvh.nodes[index].count = 0;
  return index;
}

function overlaps(box: { min: readonly number[]; max: readonly number[] }, min: Aabb["min"], max: Aabb["max"]): boolean {
  return (
    box.min[0] <= max[0] && box.max[0] >= min[0] &&
    box.min[1] <= max[1] && box.max[1] >= min[1] &&
    box.min[2] <= max[2] && box.max[2] >= min[2]
  );
}

/**
 * Every item index whose box overlaps the query volume, ascending. A leaf only
 * narrows the candidates, so each item is still tested against the volume.
 */
export function queryBvh(bvh: Bvh, min: Aabb["min"], max: Aabb["max"]): number[] {
  const out: number[] = [];
  if (bvh.nodes.length === 0) return out;
  const stack: number[] = [0];
  const seen = new Uint8Array(bvh.nodes.length);
  while (stack.length) {
    const ni = stack.pop() as number;
    if (seen[ni]) continue;
    seen[ni] = 1;
    const node = bvh.nodes[ni];
    if (!overlaps(node, min, max)) continue;
    if (node.left < 0) {
      for (let i = node.start; i < node.start + node.count; i++) {
        const index = bvh.order[i];
        const box = bvh.boxes[index];
        if (box && overlaps(box, min, max)) out.push(index);
      }
      continue;
    }
    stack.push(node.left, node.right);
  }
  out.sort((a, b) => a - b);
  return out;
}

/** Brute-force reference used to validate the tree. */
export function queryBruteForce(boxes: readonly Aabb[], min: Aabb["min"], max: Aabb["max"]): number[] {
  const out: number[] = [];
  for (let i = 0; i < boxes.length; i++) {
    const b = boxes[i];
    if (
      b.min[0] <= max[0] && b.max[0] >= min[0] &&
      b.min[1] <= max[1] && b.max[1] >= min[1] &&
      b.min[2] <= max[2] && b.max[2] >= min[2]
    ) {
      out.push(i);
    }
  }
  return out;
}

/**
 * The same tree over boxes that moved but kept their order: node bounds are
 * recomputed bottom-up without re-splitting. Children always follow their
 * parent in `nodes`, so one reverse pass sees every child before its parent.
 */
export function refitBvh(bvh: Bvh, boxes: readonly Aabb[]): Bvh {
  const out: Bvh = {
    nodes: bvh.nodes.map((n) => ({ ...n, min: [...n.min] as Vec3, max: [...n.max] as Vec3 })),
    order: bvh.order,
    boxes: boxes.map((b) => ({ min: [...b.min] as Vec3, max: [...b.max] as Vec3 })),
  };
  for (let i = out.nodes.length - 1; i >= 0; i--) {
    const n = out.nodes[i];
    let acc = emptyAabb();
    if (n.left < 0 && n.right < 0) {
      for (let k = n.start; k < n.start + n.count; k++) acc = unionAabb(acc, out.boxes[out.order[k]]);
    } else {
      if (n.left >= 0) acc = unionAabb(acc, out.nodes[n.left]);
      if (n.right >= 0) acc = unionAabb(acc, out.nodes[n.right]);
    }
    if (isEmptyAabb(acc)) continue;
    n.min = [acc.min[0], acc.min[1], acc.min[2]];
    n.max = [acc.max[0], acc.max[1], acc.max[2]];
  }
  return out;
}
