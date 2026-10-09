import { describe, expect, it } from "vitest";
import { fromRotationTranslationScale, mat4 } from "../math/mat4";
import { LineBuilder, orientedBoxWireframe } from "./overlay";

describe("oriented selection box", () => {
  it("turns and moves with the object instead of widening to the axes", () => {
    const m = mat4();
    // A quarter turn about +Z, placed at (100, 0, 0).
    const h = Math.SQRT1_2;
    fromRotationTranslationScale(m, [0, 0, h, h], [100, 0, 0], [1, 1, 1]);
    const lines = new LineBuilder();
    orientedBoxWireframe({ min: [-10, -1, -1], max: [10, 1, 1] }, m, [1, 1, 1, 1], lines);
    const mesh = lines.build();
    expect(mesh.count).toBe(24);
    let xs = [Infinity, -Infinity];
    let ys = [Infinity, -Infinity];
    for (let i = 0; i < mesh.count; i++) {
      xs = [Math.min(xs[0], mesh.positions[i * 3]), Math.max(xs[1], mesh.positions[i * 3])];
      ys = [Math.min(ys[0], mesh.positions[i * 3 + 1]), Math.max(ys[1], mesh.positions[i * 3 + 1])];
    }
    // The long side now runs along Y, as the model does.
    expect(xs[0]).toBeCloseTo(99, 5);
    expect(xs[1]).toBeCloseTo(101, 5);
    expect(ys[0]).toBeCloseTo(-10, 5);
    expect(ys[1]).toBeCloseTo(10, 5);
  });
});
