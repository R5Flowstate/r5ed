import { describe, expect, it } from "vitest";
import { lookAt, mat4, multiply, perspective } from "../math/mat4";
import type { Aabb } from "../math/vec3";
import { buildBvh } from "./bvh";
import { aabbInFrustum, cullBvh, frustumFromViewProj } from "./frustum";

function grow(b: Aabb, m: number): Aabb {
  return { min: [b.min[0] - m, b.min[1] - m, b.min[2] - m], max: [b.max[0] + m, b.max[1] + m, b.max[2] + m] };
}

describe("cullBvh", () => {
  it("keeps exactly the boxes a linear frustum test keeps", () => {
    let seed = 7;
    const rand = (): number => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed / 0x7fffffff;
    };
    const boxes: Aabb[] = [];
    for (let i = 0; i < 3000; i++) {
      const c = [rand() * 40000 - 20000, rand() * 40000 - 20000, rand() * 4000 - 2000];
      const h = 8 + rand() * 400;
      boxes.push({ min: [c[0] - h, c[1] - h, c[2] - h], max: [c[0] + h, c[1] + h, c[2] + h] });
    }
    const bvh = buildBvh(boxes);
    for (const [eye, at] of [
      [[0, 0, 500], [1000, 300, 0]],
      [[-15000, 9000, 3000], [0, 0, 0]],
      [[5000, -5000, 200], [5000, 5000, 200]],
    ] as [[number, number, number], [number, number, number]][]) {
      const view = lookAt(mat4(), eye, at, [0, 0, 1]);
      const proj = perspective(mat4(), Math.PI / 3, 16 / 9, 4, 1 << 22);
      const f = frustumFromViewProj(multiply(mat4(), proj, view));
      const want = boxes.map((b, i) => (aabbInFrustum(f, grow(b, 4)) ? i : -1)).filter((i) => i >= 0);
      const got = cullBvh(f, bvh.nodes, bvh.order, bvh.boxes, 4).sort((a, b) => a - b);
      expect(got).toEqual(want);
      expect(got.length).toBeLessThan(boxes.length);
    }
  });
});

describe("refitBvh", () => {
  it("culls a moved box set exactly as a fresh tree does", async () => {
    const { refitBvh } = await import("./bvh");
    const boxes: Aabb[] = [];
    for (let i = 0; i < 500; i++) {
      const c = [(i % 25) * 800 - 10000, Math.floor(i / 25) * 800 - 8000, 0];
      boxes.push({ min: [c[0] - 50, c[1] - 50, -50], max: [c[0] + 50, c[1] + 50, 50] });
    }
    const tree = buildBvh(boxes);
    const moved = boxes.map((b, i) => (i % 7 === 0 ? grow({ min: [b.min[0] + 3000, b.min[1], b.min[2]], max: [b.max[0] + 3000, b.max[1], b.max[2]] }, 0) : b));
    const refit = refitBvh(tree, moved);
    const view = lookAt(mat4(), [0, -12000, 3000], [0, 0, 0], [0, 0, 1]);
    const f = frustumFromViewProj(multiply(mat4(), perspective(mat4(), Math.PI / 4, 1.6, 4, 1 << 20), view));
    const want = moved.map((b, i) => (aabbInFrustum(f, grow(b, 4)) ? i : -1)).filter((i) => i >= 0);
    expect(cullBvh(f, refit.nodes, refit.order, refit.boxes, 4).sort((a, b) => a - b)).toEqual(want);
  });
});
