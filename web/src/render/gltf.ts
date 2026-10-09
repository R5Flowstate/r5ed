import { decodePng } from "./png";

export interface GltfPrimitive {
  positions: Float32Array;
  normals: Float32Array | null;
  uvs: Float32Array | null;
  indices: Uint32Array;
  material: number;
}

export interface GltfMesh {
  name: string;
  primitives: GltfPrimitive[];
}

export interface GltfMaterial {
  name: string;
  baseColorFactor: [number, number, number, number];
  metallicFactor: number;
  roughnessFactor: number;
  emissiveFactor: [number, number, number];
  doubleSided: boolean;
  baseColorTexture: number;
  metallicRoughnessTexture: number;
  normalTexture: number;
  emissiveTexture: number;
  occlusionTexture: number;
}

export interface GltfNode {
  name: string;
  mesh: number;
  /** Column-major local transform. */
  matrix: Float32Array;
}

export interface GltfImage {
  name: string;
  mimeType: string;
  /** Decoded pixels as RGBA8, when the image is storable. */
  pixels: Uint8Array | null;
  width: number;
  height: number;
}

export interface GltfTexture {
  name: string;
  source: number;
  sampler: number;
}

export interface GltfSampler {
  magFilter: number;
  minFilter: number;
  wrapS: number;
  wrapT: number;
}

export interface GltfScene {
  nodes: number[];
}

export interface Gltf {
  meshes: GltfMesh[];
  materials: GltfMaterial[];
  nodes: GltfNode[];
  scenes: GltfScene[];
  scene: number;
  images: GltfImage[];
  textures: GltfTexture[];
  samplers: GltfSampler[];
}

export class GltfError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "GltfError";
  }
}

const GLB_MAGIC = 0x46546c67;
const CHUNK_JSON = 0x4e4f534a;
const CHUNK_BIN = 0x004e4942;

const COMPONENT_BYTES: Record<number, number> = {
  5120: 1, 5121: 1, 5122: 2, 5123: 2, 5125: 4, 5126: 4,
};

const TYPE_COMPONENTS: Record<string, number> = {
  SCALAR: 1, VEC2: 2, VEC3: 3, VEC4: 4, MAT4: 16,
};

function u32(view: DataView, off: number): number {
  return view.getUint32(off, true);
}

/** Splits a GLB container into its JSON and BIN chunks. */
export function parseGlb(data: ArrayBuffer): { json: Record<string, unknown>; bin: Uint8Array | null } {
  if (data.byteLength < 12) throw new GltfError("glb too short");
  const view = new DataView(data);
  if (u32(view, 0) !== GLB_MAGIC) throw new GltfError("bad glb magic");
  const version = u32(view, 4);
  if (version !== 2) throw new GltfError(`unsupported glb version ${version}`);
  const total = Math.min(u32(view, 8), data.byteLength);
  let off = 12;
  let json: Record<string, unknown> | null = null;
  let bin: Uint8Array | null = null;
  while (off + 8 <= total) {
    const length = u32(view, off);
    const type = u32(view, off + 4);
    const start = off + 8;
    const end = start + length;
    if (length > total - start) throw new GltfError("glb chunk overruns the file");
    if (type === CHUNK_JSON && !json) {
      const text = new TextDecoder().decode(new Uint8Array(data, start, length));
      let parsed: unknown;
      try {
        parsed = JSON.parse(text);
      } catch {
        throw new GltfError("glb JSON chunk does not parse");
      }
      if (!parsed || typeof parsed !== "object") throw new GltfError("glb JSON chunk is not an object");
      json = parsed as Record<string, unknown>;
    } else if (type === CHUNK_BIN && !bin) {
      bin = new Uint8Array(data.slice(start, end));
    }
    off = end + ((end % 4 === 0) ? 0 : 4 - (end % 4));
  }
  if (!json) throw new GltfError("glb has no JSON chunk");
  return { json, bin };
}

function asArray(json: Record<string, unknown>, key: string): unknown[] {
  const v = json[key];
  return Array.isArray(v) ? v : [];
}

function record(v: unknown): Record<string, unknown> {
  return v && typeof v === "object" ? (v as Record<string, unknown>) : {};
}

