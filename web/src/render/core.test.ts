import { describe, expect, it } from "vitest";

// jsdom has no WebGPU flag globals; the pools only carry these values through.
const COPY_DST = 8;
const INDEX = 16;
const VERTEX = 32;
import { BufferPoolError, DrawList, FloatPool, IndexPool } from "./buffers";
import { LineBuilder, boxCorners, boxWireframe, planeBasis, sampleSpline, splineSampleCount, spotCone, transformedBounds } from "./overlay";
import { adaptiveGrid, gridFade, gridForSize, buildGridLines, snapToGrid, defaultGrid, GRID_SIZES } from "./grid";
import { CATEGORY_COLORS, CATEGORY_ORDER, billboardSize, categoryFor, defaultAtlasOptions, layoutAtlas, atlasSlot, atlasSize } from "./atlas";
import { categoryLabel, categoryStyle, glyphRect } from "./glyphs";
import { parseCommand, commandJson, addObjects, removeObjects, replaceObjects, batch } from "../doc/command";
import { parseObject, parseDoc, DocumentStore } from "../doc/store";
import type { Core, EditBuffers, Id, MeshBuffers, PickHit } from "../core/api";
import type { DocObject } from "../doc/model";

/** Records what was asked of the pool, so the tests need no GPU. */
function fakeDevice(): GPUDevice & { created: { label: string; size: number; usage: number }[] } {
  const created: { label: string; size: number; usage: number }[] = [];
  const buffer = (label: string, size: number, usage: number): GPUBuffer => {
    created.push({ label, size, usage });
    const store = new ArrayBuffer(Math.max(16, size));
    return {
      size,
      usage,
      label,
      getMappedRange: () => store,
      unmap: () => undefined,
      destroy: () => undefined,
    } as unknown as GPUBuffer;
  };
  return {
    created,
    createBuffer: (desc: GPUBufferDescriptor) => buffer(desc.label ?? "", desc.size, desc.usage),
    queue: { writeBuffer: () => undefined },
  } as unknown as GPUDevice & { created: { label: string; size: number; usage: number }[] };
}

describe("float pool", () => {
  it("hands out disjoint ranges that hold what the caller wrote", () => {
    const device = fakeDevice();
    const pool = new FloatPool(device, "test", VERTEX);
    pool.begin();
    const a = pool.alloc(4);
    const b = pool.alloc(3);
    expect(b).toBe(4);
    pool.view(a, 4).set([1, 2, 3, 4]);
    pool.view(b, 3).set([5, 6, 7]);
    expect(Array.from(pool.view(a, 7))).toEqual([1, 2, 3, 4, 5, 6, 7]);
    expect(pool.liveElements).toBe(7);
  });

  it("grows without losing what was already written", () => {
    const device = fakeDevice();
    const pool = new FloatPool(device, "test", VERTEX);
    pool.begin();
    pool.view(pool.alloc(2), 2).set([10, 20]);
    const far = pool.alloc(20000);
    pool.view(far, 1)[0] = 99;
    expect(Array.from(pool.view(0, 2))).toEqual([10, 20]);
    expect(pool.liveElements).toBe(20002);
  });

  it("uploads a buffer sized to the live range and null when empty", () => {
    const device = fakeDevice();
    const pool = new FloatPool(device, "verts", VERTEX);
    pool.begin();
    expect(pool.upload()).toBeNull();
    pool.alloc(6);
    const first = pool.upload();
    expect(first).not.toBeNull();
    expect(device.created[0].size).toBe(24);
    // A second upload with no new writes reuses the same buffer.
    pool.begin();
    pool.alloc(6);
    expect(pool.upload()).toBe(first);
    // More data forces a new, larger buffer.
    pool.begin();
    pool.alloc(64);
    const bigger = pool.upload();
    expect(bigger).not.toBe(first);
    expect(device.created[device.created.length - 1].size).toBe(256);
  });

  it("refuses a range it has not reserved and a bad allocation", () => {
    const pool = new FloatPool(fakeDevice(), "test", VERTEX);
    pool.begin();
    pool.alloc(4);
    expect(() => pool.view(2, 4)).toThrow(BufferPoolError);
    expect(() => pool.view(0, 9)).toThrow(BufferPoolError);
    expect(() => pool.alloc(0)).toThrow(BufferPoolError);
    expect(() => pool.alloc(1.5)).toThrow(BufferPoolError);
  });
});

