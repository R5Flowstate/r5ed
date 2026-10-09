import { describe, expect, it } from "vitest";
import { DEFAULT_PACK, lightmapRect, packLightmaps, type LightmapPreviews } from "./lightmap";
import { buildGlyphAtlas, glyphRect } from "./glyphs";
import { CATEGORY_ORDER } from "./atlas";
import { parseDdsHeader, mipLayout, DdsError } from "./dds";
import { needsCpuDecode, gpuFormatFor, defaultSlot, volumeColor, lightColor, entityCategory, NEUTRAL_GREY, FLAT_NORMAL, mipLevelsFor, uploadDds } from "./material";
import { buildDds } from "./dds.test";

/** A solid lightmap of one colour, so packing is checkable. */
function solid(width: number, height: number, rgba: [number, number, number]): LightmapPreviews[string] {
  const data = new Uint8Array(width * height * 4);
  for (let i = 0; i < width * height; i++) {
    data[i * 4] = rgba[0];
    data[i * 4 + 1] = rgba[1];
    data[i * 4 + 2] = rgba[2];
    data[i * 4 + 3] = 255;
  }
  return { width, height, rgba: data };
}

describe("lightmap packing", () => {
  it("puts every object's lightmap in the atlas with a rect", () => {
    const previews: LightmapPreviews = {
      a: solid(8, 4, [255, 0, 0]),
      b: solid(4, 8, [0, 255, 0]),
      c: solid(16, 16, [0, 0, 255]),
    };
    const packed = packLightmaps(previews, { maxSize: 256, pad: 0 });
    expect(packed.rects.size).toBe(3);
    expect(packed.width).toBeGreaterThanOrEqual(16);
    expect(packed.height).toBeGreaterThanOrEqual(16);
    expect(packed.pixels).toHaveLength(packed.width * packed.height * 4);
    for (const id of ["a", "b", "c"]) {
      const rect = packed.rects.get(id) as [number, number, number, number];
      expect(rect[2]).toBeGreaterThan(0);
      expect(rect[3]).toBeGreaterThan(0);
      expect(rect[0]).toBeGreaterThanOrEqual(0);
      expect(rect[0] + rect[2]).toBeLessThanOrEqual(1.0001);
    }
    // No two rects overlap.
    const ids = [...packed.rects.keys()];
    for (let i = 0; i < ids.length; i++) {
      for (let k = i + 1; k < ids.length; k++) {
        const a = packed.rects.get(ids[i]) as [number, number, number, number];
        const b = packed.rects.get(ids[k]) as [number, number, number, number];
        const disjoint = a[0] + a[2] <= b[0] || b[0] + b[2] <= a[0] || a[1] + a[3] <= b[1] || b[1] + b[3] <= a[1];
        expect(disjoint, `${ids[i]} overlaps ${ids[k]}`).toBe(true);
      }
    }
  });

  it("keeps each object's colour where its rect points", () => {
    const packed = packLightmaps({ a: solid(4, 4, [200, 30, 40]) }, { maxSize: 64, pad: 0 });
    const rect = packed.rects.get("a") as [number, number, number, number];
    const cx = Math.floor((rect[0] + rect[2] / 2) * packed.width);
    const cy = Math.floor((rect[1] + rect[3] / 2) * packed.height);
    const at = (cy * packed.width + cx) * 4;
    expect(packed.pixels[at]).toBe(200);
    expect(packed.pixels[at + 1]).toBe(30);
    expect(packed.pixels[at + 2]).toBe(40);
  });

  it("shrinks to fit a small atlas budget", () => {
    const packed = packLightmaps({ a: solid(512, 512, [1, 2, 3]) }, { maxSize: 64, pad: 0 });
    expect(packed.width).toBeLessThanOrEqual(64);
    expect(packed.height).toBeLessThanOrEqual(64);
  });

  it("insets the rect by half a texel so taps stay inside the cell", () => {
    const packed = packLightmaps({ a: solid(8, 8, [9, 9, 9]) }, { maxSize: 64, pad: 0 });
    const rect = packed.rects.get("a") as [number, number, number, number];
    expect(rect[0]).toBeGreaterThan(0);
    expect(rect[0] + rect[2]).toBeLessThan(1);
  });

  it("skips an empty or malformed preview", () => {
    const packed = packLightmaps({
      a: { width: 0, height: 0, rgba: new Uint8Array(0) },
      b: { width: 4, height: 4, rgba: new Uint8Array(3) },
    });
    expect(packed.rects.size).toBe(0);
    expect(lightmapRect(packed, "a")).toEqual([0, 0, 0, 0]);
  });

  it("returns a usable 1x1 atlas for no previews and a zero rect per object", () => {
    const packed = packLightmaps({});
    expect(packed.width).toBe(1);
    expect(packed.pixels).toHaveLength(4);
    expect(lightmapRect(null, "a")).toEqual([0, 0, 0, 0]);
    expect(lightmapRect(packed, "a")).toEqual([0, 0, 0, 0]);
    expect(DEFAULT_PACK.maxSize).toBeGreaterThan(0);
  });
});

