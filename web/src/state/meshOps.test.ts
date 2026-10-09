import { describe, expect, it } from "vitest";
import { EDIT_PRIMITIVES, MESH_OPS, type OpSpec, clampField, modalParams, specForRequest } from "./meshOps";

const specs = Object.entries(MESH_OPS) as [string, OpSpec][];

describe("mesh op catalog", () => {
  it("gives every field a default, so the redo panel shows it from the first run", () => {
    for (const [key, spec] of specs) {
      for (const f of spec.fields) expect(spec.params, `${key}.${f.key}`).toHaveProperty(f.key);
    }
  });

  it("only offers enum defaults among their options", () => {
    for (const [key, spec] of specs) {
      for (const f of spec.fields.filter((x) => x.kind === "enum")) {
        expect(f.options, `${key}.${f.key}`).toContain(spec.params[f.key]);
      }
    }
  });

  it("drives modal params that are real fields", () => {
    for (const [key, spec] of specs) {
      const m = spec.modal;
      if (!m) continue;
      if (m.drag) expect(spec.fields.map((f) => f.key), `${key} drag`).toContain(m.drag);
      if (m.wheel) expect(spec.fields.find((f) => f.key === m.wheel)?.kind, `${key} wheel`).toBe("int");
    }
  });

  it("tells shared ops apart by their pinned params", () => {
    expect(specForRequest("bevel", { affect: "vertices", amount: 2 })?.label).toBe("Bevel vertices");
    expect(specForRequest("bevel", { affect: "edges" })?.label).toBe("Bevel edges");
    expect(specForRequest("dissolve", { mode: "limited", angle: 3 })?.label).toBe("Limited dissolve");
    expect(specForRequest("boolean", { operation: "union", operand: "unselected" })?.label).toBe("Boolean: union");
    expect(specForRequest("inset", {})?.op).toBe("inset");
    expect(specForRequest("nope", {})).toBeNull();
  });

  it("offers each add-in-edit shape once", () => {
    const shapes = EDIT_PRIMITIVES.map((p) => p.spec.shape);
    expect(new Set(shapes).size).toBe(shapes.length);
  });
});

describe("modal parameter mapping", () => {
  const bevel = MESH_OPS.bevel_edges as OpSpec;
  const loop = MESH_OPS.loop_cut as OpSpec;

  it("turns pointer travel into a bevel amount in world units", () => {
    const p = modalParams(bevel, bevel.params, 100, 800, 0.5, 0);
    expect(p.amount).toBe(50);
    expect(p.segments).toBe(1);
  });

  it("ignores the travel direction for a distance and never goes negative", () => {
    expect(modalParams(bevel, bevel.params, -40, 800, 1, 0).amount).toBe(40);
  });

  it("steps segments with the wheel and keeps them inside 1..64", () => {
    expect(modalParams(bevel, bevel.params, 0, 800, 1, 3).segments).toBe(4);
    expect(modalParams(bevel, bevel.params, 0, 800, 1, -10).segments).toBe(1);
    expect(modalParams(bevel, { ...bevel.params, segments: 60 }, 0, 800, 1, 10).segments).toBe(64);
  });

  it("maps a loop cut slide across the view to a -1..1 factor", () => {
    expect(modalParams(loop, loop.params, 200, 800, 1, 0).factor).toBe(0.5);
    expect(modalParams(loop, loop.params, -2000, 800, 1, 0).factor).toBe(-1);
    expect(modalParams(loop, loop.params, 0, 800, 1, 2).cuts).toBe(3);
  });

  it("leaves params of a non-modal op untouched", () => {
    const sub = MESH_OPS.subdivide as OpSpec;
    expect(modalParams(sub, sub.params, 300, 800, 1, 5)).toEqual(sub.params);
  });

  it("rounds and bounds integer fields", () => {
    expect(clampField({ key: "n", kind: "int", min: 1, max: 8 }, 3.6)).toBe(4);
    expect(clampField({ key: "n", kind: "int", min: 1, max: 8 }, 99)).toBe(8);
    expect(clampField({ key: "x", kind: "number", min: 0 }, -2)).toBe(0);
  });
});
