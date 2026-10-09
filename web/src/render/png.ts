const PNG_SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

export class PngError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PngError";
  }
}

export interface PngImage {
  width: number;
  height: number;
  /** RGBA8, row-major with no padding. */
  rgba: Uint8Array;
  /** Original bit depth, 8 for RGBA8 output and 16 for RGBA16. */
  bitDepth: number;
}

const CHANNELS: Record<number, number> = { 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 };

/** True when the bytes start with the PNG signature. */
export function isPng(bytes: Uint8Array): boolean {
  if (bytes.length < 8) return false;
  for (let i = 0; i < 8; i++) if (bytes[i] !== PNG_SIGNATURE[i]) return false;
  return true;
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(bytes: Uint8Array, from: number, to: number): number {
  let c = 0xffffffff;
  for (let i = from; i < to; i++) c = CRC_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** Decodes a PNG into RGBA. Supports bit depths 1/2/4/8/16 and all colour types. */
export function decodePng(bytes: Uint8Array): PngImage {
  if (!isPng(bytes)) throw new PngError("not a PNG");
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let off = 8;
  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  let interlace = 0;
  let palette: Uint8Array | null = null;
  let trns: Uint8Array | null = null;
  const idat: Uint8Array[] = [];
  while (off + 8 <= bytes.length) {
    const length = dv.getUint32(off);
    const type = String.fromCharCode(bytes[off + 4], bytes[off + 5], bytes[off + 6], bytes[off + 7]);
    const start = off + 8;
    if (length > bytes.length - start) throw new PngError("PNG chunk overruns the file");
    // Every chunk carries a CRC over its type and body, which is the only check
    // that the bytes are the ones the encoder wrote.
    const declared = dv.getUint32(start + length);
    if (crc32(bytes, off + 4, start + length) !== declared) {
      throw new PngError(`PNG ${type} chunk has a bad crc`);
    }
    if (type === "IHDR") {
      width = dv.getUint32(start);
      height = dv.getUint32(start + 4);
      bitDepth = bytes[start + 8];
      colorType = bytes[start + 9];
      interlace = bytes[start + 12];
    } else if (type === "PLTE") {
      palette = bytes.slice(start, start + length);
    } else if (type === "tRNS") {
      trns = bytes.slice(start, start + length);
    } else if (type === "IDAT") {
      idat.push(bytes.slice(start, start + length));
    } else if (type === "IEND") {
      break;
    }
    off = start + length + 4;
  }
  if (width <= 0 || height <= 0) throw new PngError("PNG has no image header");
  if (interlace !== 0) throw new PngError("interlaced PNG is not supported");
  if (!CHANNELS[colorType]) throw new PngError(`unsupported PNG colour type ${colorType}`);
  if (colorType === 3 && !palette) throw new PngError("indexed PNG has no palette");
  if (idat.length === 0) throw new PngError("PNG has no image data");
  const raw = inflate(concat(idat));
  const channels = CHANNELS[colorType];
  const bpp = Math.max(1, (channels * bitDepth) / 8);
  const stride = Math.ceil((channels * bitDepth * width) / 8);
  if (raw.length < (stride + 1) * height) throw new PngError("PNG image data is truncated");
  const lines = unfilter(raw, height, stride, bpp);
  return expand(lines, width, height, bitDepth, colorType, palette, trns);
}

function concat(parts: Uint8Array[]): Uint8Array {
  let total = 0;
  for (const p of parts) total += p.length;
  const out = new Uint8Array(total);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

function unfilter(raw: Uint8Array, height: number, stride: number, bpp: number): Uint8Array {
  const out = new Uint8Array(stride * height);
  let src = 0;
  for (let y = 0; y < height; y++) {
    const filter = raw[src++];
    const row = y * stride;
    const prev = row - stride;
    for (let x = 0; x < stride; x++) {
      const rawByte = raw[src + x];
      const a = x >= bpp ? out[row + x - bpp] : 0;
      const b = y > 0 ? out[prev + x] : 0;
      const c = x >= bpp && y > 0 ? out[prev + x - bpp] : 0;
      let value: number;
      switch (filter) {
        case 0:
          value = rawByte;
          break;
        case 1:
          value = rawByte + a;
          break;
        case 2:
          value = rawByte + b;
          break;
        case 3:
          value = rawByte + ((a + b) >> 1);
          break;
        case 4:
          value = rawByte + paeth(a, b, c);
          break;
        default:
          throw new PngError(`unknown PNG filter ${filter}`);
      }
      out[row + x] = value & 0xff;
    }
    src += stride;
  }
  return out;
}

function paeth(a: number, b: number, c: number): number {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  if (pa <= pb && pa <= pc) return a;
  return pb <= pc ? b : c;
}


interface CodeSpec {
  base: number;
  extra: number;
}

const LENGTH_BASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

const LENGTH_SPECS: CodeSpec[] = LENGTH_BASE.map((base, i) => ({ base, extra: LENGTH_EXTRA[i] }));
const DIST_SPECS: CodeSpec[] = DIST_BASE.map((base, i) => ({ base, extra: DIST_EXTRA[i] }));

// The fixed literal/length alphabet of RFC 1951 3.2.6: 144 codes of eight
// bits, 112 of nine, the 24 length codes of seven, and eight eight-bit codes.
const LIT_CODES = [
  ...new Array(144).fill(8),
  ...new Array(112).fill(9),
  ...new Array(24).fill(7),
  ...new Array(8).fill(8),
];

const LIT_TABLE = buildTable(LIT_CODES);
const FIXED_DIST_CODES = new Array(30).fill(5);
const FIXED_DIST_TABLE = buildTable(FIXED_DIST_CODES);
const CLEN_ORDER = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

interface HuffTable {
  counts: number[];
  symbols: number[];
}

function buildTable(lengths: number[]): HuffTable {
  const counts = new Array(16).fill(0);
  for (const l of lengths) counts[l]++;
  counts[0] = 0;
  const symbols: number[] = [];
  const offsets = new Array(16).fill(0);
  for (let i = 1; i < 16; i++) offsets[i] = offsets[i - 1] + counts[i - 1];
  for (let sym = 0; sym < lengths.length; sym++) {
    if (lengths[sym]) symbols[offsets[lengths[sym]]++] = sym;
  }
  return { counts, symbols };
}

class BitReader {
  private bitPos = 0;
  constructor(private readonly data: Uint8Array, private readonly startByte: number) {}
  read(count: number): number {
    let value = 0;
    for (let i = 0; i < count; i++) {
      const byte = this.startByte + (this.bitPos >> 3);
      if (byte >= this.data.length) throw new PngError("deflate stream ends mid-symbol");
      const bit = (this.data[byte] >> (this.bitPos & 7)) & 1;
      value |= bit << i;
      this.bitPos++;
    }
    return value;
  }
  align(): void {
    this.bitPos = (this.bitPos + 7) & ~7;
  }
  readHuffman(table: HuffTable): number {
    let code = 0;
    let len = 0;
    let first = 0;
    let index = 0;
    for (;;) {
      code |= this.read(1);
      len++;
      const count = table.counts[len];
      if (code - first < count) return table.symbols[index + (code - first)];
      index += count;
      first = (first + count) << 1;
      code <<= 1;
      if (len > 15) throw new PngError("invalid huffman code");
    }
  }
}

/**
 * The two tables of a dynamic block: the code lengths are themselves run-length
 * coded with the 16/17/18 symbols, so the table is read before it can be built.
 */
function readDynamicTables(br: BitReader): { lit: HuffTable; dist: HuffTable } {
  const hlit = br.read(5) + 257;
  const hdist = br.read(5) + 1;
  const hclen = br.read(4) + 4;
  if (hlit > 286 || hdist > 30) throw new PngError("too many deflate code lengths");
  const clens = new Array(19).fill(0);
  for (let i = 0; i < hclen; i++) clens[CLEN_ORDER[i]] = br.read(3);
  const clenTable = buildTable(clens);
  const lengths = new Array(hlit + hdist).fill(0);
  let at = 0;
  while (at < lengths.length) {
    const symbol = br.readHuffman(clenTable);
    if (symbol < 16) {
      lengths[at++] = symbol;
      continue;
    }
    let repeat = 0;
    let value = 0;
    if (symbol === 16) {
      if (at === 0) throw new PngError("deflate repeat with no previous length");
      value = lengths[at - 1];
      repeat = 3 + br.read(2);
    } else if (symbol === 17) {
      repeat = 3 + br.read(3);
    } else {
      repeat = 11 + br.read(7);
    }
    if (at + repeat > lengths.length) throw new PngError("deflate length repeat runs past the tables");
    for (let i = 0; i < repeat; i++) lengths[at++] = value;
  }
  return {
    lit: buildTable(lengths.slice(0, hlit)),
    dist: buildTable(lengths.slice(hlit)),
  };
}

/** DEFLATE (RFC 1951) inside a zlib wrapper (RFC 1950). */
export function inflate(data: Uint8Array): Uint8Array {
  if (data.length < 2) throw new PngError("zlib stream is too short");
  const cmf = data[0];
  if ((cmf & 0x0f) !== 8) throw new PngError("unsupported zlib compression method");
  if (((cmf << 8) | data[1]) % 31 !== 0) throw new PngError("bad zlib header check");
  if (data[1] & 0x20) throw new PngError("preset dictionaries are not supported");
  const br = new BitReader(data, 2);
  const out: number[] = [];
  let last = 0;
  do {
    last = br.read(1);
    const type = br.read(2);
    if (type === 0) {
      br.align();
      const len = br.read(16);
      const nlen = br.read(16);
      if ((len ^ 0xffff) !== nlen) throw new PngError("stored block length check failed");
      for (let i = 0; i < len; i++) out.push(br.read(8));
      continue;
    }
    if (type === 3) throw new PngError("invalid deflate block type");
    const tables = type === 1 ? { lit: LIT_TABLE, dist: FIXED_DIST_TABLE } : readDynamicTables(br);
    for (;;) {
      const sym = br.readHuffman(tables.lit);
      if (sym === 256) break;
      if (sym < 256) {
        out.push(sym);
        continue;
      }
      const li = sym - 257;
      if (li >= LENGTH_SPECS.length) throw new PngError("invalid length symbol");
      const length = LENGTH_SPECS[li].base + br.read(LENGTH_SPECS[li].extra);
      const ds = br.readHuffman(tables.dist);
      if (ds >= DIST_SPECS.length) throw new PngError("invalid distance symbol");
      const distance = DIST_SPECS[ds].base + br.read(DIST_SPECS[ds].extra);
      if (distance > out.length) throw new PngError("distance points before the output start");
      const from = out.length - distance;
      for (let i = 0; i < length; i++) out.push(out[from + i]);
    }
  } while (!last);
  return Uint8Array.from(out);
}

/** Bytes per GPU readback row: copies demand 256-byte alignment. */
export function paddedRowBytes(width: number): number {
  return Math.ceil((width * 4) / 256) * 256;
}

/** Strips GPU readback padding, returning tightly packed RGBA. */
export function unpackRows(padded: Uint8Array, width: number, height: number): Uint8Array {
  if (!Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0) {
    throw new PngError("bad image dimensions");
  }
  const stride = paddedRowBytes(width);
  if (padded.length < stride * height) throw new PngError("readback is smaller than its dimensions");
  const out = new Uint8Array(width * height * 4);
  const row = width * 4;
  for (let y = 0; y < height; y++) out.set(padded.subarray(y * stride, y * stride + row), y * row);
  return out;
}

/** Encodes RGBA8 as a PNG. Stored deflate blocks only: no compressor to tune. */
export function encodePng(image: PngImage): Uint8Array {
  if (!Number.isInteger(image.width) || !Number.isInteger(image.height) || image.width <= 0 || image.height <= 0) {
    throw new PngError("bad image dimensions");
  }
  if (image.rgba.length < image.width * image.height * 4) throw new PngError("pixels are smaller than the dimensions");
  const row = image.width * 4;
  const raw = new Uint8Array((row + 1) * image.height);
  for (let y = 0; y < image.height; y++) {
    raw[y * (row + 1)] = 0;
    raw.set(image.rgba.subarray(y * row, y * row + row), y * (row + 1) + 1);
  }
  const ihdr = new Uint8Array(13);
  const view = new DataView(ihdr.buffer);
  view.setUint32(0, image.width);
  view.setUint32(4, image.height);
  ihdr[8] = 8;
  ihdr[9] = 6;
  const parts = [new Uint8Array(PNG_SIGNATURE), encodeChunk("IHDR", ihdr), encodeChunk("IDAT", zlibStored(raw)), encodeChunk("IEND", new Uint8Array(0))];
  let total = 0;
  for (const p of parts) total += p.length;
  const out = new Uint8Array(total);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

function encodeChunk(type: string, body: Uint8Array): Uint8Array {
  const out = new Uint8Array(body.length + 12);
  const view = new DataView(out.buffer);
  view.setUint32(0, body.length);
  for (let i = 0; i < 4; i++) out[4 + i] = type.charCodeAt(i) & 0xff;
  out.set(body, 8);
  view.setUint32(8 + body.length, crc32(out, 4, 8 + body.length));
  return out;
}

/** zlib wrapping stored blocks around the bytes, the shape the decoder reads. */
function zlibStored(data: Uint8Array): Uint8Array {
  const out: number[] = [0x78, 0x01];
  const MAX = 65535;
  for (let at = 0; at < Math.max(1, data.length); at += MAX) {
    const end = Math.min(data.length, at + MAX);
    const last = end >= data.length ? 1 : 0;
    const len = end - at;
    out.push(last, len & 0xff, (len >> 8) & 0xff, ~len & 0xff, (~len >> 8) & 0xff);
    for (let i = at; i < end; i++) out.push(data[i]);
  }
  let a = 1;
  let b = 0;
  for (const byte of data) {
    a = (a + byte) % 65521;
    b = (b + a) % 65521;
  }
  const check = ((b << 16) | a) >>> 0;
  out.push((check >>> 24) & 0xff, (check >>> 16) & 0xff, (check >>> 8) & 0xff, check & 0xff);
  return new Uint8Array(out);
}

/** Widens a sample of the given bit depth to 8 bits. */
function scale8(v: number, depth: number, max: number): number {
  return depth === 8 || depth === 16 ? v : Math.round((v * 255) / max);
}

function expand(
  lines: Uint8Array,
  width: number,
  height: number,
  depth: number,
  colorType: number,
  palette: Uint8Array | null,
  trns: Uint8Array | null,
): PngImage {
  const channels = CHANNELS[colorType];
  const stride = Math.ceil((channels * depth * width) / 8);
  const max = (1 << depth) - 1;
  const rgba = new Uint8Array(width * height * 4);
  for (let y = 0; y < height; y++) {
    const row = y * stride;
    for (let x = 0; x < width; x++) {
      const o = (y * width + x) * 4;
      const at = (c: number): number => {
        if (depth === 16) {
          const b = row + (x * channels + c) * 2;
          return ((lines[b] << 8) | lines[b + 1]) >> 8;
        }
        if (depth === 8) return lines[row + x * channels + c];
        const bitPos = (x * channels + c) * depth;
        const byte = lines[row + (bitPos >> 3)];
        return (byte >> (8 - depth - (bitPos & 7))) & max;
      };
      switch (colorType) {
        case 0: {
          const g = scale8(at(0), depth, max);
          rgba[o] = g;
          rgba[o + 1] = g;
          rgba[o + 2] = g;
          rgba[o + 3] = 255;
          break;
        }
        case 2:
          rgba[o] = scale8(at(0), depth, max);
          rgba[o + 1] = scale8(at(1), depth, max);
          rgba[o + 2] = scale8(at(2), depth, max);
          rgba[o + 3] = 255;
          break;
        case 3: {
          const idx = at(0);
          const p = palette ?? new Uint8Array(0);
          const pi = idx * 3;
          rgba[o] = pi + 2 < p.length ? p[pi] : 0;
          rgba[o + 1] = pi + 2 < p.length ? p[pi + 1] : 0;
          rgba[o + 2] = pi + 2 < p.length ? p[pi + 2] : 0;
          rgba[o + 3] = trns && idx < trns.length ? trns[idx] : 255;
          break;
        }
        case 4: {
          const g = scale8(at(0), depth, max);
          rgba[o] = g;
          rgba[o + 1] = g;
          rgba[o + 2] = g;
          rgba[o + 3] = scale8(at(1), depth, max);
          break;
        }
        case 6:
          rgba[o] = scale8(at(0), depth, max);
          rgba[o + 1] = scale8(at(1), depth, max);
          rgba[o + 2] = scale8(at(2), depth, max);
          rgba[o + 3] = scale8(at(3), depth, max);
          break;
        default:
          throw new PngError(`unsupported PNG colour type ${colorType}`);
      }
    }
  }
  return { width, height, rgba, bitDepth: depth === 16 ? 16 : 8 };
}
