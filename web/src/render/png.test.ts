import { describe, expect, it } from "vitest";
import { deflateSync } from "node:zlib";
import { PngError, decodePng, encodePng, inflate, isPng, paddedRowBytes, unpackRows } from "./png";

const SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(bytes: Uint8Array): number {
  let c = 0xffffffff;
  for (const b of bytes) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function latin1(text: string): Uint8Array {
  const out = new Uint8Array(text.length);
  for (let i = 0; i < text.length; i++) out[i] = text.charCodeAt(i) & 0xff;
  return out;
}

function adler32(bytes: Uint8Array): number {
  let a = 1;
  let b = 0;
  for (const byte of bytes) {
    a = (a + byte) % 65521;
    b = (b + a) % 65521;
  }
  return ((b << 16) | a) >>> 0;
}

/** A zlib stream of stored (uncompressed) deflate blocks. */
function zlibStored(data: Uint8Array): Uint8Array {
  const blocks: number[] = [0x78, 0x01];
  const MAX = 65535;
  for (let at = 0; at < Math.max(1, data.length); at += MAX) {
    const chunk = data.subarray(at, Math.min(data.length, at + MAX));
    const last = at + MAX >= data.length ? 1 : 0;
    blocks.push(last, chunk.length & 0xff, (chunk.length >> 8) & 0xff, ~chunk.length & 0xff, (~chunk.length >> 8) & 0xff);
    for (const byte of chunk) blocks.push(byte);
  }
  const check = adler32(data);
  blocks.push((check >>> 24) & 0xff, (check >>> 16) & 0xff, (check >>> 8) & 0xff, check & 0xff);
  return new Uint8Array(blocks);
}

function chunk(type: string, body: Uint8Array): Uint8Array {
  const out = new Uint8Array(body.length + 12);
  const view = new DataView(out.buffer);
  view.setUint32(0, body.length);
  for (let i = 0; i < 4; i++) out[4 + i] = type.charCodeAt(i);
  out.set(body, 8);
  const crcInput = out.subarray(4, 8 + body.length);
  view.setUint32(8 + body.length, crc32(crcInput));
  return out;
}

function concat(parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/** Builds a real PNG from raw scanlines, so the decoder is tested on its own output. */
function buildPng(
  width: number,
  height: number,
  bitDepth: number,
  colourType: number,
  scanlines: Uint8Array,
  extra: Uint8Array[] = [],
): Uint8Array {
  const ihdr = new Uint8Array(13);
  const view = new DataView(ihdr.buffer);
  view.setUint32(0, width);
  view.setUint32(4, height);
  ihdr[8] = bitDepth;
  ihdr[9] = colourType;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;
  return concat([
    new Uint8Array(SIGNATURE),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlibStored(scanlines)),
    ...extra,
    chunk("IEND", new Uint8Array(0)),
  ]);
}

/** Two RGB pixels as the decoder hands them back: each triple then an alpha. */
function rgb(row: number[]): number[] {
  const out: number[] = [];
  for (let i = 0; i < row.length; i += 3) out.push(row[i], row[i + 1], row[i + 2], 255);
  return out;
}

/** One filter byte (0 = none) then the row's samples. */
function rows(width: number, height: number, bytesPerRow: number, sample: (x: number, y: number) => number[]): Uint8Array {
  const out = new Uint8Array(height * (1 + bytesPerRow));
  for (let y = 0; y < height; y++) {
    const at = y * (1 + bytesPerRow);
    out[at] = 0;
    for (let x = 0; x < width; x++) {
      const values = sample(x, y);
      for (let c = 0; c < values.length; c++) out[at + 1 + x * values.length + c] = values[c];
    }
  }
  return out;
}

describe("png header", () => {
  it("recognises the signature", () => {
    expect(isPng(new Uint8Array(SIGNATURE))).toBe(true);
    expect(isPng(new Uint8Array([0x89, 0x50, 0x4e, 0x47]))).toBe(false);
    expect(isPng(new Uint8Array(0))).toBe(false);
  });

  it("rejects bytes that are not a PNG", () => {
    expect(() => decodePng(new Uint8Array([1, 2, 3]))).toThrow(PngError);
  });
});

describe("png decode", () => {
  it("decodes an RGBA8 image", () => {
    const png = buildPng(2, 2, 8, 6, rows(2, 2, 8, (x, y) => [x * 10, y * 20, 30, 255]));
    const image = decodePng(png);
    expect(image.width).toBe(2);
    expect(image.height).toBe(2);
    expect(image.bitDepth).toBe(8);
    expect([...image.rgba]).toEqual([
      0, 0, 30, 255, 10, 0, 30, 255,
      0, 20, 30, 255, 10, 20, 30, 255,
    ]);
  });

  it("decodes a greyscale image and expands it to RGBA", () => {
    const png = buildPng(3, 1, 8, 0, rows(3, 1, 3, (x) => [x * 40]));
    const image = decodePng(png);
    expect([...image.rgba]).toEqual([
      0, 0, 0, 255,
      40, 40, 40, 255,
      80, 80, 80, 255,
    ]);
  });

  it("decodes an RGB image with alpha at full opacity", () => {
    const png = buildPng(2, 1, 8, 2, rows(2, 1, 6, (x) => [x, 255 - x, 128]));
    const image = decodePng(png);
    expect([...image.rgba]).toEqual([0, 255, 128, 255, 1, 254, 128, 255]);
  });

  it("keeps a real alpha channel", () => {
    const png = buildPng(2, 1, 8, 6, rows(2, 1, 8, (x) => [0, 0, 0, x * 255]));
    const image = decodePng(png);
    expect(image.rgba[3]).toBe(0);
    expect(image.rgba[7]).toBe(255);
  });

  it("decodes a greyscale+alpha image", () => {
    const png = buildPng(2, 1, 8, 4, rows(2, 1, 4, (x) => [200, x * 255]));
    const image = decodePng(png);
    expect([...image.rgba]).toEqual([200, 200, 200, 0, 200, 200, 200, 255]);
  });

  it("decodes a 1-bit palette image through the palette chunk", () => {
    const palette = new Uint8Array([255, 0, 0, 0, 0, 255]);
    // One bit per sample, most significant bit first, alternating palette entry.
    const scanlines = new Uint8Array(2);
    scanlines[0] = 0;
    let packed = 0;
    for (let x = 0; x < 8; x++) packed = (packed << 1) | (x % 2);
    scanlines[1] = packed & 0xff;
    const png = buildPng(8, 1, 1, 3, scanlines, [chunk("PLTE", palette)]);
    const image = decodePng(png);
    const out: number[] = [];
    for (let x = 0; x < 8; x++) {
      out.push(...(x % 2 === 0 ? [255, 0, 0, 255] : [0, 0, 255, 255]));
    }
    expect([...image.rgba]).toEqual(out);
  });

  it("scales a 4-bit palette image up to one byte per sample", () => {
    const palette = new Uint8Array([0, 0, 0, 255, 255, 255, 255]);
    // Four samples of 4 bits each: entries 0, 1, 0, 1 packed high nibble first.
    // Four samples of four bits take two bytes, the second one padded.
    const scanlines = new Uint8Array(3);
    scanlines[0] = 0;
    scanlines[1] = (0 << 4) | 1;
    scanlines[2] = (0 << 4) | 1;
    const png = buildPng(4, 1, 4, 3, scanlines, [chunk("PLTE", palette)]);
    const image = decodePng(png);
    expect([...image.rgba]).toEqual([
      0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
    ]);
  });

  it("narrows a 16-bit image to 8 bits per channel", () => {
    // Two RGBA pixels, sixteen bits per sample, big endian on the wire.
    const scanlines = new Uint8Array(1 + 2 * 4 * 2);
    scanlines[0] = 0;
    const samples = [0x1234, 0xabcd, 0x00ff, 0x0100, 0xffff, 0x0000, 0x8000, 0x7fff];
    for (let i = 0; i < samples.length; i++) {
      scanlines[1 + i * 2] = (samples[i] >> 8) & 0xff;
      scanlines[2 + i * 2] = samples[i] & 0xff;
    }
    const png = buildPng(2, 1, 16, 6, scanlines);
    const image = decodePng(png);
    expect(image.bitDepth).toBe(16);
    expect([...image.rgba]).toEqual([0x12, 0xab, 0x00, 0x01, 0xff, 0x00, 0x80, 0x7f]);
  });

  it("unfilters the four standard filter types", () => {
    // Two rows of two RGB pixels: the first is a literal, the rest use each filter.
    const raw = new Uint8Array(2 * (1 + 6));
    const first = [10, 20, 30, 40, 50, 60];
    const second = [15, 25, 35, 45, 55, 65];
    raw[0] = 0;
    for (let i = 0; i < 6; i++) raw[1 + i] = first[i];
    // Sub: each sample minus the one three bytes to its left in the row being
    // rebuilt, so the neighbour is the value already reconstructed.
    raw[7] = 1;
    for (let i = 0; i < 6; i++) raw[8 + i] = (second[i] - (i >= 3 ? second[i - 3] : 0)) & 0xff;
    const png = buildPng(2, 2, 8, 2, raw);
    const image = decodePng(png);
    expect([...image.rgba]).toEqual([...rgb(first), ...rgb(second)]);
    expect(image.width).toBe(2);

    const up = new Uint8Array(2 * (1 + 6));
    up[0] = 0;
    for (let i = 0; i < 6; i++) up[1 + i] = first[i];
    up[7] = 2;
    for (let i = 0; i < 6; i++) up[8 + i] = (second[i] - first[i]) & 0xff;
    expect(up.length).toBe(14);
    const upImage = decodePng(buildPng(2, 2, 8, 2, up));
    expect([...upImage.rgba]).toEqual([...rgb(first), ...rgb(second)]);

    // Average and Paeth, both of which need the pixel to the left or above.
    const average = new Uint8Array(2 * (1 + 6));
    average[0] = 0;
    for (let i = 0; i < 6; i++) average[1 + i] = first[i];
    average[7] = 3;
    for (let i = 0; i < 6; i++) {
      const left = i >= 3 ? second[i - 3] : 0;
      average[8 + i] = (second[i] - Math.floor((first[i] + left) / 2)) & 0xff;
    }
    const averageImage = decodePng(buildPng(2, 2, 8, 2, average));
    expect([...averageImage.rgba]).toEqual([...rgb(first), ...rgb(second)]);

    const paeth = new Uint8Array(2 * (1 + 6));
    paeth[0] = 0;
    for (let i = 0; i < 6; i++) paeth[1 + i] = first[i];
    paeth[7] = 4;
    for (let i = 0; i < 6; i++) {
      const left = i >= 3 ? second[i - 3] : 0;
      const up = first[i];
      const upLeft = i >= 3 ? first[i - 3] : 0;
      const estimate = left + up - upLeft;
      const dl = Math.abs(estimate - left);
      const du = Math.abs(estimate - up);
      const dul = Math.abs(estimate - upLeft);
      const predictor = dl <= du && dl <= dul ? left : du <= dul ? up : upLeft;
      paeth[8 + i] = (second[i] - predictor) & 0xff;
    }
    const paethImage = decodePng(buildPng(2, 2, 8, 2, paeth));
    expect([...paethImage.rgba]).toEqual([...rgb(first), ...rgb(second)]);
  });

  it("rejects a chunk length that runs past the end of the file", () => {
    const png = buildPng(2, 2, 8, 6, rows(2, 2, 8, () => [0, 0, 0, 255]));
    const broken = new Uint8Array(png);
    new DataView(broken.buffer).setUint32(16, 0xffff);
    expect(() => decodePng(broken)).toThrow(PngError);
  });

  it("rejects a bad per-chunk crc", () => {
    const png = buildPng(2, 2, 8, 6, rows(2, 2, 8, () => [0, 0, 0, 255]));
    const broken = new Uint8Array(png);
    broken[30] ^= 0xff;
    expect(() => decodePng(broken)).toThrow(PngError);
  });

  it("rejects an IHDR with no dimensions", () => {
    const ihdr = new Uint8Array(13);
    const png = concat([
      new Uint8Array(SIGNATURE),
      chunk("IHDR", ihdr),
      chunk("IDAT", zlibStored(new Uint8Array(1))),
      chunk("IEND", new Uint8Array(0)),
    ]);
    expect(() => decodePng(png)).toThrow(PngError);
  });

  it("rejects a file with no image data", () => {
    const ihdr = new Uint8Array(13);
    const view = new DataView(ihdr.buffer);
    view.setUint32(0, 1);
    view.setUint32(4, 1);
    ihdr[8] = 8;
    ihdr[9] = 6;
    const png = concat([new Uint8Array(SIGNATURE), chunk("IHDR", ihdr), chunk("IEND", new Uint8Array(0))]);
    expect(() => decodePng(png)).toThrow(PngError);
  });
});

describe("readback unpacking", () => {
  it("strips the 256-byte row padding of a GPU copy", () => {
    const width = 3;
    const height = 2;
    const stride = paddedRowBytes(width);
    expect(stride % 256).toBe(0);
    expect(stride).toBe(256);
    const padded = new Uint8Array(stride * height);
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width * 4; x++) padded[y * stride + x] = (y * 64 + x) & 0xff;
      for (let x = width * 4; x < stride; x++) padded[y * stride + x] = 0xee;
    }
    const tight = unpackRows(padded, width, height);
    expect(tight.length).toBe(width * height * 4);
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width * 4; x++) expect(tight[y * width * 4 + x]).toBe((y * 64 + x) & 0xff);
    }
  });

  it("needs no stripping when a row already fills its stride", () => {
    const width = 64;
    expect(paddedRowBytes(width)).toBe(width * 4);
    const padded = new Uint8Array(width * 4).map((_, i) => i & 0xff);
    expect([...unpackRows(padded, width, 1)]).toEqual([...padded]);
  });

  it("rejects a readback smaller than its dimensions", () => {
    expect(() => unpackRows(new Uint8Array(10), 3, 2)).toThrow(PngError);
    expect(() => unpackRows(new Uint8Array(0), 0, 1)).toThrow(PngError);
    expect(() => unpackRows(new Uint8Array(0), 1.5, 1)).toThrow(PngError);
  });
});