describe("index pool", () => {
  it("stores indices and reports the live count", () => {
    const device = fakeDevice();
    const pool = new IndexPool(device, "indices", INDEX | COPY_DST);
    pool.begin();
    const at = pool.alloc(6);
    pool.view(at, 6).set([0, 1, 2, 2, 1, 3]);
    expect(Array.from(pool.view(at, 6))).toEqual([0, 1, 2, 2, 1, 3]);
    expect(pool.upload()).not.toBeNull();
    expect(device.created[0].usage).toBe(INDEX | COPY_DST);
  });

  it("grows for a large index range", () => {
    const pool = new IndexPool(fakeDevice(), "indices", INDEX | COPY_DST);
    pool.begin();
    const at = pool.alloc(300000);
    pool.view(at, 1)[0] = 7;
    expect(pool.view(at, 1)[0]).toBe(7);
  });
});

describe("draw list", () => {
  it("splits groups by pass and by material", () => {
    const list = new DrawList();
    list.push({ firstIndex: 0, indexCount: 3, firstInstance: 0, instanceCount: 2, material: "a", translucent: false, pick: 0 });
    list.push({ firstIndex: 3, indexCount: 3, firstInstance: 2, instanceCount: 1, material: "a", translucent: false, pick: 0 });
    list.push({ firstIndex: 6, indexCount: 6, firstInstance: 3, instanceCount: 1, material: "glass", translucent: true, pick: 5 });
    expect(list.all()).toHaveLength(3);
    expect(list.byPass(false)).toHaveLength(2);
    expect(list.byPass(true)).toHaveLength(1);
    const byMaterial = list.byMaterial();
    expect([...byMaterial.keys()]).toEqual(["a"]);
    expect(byMaterial.get("a")).toHaveLength(2);
    list.clear();
    expect(list.all()).toHaveLength(0);
  });
});


describe("overlay geometry", () => {
  it("builds twelve segments for a box wireframe", () => {
    const lines = new LineBuilder();
    boxWireframe({ min: [0, 0, 0], max: [8, 8, 8] }, [1, 1, 1, 1], lines);
    expect(lines.vertexCount).toBe(24);
    const mesh = lines.build();
    expect(mesh.count).toBe(24);
    expect(mesh.positions).toHaveLength(72);
    expect(mesh.colors).toHaveLength(96);
    for (const v of mesh.colors) expect(v).toBeCloseTo(1, 6);
  });

  it("numbers the box corners so the edges join the right pairs", () => {
    const corners = boxCorners([0, 0, 0], [8, 8, 8]);
    expect(corners[0]).toEqual([0, 0, 0]);
    expect(corners[3]).toEqual([8, 8, 0]);
    expect(corners[7]).toEqual([8, 8, 8]);
    expect(corners[4]).toEqual([0, 0, 8]);
  });

  it("closes a circle and leaves a partial arc open", () => {
    const lines = new LineBuilder();
    lines.circle([0, 0, 0], [0, 0, 1], 10, [1, 0, 0, 1], 12);
    const full = lines.vertexCount;
    lines.circle([0, 0, 0], [0, 0, 1], 10, [1, 0, 0, 1], 12, 0, Math.PI);
    expect(lines.vertexCount).toBe(full + 24);
    lines.circle([0, 0, 0], [0, 0, 1], 0, [1, 0, 0, 1], 12);
    expect(lines.vertexCount).toBe(full + 24);
  });

  it("builds a plane basis whose cross product is the normal", () => {
    for (const n of [[1, 0, 0], [0, 1, 0], [0, 0, 1], [1 / Math.sqrt(3), 1 / Math.sqrt(3), 1 / Math.sqrt(3)]] as [number, number, number][]) {
      const [a, b] = planeBasis(n);
      const cross: [number, number, number] = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
      cross.forEach((v, i) => expect(v).toBeCloseTo(n[i], 5));
      expect(Math.hypot(a[0], a[1], a[2])).toBeCloseTo(1, 5);
      expect(Math.hypot(b[0], b[1], b[2])).toBeCloseTo(1, 5);
    }
  });

  it("draws a spot cone as a rim plus spokes", () => {
    const lines = new LineBuilder();
    spotCone(lines, [0, 0, 0], [0, 0, -1], 128, 0.5, [1, 1, 1, 1]);
    // Twelve spokes and a twelve-segment rim.
    expect(lines.vertexCount).toBe(48);
  });

  it("samples a spline through its endpoints", () => {
    const points: [number, number, number][] = [[0, 0, 0], [64, 0, 0], [128, 0, 64]];
    const curve = sampleSpline(points);
    expect(curve).toHaveLength(splineSampleCount(points));
    expect(curve[0]).toEqual([0, 0, 0]);
    const last = curve[curve.length - 1];
    expect(last[0]).toBeCloseTo(128, 5);
    expect(last[2]).toBeCloseTo(64, 5);
    expect(sampleSpline([])).toEqual([]);
    expect(sampleSpline([[1, 2, 3]])).toEqual([[1, 2, 3]]);
    expect(sampleSpline([[0, 0, 0], [1, 1, 1]])).toHaveLength(2);
  });

  it("transforms a box conservatively", () => {
    const scaleOnly = new Float32Array([2, 0, 0, 0, 0, 2, 0, 0, 0, 0, 2, 0, 5, 6, 7, 1]);
    const b = transformedBounds({ min: [-1, -1, -1], max: [1, 1, 1] }, scaleOnly);
    expect(b.min).toEqual([3, 4, 5]);
    expect(b.max).toEqual([7, 8, 9]);
  });
});

