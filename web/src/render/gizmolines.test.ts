import { describe, expect, it } from "vitest";
import { LineBuilder } from "./overlay";
import { QUAT_IDENTITY, quatFromAxisAngle } from "../math/quat";
import { AXIS_COLORS, gizmoLines, handleAxis } from "./gizmo";
import type { GizmoHandle, GizmoMode } from "./gizmo";

function drawn(mode: GizmoMode, space: "world" | "local" = "world", active: GizmoHandle | null = null) {
  const into = new LineBuilder();
  gizmoLines(into, [0, 0, 0], QUAT_IDENTITY, mode, space, 100, active);
  return into.build();
}

describe("gizmo geometry", () => {
  it("draws something for every mode and never an odd vertex count", () => {
    for (const mode of ["translate", "rotate", "scale"] as GizmoMode[]) {
      const mesh = drawn(mode);
      expect(mesh.count).toBeGreaterThan(0);
      expect(mesh.count % 2).toBe(0);
      expect(mesh.positions.length).toBe(mesh.count * 3);
      expect(mesh.colors.length).toBe(mesh.count * 4);
    }
  });

  it("puts a translate axis shaft along each world axis", () => {
    const mesh = drawn("translate");
    const first = [mesh.positions[0], mesh.positions[1], mesh.positions[2]];
    const second = [mesh.positions[3], mesh.positions[4], mesh.positions[5]];
    expect(first).toEqual([0, 0, 0]);
    // The first axis handle is X, so its tip is a hundred units along it.
    expect(Math.abs(second[0] - 100)).toBeLessThan(1e-6);
    expect(Math.abs(second[1])).toBeLessThan(1e-6);
    expect(Math.abs(second[2])).toBeLessThan(1e-6);
  });

  it("gives each axis its own colour and keeps every channel in range", () => {
    for (const [axis, color] of Object.entries(AXIS_COLORS)) {
      expect(color.length, axis).toBe(3);
      for (const c of color) {
        expect(c).toBeGreaterThan(0);
        expect(c).toBeLessThanOrEqual(1);
      }
    }
    const mesh = drawn("translate");
    for (let i = 0; i < mesh.colors.length; i += 4) {
      expect(mesh.colors[i + 3]).toBeGreaterThan(0);
      expect(mesh.colors[i + 3]).toBeLessThanOrEqual(1);
    }
  });

  it("rotates the handles with the object in local space and not in world space", () => {
    const spin = quatFromAxisAngle([0, 0, 1], Math.PI / 2);
    const world = new LineBuilder();
    gizmoLines(world, [0, 0, 0], spin, "translate", "world", 100, null);
    const local = new LineBuilder();
    gizmoLines(local, [0, 0, 0], spin, "translate", "local", 100, null);
    expect([...world.build().positions]).not.toEqual([...local.build().positions]);
    const turned = handleAxis("x", spin, "local");
    expect(Math.abs(turned[0])).toBeLessThan(1e-6);
    expect(Math.abs(turned[1] - 1)).toBeLessThan(1e-6);
  });

  it("draws a circle per axis when rotating and none when translating", () => {
    const rotate = drawn("rotate");
    const translate = drawn("translate");
    expect(rotate.count).toBeGreaterThan(translate.count);
    // A 48-segment circle per axis is 48 segments of two vertices.
    expect(rotate.count).toBe(3 * 48 * 2);
  });

  it("moves the pivot to the object", () => {
    const into = new LineBuilder();
    gizmoLines(into, [10, 20, 30], QUAT_IDENTITY, "translate", "world", 50, null);
    const mesh = into.build();
    expect([mesh.positions[0], mesh.positions[1], mesh.positions[2]]).toEqual([10, 20, 30]);
  });

  it("highlights the handle under the pointer", () => {
    const plain = drawn("translate");
    const hot = drawn("translate", "world", "x");
    expect(hot.count).toBe(plain.count);
    // The first segment of the hot gizmo is white rather than the X red.
    expect(hot.colors[0]).toBeCloseTo(1, 5);
    expect(hot.colors[1]).toBeCloseTo(1, 5);
    expect(plain.colors[0]).toBeCloseTo(AXIS_COLORS.x[0], 5);
    expect(plain.colors[1]).toBeCloseTo(AXIS_COLORS.x[1], 5);
  });

  it("scales every handle with the size it is given", () => {
    const small = drawn("translate");
    const into = new LineBuilder();
    gizmoLines(into, [0, 0, 0], QUAT_IDENTITY, "translate", "world", 10, null);
    const tiny = into.build();
    const reach = (mesh: { positions: Float32Array }) => Math.max(...Array.from(mesh.positions).map(Math.abs));
    expect(reach(small)).toBeGreaterThan(reach(tiny));
  });

  it("ignores a size of zero rather than collapsing", () => {
    const into = new LineBuilder();
    gizmoLines(into, [0, 0, 0], QUAT_IDENTITY, "translate", "world", 0, null);
    expect(into.build().count).toBeGreaterThan(0);
  });
});
