import { describe, expect, it } from "vitest";
import { DEFAULT_BRUSH_HEIGHT, MIN_BRUSH_HEIGHT, brushHeight, centredBox, dragBox, normalAxis } from "./boxdrag";
import type { Vec3 } from "../core/api";

const Z: Vec3 = [0, 0, 1];
const Y: Vec3 = [0, 1, 0];
const X: Vec3 = [1, 0, 0];

describe("brush height", () => {
  it("rounds to a whole unit and never collapses to nothing", () => {
    expect(brushHeight(64)).toBe(64);
    expect(brushHeight(63.6)).toBe(64);
    expect(brushHeight(0)).toBe(MIN_BRUSH_HEIGHT);
    expect(brushHeight(-500)).toBe(MIN_BRUSH_HEIGHT);
    expect(brushHeight(Number.NaN)).toBe(DEFAULT_BRUSH_HEIGHT);
    expect(brushHeight(Number.POSITIVE_INFINITY, 96)).toBe(96);
    expect(brushHeight(1e9)).toBe(1_000_000);
  });

  it("defaults to the thickness the tool used before it was configurable", () => {
    expect(DEFAULT_BRUSH_HEIGHT).toBe(128);
  });
});

describe("drag box", () => {
  it("grows upward from the drag by the tool height", () => {
    const b = dragBox([0, 0, 0], [96, 64, 0], Z, 16, 512);
    expect(b.min).toEqual([0, 0, 0]);
    expect(b.max).toEqual([96, 64, 512]);
    expect(b.max[2] - b.min[2]).toBe(512);
  });

  it("is not pinned to 128 any more", () => {
    for (const height of [16, 32, 64, 256, 1024]) {
      const b = dragBox([0, 0, 0], [64, 64, 0], Z, 16, height);
      expect(b.max[2] - b.min[2], `height ${height}`).toBe(height);
    }
  });

  it("keeps a front or side drag centred on the view plane", () => {
    const front = dragBox([0, 0, 0], [96, 0, 64], Y, 16, 200);
    expect(front.min[1]).toBe(-100);
    expect(front.max[1]).toBe(100);
    const side = dragBox([0, 0, 0], [0, 0, 64], X, 16, 200);
    expect(side.min[0]).toBe(-100);
    expect(side.max[0]).toBe(100);
  });

  it("snaps the cursor points to the grid and orders the corners", () => {
    const b = dragBox([17, -17, 0], [200, 130, 0], Z, 16, 64);
    expect(b.min).toEqual([16, -16, 0]);
    expect(b.max).toEqual([208, 128, 64]);
  });

  it("never comes out thinner than the grid", () => {
    const b = dragBox([0, 0, 0], [4, 4, 0], Z, 64, 8);
    expect(b.max[0] - b.min[0]).toBe(64);
    expect(b.max[1] - b.min[1]).toBe(64);
    expect(b.max[2] - b.min[2]).toBe(64);
  });

  it("falls back to a usable height when the field is empty", () => {
    const b = dragBox([0, 0, 0], [64, 64, 0], Z, 16, brushHeight(Number.NaN, DEFAULT_BRUSH_HEIGHT));
    expect(b.max[2] - b.min[2]).toBe(DEFAULT_BRUSH_HEIGHT);
  });

  it("names the axis a plane normal points along", () => {
    expect(normalAxis(X)).toBe(0);
    expect(normalAxis(Y)).toBe(1);
    expect(normalAxis(Z)).toBe(2);
  });
});

describe("centred box", () => {
  it("spans the requested size around the point", () => {
    expect(centredBox([0, 0, 0], [64, 32, 128])).toEqual({ min: [-32, -16, -64], max: [32, 16, 64] });
  });
});