describe("grid", () => {
  it("keeps a grid visible from map-scale distances", () => {
    const near = adaptiveGrid(16, 512, [0, 0, 0]);
    const far = adaptiveGrid(16, 40000, [30000, -20000, 0]);
    expect(near.minor).toBe(16);
    expect(far.minor).toBeGreaterThanOrEqual(256);
    expect(far.fadeDistance).toBeGreaterThan(40000);
    expect(far.center?.[0]).toBeCloseTo(30000, -4);
    const lines = buildGridLines(far).count / 2;
    expect(lines).toBeLessThanOrEqual(4096 + 2);
    expect(lines).toBeGreaterThan(50);
  });

  it("scales the grid with the snap size", () => {
    expect(gridForSize(16).minor).toBe(16);
    expect(gridForSize(64).minor).toBe(64);
    expect(gridForSize(1024).minor).toBe(1024);
    expect(gridForSize(1024).extent).toBeGreaterThan(gridForSize(16).extent);
    for (const size of GRID_SIZES) {
      const g = gridForSize(size);
      expect(g.minor).toBeGreaterThan(0);
      expect(g.majorEvery).toBeGreaterThanOrEqual(1);
    }
    expect(defaultGrid().minor).toBeGreaterThan(0);
  });

  it("emits paired vertices and marks the major lines", () => {
    const grid = buildGridLines(defaultGrid());
    expect(grid.count % 2).toBe(0);
    expect(grid.positions).toHaveLength(grid.count * 3);
    expect(grid.weight).toHaveLength(grid.count);
    const major = grid.weight.filter((w) => w > 0.5).length;
    expect(major).toBeGreaterThan(0);
    expect(major % 2).toBe(0);
    // Every minor line lies on a multiple of the spacing.
    for (let i = 0; i < grid.count; i += 2) {
      const x = grid.positions[i * 3];
      const y = grid.positions[i * 3 + 1];
      expect(Math.abs(x % defaultGrid().minor)).toBeCloseTo(0, 4);
      expect(Math.abs(y % defaultGrid().minor)).toBeCloseTo(0, 4);
    }
  });

  it("fades lines out with distance and snaps positions", () => {
    expect(gridFade(0, 1000)).toBeCloseTo(1, 6);
    expect(gridFade(500, 1000)).toBeCloseTo(0.75, 6);
    expect(gridFade(1000, 1000)).toBe(0);
    expect(gridFade(5000, 1000)).toBe(0);
    expect(gridFade(10, 0)).toBe(1);
    expect(snapToGrid([17, -17, 55], 16)).toEqual([16, -16, 55]);
  });
});

