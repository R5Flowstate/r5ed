export class BcError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "BcError";
  }
}

/** Expands a 5:6:5 colour to 8 bits per channel, replicating the top bits. */
export function rgb565(r: number, g: number, b: number): [number, number, number] {
  const r8 = (r << 3) | (r >> 2);
  const g8 = (g << 2) | (g >> 4);
  const b8 = (b << 3) | (b >> 2);
  return [r8, g8, b8];
}

/** Expands an 8-bit value to 16 bits, replicating the top bits. */
export function expand8To16(v: number): number {
  return (v << 8) | v;
}

/**
 * Decodes one 8-byte BC1 block into 16 RGBA pixels.
 * `c0 > c1` selects the four-opaque-colour mode; `c0 <= c1` selects the
 * three-colour-plus-transparent mode where index 3 is transparent black.
 */
export function decodeBc1Block(block: Uint8Array, offset: number, out: Uint8Array, outOffset: number): void {
  if (offset < 0 || offset + 8 > block.length) throw new BcError("BC1 block is out of bounds");
  const dv = new DataView(block.buffer, block.byteOffset, block.byteLength);
  const c0 = dv.getUint16(offset, true);
  const c1 = dv.getUint16(offset + 2, true);
  const bits = dv.getUint32(offset + 4, true);
  const c0rgb = rgb565((c0 >> 11) & 0x1f, (c0 >> 5) & 0x3f, c0 & 0x1f);
  const c1rgb = rgb565((c1 >> 11) & 0x1f, (c1 >> 5) & 0x3f, c1 & 0x1f);
  const fourColor = c0 > c1;
  // Four-colour mode: c2 = (2*c0 + c1) / 3 and c3 = (c0 + 2*c1) / 3.
  const palette: [number, number, number][] = fourColor
    ? [c0rgb, c1rgb, mixDiv3(c0rgb, c1rgb, 1), mixDiv3(c0rgb, c1rgb, 2)]
    : [c0rgb, c1rgb, mixHalf(c0rgb, c1rgb), [0, 0, 0]];
  for (let i = 0; i < 16; i++) {
    const index = (bits >>> (i * 2)) & 3;
    const c = palette[index];
    const o = outOffset + i * 4;
    const transparent = !fourColor && index === 3;
    out[o] = c[0];
    out[o + 1] = c[1];
    out[o + 2] = c[2];
    out[o + 3] = transparent ? 0 : 255;
  }
}

/** BC1 four-colour mode: `(2*a + b) / 3` with `w` weighting toward `a`. */
function mixDiv3(a: [number, number, number], b: [number, number, number], w: number): [number, number, number] {
  const m = (x: number, y: number) => Math.round(((3 - w) * x + w * y) / 3);
  return [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])];
}

/** BC1 three-colour mode: the midpoint `(a + b) / 2`. */
function mixHalf(a: [number, number, number], b: [number, number, number]): [number, number, number] {
  return [Math.round((a[0] + b[0]) / 2), Math.round((a[1] + b[1]) / 2), Math.round((a[2] + b[2]) / 2)];
}

/**
 * Decodes one 4-byte BC4 block into 16 single-channel values. The block is
 * two endpoint bytes plus six bytes of packed 3-bit indices; texel `i` uses
 * bits `3i`..`3i+2` counting from the least significant bit of byte 2.
 */
export function decodeBc4Block(block: Uint8Array, offset: number, out: Uint8Array, outOffset: number): void {
  if (offset < 0 || offset + 8 > block.length) throw new BcError("BC4 block is out of bounds");
  const r0 = block[offset];
  const r1 = block[offset + 1];
  // 48 index bits do not fit a 32-bit word, so shift arithmetically.
  let bits = 0;
  for (let i = 0; i < 6; i++) bits += block[offset + 2 + i] * Math.pow(2, i * 8);
  for (let i = 0; i < 16; i++) {
    out[outOffset + i] = bc4Value(r0, r1, Math.floor(bits / Math.pow(2, i * 3)) % 8);
  }
}

/**
 * One BC4/BC5 palette lookup. `r0 > r1` selects the eight-value mode
 * `((7-i)*r0 + (i-1)*r1) / 6`; otherwise the seven-value mode
 * `((6-i)*r0 + i*r1) / 7` with index 7 reserved for zero.
 */
function bc4Value(r0: number, r1: number, index: number): number {
  if (index === 0) return r0;
  if (index === 1) return r1;
  if (r0 > r1) return Math.round(((7 - index) * r0 + (index - 1) * r1) / 6);
  if (index === 7) return 0;
  return Math.round(((6 - index) * r0 + index * r1) / 7);
}

