import { describe, expect, it } from "vitest";
import { GltfError, base64Decode, parseGltf } from "./gltf";

/** Builds a GLB from a JSON object and an optional binary chunk. */
export function buildGlb(json: unknown, bin?: Uint8Array): ArrayBuffer {
  const jsonText = new TextEncoder().encode(JSON.stringify(json));
  const jsonPad = (4 - (jsonText.length % 4)) % 4;
  const binPad = bin ? (4 - (bin.length % 4)) % 4 : 0;
  const total = 12 + 8 + jsonText.length + jsonPad + (bin ? 8 + bin.length + binPad : 0);
  const buf = new ArrayBuffer(total);
  const dv = new DataView(buf);
  const bytes = new Uint8Array(buf);
  dv.setUint32(0, 0x46546c67, true);
  dv.setUint32(4, 2, true);
  dv.setUint32(8, total, true);
  dv.setUint32(12, jsonText.length + jsonPad, true);
  dv.setUint32(16, 0x4e4f534a, true);
  bytes.set(jsonText, 20);
  for (let i = 0; i < jsonPad; i++) bytes[20 + jsonText.length + i] = 0x20;
  if (bin) {
    const off = 20 + jsonText.length + jsonPad;
    dv.setUint32(off, bin.length + binPad, true);
    dv.setUint32(off + 4, 0x004e4942, true);
    bytes.set(bin, off + 8);
  }
  return buf;
}

/** A single triangle: 3 positions, 3 normals, 3 uvs, 3 indices. */
export function triangleGlb(): ArrayBuffer {
  const positions = new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]);
  const normals = new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1]);
  const uvs = new Float32Array([0, 0, 1, 0, 0, 1]);
  const indices = new Uint16Array([0, 1, 2]);
  const bin = new Uint8Array(positions.byteLength + normals.byteLength + uvs.byteLength + indices.byteLength);
  let off = 0;
  bin.set(new Uint8Array(positions.buffer), off);
  off += positions.byteLength;
  bin.set(new Uint8Array(normals.buffer), off);
  off += normals.byteLength;
  bin.set(new Uint8Array(uvs.buffer), off);
  off += uvs.byteLength;
  bin.set(new Uint8Array(indices.buffer), off);
  return buildGlb(
    {
      asset: { version: "2.0" },
      scene: 0,
      scenes: [{ nodes: [0] }],
      nodes: [{ name: "root", mesh: 0, translation: [10, 20, 30] }],
      meshes: [{ name: "tri", primitives: [{ attributes: { POSITION: 0, NORMAL: 1, TEXCOORD_0: 2 }, indices: 3, material: 0 }] }],
      materials: [{ name: "brick", pbrMetallicRoughness: { baseColorFactor: [1, 0.5, 0.25, 1], metallicFactor: 0.25, roughnessFactor: 0.75 } }],
      accessors: [
        { bufferView: 0, componentType: 5126, count: 3, type: "VEC3" },
        { bufferView: 1, componentType: 5126, count: 3, type: "VEC3" },
        { bufferView: 2, componentType: 5126, count: 3, type: "VEC2" },
        { bufferView: 3, componentType: 5123, count: 3, type: "SCALAR" },
      ],
      bufferViews: [
        { buffer: 0, byteOffset: 0, byteLength: positions.byteLength },
        { buffer: 0, byteOffset: positions.byteLength, byteLength: normals.byteLength },
        { buffer: 0, byteOffset: positions.byteLength + normals.byteLength, byteLength: uvs.byteLength },
        { buffer: 0, byteOffset: positions.byteLength + normals.byteLength + uvs.byteLength, byteLength: indices.byteLength },
      ],
      buffers: [{ byteLength: bin.length }],
    },
    bin,
  );
}

describe("gltf parser", () => {
  it("reads positions, normals, uvs and indices from a tiny glb", () => {
    const gltf = parseGltf(triangleGlb());
    expect(gltf.meshes).toHaveLength(1);
    const prim = gltf.meshes[0].primitives[0];
    expect(Array.from(prim.positions)).toEqual([0, 0, 0, 1, 0, 0, 0, 1, 0]);
    expect(Array.from(prim.normals ?? [])).toEqual([0, 0, 1, 0, 0, 1, 0, 0, 1]);
    expect(Array.from(prim.uvs ?? [])).toEqual([0, 0, 1, 0, 0, 1]);
    expect(Array.from(prim.indices)).toEqual([0, 1, 2]);
  });

  it("reads material factors and the node transform", () => {
    const gltf = parseGltf(triangleGlb());
    expect(gltf.materials[0].name).toBe("brick");
    expect(gltf.materials[0].baseColorFactor).toEqual([1, 0.5, 0.25, 1]);
    expect(gltf.materials[0].metallicFactor).toBe(0.25);
    expect(gltf.materials[0].roughnessFactor).toBe(0.75);
    const node = gltf.nodes[0];
    expect(node.name).toBe("root");
    expect(node.mesh).toBe(0);
    expect([node.matrix[12], node.matrix[13], node.matrix[14]]).toEqual([10, 20, 30]);
  });

  it("rejects a bad magic and a truncated buffer", () => {
    const bad = new ArrayBuffer(16);
    expect(() => parseGltf(bad)).toThrow(GltfError);
    const glb = triangleGlb();
    expect(() => parseGltf(glb.slice(0, 20))).toThrow(GltfError);
  });

  it("rejects an out-of-range index and a bad component type", () => {
    const base = {
      asset: { version: "2.0" },
      meshes: [{ primitives: [{ attributes: { POSITION: 0 }, indices: 1 }] }],
      accessors: [
        { bufferView: 0, componentType: 5126, count: 3, type: "VEC3" },
        { bufferView: 0, componentType: 5123, count: 3, type: "SCALAR" },
      ],
      bufferViews: [{ buffer: 0, byteOffset: 0, byteLength: 36 }],
      buffers: [{ byteLength: 36 }],
    };
    const bin = new Uint8Array(36);
    const dv = new DataView(bin.buffer);
    dv.setUint16(0, 99, true);
    expect(() => parseGltf(buildGlb(base, bin))).toThrow(/index out of range/);
    const badType = { ...base, accessors: [{ ...base.accessors[0], componentType: 9999 }] };
    expect(() => parseGltf(buildGlb(badType, bin))).toThrow(/component type/);
  });

  it("decodes base64 with padding", () => {
    expect(Array.from(base64Decode("AQID"))).toEqual([1, 2, 3]);
    expect(Array.from(base64Decode("AQI="))).toEqual([1, 2]);
    expect(() => base64Decode("!!!!")).toThrow(GltfError);
  });
});