describe("glyph atlas", () => {
  it("packs one slot per category inside the image", () => {
    const options = defaultAtlasOptions(CATEGORY_ORDER.length);
    const atlas = layoutAtlas([...CATEGORY_ORDER], options);
    expect(atlas.entries.size).toBe(CATEGORY_ORDER.length);
    expect(atlas.pixels).toHaveLength(atlas.width * atlas.height * 4);
    const { width, height } = atlasSize(CATEGORY_ORDER.length, options);
    expect(atlas.width).toBe(width);
    expect(atlas.height).toBe(height);
    for (const name of CATEGORY_ORDER) {
      const entry = atlas.entries.get(name);
      if (!entry) throw new Error(`no entry for ${name}`);
      expect(entry.uv[2]).toBeGreaterThan(entry.uv[0]);
      expect(entry.uv[3]).toBeGreaterThan(entry.uv[1]);
      expect(entry.uv[0]).toBeGreaterThanOrEqual(0);
      expect(entry.uv[2]).toBeLessThanOrEqual(1);
    }
    // Two neighbouring cells must not overlap.
    const a = atlas.entries.get(CATEGORY_ORDER[0]) as { uv: [number, number, number, number] };
    const b = atlas.entries.get(CATEGORY_ORDER[1]) as { uv: [number, number, number, number] };
    expect(a.uv[2]).toBeLessThanOrEqual(b.uv[0]);
  });

  it("keeps every slot inside the atlas", () => {
    const options = defaultAtlasOptions(3);
    expect(atlasSize(3, options).height).toBeGreaterThan(0);
    const slot = atlasSlot(2, options);
    expect(slot.x + options.cell).toBeLessThanOrEqual(atlasSize(3, options).width);
  });

  it("maps a classname to a category with a colour for each", () => {
    expect(categoryFor("trigger_multiple")).toBe("trigger");
    expect(categoryFor("light")).toBe("light");
    expect(categoryFor("light_environment_volume")).toBe("light");
    expect(categoryFor("info_target")).toBe("info_target");
    expect(categoryFor("func_door")).toBe("func");
    expect(categoryFor("env_explosion")).toBe("env");
    expect(categoryFor("zipline_start")).toBe("path");
    expect(categoryFor("prop_dynamic")).toBe("prop");
    expect(categoryFor("totally_unknown")).toBe("other");
    for (const c of CATEGORY_ORDER) expect(CATEGORY_COLORS[c]).toHaveLength(3);
  });

  it("gives each category a two-letter label and a theme colour", () => {
    for (const c of CATEGORY_ORDER) {
      expect(categoryLabel(c)).toHaveLength(2);
      expect(categoryStyle(c, "dark").fill).toMatch(/^rgb\(/);
      expect(categoryStyle(c, "light").stroke).toMatch(/rgba?\(/);
    }
  });

  it("resolves a glyph rect and falls back for an unknown category", () => {
    const atlas = layoutAtlas([...CATEGORY_ORDER], defaultAtlasOptions(CATEGORY_ORDER.length));
    expect(glyphRect(atlas, "light")).toEqual(atlas.entries.get("light")?.uv);
    expect(glyphRect(atlas, "nope" as never)).toEqual([0, 0, 1, 1]);
  });

  it("keeps a billboard readable at any camera distance", () => {
    expect(billboardSize(1000, 1000)).toBeCloseTo(26, 6);
    expect(billboardSize(2000, 1000)).toBeCloseTo(52, 6);
  });
});

describe("command json", () => {
  it("writes each op with the name the core expects", () => {
    const object = { id: "a", name: "n", layer: "l", parent: null, transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] }, hidden: false, locked: false, kind: { type: "group" } } as unknown as DocObject;
    expect(JSON.parse(commandJson(addObjects([object]))).op).toBe("add_objects");
    expect(JSON.parse(commandJson(removeObjects(["a"]))).ids).toEqual(["a"]);
    expect(JSON.parse(commandJson(replaceObjects([object]))).op).toBe("replace_objects");
    const nested = batch("edit", [addObjects([object]), removeObjects(["b"])]);
    const parsed = JSON.parse(commandJson(nested));
    expect(parsed.label).toBe("edit");
    expect(parsed.commands).toHaveLength(2);
  });

  it("writes file bytes as a plain array and clears them with null", () => {
    const set = (path: string, bytes: number[] | null) => JSON.parse(commandJson({ op: "set_file", path, bytes }));
    expect(set("terrain/a.f32", [1, 2, 3]).bytes).toEqual([1, 2, 3]);
    expect(set("terrain/a.f32", null).bytes).toBeNull();
  });

  it("rejects command json that is malformed", () => {
    expect(() => parseCommand("{")).toThrow(/does not parse/);
    expect(() => parseCommand("[]")).toThrow(/not an object/);
    expect(() => parseCommand("{}")).toThrow(/no op/);
    expect(parseCommand('{"op":"add_objects","objects":[]}').op).toBe("add_objects");
  });
});

