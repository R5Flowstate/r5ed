import { describe, expect, it } from "vitest";
import { buildBvh, queryBvh, queryBruteForce } from "./bvh";
import type { Aabb } from "../math/vec3";

/** Deterministic pseudo-random boxes, so a failure is reproducible. */
function randomBoxes(count: number, seed = 1): Aabb[] {
  let s = seed >>> 0;
  const next = (): number => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 4294967296;
  };
  const out: Aabb[] = [];
  for (let i = 0; i < count; i++) {
    const x = next() * 4000 - 2000;
    const y = next() * 4000 - 2000;
    const z = next() * 4000 - 2000;
    const w = 1 + next() * 120;
    const h = 1 + next() * 120;
    const d = 1 + next() * 120;
    out.push({ min: [x, y, z], max: [x + w, y + h, z + d] });
  }
  return out;
}

function overlaps(a: Aabb, b: Aabb): boolean {
  return (
    a.min[0] <= b.max[0] && a.max[0] >= b.min[0] &&
    a.min[1] <= b.max[1] && a.max[1] >= b.min[1] &&
    a.min[2] <= b.max[2] && a.max[2] >= b.min[2]
  );
}

describe("bvh construction", () => {
  it("makes a leaf-only tree for a handful of boxes", () => {
    const boxes = randomBoxes(3);
    const bvh = buildBvh(boxes);
    expect(bvh.nodes).toHaveLength(1);
    expect(bvh.nodes[0].left).toBe(-1);
    expect(bvh.nodes[0].count).toBe(3);
    expect(bvh.order).toEqual([0, 1, 2]);
  });

  it("splits a larger set and keeps every leaf index once", () => {
    const boxes = randomBoxes(64);
    const bvh = buildBvh(boxes);
    expect(bvh.nodes.length).toBeGreaterThan(1);
    const seen = new Set<number>();
    for (const node of bvh.nodes) {
      if (node.left < 0) {
        expect(node.count).toBeGreaterThan(0);
        for (let i = node.start; i < node.start + node.count; i++) {
          expect(seen.has(bvh.order[i])).toBe(false);
          seen.add(bvh.order[i]);
        }
      } else {
        expect(node.right).toBeGreaterThanOrEqual(0);
        expect(node.count).toBe(0);
      }
    }
    expect(seen.size).toBe(64);
  });

  it("keeps each node's box around the boxes below it", () => {
    const boxes = randomBoxes(200);
    const bvh = buildBvh(boxes);
    const range = (index: number): Aabb => {
      const node = bvh.nodes[index];
      const acc: Aabb = { min: [...node.min] as [number, number, number], max: [...node.max] as [number, number, number] };
      if (node.left < 0) {
        for (let i = node.start; i < node.start + node.count; i++) {
          const b = boxes[bvh.order[i]];
          for (let c = 0; c < 3; c++) {
            acc.min[c] = Math.min(acc.min[c], b.min[c]);
            acc.max[c] = Math.max(acc.max[c], b.max[c]);
          }
        }
        return acc;
      }
      const a = range(node.left);
      const b = range(node.right);
      for (let c = 0; c < 3; c++) {
        acc.min[c] = Math.min(a.min[c], b.min[c]);
        acc.max[c] = Math.max(a.max[c], b.max[c]);
      }
      return acc;
    };
    const walk = (index: number): void => {
      const node = bvh.nodes[index];
      const actual = range(index);
      for (let c = 0; c < 3; c++) {
        expect(node.min[c]).toBeCloseTo(actual.min[c], 4);
        expect(node.max[c]).toBeCloseTo(actual.max[c], 4);
      }
      if (node.left >= 0) {
        walk(node.left);
        walk(node.right);
      }
    };
    walk(0);
  });

  it("returns an empty tree for no input", () => {
    const bvh = buildBvh([]);
    expect(bvh.nodes).toHaveLength(0);
    expect(bvh.order).toHaveLength(0);
    expect(queryBvh(bvh, [-1, -1, -1], [1, 1, 1])).toEqual([]);
  });

  it("does not split when every box sits at the same spot", () => {
    const boxes: Aabb[] = Array.from({ length: 32 }, () => ({ min: [0, 0, 0], max: [1, 1, 1] }));
    const bvh = buildBvh(boxes);
    expect(bvh.nodes).toHaveLength(1);
    expect(queryBvh(bvh, [-1, -1, -1], [2, 2, 2])).toHaveLength(32);
  });
});

describe("bvh queries match brute force", () => {
  const cases = [0, 1, 2, 3, 5, 17, 64, 250];
  for (const count of cases) {
    it(`agrees on ${count} boxes across many query volumes`, () => {
      const boxes = randomBoxes(count, count + 7);
      const bvh = buildBvh(boxes);
      let s = count * 31 + 11;
      const next = (): number => {
        s = (s * 1103515245 + 12345) >>> 0;
        return s / 4294967296;
      };
      for (let q = 0; q < 40; q++) {
        const min: [number, number, number] = [next() * 4200 - 2100, next() * 4200 - 2100, next() * 4200 - 2100];
        const max: [number, number, number] = [min[0] + next() * 900, min[1] + next() * 900, min[2] + next() * 900];
        const want = queryBruteForce(boxes, min, max);
        const got = queryBvh(bvh, min, max);
        expect(got).toEqual(want);
        for (const i of got) expect(overlaps(boxes[i], { min, max })).toBe(true);
      }
    });
  }

  it("finds a box the query volume only touches on one corner", () => {
    const boxes: Aabb[] = [{ min: [0, 0, 0], max: [10, 10, 10] }];
    const bvh = buildBvh(boxes);
    expect(queryBvh(bvh, [10, 10, 10], [20, 20, 20])).toEqual([0]);
    expect(queryBvh(bvh, [10.001, 0, 0], [20, 20, 20])).toEqual([]);
  });

  it("handles a query larger than everything", () => {
    const boxes = randomBoxes(50, 3);
    const bvh = buildBvh(boxes);
    expect(queryBvh(bvh, [-1e6, -1e6, -1e6], [1e6, 1e6, 1e6])).toHaveLength(50);
  });
});
