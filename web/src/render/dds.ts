export type DdsFormat =
  | "bc1" | "bc2" | "bc3" | "bc4" | "bc5" | "bc6h" | "bc6hs" | "bc7"
  | "rgba8" | "bgra8" | "r8" | "rg8";

export interface DdsInfo {
  width: number;
  height: number;
  mipCount: number;
  format: DdsFormat;
  /** Bytes per 4x4 block, or per pixel for uncompressed formats. */
  blockBytes: number;
  uncompressed: boolean;
  hasAlpha: boolean;
  arraySize: number;
}

export class DdsError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DdsError";
  }
}

const MAGIC = 0x20534444;
const HEADER_BYTES = 128;
const DDPF_ALPHAPIXELS = 0x1;
const DDPF_FOURCC = 0x4;
const DDPF_RGB = 0x40;
const DDPF_LUMINANCE = 0x20000;

const FOURCC_DXT1 = 0x31545844;
const FOURCC_DXT2 = 0x32545844;
const FOURCC_DXT3 = 0x33545844;
const FOURCC_DXT4 = 0x34545844;
const FOURCC_DXT5 = 0x35545844;
const FOURCC_ATI1 = 0x31495441;
const FOURCC_BC4U = 0x55343442;
const FOURCC_ATI2 = 0x32495441;
const FOURCC_BC5U = 0x55353442;
const FOURCC_DX10 = 0x30315844;

/** DXGI format numbers, from dxgiformat.h. */
export const DXGI = {
  BC1_TYPELESS: 70, BC1_UNORM: 71, BC1_UNORM_SRGB: 72,
  BC2_TYPELESS: 73, BC2_UNORM: 74, BC2_UNORM_SRGB: 75,
  BC3_TYPELESS: 76, BC3_UNORM: 77, BC3_UNORM_SRGB: 78,
  BC4_TYPELESS: 79, BC4_UNORM: 80, BC4_SNORM: 81,
  BC5_TYPELESS: 82, BC5_UNORM: 83, BC5_SNORM: 84,
  BC6H_TYPELESS: 94, BC6H_UF16: 95, BC6H_SF16: 96,
  BC7_TYPELESS: 97, BC7_UNORM: 98, BC7_UNORM_SRGB: 99,
  R8G8B8A8_UNORM: 28, R8G8B8A8_UNORM_SRGB: 29,
  B8G8R8A8_UNORM: 87, B8G8R8X8_UNORM: 88,
  R8_UNORM: 61, R8G8_UNORM: 62,
} as const;

const BC_FORMATS: Record<number, DdsFormat> = {
  [DXGI.BC1_TYPELESS]: "bc1", [DXGI.BC1_UNORM]: "bc1", [DXGI.BC1_UNORM_SRGB]: "bc1",
  [DXGI.BC2_TYPELESS]: "bc2", [DXGI.BC2_UNORM]: "bc2", [DXGI.BC2_UNORM_SRGB]: "bc2",
  [DXGI.BC3_TYPELESS]: "bc3", [DXGI.BC3_UNORM]: "bc3", [DXGI.BC3_UNORM_SRGB]: "bc3",
  [DXGI.BC4_TYPELESS]: "bc4", [DXGI.BC4_UNORM]: "bc4", [DXGI.BC4_SNORM]: "bc4",
  [DXGI.BC5_TYPELESS]: "bc5", [DXGI.BC5_UNORM]: "bc5", [DXGI.BC5_SNORM]: "bc5",
  [DXGI.BC6H_TYPELESS]: "bc6h", [DXGI.BC6H_UF16]: "bc6h", [DXGI.BC6H_SF16]: "bc6hs",
  [DXGI.BC7_TYPELESS]: "bc7", [DXGI.BC7_UNORM]: "bc7", [DXGI.BC7_UNORM_SRGB]: "bc7",
};

const BLOCK_BYTES: Record<DdsFormat, number> = {
  bc1: 8, bc2: 16, bc3: 16, bc4: 8, bc5: 16, bc6h: 16, bc6hs: 16, bc7: 16,
  rgba8: 4, bgra8: 4, r8: 1, rg8: 2,
};

function le32(dv: DataView, off: number): number {
  return dv.getUint32(off, true);
}


