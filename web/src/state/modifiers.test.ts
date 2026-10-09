import { describe, expect, it } from "vitest";
import { MODIFIER_KINDS, modifierKind, parseVec, stackRequest, withField } from "./modifiers";

describe("modifier stack requests", () => {
  it("names each stack edit as the core op with an empty selection", () => {
    const parsed = (json: string) => JSON.parse(json) as { op: string; object: string; sel: { ids: number[] }; params: Record<string, unknown> };
    expect(parsed(stackRequest.add("m", { type: "subdivision", levels: 2 }))).toMatchObject({
      op: "modifier_add",
      object: "m",
      sel: { ids: [] },
      params: { modifier: { type: "subdivision", levels: 2 } },
    });
    expect(parsed(stackRequest.move("m", 2, 0)).params).toEqual({ index: 2, to: 0 });
    expect(parsed(stackRequest.toggle("m", 1)).op).toBe("modifier_toggle");
    expect(parsed(stackRequest.apply("m", 0)).op).toBe("modifier_apply");
    expect(parsed(stackRequest.remove("m", 3)).params).toEqual({ index: 3 });
    expect(parsed(stackRequest.set("m", 0, { type: "displace", strength: 2 })).params).toEqual({ index: 0, modifier: { type: "displace", strength: 2 } });
  });
});

describe("modifier fields", () => {
  it("sets a field without touching the rest", () => {
    const m = { type: "array" as const, count: 2, relative_offset: [1, 0, 0] };
    expect(withField(m, "count", 5)).toEqual({ type: "array", count: 5, relative_offset: [1, 0, 0] });
    expect(m.count).toBe(2);
  });

  it("drops a cleared optional field so the core default applies", () => {
    expect(withField({ type: "array", count: 2, fit_length: 512 }, "fit_length", null)).toEqual({ type: "array", count: 2 });
  });

  it("parses vectors with spaces or commas and refuses the wrong size or junk", () => {
    expect(parseVec("1 0 0", 3)).toEqual([1, 0, 0]);
    expect(parseVec(" 0.5, -2 ,3 ", 3)).toEqual([0.5, -2, 3]);
    expect(parseVec("1 2", 3)).toBeNull();
    expect(parseVec("1 x 3", 3)).toBeNull();
    expect(parseVec("", 2)).toBeNull();
  });
});

describe("modifier kinds", () => {
  it("makes every kind that needs no other object straight away", () => {
    for (const kind of MODIFIER_KINDS) {
      const made = kind.make(() => "other");
      expect(made?.type, kind.type).toBe(kind.type);
    }
  });

  it("refuses a curve or boolean when there is nothing to point at", () => {
    expect(modifierKind("curve")?.make(() => null)).toBeNull();
    expect(modifierKind("boolean")?.make(() => null)).toBeNull();
    expect(modifierKind("array")?.make(() => null)).not.toBeNull();
  });

  it("asks the boolean for a mesh or brush and the curve for a path", () => {
    const asked: string[][] = [];
    modifierKind("boolean")?.make((k) => (asked.push([...k]), "x"));
    modifierKind("curve")?.make((k) => (asked.push([...k]), "y"));
    expect(asked).toEqual([["mesh", "brush"], ["path"]]);
  });

  it("only offers enum values the core knows", () => {
    expect(modifierKind("subdivision")?.fields.find((f) => f.key === "kind")?.options).toEqual(["catmull_clark", "simple"]);
    expect(modifierKind("curve")?.fields.find((f) => f.key === "axis")?.options).toEqual(["x", "y", "z", "-x", "-y", "-z"]);
  });
});