/** A core that keeps one object and reports the ids it was asked about. */
class TinyCore implements Core {
  objects: DocObject[] = [];
  lastCommand = "";

  load(docJson: string): string {
    return docJson;
  }

  save(): string {
    return JSON.stringify({ format: 1, meta: { stem: "mp_rr_test" }, layers: [{ id: "l1", name: "default", visible: true, locked: false }], objects: this.objects, settings: {} });
  }

  apply(commandJson: string): Id[] {
    this.lastCommand = commandJson;
    const command = JSON.parse(commandJson) as { op: string; objects?: DocObject[]; ids?: string[] };
    if (command.op === "add_objects") this.objects.push(...(command.objects ?? []));
    if (command.op === "remove_objects") this.objects = this.objects.filter((o) => !(command.ids ?? []).includes(o.id));
    if (command.op === "replace_objects") {
      for (const next of command.objects ?? []) {
        const at = this.objects.findIndex((o) => o.id === next.id);
        if (at >= 0) this.objects[at] = next;
        else this.objects.push(next);
      }
    }
    return (command.objects ?? []).map((o) => o.id).concat(command.ids ?? []);
  }

  undo(): Id[] {
    return [];
  }

  redo(): Id[] {
    return [];
  }

  canUndo(): boolean {
    return false;
  }

  canRedo(): boolean {
    return false;
  }

  object(id: Id): string | null {
    const found = this.objects.find((o) => o.id === id);
    return found ? JSON.stringify(found) : null;
  }

  objectIds(): Id[] {
    return this.objects.map((o) => o.id);
  }

  objectMesh(): MeshBuffers | null {
    return null;
  }

  objectEdges(): { positions: Float32Array; edges: Uint32Array } | null {
    return null;
  }

  soupEdges(): { positions: Float32Array; edges: Uint32Array } | null {
    return null;
  }

  pick(): PickHit | null {
    return null;
  }

  brushBox(): string {
    return JSON.stringify({ op: "add_objects", objects: [] });
  }

  brushClip(): string {
    return "{}";
  }

  brushHollow(): string {
    return "{}";
  }

  brushCarve(): string {
    return "{}";
  }

  brushMergeConvex(): string {
    return "{}";
  }

  brushExtrudeFace(): string {
    return "{}";
  }

  brushMoveVertex(): string {
    return "{}";
  }

  brushResize(): string {
    return JSON.stringify({ op: "batch", label: "resize", commands: [] });
  }
  brushBounds(): number[] {
    return [0, 0, 0, 1, 1, 1];
  }
  meshCreate(): string {
    return JSON.stringify({ op: "batch", label: "mesh", commands: [] });
  }
  meshOp(): string {
    return JSON.stringify({ command: { op: "batch", label: "mesh", commands: [] }, selection: { mode: "face", ids: [] }, report: { verts: 0, edges: 0, faces: 0, warnings: [] } });
  }
  meshMakeEditable(): string {
    return JSON.stringify({ op: "batch", label: "mesh", commands: [] });
  }
  brushToMesh(): string {
    return JSON.stringify({ op: "batch", label: "mesh", commands: [] });
  }
  editBegin(): string {
    return JSON.stringify({ verts: 0, edges: 0, faces: 0 });
  }
  editEnd(): void {
  }
  editSelect(): string {
    return JSON.stringify({ mode: "vertex", ids: [] });
  }
  editBuffers(): EditBuffers | null {
    return null;
  }
  editPick(): string {
    return JSON.stringify({ kind: "none" });
  }
  editPreview(): string {
    return JSON.stringify({ selection: { mode: "vertex", ids: [] }, report: { verts: 0, edges: 0, faces: 0, warnings: [] } });
  }
  editCommit(): string {
    return JSON.stringify({ op: "batch", label: "mesh", commands: [] });
  }
  editHide(): void {
  }
  editReveal(): void {
  }
  terrainGenerate(): Float32Array {
    return new Float32Array(0);
  }