/** Decodes one 16-byte BC3 block: BC4 alpha followed by a BC1 colour block. */
export function decodeBc3Block(block: Uint8Array, offset: number, out: Uint8Array, outOffset: number): void {
  if (offset < 0 || offset + 16 > block.length) throw new BcError("BC3 block is out of bounds");
  const alpha = new Uint8Array(16);
  decodeBc4Block(block, offset, alpha, 0);
  const color = new Uint8Array(64);
  decodeBc1Block(block.subarray(offset + 8, offset + 16), 0, color, 0);
  for (let i = 0; i < 16; i++) {
    out[outOffset + i * 4] = color[i * 4];
    out[outOffset + i * 4 + 1] = color[i * 4 + 1];
    out[outOffset + i * 4 + 2] = color[i * 4 + 2];
    out[outOffset + i * 4 + 3] = alpha[i];
  }
}

/** Decodes one 16-byte BC5 block into 16 RG pixels. */
export function decodeBc5Block(block: Uint8Array, offset: number, out: Uint8Array, outOffset: number): void {
  if (offset < 0 || offset + 16 > block.length) throw new BcError("BC5 block is out of bounds");
  const r = new Uint8Array(16);
  decodeBc4Block(block, offset, r, 0);
  const g = new Uint8Array(16);
  decodeBc4Block(block, offset + 8, g, 0);
  for (let i = 0; i < 16; i++) {
    out[outOffset + i * 4] = r[i];
    out[outOffset + i * 4 + 1] = g[i];
    out[outOffset + i * 4 + 2] = 0;
    out[outOffset + i * 4 + 3] = 255;
  }
}


/**
 * Decodes a whole mip level to RGBA8. Only the formats the CPU path can
 * reconstruct exactly are accepted: BC1, BC3, BC4, BC5 and uncompressed.
 */
export function decodeBlocksToRgba(format: string, data: Uint8Array, width: number, height: number): Uint8Array {
  const out = new Uint8Array(width * height * 4);
  if (format === "rgba8" || format === "bgra8") {
    if (data.length < width * height * 4) throw new BcError("uncompressed mip data is truncated");
    for (let i = 0; i < width * height; i++) {
      const s = i * 4;
      out[s] = format === "bgra8" ? data[s + 2] : data[s];
      out[s + 1] = data[s + 1];
      out[s + 2] = format === "bgra8" ? data[s] : data[s + 2];
      out[s + 3] = 255;
    }
    return out;
  }
  if (format === "r8" || format === "rg8") {
    const channels = format === "r8" ? 1 : 2;
    if (data.length < width * height * channels) throw new BcError("luminance mip data is truncated");
    for (let i = 0; i < width * height; i++) {
      const g = data[i * channels];
      out[i * 4] = g;
      out[i * 4 + 1] = g;
      out[i * 4 + 2] = g;
      out[i * 4 + 3] = 255;
    }
    return out;
  }
  const blockBytes = format === "bc1" || format === "bc4" ? 8 : 16;
  const blocksX = Math.max(1, Math.ceil(width / 4));
  const blocksY = Math.max(1, Math.ceil(height / 4));
  if (data.length < blocksX * blocksY * blockBytes) throw new BcError("block data is truncated");
  const block = new Uint8Array(64);
  for (let by = 0; by < blocksY; by++) {
    for (let bx = 0; bx < blocksX; bx++) {
      const src = (by * blocksX + bx) * blockBytes;
      if (format === "bc1") {
        decodeBc1Block(data, src, block, 0);
      } else if (format === "bc3") {
        decodeBc3Block(data, src, block, 0);
      } else if (format === "bc4") {
        decodeBc4Block(data, src, block, 0);
        for (let i = 0; i < 16; i++) {
          block[i * 4] = block[i];
          block[i * 4 + 1] = block[i];
          block[i * 4 + 2] = block[i];
          block[i * 4 + 3] = 255;
        }
      } else if (format === "bc5") {
        decodeBc5Block(data, src, block, 0);
      } else {
        throw new BcError(`no CPU decoder for ${format}`);
      }
      for (let y = 0; y < 4 && by * 4 + y < height; y++) {
        for (let x = 0; x < 4 && bx * 4 + x < width; x++) {
          const s = (y * 4 + x) * 4;
          const d = ((by * 4 + y) * width + bx * 4 + x) * 4;
          out[d] = block[s];
          out[d + 1] = block[s + 1];
          out[d + 2] = block[s + 2];
          out[d + 3] = block[s + 3];
        }
      }
    }
  }
  return out;
}