function num(v: unknown, fallback = 0): number {
  return typeof v === "number" && Number.isFinite(v) ? v : fallback;
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function intOr(v: unknown, fallback: number): number {
  return typeof v === "number" && Number.isInteger(v) ? v : fallback;
}

interface BufferViewInfo {
  byteOffset: number;
  byteLength: number;
  byteStride: number;
}

function readAccessor(
  json: Record<string, unknown>,
  buffers: Uint8Array[],
  index: number,
): { data: Float32Array; components: number; count: number } {
  const accessors = asArray(json, "accessors");
  const acc = record(accessors[index]);
  const count = intOr(acc.count, 0);
  if (count < 0) throw new GltfError("accessor count is negative");
  const type = str(acc.type);
  const components = TYPE_COMPONENTS[type];
  if (!components) throw new GltfError(`unsupported accessor type ${type}`);
  const componentType = intOr(acc.componentType, 0);
  const componentBytes = COMPONENT_BYTES[componentType];
  if (!componentBytes) throw new GltfError(`unsupported component type ${componentType}`);
  const out = new Float32Array(count * components);
  if (count === 0) return { data: out, components, count };
  if (intOr(acc.bufferView, -1) < 0) {
    // A sparse-only accessor with no view reads as zeros.
    return { data: out, components, count };
  }
  const views = asArray(json, "bufferViews");
  const view = readBufferView(views, buffers, intOr(acc.bufferView, -1));
  const base = intOr(acc.byteOffset, 0);
  const stride = view.byteStride || components * componentBytes;
  const elemBytes = components * componentBytes;
  if (base < 0 || base + (count - 1) * stride + elemBytes > view.byteLength) {
    throw new GltfError(`accessor ${index} reads past its buffer view`);
  }
  const dv = new DataView(view.data.buffer, view.data.byteOffset, view.data.byteLength);
  for (let i = 0; i < count; i++) {
    const at = base + i * stride;
    for (let c = 0; c < components; c++) {
      out[i * components + c] = readComponent(dv, at + c * componentBytes, componentType);
    }
  }
  return { data: out, components, count };
}

function readComponent(dv: DataView, at: number, componentType: number): number {
  if (at < 0 || at + COMPONENT_BYTES[componentType] > dv.byteLength) {
    throw new GltfError("accessor element out of bounds");
  }
  switch (componentType) {
    case 5120:
      return dv.getInt8(at);
    case 5121:
      return dv.getUint8(at);
    case 5122:
      return dv.getInt16(at, true);
    case 5123:
      return dv.getUint16(at, true);
    case 5125:
      return dv.getUint32(at, true);
    case 5126:
      return dv.getFloat32(at, true);
    default:
      throw new GltfError(`unsupported component type ${componentType}`);
  }
}

function readBufferView(views: unknown[], buffers: Uint8Array[], index: number): BufferViewInfo & { data: Uint8Array } {
  if (index < 0 || index >= views.length) throw new GltfError("buffer view index out of range");
  const v = record(views[index]);
  const bufferIndex = intOr(v.buffer, -1);
  if (bufferIndex < 0 || bufferIndex >= buffers.length) throw new GltfError("buffer index out of range");
  const buffer = buffers[bufferIndex];
  const byteOffset = intOr(v.byteOffset, 0);
  const byteLength = intOr(v.byteLength, 0);
  if (byteOffset < 0 || byteLength < 0 || byteOffset + byteLength > buffer.byteLength) {
    throw new GltfError("buffer view is out of bounds");
  }
  return {
    byteOffset,
    byteLength,
    byteStride: intOr(v.byteStride, 0),
    data: buffer.subarray(byteOffset, byteOffset + byteLength),
  };
}

function defaultMaterial(): GltfMaterial {
  return {
    name: "",
    baseColorFactor: [1, 1, 1, 1],
    metallicFactor: 1,
    roughnessFactor: 1,
    emissiveFactor: [0, 0, 0],
    doubleSided: false,
    baseColorTexture: -1,
    metallicRoughnessTexture: -1,
    normalTexture: -1,
    emissiveTexture: -1,
    occlusionTexture: -1,
  };
}

function textureIndex(t: unknown): number {
  return intOr(record(t).index, -1);
}

function readMaterials(json: Record<string, unknown>): GltfMaterial[] {
  return asArray(json, "materials").map((m) => {
    const r = record(m);
    const pbr = record(r.pbrMetallicRoughness);
    const bc = Array.isArray(pbr.baseColorFactor) ? (pbr.baseColorFactor as unknown[]) : [];
    const ef = Array.isArray(r.emissiveFactor) ? (r.emissiveFactor as unknown[]) : [];
    return {
      ...defaultMaterial(),
      name: str(r.name),
      baseColorFactor: [num(bc[0], 1), num(bc[1], 1), num(bc[2], 1), bc.length > 3 ? num(bc[3], 1) : 1],
      metallicFactor: num(pbr.metallicFactor, 1),
      roughnessFactor: num(pbr.roughnessFactor, 1),
      emissiveFactor: [num(ef[0]), num(ef[1]), num(ef[2])],
      doubleSided: r.doubleSided === true,
      baseColorTexture: textureIndex(pbr.baseColorTexture),
      metallicRoughnessTexture: textureIndex(pbr.metallicRoughnessTexture),
      normalTexture: textureIndex(r.normalTexture),
      emissiveTexture: textureIndex(r.emissiveTexture),
      occlusionTexture: textureIndex(r.occlusionTexture),
    };
  });
}

function readPrimitives(
  json: Record<string, unknown>,
  buffers: Uint8Array[],
  prims: unknown[],
): GltfPrimitive[] {
  const out: GltfPrimitive[] = [];
  for (const p of prims) {
    const r = record(p);
    const attrs = record(r.attributes);
    const posIndex = intOr(attrs.POSITION, -1);
    if (posIndex < 0) continue;
    const pos = readAccessor(json, buffers, posIndex);
    if (pos.components !== 3) throw new GltfError("POSITION accessor is not VEC3");
    let normals: Float32Array | null = null;
    if (typeof attrs.NORMAL === "number") {
      const n = readAccessor(json, buffers, attrs.NORMAL);
      normals = n.components === 3 ? n.data : null;
    }
    let uvs: Float32Array | null = null;
    if (typeof attrs.TEXCOORD_0 === "number") {
      const t = readAccessor(json, buffers, attrs.TEXCOORD_0);
      uvs = t.count === pos.count ? t.data.slice(0, pos.count * 2) : null;
    }
    let indices: Uint32Array;
    if (typeof r.indices === "number") {
      const idx = readAccessor(json, buffers, r.indices);
      indices = new Uint32Array(idx.data.length);
      for (let i = 0; i < idx.data.length; i++) {
        const v = idx.data[i];
        if (v < 0 || v >= pos.count) throw new GltfError("index out of range");
        indices[i] = v;
      }
    } else {
      indices = new Uint32Array(pos.count);
      for (let i = 0; i < pos.count; i++) indices[i] = i;
    }
    out.push({ positions: pos.data, normals, uvs, indices, material: intOr(r.material, 0) });
  }
  return out;
}


function readNodes(json: Record<string, unknown>): GltfNode[] {
  return asArray(json, "nodes").map((n) => {
    const r = record(n);
    const matrix = new Float32Array(16);
    if (Array.isArray(r.matrix) && r.matrix.length === 16) {
      for (let i = 0; i < 16; i++) matrix[i] = num((r.matrix as unknown[])[i]);
    } else {
      const t = Array.isArray(r.translation) ? (r.translation as unknown[]) : [];
      const q = Array.isArray(r.rotation) ? (r.rotation as unknown[]) : [];
      const s = Array.isArray(r.scale) ? (r.scale as unknown[]) : [];
      const qx = num(q[0]), qy = num(q[1]), qz = num(q[2]), qw = q[3] === undefined ? 1 : num(q[3]);
      const x2 = qx + qx, y2 = qy + qy, z2 = qz + qz;
      const xx = qx * x2, xy = qx * y2, xz = qx * z2;
      const yy = qy * y2, yz = qy * z2, zz = qz * z2;
      const wx = qw * x2, wy = qw * y2, wz = qw * z2;
      const sx = num(s[0], 1), sy = num(s[1], 1), sz = num(s[2], 1);
      matrix[0] = (1 - (yy + zz)) * sx;
      matrix[1] = (xy + wz) * sx;
      matrix[2] = (xz - wy) * sx;
      matrix[4] = (xy - wz) * sy;
      matrix[5] = (1 - (xx + zz)) * sy;
      matrix[6] = (yz + wx) * sy;
      matrix[8] = (xz + wy) * sz;
      matrix[9] = (yz - wx) * sz;
      matrix[10] = (1 - (xx + yy)) * sz;
      matrix[12] = num(t[0]);
      matrix[13] = num(t[1]);
      matrix[14] = num(t[2]);
      matrix[15] = 1;
    }
    return { name: str(r.name), mesh: intOr(r.mesh, -1), matrix };
  });
}

function readImages(json: Record<string, unknown>, buffers: Uint8Array[]): GltfImage[] {
  const views = asArray(json, "bufferViews");
  return asArray(json, "images").map((im) => {
    const r = record(im);
    const mimeType = str(r.mimeType) || "image/png";
    const name = str(r.name);
    const bi = intOr(r.bufferView, -1);
    if (bi < 0) return { name, mimeType, pixels: null, width: 0, height: 0 };
    const view = readBufferView(views, buffers, bi);
    if (mimeType !== "image/png") return { name, mimeType, pixels: null, width: 0, height: 0 };
    const png = decodePng(view.data);
    return { name, mimeType, pixels: png.rgba, width: png.width, height: png.height };
  });
}

function readSamplers(json: Record<string, unknown>): GltfSampler[] {
  return asArray(json, "samplers").map((s) => {
    const r = record(s);
    return {
      magFilter: intOr(r.magFilter, 9729),
      minFilter: intOr(r.minFilter, 9987),
      wrapS: intOr(r.wrapS, 10497),
      wrapT: intOr(r.wrapT, 10497),
    };
  });
}

function readTextures(json: Record<string, unknown>): GltfTexture[] {
  return asArray(json, "textures").map((t) => {
    const r = record(t);
    return { name: str(r.name), source: intOr(r.source, -1), sampler: intOr(r.sampler, -1) };
  });
}


const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/** Decodes standard base64, ignoring whitespace; rejects invalid characters. */
export function base64Decode(text: string): Uint8Array {
  const clean = text.replace(/\s+/g, "");
  const out = new Uint8Array(Math.floor((clean.length * 3) / 4));
  let acc = 0;
  let bits = 0;
  let o = 0;
  for (let i = 0; i < clean.length; i++) {
    const ch = clean[i];
    if (ch === "=") break;
    const v = B64.indexOf(ch);
    if (v < 0) throw new GltfError("invalid base64 character");
    acc = (acc << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[o++] = (acc >> bits) & 0xff;
    }
  }
  return out.subarray(0, o);
}

/** Parses a GLB buffer into the renderer's document model. */
export function parseGltf(data: ArrayBuffer): Gltf {
  const { json, bin } = parseGlb(data);
  const buffers: Uint8Array[] = [];
  // A buffer with no `uri` is the GLB BIN chunk, already at index 0.
  if (bin) buffers.push(bin);
  for (const entry of asArray(json, "buffers")) {
    const uri = str(record(entry).uri);
    if (!uri) continue;
    if (!uri.startsWith("data:")) throw new GltfError("only GLB-embedded buffers are supported");
    const comma = uri.indexOf(",");
    if (comma < 0) throw new GltfError("malformed data uri");
    if (!uri.slice(5, comma).endsWith(";base64")) throw new GltfError("only base64 data uris are supported");
    buffers.push(base64Decode(uri.slice(comma + 1)));
  }
  const meshes: GltfMesh[] = asArray(json, "meshes").map((m) => {
    const r = record(m);
    return { name: str(r.name), primitives: readPrimitives(json, buffers, asArray(r, "primitives")) };
  });
  const scenes: GltfScene[] = asArray(json, "scenes").map((s) => ({
    nodes: asArray(record(s), "nodes").filter((n): n is number => typeof n === "number"),
  }));
  return {
    meshes,
    materials: readMaterials(json),
    nodes: readNodes(json),
    scenes,
    scene: intOr(json.scene, 0),
    images: readImages(json, buffers),
    textures: readTextures(json),
    samplers: readSamplers(json),
  };
}
