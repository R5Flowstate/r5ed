import { describe, expect, it } from "vitest";
import {
  FALLOFFS,
  SCULPT_TOOLS,
  StrokeError,
  defaultStroke,
  falloffName,
  falloffValue,
  falloffWeight,
  parseStroke,
  rampEnd,
  strokeDabs,
  strokeDirtyRect,
  strokeJson,
  validateStroke,
  type Stroke,
} from "./sculpt";

function stroke(over: Partial<Stroke> = {}): Stroke {
  return { ...defaultStroke("raise"), points: [{ x: 0, y: 0, pressure: 1 }], ...over };
}

describe("stroke mirror", () => {
  it("names every tool the core declares", () => {
    expect(SCULPT_TOOLS).toEqual([
      "raise", "lower", "smooth", "flatten", "set_height",
      "noise", "erode", "terrace", "ramp", "stamp",
    ]);
  });

  it("starts at the defaults the core's own Default impl uses", () => {
    const s = defaultStroke();
    expect(s.radius).toBe(128);
    expect(s.strength).toBe(32);
    expect(s.falloff).toBe(0.5);
    expect(s.target_height).toBe(0);
    expect(s.brush).toBeNull();
    expect(s.rotation).toBe(0);
    expect(s.blend).toBe("add");
    expect(s.step).toBe(24);
    expect(s.seed).toBe(0);
    expect(s.noise_frequency).toBe(0.01);
  });

  it("maps every named falloff onto the core's 0..1 axis", () => {
    expect(FALLOFFS).toEqual(["hard", "sharp", "smooth", "linear", "soft"]);
    expect(falloffValue("hard")).toBe(0);
    expect(falloffValue("soft")).toBe(1);
    for (const name of FALLOFFS) {
      const value = falloffValue(name);
      expect(value).toBeGreaterThanOrEqual(0);
      expect(value).toBeLessThanOrEqual(1);
      expect(falloffName(value)).toBe(name);
    }
    expect(falloffName(0.4)).toBe("smooth");
  });

  it("applies the same weight curve the core does", () => {
    // falloff 0 is a flat disc, so the weight does not fall with distance.
    expect(falloffWeight(0.9, 1, 0)).toBeCloseTo(1, 6);
    expect(falloffWeight(0.5, 1, 0)).toBeCloseTo(1, 6);
    // falloff 1 is a smoothstep cone, zero at the rim and half at halfway.
    expect(falloffWeight(1, 1, 1)).toBe(0);
    expect(falloffWeight(0.5, 1, 1)).toBeCloseTo(0.5, 6);
    expect(falloffWeight(0.25, 1, 1)).toBeCloseTo(0.84375, 6);
    expect(falloffWeight(2, 1, 1)).toBe(0);
    expect(falloffWeight(0.5, 0, 1)).toBe(0);
  });
});

describe("stroke json", () => {
  it("emits points as [x, y, pressure] triples and every field the core declares", () => {
    const out = JSON.parse(strokeJson(stroke({ points: [{ x: 8, y: -16, pressure: 0.5 }] }))) as Record<string, unknown>;
    expect(out.tool).toBe("raise");
    expect(out.points).toEqual([[8, -16, 0.5]]);
    expect(Object.keys(out).sort()).toEqual([
      "blend", "brush", "falloff", "noise_frequency", "points", "radius",
      "rotation", "seed", "step", "strength", "target_height", "tool",
    ]);
  });

  it("round-trips through parse", () => {
    const first = strokeJson(stroke({ tool: "flatten", radius: 256, strength: 12.5, target_height: 512, step: 8, seed: 9 }));
    expect(strokeJson(parseStroke(first))).toBe(first);
  });

  it("carries a stamp brush and rejects one with the wrong sample count", () => {
    const brush = { width: 2, height: 2, cell_size: 8, data: [0, 1, 2, 3] };
    const first = strokeJson(stroke({ tool: "stamp", brush, blend: "overlay", rotation: 0.5 }));
    const back = parseStroke(first);
    expect(back.brush).toEqual(brush);
    expect(back.blend).toBe("overlay");
    expect(strokeJson(back)).toBe(first);
    const bad = JSON.parse(first) as Record<string, unknown>;
    bad.brush = { width: 2, height: 2, cell_size: 8, data: [0, 1] };
    expect(parseStroke(JSON.stringify(bad)).brush).toBeNull();
  });

  it("fills in defaults and drops a point it cannot read", () => {
    const back = parseStroke(JSON.stringify({ tool: "erode", points: [[1, 2, 3], { x: 4, y: 5, pressure: 0.25 }, "junk", [1, 2]] }));
    expect(back.tool).toBe("erode");
    expect(back.points).toEqual([
      { x: 1, y: 2, pressure: 3 },
      { x: 4, y: 5, pressure: 0.25 },
    ]);
    expect(back.radius).toBe(128);
    expect(back.blend).toBe("add");
  });

  it("clamps a falloff and rounds a seed on the way in", () => {
    const back = parseStroke(JSON.stringify({ falloff: 4, seed: 3.7 }));
    expect(back.falloff).toBe(1);
    expect(back.seed).toBe(4);
  });

  it("rejects json that is not a stroke", () => {
    expect(() => parseStroke("nope")).toThrow(StrokeError);
    expect(() => parseStroke("[]")).toThrow(/not an object/);
  });
});

