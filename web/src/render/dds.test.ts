import { describe, expect, it } from "vitest";
import { decodeBc1Block, decodeBc3Block, decodeBc4Block, decodeBc5Block, rgb565 } from "./bc";
import { DdsError, mipLayout, parseDdsHeader, readDdsMips } from "./dds";

/** Builds a DDS file with a 128-byte header (148 for DX10) and pixel bytes. */
export function buildDds(
  opts: { width: number; height: number; fourCC?: number; mips?: number; rgbBitCount?: number; pfFlags?: number; dxgi?: number },
  pixels: Uint8Array,
): Uint8Array {
  const headerBytes = opts.dxgi !== undefined ? 148 : 128;
  const out = new Uint8Array(headerBytes + pixels.length);
  const dv = new DataView(out.buffer);
  dv.setUint32(0, 0x20534444, true);
  dv.setUint32(4, 124, true);
  dv.setUint32(8, headerBytes, true);
  dv.setUint32(12, opts.height, true);
  dv.setUint32(16, opts.width, true);
  dv.setUint32(20, pixels.length, true);
  dv.setUint32(28, opts.mips ?? 1, true);
  if (opts.fourCC !== undefined) {
    dv.setUint32(80, 0x4, true);
    dv.setUint32(84, opts.fourCC, true);
  } else {
    dv.setUint32(80, opts.pfFlags ?? 0x41, true);
    dv.setUint32(88, opts.rgbBitCount ?? 32, true);
  }
  if (opts.dxgi !== undefined) {
    // The DX10 extension begins at 128; the pixel data follows it.
    dv.setUint32(128, opts.dxgi, true);
    dv.setUint32(132, 3, true);
    dv.setUint32(140, 1, true);
  }
  out.set(pixels, headerBytes);
  return out;
}

/** BC1/BC3 2-bit indices: texel `i` takes bits `2i` and `2i+1`. */
function indices2(indices: number[]): Uint32Array {
  let bits = 0;
  for (let i = 0; i < indices.length; i++) bits |= (indices[i] & 3) << (i * 2);
  return new Uint32Array([bits >>> 0]);
}

/** BC4 index bytes: six bytes of packed 3-bit indices, texel `i` at bit 3i. */
function indices4(indices: number[]): number[] {
  const bytes = new Array(6).fill(0);
  indices.forEach((value, i) => {
    const nib = value & 7;
    const base = i * 3;
    for (let b = 0; b < 3; b++) {
      if (nib & (1 << b)) bytes[(base + b) >> 3] |= 1 << ((base + b) & 7);
    }
  });
  return bytes;
}

describe("dds header parser", () => {
  it("reads a DXT1 header", () => {
    const dds = buildDds({ width: 64, height: 32, fourCC: 0x31545844, mips: 5 }, new Uint8Array(8 * 16 * 8));
    const info = parseDdsHeader(dds);
    expect(info.width).toBe(64);
    expect(info.height).toBe(32);
    expect(info.mipCount).toBe(5);
    expect(info.format).toBe("bc1");
    expect(info.blockBytes).toBe(8);
    expect(info.uncompressed).toBe(false);
  });

  it("reads a DX10 BC7 header and its mip offsets", () => {
    const mips: number[] = [];
    let w = 64;
    let h = 64;
    for (let i = 0; i < 4; i++) {
      mips.push(Math.max(1, w >> 1) * Math.max(1, h >> 1) * 16);
      w = Math.max(1, w >> 1);
      h = Math.max(1, h >> 1);
    }
    const pixels = new Uint8Array(mips.reduce((a, b) => a + b, 0));
    const dds = buildDds({ width: 64, height: 64, fourCC: 0x30315844, mips: 4, dxgi: 98 }, pixels);
    const info = parseDdsHeader(dds);
    expect(info.format).toBe("bc7");
    expect(info.arraySize).toBe(1);
    expect(mipLayout(info, 0).size).toBe(16 * 16 * 16);
    expect(mipLayout(info, 1).offset).toBe(16 * 16 * 16);
    expect(mipLayout(info, 1).width).toBe(32);
    const { mips: levels } = readDdsMips(dds);
    expect(levels).toHaveLength(4);
    expect(levels[3].width).toBe(8);
    expect(levels[3].height).toBe(8);
  });

  it("rejects a bad magic, size and pixel format", () => {
    expect(() => parseDdsHeader(new Uint8Array(128))).toThrow(DdsError);
    const dds = buildDds({ width: 4, height: 4, fourCC: 0x31545844 }, new Uint8Array(16));
    new DataView(dds.buffer).setUint32(4, 999, true);
    expect(() => parseDdsHeader(dds)).toThrow(/header size/);
    expect(() => parseDdsHeader(buildDds({ width: 4, height: 4, fourCC: 0x12345678 }, new Uint8Array(16)))).toThrow(/FourCC/);
    expect(() => parseDdsHeader(new Uint8Array(10))).toThrow(/truncated/);
  });
});


