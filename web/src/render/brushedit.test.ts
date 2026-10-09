import { describe, expect, it } from "vitest";
import type { BrushFace } from "../doc/model";
import { brushEdges, brushVertices, faceAt, nearestOnScreen, segmentDistance } from "./brushedit";

function face(normal: [number, number, number], dist: number): BrushFace {
  return { normal, dist, material: "m", uv: { offset: [0, 0], scale: [1, 1], rotation: 0 } as never, lightmap_scale: 16 };
}

/** A 0..64 box as six outward planes. */
const BOX: BrushFace[] = [
  face([1, 0, 0], 64), face([-1, 0, 0], 0),
  face([0, 1, 0], 64), face([0, -1, 0], 0),
  face([0, 0, 1], 64), face([0, 0, -1], 0),
];

describe("brush editing geometry", () => {
  it("finds the eight corners and twelve edges of a box", () => {
    const v = brushVertices(BOX);
    expect(v).toHaveLength(8);
    expect(v).toContainEqual([64, 64, 64]);
    expect(v).toContainEqual([0, 0, 0]);
    expect(brushEdges(BOX, v)).toHaveLength(12);
  });

  it("names the face a surface point lies on", () => {
    expect(faceAt(BOX, [32, 32, 64])).toBe(4);
    expect(faceAt(BOX, [0, 10, 10])).toBe(1);
  });

  it("picks the nearest screen point within tolerance and measures to a segment", () => {
    expect(nearestOnScreen([{ x: 0, y: 0 }, { x: 10, y: 0 }, null], { x: 9, y: 1 }, 5)).toBe(1);
    expect(nearestOnScreen([{ x: 0, y: 0 }], { x: 50, y: 50 }, 5)).toBe(-1);
    expect(segmentDistance({ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 5, y: 3 })).toBeCloseTo(3);
  });
});