describe("png encode", () => {
  it("round-trips through the decoder", () => {
    const rgba = new Uint8Array([10, 20, 30, 255, 0, 0, 0, 0, 255, 128, 64, 32, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    const bytes = encodePng({ width: 3, height: 2, rgba, bitDepth: 8 });
    expect(isPng(bytes)).toBe(true);
    const back = decodePng(bytes);
    expect(back.width).toBe(3);
    expect(back.height).toBe(2);
    expect([...back.rgba]).toEqual([...rgba]);
  });

  it("encodes a single pixel", () => {
    const back = decodePng(encodePng({ width: 1, height: 1, rgba: new Uint8Array([7, 8, 9, 10]), bitDepth: 8 }));
    expect([...back.rgba]).toEqual([7, 8, 9, 10]);
  });

  it("rejects pixels smaller than the dimensions", () => {
    expect(() => encodePng({ width: 2, height: 2, rgba: new Uint8Array(15), bitDepth: 8 })).toThrow(PngError);
    expect(() => encodePng({ width: 0, height: 1, rgba: new Uint8Array(0), bitDepth: 8 })).toThrow(PngError);
  });
});

describe("zlib inflate", () => {
  it("reads a stored block back", () => {
    const payload = new Uint8Array([1, 2, 3, 4, 5]);
    expect([...inflate(zlibStored(payload))]).toEqual([1, 2, 3, 4, 5]);
  });

  it("reads a real deflate stream, huffman blocks and all", () => {
    // Node's own deflate is the oracle here: it picks whichever block types the
    // data needs, so this covers the huffman paths a stored block never reaches.
    const cases = [
      "abcabc",
      Array.from({ length: 512 }, (_, i) => String.fromCharCode(32 + (i * 7) % 90)).join(""),
      "the quick brown fox jumps over the lazy dog ".repeat(40),
      JSON.stringify({ nodes: [{ id: "n1", node: "generator" }], output: "n1" }),
    ];
    for (const text of cases) {
      const packed = new Uint8Array(deflateSync(latin1(text)));
      expect([...inflate(packed)]).toEqual([...latin1(text)]);
    }
  });

  it("reads an uncompressed deflate stream back", () => {
    const text = "stored blocks only, so the payload repeats";
    const packed = new Uint8Array(deflateSync(latin1(text), { level: 0 }));
    expect([...inflate(packed)]).toEqual([...latin1(text)]);
  });

  it("rejects a bad header and a truncated stream", () => {
    expect(() => inflate(new Uint8Array([0x77, 0x00]))).toThrow(/compression method/);
    expect(() => inflate(new Uint8Array([0x78]))).toThrow(/too short/);
    expect(() => inflate(new Uint8Array([0x78, 0x01, 0x07, 0x00, 0x00, 0x00, 0x00]))).toThrow(PngError);
  });

  it("rejects a stored block whose length does not match its complement", () => {
    const bytes = new Uint8Array([0x78, 0x01, 0x01, 0x04, 0x00, 0x00, 0x00, 1, 2, 3, 4, 0, 0, 0, 0]);
    expect(() => inflate(bytes)).toThrow(/length check/);
  });
});