describe("bc1 cpu decoder", () => {
  it("expands rgb565 by replicating the high bits", () => {
    expect(rgb565(31, 63, 31)).toEqual([255, 255, 255]);
    expect(rgb565(0, 0, 0)).toEqual([0, 0, 0]);
    // 16 -> 0b10000000 | 0b100 = 0x84 = 132; 32 -> 0b10000000 | 0b10 = 0x82 = 130; 8 -> 0b01000000|0b10 = 66.
    expect(rgb565(16, 32, 8)).toEqual([132, 130, 66]);
  });

  it("decodes a four-colour block: two endpoints plus the 2/3 and 1/3 mixes", () => {
    // c0 = pure red (0xF800), c1 = pure blue (0x001F). c0 > c1 so four colours.
    const block = new Uint8Array(8);
    const dv = new DataView(block.buffer);
    dv.setUint16(0, 0xf800, true);
    dv.setUint16(2, 0x001f, true);
    dv.setUint32(4, indices2([0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3])[0], true);
    const out = new Uint8Array(64);
    decodeBc1Block(block, 0, out, 0);
    expect(Array.from(out.slice(0, 4))).toEqual([255, 0, 0, 255]);
    expect(Array.from(out.slice(4, 8))).toEqual([0, 0, 255, 255]);
    expect(Array.from(out.slice(8, 12))).toEqual([170, 0, 85, 255]);
    expect(Array.from(out.slice(12, 16))).toEqual([85, 0, 170, 255]);
  });

  it("decodes a three-colour block with a transparent index", () => {
    // c0 = dark green (0x0380), c1 = bright green (0x07E0). c0 <= c1 selects
    // the three-colour mode, where index 3 is transparent black.
    const block = new Uint8Array(8);
    const dv = new DataView(block.buffer);
    dv.setUint16(0, 0x0380, true);
    dv.setUint16(2, 0x07e0, true);
    dv.setUint32(4, indices2(new Array(16).fill(3))[0], true);
    const out = new Uint8Array(64);
    decodeBc1Block(block, 0, out, 0);
    expect(Array.from(out.slice(0, 4))).toEqual([0, 0, 0, 0]);
    dv.setUint32(4, indices2([0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2, 0])[0], true);
    decodeBc1Block(block, 0, out, 0);
    expect(Array.from(out.slice(0, 4))).toEqual([0, 113, 0, 255]);
    // The midpoint of (0,113,0) and (0,255,0) rounds to 184.
    expect(Array.from(out.slice(8, 12))).toEqual([0, 184, 0, 255]);
  });

  it("rejects an out-of-bounds block", () => {
    expect(() => decodeBc1Block(new Uint8Array(4), 0, new Uint8Array(64), 0)).toThrow(/out of bounds/);
  });
});

describe("bc4 and bc5 cpu decoders", () => {
  it("decodes the eight bc4 modes for a 200/100 pair", () => {
    const idx = indices4([0, 1, 2, 3, 4, 5, 6, 7, 0, 1, 2, 3, 4, 5, 6, 7]);
    const out = new Uint8Array(16);
    decodeBc4Block(new Uint8Array([200, 100, ...idx]), 0, out, 0);
    // r0 = 200 > r1 = 100, so the eight-value mode divides by 6.
    expect(out[0]).toBe(200);
    expect(out[1]).toBe(100);
    expect(out[2]).toBe(Math.round((5 * 200 + 100) / 6));
    expect(out[7]).toBe(Math.round((0 * 200 + 6 * 100) / 6));
    expect(out[8]).toBe(200);
    expect(out[15]).toBe(100);
  });

  it("decodes the seven-mode table for a 0/200 pair", () => {
    const idx = indices4([0, 1, 2, 3, 4, 5, 6, 7, 0, 0, 0, 0, 0, 0, 0, 0]);
    const out = new Uint8Array(16);
    decodeBc4Block(new Uint8Array([0, 200, ...idx]), 0, out, 0);
    expect(out[0]).toBe(0);
    expect(out[1]).toBe(200);
    expect(out[2]).toBe(Math.round((4 * 0 + 2 * 200) / 7));
    expect(out[6]).toBe(Math.round((0 * 0 + 6 * 200) / 7));
    expect(out[7]).toBe(0);
  });

  it("decodes bc3 alpha plus colour", () => {
    const block = new Uint8Array(16);
    const dv = new DataView(block.buffer);
    // Alpha block: a0 = a1 = 255 with all indices 0, so alpha is opaque.
    block[0] = 255;
    block[1] = 255;
    dv.setUint16(8, 0xf800, true);
    dv.setUint16(10, 0x001f, true);
    dv.setUint32(12, 0, true);
    const out = new Uint8Array(64);
    decodeBc3Block(block, 0, out, 0);
    expect(Array.from(out.slice(0, 4))).toEqual([255, 0, 0, 255]);
  });

  it("decodes bc5 into two channels", () => {
    const block = new Uint8Array(16);
    const rIdx = indices4([0, 1, 2, 3, 4, 5, 6, 7, 0, 0, 0, 0, 0, 0, 0, 0]);
    const gIdx = indices4([7, 6, 5, 4, 3, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    block.set([255, 0, ...rIdx], 0);
    block.set([0, 255, ...gIdx], 8);
    const out = new Uint8Array(64);
    decodeBc5Block(block, 0, out, 0);
    // Texel 0: red index 0 of a 255/0 pair, green index 7 which is zero.
    expect(Array.from(out.slice(0, 4))).toEqual([255, 0, 0, 255]);
    // Texel 1: red index 1 of 255/0 is 0; green index 6 of a 0/255 pair.
    expect(out[4]).toBe(0);
    expect(out[5]).toBe(Math.round((6 * 255) / 7));
    // Texel 7: red index 7 of a 255/0 pair is ((7-7)*255 + 6*0)/6 = 0;
    // green index 0 of a 0/255 pair is 0 too.
    expect(out[28]).toBe(0);
    expect(out[29]).toBe(0);
    // Texel 2: red index 2 of 255/0 is ((7-2)*255 + 1*0)/6, green index 5 of
    // 0/255 is ((6-5)*0 + 5*255)/7.
    expect(out[8]).toBe(Math.round((5 * 255) / 6));
    expect(out[9]).toBe(Math.round((5 * 255) / 7));
  });
});
