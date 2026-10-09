import { describe, expect, it } from "vitest";
import {
  MESH_SHAPES,
  defaultValues,
  meshRequestJson,
  parseSpec,
  specErrorOf,
  specTextValues,
  type MeshShapeId,
  type MeshSpec,
} from "./meshgen";

const IDS = MESH_SHAPES.map((s) => s.id);

describe("mesh shape catalogue", () => {
  it("covers every shape the core can build", () => {
    expect(IDS).toEqual([
      "box",
      "cylinder",
      "cone",
      "sphere",
      "torus",
      "plane",
      "stairs",
      "ramp",
      "arch",
      "extrude",
      "lathe",
      "loft",
      "raw",
    ]);
    expect(IDS).toHaveLength(13);
  });

  it("describes each shape with the field names the core expects", () => {
    const fields: Record<MeshShapeId, string[]> = {
      box: ["size"],
      cylinder: ["radius", "height", "segments", "caps"],
      cone: ["radius", "height", "segments"],
      sphere: ["radius", "segments", "rings"],
      torus: ["major", "minor", "segments", "sides"],
      plane: ["size", "divisions"],
      stairs: ["steps", "run", "rise", "width"],
      ramp: ["length", "width", "height"],
      arch: ["width", "height", "thickness", "depth", "segments"],
      extrude: ["outline", "height"],
      lathe: ["profile", "segments"],
      loft: ["section", "path", "closed_section"],
      raw: ["positions", "indices"],
    };
    for (const shape of MESH_SHAPES) {
      expect(shape.fields.map((f) => f.name), shape.id).toEqual(fields[shape.id]);
      for (const f of shape.fields) expect(f.label.length, `${shape.id}.${f.name}`).toBeGreaterThan(0);
    }
  });

  it("turns every shape's starting values into a spec the core can build", () => {
    for (const id of IDS) {
      const result = parseSpec(id, defaultValues(id));
      expect(result.ok, `${id}: ${specErrorOf(result)}`).toBe(true);
      if (!result.ok) continue;
      expect(result.spec.shape).toBe(id);
      const text = specTextValues(result.spec);
      expect(Object.keys(result.spec).sort()).toEqual(["shape", ...Object.keys(text)].sort());
    }
  });

  it("round-trips a spec through its text values", () => {
    for (const id of IDS) {
      const first = parseSpec(id, defaultValues(id));
      if (!first.ok) throw new Error(specErrorOf(first));
      const second = parseSpec(id, specTextValues(first.spec));
      expect(second.ok, `${id}: ${specErrorOf(second)}`).toBe(true);
      if (second.ok) expect(second.spec).toEqual(first.spec);
    }
  });
});

describe("mesh spec validation", () => {
  it("refuses dimensions the core would reject", () => {
    expect(specErrorOf(parseSpec("box", { size: "" }))).toBe("size x y z is required");
    expect(specErrorOf(parseSpec("box", { size: "0 64 64" }))).toMatch(/out of range/);
    expect(specErrorOf(parseSpec("box", { size: "64 64" }))).toMatch(/out of range/);
    expect(specErrorOf(parseSpec("box", { size: "1e9 1 1" }))).toMatch(/out of range/);
    expect(specErrorOf(parseSpec("box", { size: "64 x 64 64" }))).toMatch(/out of range/);
  });

  it("holds segment counts to the core's minimum", () => {
    expect(specErrorOf(parseSpec("cylinder", { radius: "32", height: "64", segments: "2" }))).toMatch(/out of range/);
    expect(specErrorOf(parseSpec("sphere", { radius: "32", segments: "16", rings: "1" }))).toMatch(/out of range/);
    const ok = parseSpec("cylinder", { radius: "32", height: "64", segments: "3", caps: "1" });
    expect(ok.ok).toBe(true);
  });

  it("rejects an arch that is not taller than half its width", () => {
    const flat = parseSpec("arch", { width: "128", height: "64", thickness: "16", depth: "32", segments: "12" });
    expect(specErrorOf(flat)).toMatch(/half the width/);
    const tall = parseSpec("arch", { width: "96", height: "128", thickness: "16", depth: "32", segments: "12" });
    expect(tall.ok).toBe(true);
  });

  it("rejects outlines and profiles the core would triangulate into nothing", () => {
    const two = parseSpec("extrude", { outline: "0,0;64,0", height: "32" });
    expect(specErrorOf(two)).toMatch(/three or more/);
    const collinear = parseSpec("extrude", { outline: "0,0;64,64;128,128", height: "32" });
    expect(specErrorOf(collinear)).toMatch(/degenerate/);
    const short = parseSpec("lathe", { profile: "0,0", segments: "16" });
    expect(specErrorOf(short)).toMatch(/two or more/);
    const negative = parseSpec("lathe", { profile: "-1,0;0,0;0,16", segments: "16" });
    expect(specErrorOf(negative)).toMatch(/not be negative/);
  });

  it("rejects a raw triangle list that does not index its own positions", () => {
    const odd = parseSpec("raw", { positions: "0,0,0;1,0,0;0,1,0", indices: "0 1" });
    expect(specErrorOf(odd)).toMatch(/triangle list/);
    const over = parseSpec("raw", { positions: "0,0,0;1,0,0;0,1,0", indices: "0 1 9" });
    expect(specErrorOf(over)).toMatch(/out of range/);
    const good = parseSpec("raw", { positions: "0,0,0;1,0,0;0,1,0", indices: "0 1 2" });
    expect(good.ok).toBe(true);
  });

  it("names a shape it does not know", () => {
    expect(specErrorOf(parseSpec("donut" as MeshShapeId, {}))).toMatch(/not a shape/);
  });
});

describe("mesh create request", () => {
  const spec: MeshSpec = { shape: "stairs", steps: 4, run: 128, rise: 64, width: 64 };

  it("tags the spec and carries the placement", () => {
    const request = JSON.parse(meshRequestJson(spec, { material: "world/dev/dev_blue_512", name: "stair", position: [8, 16, 24] }));
    expect(request.spec).toEqual(spec);
    expect(request.spec.shape).toBe("stairs");
    expect(request.material).toBe("world/dev/dev_blue_512");
    expect(request.name).toBe("stair");
    expect(request.collision).toBe("triangles");
    expect(request.transform).toEqual({ position: [8, 16, 24], rotation: [0, 0, 0, 1], scale: [1, 1, 1] });
  });

  it("leaves out the fields the core defaults when they are not asked for", () => {
    const request = JSON.parse(meshRequestJson(spec, { material: "world/dev/dev_gray_512" }));
    expect("name" in request).toBe(false);
    expect("transform" in request).toBe(false);
    expect("uv_scale" in request).toBe(false);
  });

  it("passes a uv scale and a collision mode through", () => {
    const request = JSON.parse(meshRequestJson(spec, { material: "m", uvScale: 0.01, collision: "convex_hulls" }));
    expect(request.uv_scale).toBe(0.01);
    expect(request.collision).toBe("convex_hulls");
    const ignored = JSON.parse(meshRequestJson(spec, { material: "m", uvScale: 0 }));
    expect("uv_scale" in ignored).toBe(false);
  });
});