describe("stroke validation", () => {
  it("accepts every tool with the fields that tool needs", () => {
    for (const tool of SCULPT_TOOLS) {
      const base = stroke({ tool });
      if (tool === "stamp") base.brush = { width: 1, height: 1, cell_size: 8, data: [0] };
      if (tool === "ramp") base.points = [{ x: 0, y: 0, pressure: 1 }, { x: 64, y: 0, pressure: 1 }];
      expect(validateStroke(base)).toBe(base);
    }
  });

  it("rejects an unknown tool, a bad radius and a falloff out of range", () => {
    expect(() => validateStroke(stroke({ tool: "spray" as never }))).toThrow(/unknown sculpt tool/);
    expect(() => validateStroke(stroke({ radius: 0 }))).toThrow(/radius/);
    expect(() => validateStroke(stroke({ radius: 1e12 }))).toThrow(/radius/);
    expect(() => validateStroke(stroke({ falloff: 1.5 }))).toThrow(/falloff/);
    expect(() => validateStroke(stroke({ step: 0 }))).toThrow(/terrace step/);
    expect(() => validateStroke(stroke({ seed: -1 }))).toThrow(/seed/);
    expect(() => validateStroke(stroke({ noise_frequency: 0 }))).toThrow(/noise frequency/);
  });

  it("rejects a ramp with one point and a stamp with no brush", () => {
    expect(() => validateStroke(stroke({ tool: "ramp" }))).toThrow(/ramp stroke needs at least two points/);
    expect(() => validateStroke(stroke({ tool: "stamp" }))).toThrow(/needs a brush/);
  });

  it("rejects a point out of range and one with a bad pressure", () => {
    expect(() => validateStroke(stroke({ points: [{ x: 1e12, y: 0, pressure: 1 }] }))).toThrow(/out of range/);
    expect(() => validateStroke(stroke({ points: [{ x: 0, y: 0, pressure: 2 }] }))).toThrow(/pressure must be 0..1/);
    expect(() => validateStroke(stroke({ points: new Array(5000).fill({ x: 0, y: 0, pressure: 1 }) })))
      .toThrow(/too many points/);
  });
});

describe("stroke geometry", () => {
  it("samples dabs along the path and interpolates pressure", () => {
    const dabs = strokeDabs(stroke({
      points: [
        { x: 0, y: 0, pressure: 0.25 },
        { x: 32, y: 0, pressure: 1 },
      ],
    }), 16);
    expect(dabs).toHaveLength(3);
    expect(dabs[0]).toEqual({ x: 0, y: 0, pressure: 0.25 });
    expect(dabs[1].x).toBeCloseTo(16, 6);
    expect(dabs[1].pressure).toBeCloseTo(0.625, 6);
    expect(dabs[2]).toEqual({ x: 32, y: 0, pressure: 1 });
  });

  it("jumps instead of interpolating when the brush was lifted", () => {
    const dabs = strokeDabs(stroke({
      points: [{ x: 0, y: 0, pressure: 0 }, { x: 32, y: 0, pressure: 1 }],
    }), 16);
    expect(dabs).toEqual([
      { x: 0, y: 0, pressure: 0 },
      { x: 32, y: 0, pressure: 1 },
    ]);
  });

  it("returns no dabs for a stroke with no points", () => {
    expect(strokeDabs(defaultStroke())).toEqual([]);
  });

  it("reports the far end of a ramp", () => {
    expect(rampEnd(stroke({ tool: "ramp" }))).toBeNull();
    const s = stroke({ tool: "ramp", points: [{ x: 0, y: 0, pressure: 1 }, { x: 96, y: 32, pressure: 1 }] });
    expect(rampEnd(s)).toEqual({ x: 96, y: 32, pressure: 1 });
  });

  it("reports nothing for a stroke that misses the grid entirely", () => {
    const s = stroke({ radius: 1024, points: [{ x: -8192, y: -8192, pressure: 1 }] });
    expect(strokeDirtyRect(s, [8, 8], 16)).toBeNull();
  });

  it("reports the dirty sample rect, half open on the high edges", () => {
    const s = stroke({ radius: 64, points: [{ x: 128, y: 128, pressure: 1 }] });
    expect(strokeDirtyRect(s, [64, 64], 16)).toEqual({ x0: 3, y0: 3, x1: 14, y1: 14 });
    expect(strokeDirtyRect(defaultStroke(), [64, 64], 16)).toBeNull();
    expect(strokeDirtyRect(s, [64, 64], 0)).toBeNull();
    expect(strokeDirtyRect(s, [0, 0], 16)).toBeNull();
  });

  it("clamps the dirty rect to the grid", () => {
    const s = stroke({ radius: 1024, points: [{ x: -128, y: -128, pressure: 1 }] });
    const rect = strokeDirtyRect(s, [8, 8], 16);
    expect(rect).toEqual({ x0: 0, y0: 0, x1: 8, y1: 8 });
  });
});
