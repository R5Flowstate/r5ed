import { describe, expect, it } from "vitest";
import { frustumFromViewProj } from "./frustum";
import { lookAt, mat4, multiply, perspective } from "../math/mat4";
import { REF_INSTANCE_FLOATS, REF_PICK_BASE, RefPropLayer, parseRefProps, refPropId, refPropIndex } from "./refprops";
import type { GeometryChunk } from "./scene";

/** A table as the decompiler writes it. */
function table(models: string[], props: { model: number; origin: [number, number, number]; scale?: number }[]): Uint8Array {
  const names = models.map((m) => new TextEncoder().encode(m));
  let head = 16 + names.reduce((n, b) => n + 2 + b.length, 0);
  head = (head + 3) & ~3;
  const bytes = new Uint8Array(head + props.length * 48);
  const view = new DataView(bytes.buffer);
  view.setUint32(0, 0x50525052, true);
  view.setUint32(4, 1, true);
  view.setUint32(8, models.length, true);
  view.setUint32(12, props.length, true);
  let o = 16;
  for (const b of names) {
    view.setUint16(o, b.length, true);
    bytes.set(b, o + 2);
    o += 2 + b.length;
  }
  o = head;
  for (const p of props) {
    view.setUint32(o, p.model, true);
    for (let k = 0; k < 3; k++) view.setFloat32(o + 12 + k * 4, p.origin[k], true);
    view.setFloat32(o + 36, p.scale ?? 1, true);
    o += 48;
  }
  return bytes;
}

function cube(): GeometryChunk {
  const v: number[] = [];
  for (const [x, y, z] of [[-8, -8, -8], [8, -8, -8], [8, 8, -8], [-8, 8, -8], [-8, -8, 8], [8, -8, 8], [8, 8, 8], [-8, 8, 8]]) v.push(x, y, z, 0, 0, 1, 0, 0);
  const i = [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7, 4, 1, 2, 6, 1, 6, 5];
  return { key: "cube", vertices: new Float32Array(v), indices: new Uint32Array(i), bounds: { min: [-8, -8, -8], max: [8, 8, 8] }, subRanges: [{ start: 0, count: i.length, material: "m" }] };
}

describe("reference props", () => {
  it("reads the table the decompiler writes", () => {
    const t = parseRefProps(table(["mdl/a.rmdl", "mdl/bb.rmdl"], [{ model: 1, origin: [1, 2, 3], scale: 2 }, { model: 0, origin: [-5, 0, 0] }]));
    expect(t.models).toEqual(["mdl/a.rmdl", "mdl/bb.rmdl"]);
    expect(t.count).toBe(2);
    expect(t.model[0]).toBe(1);
    expect(Array.from(t.origin.subarray(0, 3))).toEqual([1, 2, 3]);
    expect(t.scale[0]).toBe(2);
    expect(() => parseRefProps(new Uint8Array(8))).toThrow();
  });

  it("names a record by an id no document object can have", () => {
    expect(refPropIndex(refPropId(41))).toBe(41);
    expect(refPropIndex("4c2b8080-5a9a-4fcc-9851-86f836a95831")).toBeNull();
  });

  it("draws loaded models in view, stops drawing a hidden record, and picks by surface", () => {
    const layer = new RefPropLayer(parseRefProps(table(["mdl/a.rmdl"], [{ model: 0, origin: [0, 0, 0] }, { model: 0, origin: [0, 64, 0] }])));
    const chunks = [cube()];
    const view = mat4();
    const proj = mat4();
    const vp = mat4();
    lookAt(view, [-200, 0, 0], [0, 0, 0], [0, 0, 1]);
    perspective(proj, Math.PI / 2, 1, 1, 10000);
    multiply(vp, proj, view);
    const f = frustumFromViewProj(vp);
    const out: { chunk: number; first: number; count: number }[] = [];
    expect(layer.cull(f, [-200, 0, 0], out)).toBe(0);
    layer.bindModels(() => 0, chunks);
    expect(layer.cull(f, [-200, 0, 0], out)).toBe(2);
    const hit = layer.pick(chunks, [-200, 0, 0], [1, 0, 0], 1 << 20);
    expect(hit?.record).toBe(0);
    expect(hit?.position[0]).toBeCloseTo(-8);
    layer.setHidden(new Set([0]));
    const slot = layer.slotOf[0];
    expect(Array.from(layer.instances.subarray(slot * REF_INSTANCE_FLOATS, slot * REF_INSTANCE_FLOATS + 16)).every((x) => x === 0)).toBe(true);
    expect(layer.instances[layer.slotOf[1] * REF_INSTANCE_FLOATS + 19]).toBe(REF_PICK_BASE + 1);
    expect(layer.pick(chunks, [-200, 0, 0], [1, 0, 0], 1 << 20)).toBeNull();
    layer.setHidden(new Set());
    expect(layer.instances[slot * REF_INSTANCE_FLOATS + 15]).toBe(1);
  });
});