/** A 2D context that records what was drawn, so the atlas can be checked. */
function recordingCanvas(): { canvas: HTMLCanvasElement; drawn: string[] } {
  const drawn: string[] = [];
  const canvas = document.createElement("canvas");
  const ctx = {
    clearRect: () => undefined,
    beginPath: () => undefined,
    moveTo: () => undefined,
    lineTo: () => undefined,
    quadraticCurveTo: () => undefined,
    closePath: () => undefined,
    fill: () => undefined,
    stroke: () => undefined,
    fillText: (text: string) => drawn.push(text),
    save: () => undefined,
    restore: () => undefined,
  };
  canvas.getContext = (() => ctx) as unknown as HTMLCanvasElement["getContext"];
  return { canvas, drawn };
}

describe("runtime glyph atlas", () => {
  it("draws one label per category and reads the pixels back", () => {
    const { canvas, drawn } = recordingCanvas();
    const atlas = buildGlyphAtlas(canvas, "dark");
    expect(atlas.entries.size).toBe(CATEGORY_ORDER.length);
    expect(drawn).toHaveLength(CATEGORY_ORDER.length);
    expect(new Set(drawn).size).toBe(CATEGORY_ORDER.length);
    for (const label of drawn) expect(label).toMatch(/^[A-Z]{2}$/);
    expect(canvas.width).toBe(atlas.width);
    expect(canvas.height).toBe(atlas.height);
    // No 2d context means no atlas rather than a blank one.
    const bare = document.createElement("canvas");
    bare.getContext = (() => null) as unknown as HTMLCanvasElement["getContext"];
    expect(() => buildGlyphAtlas(bare)).toThrow(/2d context/);
  });

  it("resolves a category rect from the drawn atlas", () => {
    const { canvas } = recordingCanvas();
    const atlas = buildGlyphAtlas(canvas, "light");
    expect(glyphRect(atlas, "func")).toEqual(atlas.entries.get("func")?.uv);
  });
});

/** A device that records the textures the uploader asks for. */
function fakeDevice(): GPUDevice {
  const sizes: number[][] = [];
  return {
    createTexture: (desc: GPUTextureDescriptor) => {
      const size = desc.size as number[];
      sizes.push(size);
      return { createView: () => ({}), destroy: () => undefined, width: size[0], height: size[1] } as unknown as GPUTexture;
    },
    queue: { writeTexture: () => undefined },
  } as unknown as GPUDevice;
}

