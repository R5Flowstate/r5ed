import { describe, expect, it } from "vitest";
import type { DocObject } from "../doc/model";
import { outlineRows } from "./Outliner";

function obj(id: string, parent: string | null = null, name = id): DocObject {
  return {
    id,
    name,
    layer: "l",
    parent,
    transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    hidden: false,
    locked: false,
    kind: { type: "group" },
  };
}

describe("outline rows", () => {
  const objects = [obj("a"), obj("a1", "a"), obj("a2", "a"), obj("b"), obj("b1", "b", "lamp")];
  const kids = (id: string): DocObject[] => objects.filter((o) => o.parent === id);
  const roots = objects.filter((o) => o.parent === null);

  it("lists the tree depth first, children indented under their parent", () => {
    const rows = outlineRows(roots, kids, () => true, new Set());
    expect(rows.map((r) => `${r.object.id}:${r.depth}`)).toEqual(["a:0", "a1:1", "a2:1", "b:0", "b1:1"]);
    expect(rows[0].hasKids).toBe(true);
    expect(rows[1].hasKids).toBe(false);
  });

  it("hides a collapsed row's children and keeps a parent whose child matches", () => {
    expect(outlineRows(roots, kids, () => true, new Set(["a"])).map((r) => r.object.id)).toEqual(["a", "b", "b1"]);
    const lamp = (o: DocObject): boolean => o.name.includes("lamp");
    expect(outlineRows(roots, kids, lamp, new Set()).map((r) => r.object.id)).toEqual(["b", "b1"]);
  });

  it("flattens a map-sized list without recursion", () => {
    const many = Array.from({ length: 50000 }, (_, i) => obj(`o${i}`));
    expect(outlineRows(many, () => [], () => true, new Set())).toHaveLength(50000);
  });
});
