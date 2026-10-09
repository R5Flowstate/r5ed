import { describe, expect, it } from "vitest";
import { BC_FEATURE, DEPTH_FORMAT, compressedTextureBytes, mipChainCount, unsupportedMessage, hasBcSupport, gpuAdapterOptions, SUPPORTED_BROWSERS } from "./device";
import { expand8To16, rgb565, decodeBc1Block } from "./bc";
import { BOX_SHADER, BILLBOARD_SHADER, FULLSCREEN_SHADER, LINE_SHADER, SURFACE_SHADER, FRAME_STRUCT, MATERIAL_STRUCT } from "./shaders";

describe("device helpers", () => {
  it("names the browsers the editor supports", () => {
    expect(SUPPORTED_BROWSERS.length).toBeGreaterThan(1);
    for (const name of SUPPORTED_BROWSERS) expect(name.length).toBeGreaterThan(0);
    expect(unsupportedMessage()).toContain(SUPPORTED_BROWSERS[0]);
    expect(unsupportedMessage("no adapter")).toContain("no adapter");
  });

  it("asks for the block-compression feature when it can", () => {
    expect(BC_FEATURE).toBe("texture-compression-bc");
    const plain = gpuAdapterOptions();
    expect(plain === null || typeof plain === "object").toBe(true);
    const withBc = { features: new Set([BC_FEATURE]) } as unknown as GPUDevice;
    expect(hasBcSupport(withBc)).toBe(true);
    const bare = { features: new Set<string>() } as unknown as GPUDevice;
    expect(hasBcSupport(bare)).toBe(false);
  });

  it("counts the bytes a block-compressed mip needs", () => {
    // BC1 is 8 bytes per 4x4 block, so a 4x4 texture is one block.
    expect(compressedTextureBytes(4, 4, 8, 4, 4)).toBe(8);
    // A 7x5 texture needs two blocks across and two down.
    expect(compressedTextureBytes(7, 5, 8, 4, 4)).toBe(32);
    // BC5 is 16 bytes per block.
    expect(compressedTextureBytes(8, 8, 16, 4, 4)).toBe(64);
    // A degenerate size still reports one block rather than zero.
    expect(compressedTextureBytes(0, 0, 8, 4, 4)).toBe(8);
  });

  it("counts mip levels down to one pixel", () => {
    expect(mipChainCount(1, 1)).toBe(1);
    expect(mipChainCount(2, 2)).toBe(2);
    expect(mipChainCount(4, 4)).toBe(3);
    expect(mipChainCount(256, 128)).toBe(9);
  });

  it("compares depth with less-equal", () => {
    expect(DEPTH_FORMAT).toBe("depth24plus");
  });
});

describe("block compression helpers", () => {
  it("replicates a byte into a 16-bit channel", () => {
    expect(expand8To16(0)).toBe(0);
    expect(expand8To16(255)).toBe(65535);
    expect(expand8To16(0x12)).toBe(0x1212);
  });

  it("widens a 565 channel back to eight bits", () => {
    expect(rgb565(0, 0, 0)).toEqual([0, 0, 0]);
    expect(rgb565(31, 63, 31)).toEqual([255, 255, 255]);
    // Six bits of green lose two more than five bits of red and blue.
    expect(rgb565(16, 32, 16)).toEqual([132, 130, 132]);
  });

  it("decodes a known BC1 block", () => {
    // Endpoint zero is pure red in 565, endpoint one pure blue, and the indices
    // alternate between them across the block.
    const block = new Uint8Array(8);
    const view = new DataView(block.buffer);
    view.setUint16(0, 0xf800, true);
    view.setUint16(2, 0x001f, true);
    // Index 0x44 is 01 00 01 00 per byte, so the pixels alternate red, blue.
    view.setUint32(4, 0x44444444, true);
    const out = new Uint8Array(4 * 4 * 4);
    decodeBc1Block(block, 0, out, 0);
    for (let i = 0; i < 16; i++) {
      const expected = i % 2 === 0 ? [255, 0, 0, 255] : [0, 0, 255, 255];
      expect([...out.slice(i * 4, i * 4 + 4)], "pixel " + i).toEqual(expected);
    }
  });

  it("rejects a BC1 block that runs off the end of the buffer", () => {
    expect(() => decodeBc1Block(new Uint8Array(4), 0, new Uint8Array(64), 0)).toThrow(/out of bounds/);
  });
});

describe("shader sources", () => {
  it("binds the frame and material blocks every pass shares", () => {
    for (const src of [SURFACE_SHADER, LINE_SHADER, BILLBOARD_SHADER, BOX_SHADER]) {
      expect(src).toContain(FRAME_STRUCT);
    }
    expect(SURFACE_SHADER).toContain(MATERIAL_STRUCT);
  });

  it("exposes the entry points the renderer binds", () => {
    for (const entry of ["vs", "fs_surface", "fs_translucent", "fs_pick"]) {
      expect(SURFACE_SHADER).toContain(`fn ${entry}`);
    }
    expect(LINE_SHADER).toContain("fn vs");
    expect(LINE_SHADER).toContain("fn fs_color");
    expect(LINE_SHADER).toContain("fn fs_grid");
    expect(BILLBOARD_SHADER).toContain("fn fs_billboard");
    expect(BOX_SHADER).toContain("fn fs_box");
    // The fullscreen pass is only its vertex stage; the fragment side is a
    // blit the render pass does with a pipeline of its own.
    expect(FULLSCREEN_SHADER).toContain("fn vs");
    expect(FULLSCREEN_SHADER).toContain("@vertex");
  });

  it("keeps the binding numbers the pipeline layouts declare", () => {
    expect(SURFACE_SHADER).toContain("@group(0) @binding(0)");
    expect(SURFACE_SHADER).toContain("@group(1)");
  });
});