describe("material upload decisions", () => {
  it("maps every DDS format onto a GPU format or none", () => {
    const bc1 = parseDdsHeader(buildDds({ width: 8, height: 8, fourCC: 0x31545844, mips: 1 }, new Uint8Array(8 * 8 * 2)));
    expect(gpuFormatFor(bc1)?.format).toBe("bc1-rgba-unorm");
    expect(gpuFormatFor(bc1)?.compressed).toBe(true);
    const rgba = parseDdsHeader(buildDds({ width: 8, height: 8, pfFlags: 0x41, rgbBitCount: 32, mips: 1 }, new Uint8Array(8 * 8 * 4)));
    expect(gpuFormatFor(rgba)?.format).toBe("bgra8unorm");
    expect(gpuFormatFor({ ...bc1, format: "bc2" })).toBeNull();
    // HDR skies and cubemaps ship as BC6H, unsigned or signed.
    expect(gpuFormatFor({ ...bc1, format: "bc6h" })?.format).toBe("bc6h-rgb-ufloat");
    expect(gpuFormatFor({ ...bc1, format: "bc6hs" })?.format).toBe("bc6h-rgb-float");
    const sky = parseDdsHeader(buildDds({ width: 8, height: 8, fourCC: 0x30315844, dxgi: 95, mips: 1 }, new Uint8Array(8 * 8)));
    expect(sky.format).toBe("bc6h");
  });

  it("decodes on the CPU only when the GPU cannot take the blocks", () => {
    const bc1 = parseDdsHeader(buildDds({ width: 8, height: 8, fourCC: 0x31545844, mips: 1 }, new Uint8Array(8 * 8 * 2)));
    expect(needsCpuDecode(bc1, true)).toBe(false);
    expect(needsCpuDecode(bc1, false)).toBe(true);
    const rgba = parseDdsHeader(buildDds({ width: 8, height: 8, pfFlags: 0x41, rgbBitCount: 32, mips: 1 }, new Uint8Array(8 * 8 * 4)));
    expect(needsCpuDecode(rgba, false)).toBe(false);
    const bc7 = parseDdsHeader(buildDds({ width: 8, height: 8, fourCC: 0x30315844, dxgi: 98, mips: 1 }, new Uint8Array(8 * 8 * 4)));
    // BC7 has no CPU decoder, so it needs the feature rather than a fallback.
    expect(needsCpuDecode(bc7, false)).toBe(false);
    expect(() => uploadDds(fakeDevice(), buildDds({ width: 8, height: 8, fourCC: 0x30315844, dxgi: 98, mips: 1 }, new Uint8Array(8 * 8 * 4)), "bc7", false)).toThrow(/needs texture-compression-bc/);
    expect(() => parseDdsHeader(new Uint8Array(4))).toThrow(DdsError);
  });

  it("counts mip levels the same way the DDS rules do", () => {
    expect(mipLevelsFor(8, 8)).toBe(4);
    expect(mipLevelsFor(1, 1)).toBe(1);
    expect(mipLevelsFor(16, 4)).toBe(5);
    const info = parseDdsHeader(buildDds({ width: 8, height: 8, fourCC: 0x31545844, mips: 4 }, new Uint8Array(8 * 8 * 2 * 4 / 4 * 4)));
    expect(mipLevelsFor(info.width, info.height)).toBe(4);
    expect(mipLayout(info, 3).width).toBe(1);
    expect(mipLayout(info, 3).size).toBe(8);
    expect(() => mipLayout(info, 4)).toThrow(DdsError);
  });

  it("starts a material at a neutral grey with no channels", () => {
    const slot = defaultSlot("props/unknown");
    expect(slot.baseColor).toEqual([0.62, 0.62, 0.64, 1]);
    expect(slot.albedoGuid).toBeNull();
    expect(NEUTRAL_GREY).toHaveLength(4);
    expect(FLAT_NORMAL[2]).toBeGreaterThan(200);
  });

  it("gives every volume kind a translucent colour and every light a tint", () => {
    for (const kind of ["trigger", "trigger_hurt", "fog_volume", "player_clip", "out_of_bounds", "custom"]) {
      const c = volumeColor(kind);
      expect(c).toHaveLength(4);
      expect(c[3]).toBeLessThan(1);
      expect(c[3]).toBeGreaterThan(0);
    }
    expect(volumeColor("not_a_kind")).toEqual(volumeColor("custom"));
    expect(lightColor("sun")).toEqual(lightColor("sky"));
    expect(entityCategory("trigger_once")).toBe("trigger");
  });
});