/** Parses a DDS header. Throws on a bad magic, size or unsupported layout. */
export function parseDdsHeader(bytes: Uint8Array): DdsInfo {
  if (bytes.length < HEADER_BYTES) throw new DdsError("DDS header is truncated");
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (le32(dv, 0) !== MAGIC) throw new DdsError("not a DDS file");
  const size = le32(dv, 4);
  if (size !== 124) throw new DdsError(`unexpected DDS header size ${size}`);
  const height = le32(dv, 12);
  const width = le32(dv, 16);
  const mipCount = le32(dv, 28);
  const pfFlags = le32(dv, 80);
  const fourCC = le32(dv, 84);
  const rgbBitCount = le32(dv, 88);
  if (width <= 0 || height <= 0 || width > 32768 || height > 32768) {
    throw new DdsError("DDS dimensions are out of range");
  }
  if (mipCount < 1 || mipCount > 16) throw new DdsError("DDS mip count is out of range");
  let format: DdsFormat;
  let arraySize = 1;
  if (pfFlags & DDPF_FOURCC) {
    if (fourCC === FOURCC_DXT1) format = "bc1";
    else if (fourCC === FOURCC_DXT2 || fourCC === FOURCC_DXT4) format = "bc2";
    else if (fourCC === FOURCC_DXT3 || fourCC === FOURCC_DXT5) format = "bc3";
    else if (fourCC === FOURCC_ATI1 || fourCC === FOURCC_BC4U) format = "bc4";
    else if (fourCC === FOURCC_ATI2 || fourCC === FOURCC_BC5U) format = "bc5";
    else if (fourCC === FOURCC_DX10) {
      if (bytes.length < HEADER_BYTES + 20) throw new DdsError("DX10 extension is truncated");
      const dxgi = le32(dv, 128);
      const mapped = BC_FORMATS[dxgi];
      if (!mapped) throw new DdsError(`unsupported DXGI format ${dxgi}`);
      format = mapped;
      // DX10 header: dxgiFormat, resourceDimension, miscFlag, arraySize, miscFlags2.
      arraySize = Math.max(1, le32(dv, 128 + 12));
    } else {
      throw new DdsError(`unsupported FourCC 0x${fourCC.toString(16)}`);
    }
  } else if (pfFlags & DDPF_RGB) {
    if (rgbBitCount === 32) format = "bgra8";
    else if (rgbBitCount === 24) throw new DdsError("24-bit DDS is not supported; use 32-bit");
    else throw new DdsError(`unsupported RGB bit count ${rgbBitCount}`);
  } else if (pfFlags & DDPF_LUMINANCE) {
    if (rgbBitCount === 8) format = "r8";
    else if (rgbBitCount === 16) format = "rg8";
    else throw new DdsError(`unsupported luminance bit count ${rgbBitCount}`);
  } else {
    throw new DdsError("DDS pixel format has no recognised flags");
  }
  const uncompressed = format === "rgba8" || format === "bgra8" || format === "r8" || format === "rg8";
  return {
    width,
    height,
    mipCount,
    format,
    blockBytes: BLOCK_BYTES[format],
    uncompressed,
    hasAlpha: (pfFlags & DDPF_ALPHAPIXELS) !== 0 || !uncompressed,
    arraySize,
  };
}

/** Header size in bytes; the DX10 extension adds 20 bytes. */
export function ddsHeaderSize(bytes: Uint8Array): number {
  if (bytes.length < HEADER_BYTES) return HEADER_BYTES;
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const pfFlags = le32(dv, 80);
  return pfFlags & DDPF_FOURCC && le32(dv, 84) === FOURCC_DX10 ? 148 : 128;
}

/** Byte size of one mip level of the given dimensions. */
export function mipSize(info: DdsInfo, w: number, h: number): number {
  if (info.uncompressed) return Math.max(1, (w * h * info.blockBytes + 3) & ~3);
  return Math.max(1, Math.ceil(w / 4)) * Math.max(1, Math.ceil(h / 4)) * info.blockBytes;
}

/** Byte offset and length of mip level `level` within the pixel data. */
export function mipLayout(info: DdsInfo, level: number): { offset: number; size: number; width: number; height: number } {
  if (level < 0 || level >= info.mipCount) throw new DdsError("mip level out of range");
  let offset = 0;
  let w = info.width;
  let h = info.height;
  for (let i = 0; i < level; i++) {
    offset += mipSize(info, w, h);
    w = Math.max(1, w >> 1);
    h = Math.max(1, h >> 1);
  }
  return { offset, size: mipSize(info, w, h), width: w, height: h };
}

/** Splits a DDS file into its mip levels as raw block data. */
export function readDdsMips(bytes: Uint8Array): {
  info: DdsInfo;
  mips: { width: number; height: number; data: Uint8Array }[];
} {
  const info = parseDdsHeader(bytes);
  const dataStart = ddsHeaderSize(bytes);
  const out: { width: number; height: number; data: Uint8Array }[] = [];
  for (let level = 0; level < info.mipCount; level++) {
    const layout = mipLayout(info, level);
    const start = dataStart + layout.offset;
    if (start + layout.size > bytes.length) {
      if (level === 0) throw new DdsError("DDS pixel data is truncated");
      break;
    }
    out.push({ width: layout.width, height: layout.height, data: bytes.subarray(start, start + layout.size) });
  }
  if (out.length === 0) throw new DdsError("DDS has no readable mip levels");
  return { info, mips: out };
}