  terrainSculpt(): string {
    return "{}";
  }

  terrainAutoPaint(): string {
    return "{}";
  }

  terrainScatter(): string {
    return "{}";
  }

  putFile(): void {
    /* the store test never writes files */
  }

  getFile(): Uint8Array | null {
    return null;
  }

  fileNames(): string[] {
    return [];
  }

  reset(): string {
    return this.save();
  }

  schema(): string {
    return "{}";
  }

  validate(): string {
    return "[]";
  }

  compile(): never {
    throw new Error("not used");
  }

  bakePreview(): never {
    throw new Error("not used");
  }

  importObj(): string {
    return "";
  }

  importVmf(): string {
    return "{}";
  }

  importQuakeMap(): string {
    return "{}";
  }

  importReMap(): string {
    return "{}";
  }

  importGltf(): string {
    return "{}";
  }
}

describe("document store", () => {
  function group(id: string, name: string): DocObject {
    return {
      id,
      name,
      layer: "l1",
      parent: null,
      transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
      hidden: false,
      locked: false,
      kind: { type: "group" },
    };
  }

  it("mirrors the objects the core reports, in draw order", () => {
    const core = new TinyCore();
    core.objects = [group("a", "first"), group("b", "second")];
    const store = DocumentStore.empty(core);
    expect(store.ids()).toEqual(["a", "b"]);
    expect(store.object("a")?.name).toBe("first");
    expect(store.object("missing")).toBeNull();
    expect(store.meta.stem).toBe("mp_rr_test");
    expect(store.layers).toHaveLength(1);
    expect(store.layerVisible("l1")).toBe(true);
    expect(store.layerVisible("nope")).toBe(true);
  });

  it("refreshes the objects an apply touched and marks the document dirty", () => {
    const core = new TinyCore();
    core.objects = [group("a", "first")];
    const store = DocumentStore.empty(core);
    expect(store.snapshot().dirty).toBe(false);
    const touched = store.apply(replaceObjects([{ ...group("a", "renamed") }]));
    expect(touched).toEqual(["a"]);
    expect(store.object("a")?.name).toBe("renamed");
    expect(store.snapshot().dirty).toBe(true);
    store.clearDirty();
    expect(store.snapshot().dirty).toBe(false);
  });

  it("drops an object the core removed", () => {
    const core = new TinyCore();
    core.objects = [group("a", "first"), group("b", "second")];
    const store = DocumentStore.empty(core);
    store.apply(removeObjects(["a"]));
    expect(store.object("a")).toBeNull();
    expect(store.ids()).toEqual(["b"]);
  });

  it("applies command json the core produced and validates it first", () => {
    const core = new TinyCore();
    const store = DocumentStore.empty(core);
    expect(() => store.applyJson("{")).toThrow();
    store.applyJson(JSON.stringify({ op: "add_objects", objects: [group("c", "third")] }));
    expect(store.object("c")?.name).toBe("third");
  });

  it("keeps the object set and flags in step with undo and redo", () => {
    const core = new TinyCore();
    core.objects = [group("a", "first")];
    const store = DocumentStore.empty(core);
    store.apply(replaceObjects([{ ...group("a", "renamed") }]));
    store.undo();
    store.redo();
    expect(store.object("a")?.name).toBe("renamed");
    expect(store.snapshot().canUndo).toBe(false);
  });
});

describe("json parsing helpers", () => {
  it("rejects a document that is not an object", () => {
    expect(() => parseDoc("[]")).toThrow(/not an object/);
    expect(() => parseDoc("nope")).toThrow(/malformed/);
    expect(() => parseObject("null")).toThrow(/not an object/);
    expect(parseObject('{"id":"a"}').id).toBe("a");
  });
});
