import { describe, expect, it } from "vitest";
import { DEFAULT_BRUSH_HEIGHT, MIN_BRUSH_HEIGHT } from "../render/boxdrag";
import { DEFAULT_MATERIAL } from "../doc/model";
import { defaultBrushOptions, setBrush, state } from "./stores";

describe("brush tool options", () => {
  it("starts at the thickness the tool used before it was configurable", () => {
    const options = defaultBrushOptions();
    expect(options.height).toBe(DEFAULT_BRUSH_HEIGHT);
    expect(options.material).toBe(DEFAULT_MATERIAL);
    expect(options.width).toBeGreaterThan(0);
    expect(options.depth).toBeGreaterThan(0);
  });

  it("keeps the height a whole number that the core can build a solid from", () => {
    setBrush({ height: 512 });
    expect(state.tool.brush.height).toBe(512);
    setBrush({ height: 96.4 });
    expect(state.tool.brush.height).toBe(96);
    setBrush({ height: 0 });
    expect(state.tool.brush.height).toBe(MIN_BRUSH_HEIGHT);
    setBrush({ height: -8 });
    expect(state.tool.brush.height).toBe(MIN_BRUSH_HEIGHT);
  });

  it("keeps the last good height when the field is cleared", () => {
    setBrush({ height: 256 });
    setBrush({ height: Number.NaN });
    expect(state.tool.brush.height).toBe(256);
  });

  it("takes the material and the plan size without touching the height", () => {
    setBrush({ height: 128 });
    setBrush({ material: "world/dev/dev_blue_512", width: 96, depth: 32 });
    expect(state.tool.brush.material).toBe("world/dev/dev_blue_512");
    expect(state.tool.brush.width).toBe(96);
    expect(state.tool.brush.depth).toBe(32);
    expect(state.tool.brush.height).toBe(128);
  });
});
