import { describe, expect, it } from "vitest";
import {
  clampSingle,
  clampXray,
  defaultShading,
  drawsWire,
  editModeShading,
  hashObjectColor,
  isShadingMode,
  isSolidColor,
  selectSegmentBudget,
  setMode,
  sortNearest,
  takeUntilBudget,
  toggleWireframe,
  toggleXray,
  WIRE_LINE_BUDGET,
  XRAY_DEFAULT,
} from "./shading";
import { defaultKeymap, resolve, type KeyContext } from "../input/keymap";

const press = (init: Partial<KeyboardEvent>): KeyboardEvent =>
  ({ key: "", code: "", ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, ...init }) as KeyboardEvent;

describe("shading state", () => {
  it("starts textured with x-ray off and no overlay", () => {
    const s = defaultShading();
    expect(s.mode).toBe("textured");
    expect(s.xray).toBe(0);
    expect(s.wireOverlay).toBe(false);
    expect(WIRE_LINE_BUDGET).toBeGreaterThan(0);
  });

  it("sets modes and remembers the last non-wireframe one", () => {
    let s = defaultShading();
    s = setMode(s, "solid");
    expect(s.mode).toBe("solid");
    s = setMode(s, "wireframe");
    expect(s.mode).toBe("wireframe");
    expect(s.prev).toBe("solid");
    s = setMode(s, "textured");
    expect(s.mode).toBe("textured");
  });

  it("toggles wireframe back to the mode before it", () => {
    let s = setMode(defaultShading(), "solid");
    s = toggleWireframe(s);
    expect(s.mode).toBe("wireframe");
    s = toggleWireframe(s);
    expect(s.mode).toBe("solid");
    s = toggleWireframe(toggleWireframe(defaultShading()));
    expect(s.mode).toBe("textured");
  });

  it("toggles x-ray between off and the default amount", () => {
    let s = defaultShading();
    s = toggleXray(s);
    expect(s.xray).toBe(XRAY_DEFAULT);
    s = toggleXray(s);
    expect(s.xray).toBe(0);
  });

  it("enters edit mode solid with a wire overlay", () => {
    expect(editModeShading(defaultShading())).toMatchObject({ mode: "solid", wireOverlay: true });
    expect(editModeShading(setMode(defaultShading(), "wireframe")).mode).toBe("solid");
  });

  it("draws wire buffers in wireframe and in solid with the overlay", () => {
    expect(drawsWire(defaultShading())).toBe(false);
    expect(drawsWire(setMode(defaultShading(), "wireframe"))).toBe(true);
    expect(drawsWire(setMode(defaultShading(), "solid"))).toBe(false);
    expect(drawsWire(editModeShading(defaultShading()))).toBe(true);
    expect(drawsWire({ ...defaultShading(), wireOverlay: true })).toBe(false);
  });

  it("validates modes, colours, x-ray and single colours", () => {
    expect(isShadingMode("solid")).toBe(true);
    expect(isShadingMode("flat")).toBe(false);
    expect(isSolidColor("uv_checker")).toBe(true);
    expect(isSolidColor("vertex")).toBe(false);
    expect(clampXray(0.5)).toBe(0.5);
    expect(clampXray(0)).toBe(0);
    expect(clampXray(1)).toBe(1);
    expect(clampXray(-0.1)).toBeNull();
    expect(clampXray(2)).toBeNull();
    expect(clampXray(Number.NaN)).toBeNull();
    expect(clampXray(true)).toBeNull();
    expect(clampSingle([1, 0.5, 0])).toEqual([1, 0.5, 0]);
    expect(clampSingle([0, 0])).toBeNull();
    expect(clampSingle([0, 0, 2])).toBeNull();
  });

  it("hashes object ids to stable distinct colours", () => {
    const a = hashObjectColor("00000000-0000-4000-8000-000000000001");
    expect(hashObjectColor("00000000-0000-4000-8000-000000000001")).toEqual(a);
    expect(hashObjectColor("00000000-0000-4000-8000-000000000002")).not.toEqual(a);
    for (const c of a) expect(c).toBeGreaterThanOrEqual(0.35);
  });
});

describe("wireframe budget and LOD", () => {
  it("sorts nearest first with ties by index and defers bad distances", () => {
    expect(sortNearest([9, 1, 4])).toEqual([1, 2, 0]);
    expect(sortNearest([5, 5, 5]).slice(0, 2)).toEqual([0, 1]);
    expect(sortNearest([Number.NaN, 1, Number.POSITIVE_INFINITY]).slice(0, 2)).toEqual([1, 0]);
  });

  it("takes whole items only while they fit the budget", () => {
    expect(takeUntilBudget([12, 12, 12], 24)).toBe(2);
    expect(takeUntilBudget([12, 12, 12], 0)).toBe(0);
    expect(takeUntilBudget([30, 4], 20)).toBe(0);
    expect(takeUntilBudget([4, 30], 20)).toBe(1);
  });

  it("selects the nearest segments under the budget", () => {
    expect(selectSegmentBudget([9, 1, 4, 16], 2)).toEqual([1, 2]);
    expect(selectSegmentBudget([3, 1, 2], 99)).toEqual([1, 2, 0]);
    expect(selectSegmentBudget([1, 2], 0)).toEqual([]);
  });
});

describe("shading keymap", () => {
  const VIEW: KeyContext[] = ["viewport", "window"];
  for (const preset of ["blender", "legacy"] as const) {
    it(`${preset}: Z opens the shading pie`, () => {
      expect(resolve(defaultKeymap(preset), VIEW, press({ code: "KeyZ" }))).toBe("shading_pie");
    });

    it(`${preset}: Shift+Z toggles wireframe and Alt+Z toggles x-ray`, () => {
      const km = defaultKeymap(preset);
      expect(resolve(km, VIEW, press({ code: "KeyZ", shiftKey: true }))).toBe("shading_wire_toggle");
      expect(resolve(km, VIEW, press({ code: "KeyZ", altKey: true }))).toBe("shading_xray_toggle");
    });
  }
});
