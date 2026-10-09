import { describe, expect, it } from "vitest";
import type { Core, EditBuffers, Id, MeshBuffers, PickHit, Vec3 } from "../core/api";
import { DocumentStore } from "../doc/store";
import type { DocObject } from "../doc/model";
import { HostClient } from "../host/client";
import { fromRotationTranslationScale, mat4 } from "../math/mat4";
import { SceneBuilder, defaultSceneOptions, type SceneObject, type SceneOptions, type SceneSource } from "../render/scene";
import { Editor, MATERIAL_SHADER_SUFFIXES, NEW_OBJECT_KINDS, albedoSlot } from "./editor";

function uuid(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (typeof g.crypto?.randomUUID === "function") return g.crypto.randomUUID();
  let out = "";
  for (let i = 0; i < 32; i++) out += Math.floor(Math.random() * 16).toString(16);
  return out;
}

function object(name: string, kind: DocObject["kind"]): DocObject {
  return {
    id: uuid(),
    name,
    layer: "l1",
    parent: null,
    transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    hidden: false,
    locked: false,
    kind,
  };
}

function wireObject(name: string, kind: Record<string, unknown>, position?: number[]): Record<string, unknown> {
  return {
    id: uuid(),
    name,
    layer: "l1",
    parent: null,
    transform: { position: position ?? [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    hidden: false,
    locked: false,
    kind,
  };
}

/**
 * A core that records the commands it is given and answers with the objects
 * they add, so the editor's create paths run without the wasm.
 */
class RecordingCore implements Core {
  objects: DocObject[] = [];
  lastCommand = "";
  meshRequests: string[] = [];
  meshError: string | null = null;

  load(docJson: string): string {
    return docJson;
  }

  save(): string {
    return JSON.stringify({
      format: 1,
      meta: { stem: "mp_rr_test" },
      layers: [{ id: "l1", name: "default", visible: true, locked: false }],
      objects: this.objects,
      settings: {},
    });
  }

  apply(commandJson: string): Id[] {
    this.lastCommand = commandJson;
    const command = JSON.parse(commandJson) as {
      op: string;
      objects?: Record<string, unknown>[];
      ids?: string[];
      commands?: { op: string; objects?: Record<string, unknown>[] }[];
    };
    const all = command.op === "batch" ? (command.commands ?? []) : [command];
    for (const c of all as { op: string; path?: string; bytes?: number[] | null }[]) {
      if (c.op !== "set_file" || !c.path) continue;
      if (c.bytes) this.files.set(c.path, Uint8Array.from(c.bytes));
      else this.files.delete(c.path);
    }
    const adds = command.op === "batch" ? (command.commands ?? []).flatMap((c) => c.objects ?? []) : (command.objects ?? []);
    for (const added of adds) this.objects.push(added as unknown as DocObject);
    if (command.op === "remove_objects") this.objects = this.objects.filter((o) => !(command.ids ?? []).includes(o.id));
    if (command.op === "replace_objects") {
      for (const next of command.objects ?? []) {
        const id = (next as { id: string }).id;
        const at = this.objects.findIndex((o) => o.id === id);
        if (at >= 0) this.objects[at] = next as unknown as DocObject;
        else this.objects.push(next as unknown as DocObject);
      }
    }
    return adds.map((o) => (o as { id: string }).id).concat(command.ids ?? []);
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

  brushBox(min: number[], max: number[], material: string): string {
    const faces = [
      [1, 0, 0],
      [-1, 0, 0],
      [0, 1, 0],
      [0, -1, 0],
      [0, 0, 1],
      [0, 0, -1],
    ].map((n) => ({
      normal: n,
      dist: n[0] * max[0] + n[1] * max[1] + n[2] * max[2],
      material,
      uv: { u_axis: [1, 0, 0], v_axis: [0, -1, 0], offset: [0, 0], scale: [1, 1], rotation: 0 },
      lightmap_scale: 0,
    }));
    const center: [number, number, number] = [
      (min[0] + max[0]) / 2,
      (min[1] + max[1]) / 2,
      (min[2] + max[2]) / 2,
    ];
    return JSON.stringify({
      op: "add_objects",
      objects: [wireObject("brush", { type: "brush", faces, contents: 1, detail: false }, center)],
    });
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

  meshCreate(request: string): string {
    if (this.meshError) throw new Error(this.meshError);
    this.meshRequests.push(request);
    const parsed = JSON.parse(request) as { name?: string; material?: string };
    const id = uuid();
    const source = `meshes/${id}.glb`;
    return JSON.stringify({
      op: "batch",
      label: "mesh",
      commands: [
        { op: "set_file", path: source, bytes: [103, 108, 84, 70] },
        {
          op: "add_objects",
          objects: [
            wireObject(parsed.name ?? "mesh", {
              type: "mesh",
              source,
              material_map: { [parsed.material ?? ""]: parsed.material ?? "" },
              collision: "triangles",
              contents: 1,
              cast_shadows: true,
              lightmap_scale: 0,
            }),
          ],
        },
      ],
    });
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

  files = new Map<string, Uint8Array>();

  putFile(path: string, bytes: Uint8Array): void {
    this.files.set(path, bytes);
  }

  getFile(path: string): Uint8Array | null {
    return this.files.get(path) ?? null;
  }

  fileNames(): string[] {
    return [...this.files.keys()];
  }

  reset(): string {
    this.objects = [];
    this.files.clear();
    return this.save();
  }

  schema(): string {
    return "{}";
  }

  validate(): string {
    return "[]";
  }

  compile(): never {
    throw new Error("the create paths never compile");
  }

  bakePreview(): never {
    throw new Error("the create paths never bake");
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

function editorWith(core: RecordingCore): Editor {
  const editor = new Editor();
  editor.core = core;
  editor.store = DocumentStore.empty(core);
  return editor;
}

/** Field names the Rust document model requires, taken from `rdoc::model`. */
const RUST_FIELDS: Record<string, string[]> = {
  prop: ["type", "model", "skin", "static_prop", "solid", "cast_shadows", "fade_distance", "keyvalues"],
  entity: ["type", "classname", "keyvalues", "partition", "links"],
  light: [
    "type",
    "kind",
    "color",
    "intensity",
    "range",
    "inner_angle",
    "outer_angle",
    "size",
    "cast_shadows",
    "baked",
    "realtime",
    "keyvalues",
  ],
  volume: ["type", "kind", "classname", "brushes", "keyvalues"],
  path: ["type", "kind", "points", "keyvalues"],
  terrain: [
    "type",
    "heights",
    "resolution",
    "cell_size",
    "layers",
    "chunk_size",
    "collision",
    "contents",
    "lightmap_scale",
    "recipe",
  ],
  voxel_terrain: [
    "type",
    "bounds",
    "voxel_size",
    "ops",
    "material",
    "uv_scale",
    "collision",
    "contents",
    "lightmap_scale",
  ],
  group: ["type"],
  brush: ["type", "faces", "contents", "detail"],
  mesh: ["type", "source", "material_map", "collision", "contents", "cast_shadows", "lightmap_scale"],
};

/** The wire key set a command carried for the one object it added. */
function wireKind(command: string): string[] {
  const parsed = JSON.parse(command) as {
    objects?: { kind: Record<string, unknown> }[];
    commands?: { op: string; objects?: { kind: Record<string, unknown> }[] }[];
  };
  const added = parsed.objects ?? (parsed.commands ?? []).flatMap((c) => c.objects ?? []);
  if (!added || added.length === 0) throw new Error(`the command added nothing: ${command.slice(0, 80)}`);
  return Object.keys(added[0].kind).sort();
}

/** Answers a missing object the way wasm-bindgen does for a Rust `None`. */
class WasmLikeCore extends RecordingCore {
  override object(id: Id): string | null {
    return super.object(id) ?? (undefined as unknown as null);
  }
}

describe("removeObjects", () => {
  it("drops a deleted object when the core answers undefined for it", () => {
    const editor = editorWith(new WasmLikeCore());
    const [id] = editor.createObject("light", [0, 0, 0]);
    expect(() => editor.removeObjects([id])).not.toThrow();
    expect(editor.store?.object(id)).toBeNull();
    expect(editor.store?.ids()).toEqual([]);
  });
});

describe("createObject", () => {
  it("creates a real object for every entry in the create menu", () => {
    const editor = editorWith(new RecordingCore());
    for (const { id, label } of NEW_OBJECT_KINDS) {
      const added = editor.createObject(id, [0, 0, 0]);
      expect(added.length, `${label} created nothing`).toBeGreaterThan(0);
      expect(editor.store?.object(added[added.length - 1]), `${label} is not in the document`).not.toBeNull();
    }
  });

  it("lists both terrain kinds, the group and a mesh in the menu", () => {
    const ids = NEW_OBJECT_KINDS.map((k) => k.id);
    expect(ids).toContain("voxel_terrain");
    expect(ids).toContain("group");
    expect(ids).toContain("mesh");
  });

  it("writes a voxel terrain object with every field the core deserializes", () => {
    const core = new RecordingCore();
    const editor = editorWith(core);
    const added = editor.createObject("voxel_terrain", [64, 0, 32]);
    expect(editor.store?.object(added[0])?.kind.type).toBe("voxel_terrain");
    expect(wireKind(core.lastCommand)).toEqual([...RUST_FIELDS.voxel_terrain].sort());
  });

  it("writes a group object as the bare tag the core expects", () => {
    const core = new RecordingCore();
    const editor = editorWith(core);
    expect(editor.createObject("group")).toHaveLength(1);
    expect(JSON.parse(core.lastCommand).objects[0].kind).toEqual({ type: "group" });
  });

  it("gives every kind the wire field set the Rust model requires", () => {
    const core = new RecordingCore();
    const editor = editorWith(core);
    for (const { id } of NEW_OBJECT_KINDS) {
      editor.createObject(id, [0, 0, 0]);
      expect(wireKind(core.lastCommand), id).toEqual([...RUST_FIELDS[id]].sort());
    }
  });

  it("sizes a toolbar brush from the brush tool options", () => {
    const core = new RecordingCore();
    const editor = editorWith(core);
    const added = editor.createObject("brush", [0, 0, 0]);
    const wire = JSON.parse(core.lastCommand);
    const faces = (wire.objects[0].kind as { faces: { normal: number[]; dist: number }[] }).faces;
    const centre = wire.objects[0].transform.position as number[];
    const plusX = faces.find((f) => f.normal[0] === 1);
    const plusZ = faces.find((f) => f.normal[2] === 1);
    if (!plusX || !plusZ) throw new Error("the box has no +x or +z face");
    expect(plusX.dist - centre[0]).toBe(32);
    expect(plusZ.dist - centre[2]).toBe(64);
    expect(added).toHaveLength(1);
  });
});

describe("meshCreate", () => {
  it("hands the core a spec plus a placement and adds the object", () => {
    const core = new RecordingCore();
    const editor = editorWith(core);
    const added = editor.meshCreate(
      { shape: "cylinder", radius: 32, height: 64, segments: 16, caps: true },
      { position: [16, 32, 48], material: "world/dev/dev_blue_512", name: "pillar" },
    );
    expect(added).toHaveLength(1);
    const request = JSON.parse(core.meshRequests[0]);
    expect(request.spec).toEqual({ shape: "cylinder", radius: 32, height: 64, segments: 16, caps: true });
    expect(request.material).toBe("world/dev/dev_blue_512");
    expect(request.name).toBe("pillar");
    expect(request.transform.position).toEqual([16, 32, 48]);
    expect(editor.store?.object(added[0])?.kind.type).toBe("mesh");
  });

  it("is what the menu's Mesh entry falls back to", () => {
    const core = new RecordingCore();
    const editor = editorWith(core);
    editor.createObject("mesh", [0, 0, 0]);
    expect(JSON.parse(core.meshRequests[0]).spec).toEqual({ shape: "box", size: [64, 64, 64] });
  });

  it("lets a core that refuses the spec throw rather than add nothing", () => {
    const core = new RecordingCore();
    core.meshError = "dimensions must be positive and finite";
    const editor = editorWith(core);
    expect(() => editor.meshCreate({ shape: "box", size: [1, 1, 1] })).toThrow(/positive/);
  });
});

describe("project materials", () => {
  it("collects every material the document already names, sorted", () => {
    const editor = editorWith(new RecordingCore());
    editor.addObjects([
      object("mesh", {
        type: "mesh",
        mesh: {
          source: "meshes/a.glb",
          material_map: { M: "world/dev/dev_white_512" },
          collision: "triangles",
          contents: 1,
          cast_shadows: true,
          lightmap_scale: 0,
        },
      }),
      object("terrain", {
        type: "terrain",
        terrain: {
          heights: "",
          resolution: [2, 2],
          cell_size: 64,
          layers: [{ name: "base", material: "terrain/rock", weights: "", uv_scale: 0.02, scatter: [] }],
          chunk_size: 64,
          collision: true,
          contents: 1,
          lightmap_scale: 16,
          recipe: null,
        },
      }),
      object("voxel", {
        type: "voxel_terrain",
        voxel_terrain: {
          bounds: [
            [0, 0, 0],
            [1, 1, 1],
          ],
          voxel_size: 16,
          ops: [],
          material: "world/dev/dev_red_512",
          uv_scale: 0.05,
          collision: true,
          contents: 1,
          lightmap_scale: 0,
        },
      }),
    ]);
    expect(editor.projectMaterials()).toEqual([
      "terrain/rock",
      "world/dev/dev_red_512",
      "world/dev/dev_white_512",
    ]);
  });

  it("reads a brush face material too", () => {
    const core = new RecordingCore();
    const editor = editorWith(core);
    editor.createObject("brush", [0, 0, 0]);
    expect(editor.projectMaterials()).toContain("world/dev/dev_gray_512");
    expect(core.lastCommand.length).toBeGreaterThan(0);
  });

  it("is empty for a document that names none", () => {
    const editor = editorWith(new RecordingCore());
    expect(editor.projectMaterials()).toEqual([]);
  });
});

/** The bytes of a 4x4 BC1 DDS, enough for the client to hand back. */
const DDS_BYTES = new Uint8Array([68, 68, 83, 32, 0, 0, 0, 0]);

interface Route {
  material?: Record<string, unknown>;
  texture?: Uint8Array;
}

/** A minimal Response, so the client is exercised without a fetch runtime. */
function reply(status: number, body: Uint8Array, type: string): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: status === 200 ? "OK" : "Not Found",
    headers: { get: (name: string) => (name.toLowerCase() === "content-type" ? type : null) },
    json: async () => JSON.parse(new TextDecoder().decode(body)) as unknown,
    text: async () => new TextDecoder().decode(body),
    arrayBuffer: async () => body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength) as ArrayBuffer,
  } as unknown as Response;
}

/**
 * A host that answers the batch material endpoint the way the library does:
 * each name is tried with each shader suffix, the first material whose slot 0
 * resolves gives the albedo, and textures come from their own route.
 */
function hostClient(routes: Record<string, Route>): { client: HostClient; asked: string[] } {
  const asked: string[] = [];
  const impl = (async (url: string, init?: { body?: string }) => {
    const parsed = new URL(url, "http://127.0.0.1");
    if (parsed.pathname === "/api/assets/materials/prefetch") {
      const body = JSON.parse(init?.body ?? "{}") as { names: string[]; suffixes: string[] };
      const out = body.names.map((name) => {
        const item = { name, path: null as string | null, albedo: null as string | null, translucent: false, error: null as string | null };
        const found = body.suffixes.map((sfx) => `material/${name}${sfx}.rpak`).filter((path) => routes[path]?.material);
        if (found.length === 0) item.error = "not in the indexed paks";
        for (const path of found) {
          asked.push(path);
          const m = routes[path].material as { textures: Record<string, string>; translucent: boolean };
          const slot = m.textures["0"];
          if (slot) {
            Object.assign(item, { path, albedo: slot, translucent: m.translucent, error: null });
            break;
          }
          item.error = `${path} has no albedo the host can resolve`;
        }
        return item;
      });
      return reply(200, new TextEncoder().encode(JSON.stringify(out)), "application/json");
    }
    asked.push(parsed.search);
    const route = routes[parsed.search];
    if (route?.texture) return reply(200, route.texture, "image/vnd.ms.dds");
    return reply(404, new TextEncoder().encode("not indexed"), "text/plain");
  }) as unknown as typeof fetch;
  return { client: new HostClient("token", { base: "", fetch: impl }), asked };
}

const GRAY_MATERIAL: Route = {
  material: {
    path: "world/dev/dev_gray_512",
    guid: "0x0f833136f1f53709",
    shader_set: "0xafc934f3f1896044",
    vertex_format: "lit_bump",
    surface_prop: "default",
    translucent: true,
    nodraw: false,
    textures: {
      "0": "0x8835fd83f2fb1673",
      "1": "0x8c9647619e10e9a6",
      "2": "0xc26aaaaff2b114147",
      "3": "0x556001e2ebafd73e",
    },
  },
};

const BLUE_MATERIAL: Route = {
  material: {
    path: "world/dev/dev_blue_512",
    guid: "0x85d07e923b9dc66c",
    shader_set: "0x8362a4d5383196bd",
    vertex_format: "lit_bump",
    surface_prop: "dirt",
    translucent: true,
    nodraw: false,
    textures: {},
  },
};

const GRAY_WLDC = "material/world/dev/dev_gray_512_wldc.rpak";
const GRAY_RGDP = "material/world/dev/dev_gray_512_rgdp.rpak";
const BLUE_WLDC = "material/world/dev/dev_blue_512_wldc.rpak";
const GRAY_ALBEDO = "?guid=0x8835fd83f2fb1673";

describe("material albedo resolution", () => {
  it("tries only the shader suffixes a material can be shipped under", () => {
    // The tails `rpak::material::vertex_format_of` maps to a vertex format.
    const known = ["wldu", "wldc", "rgdu", "rgdp", "rgdc", "sknu", "sknp", "sknc", "ptcu", "ptcs", "rgbs"];
    expect(MATERIAL_SHADER_SUFFIXES[0]).toBe("_wldc");
    expect(MATERIAL_SHADER_SUFFIXES[MATERIAL_SHADER_SUFFIXES.length - 1]).toBe("");
    for (const suffix of MATERIAL_SHADER_SUFFIXES) {
      if (suffix === "") continue;
      expect(known, suffix).toContain(suffix.slice(1));
    }
  });

  it("reads the albedo guid out of a material whose slots resolve", async () => {
    const { client, asked } = hostClient({ [GRAY_WLDC]: GRAY_MATERIAL, [GRAY_ALBEDO]: { texture: DDS_BYTES } });
    const editor = editorWith(new RecordingCore());
    editor.host = client;
    const hit = await editor.resolveMaterialAlbedo("world/dev/dev_gray_512");
    expect(hit?.albedo).toBe("0x8835fd83f2fb1673");
    expect(hit?.bytes).toHaveLength(DDS_BYTES.length);
    expect(asked).toContain(GRAY_ALBEDO);
    expect(editor.materialWarnings).toEqual([]);
  });

  it("says so when a material resolves no albedo, rather than going quietly grey", async () => {
    const { client } = hostClient({ [BLUE_WLDC]: BLUE_MATERIAL });
    const editor = editorWith(new RecordingCore());
    editor.host = client;
    expect(await editor.resolveMaterialAlbedo("world/dev/dev_blue_512")).toBeNull();
    expect(editor.materialWarnings).toHaveLength(1);
    expect(editor.materialWarnings[0].material).toBe("world/dev/dev_blue_512");
    expect(editor.materialWarnings[0].source).toBe("host");
    expect(editor.materialWarnings[0].message).toMatch(/no albedo/);
  });

  it("keeps trying the other shader suffixes after one resolves nothing", async () => {
    const { client, asked } = hostClient({
      [GRAY_WLDC]: BLUE_MATERIAL,
      [GRAY_RGDP]: GRAY_MATERIAL,
      [GRAY_ALBEDO]: { texture: DDS_BYTES },
    });
    const editor = editorWith(new RecordingCore());
    editor.host = client;
    expect((await editor.resolveMaterialAlbedo("world/dev/dev_gray_512"))?.albedo).toBe("0x8835fd83f2fb1673");
    expect(asked[0]).toBe(GRAY_WLDC);
    expect(asked).toContain(GRAY_RGDP);
  });

  it("reports a material the indexed paks do not carry", async () => {
    const editor = editorWith(new RecordingCore());
    editor.host = hostClient({}).client;
    expect(await editor.resolveMaterialAlbedo("world/nope/nothing")).toBeNull();
    expect(editor.materialWarnings[0].message).toBe("not in the indexed paks");
  });

  it("asks for a frame's materials in one request", async () => {
    const { client, asked } = hostClient({ [GRAY_WLDC]: GRAY_MATERIAL, [BLUE_WLDC]: BLUE_MATERIAL, [GRAY_ALBEDO]: { texture: DDS_BYTES } });
    const editor = editorWith(new RecordingCore());
    editor.host = client;
    const calls: string[] = [];
    const inner = client.prefetchMaterials.bind(client);
    client.prefetchMaterials = (names, suffixes) => {
      calls.push(names.join(","));
      return inner(names, suffixes);
    };
    const [gray, blue] = await Promise.all([editor.resolveMaterialAlbedo("world/dev/dev_gray_512"), editor.resolveMaterialAlbedo("world/dev/dev_blue_512")]);
    expect(gray?.albedo).toBe("0x8835fd83f2fb1673");
    expect(blue).toBeNull();
    expect(calls).toEqual(["world/dev/dev_gray_512,world/dev/dev_blue_512"]);
    expect(asked).toContain(GRAY_WLDC);
  });

  it("reports a texture the host could not hand back", async () => {
    const { client } = hostClient({ [GRAY_WLDC]: GRAY_MATERIAL });
    const editor = editorWith(new RecordingCore());
    editor.host = client;
    expect(await editor.resolveMaterialAlbedo("world/dev/dev_gray_512")).toBeNull();
    expect(editor.materialWarnings.some((w) => w.message.includes("8835fd83f2fb1673"))).toBe(true);
  });

  it("looks a name up once, whatever case or separator it arrives in", async () => {
    const { client, asked } = hostClient({ [GRAY_WLDC]: GRAY_MATERIAL, [GRAY_ALBEDO]: { texture: DDS_BYTES } });
    const editor = editorWith(new RecordingCore());
    editor.host = client;
    expect((await editor.resolveMaterialAlbedo("World/Dev/dev_gray_512"))?.albedo).toBe("0x8835fd83f2fb1673");
    const after = asked.length;
    await editor.resolveMaterialAlbedo("world\\dev\\dev_gray_512");
    expect(asked).toHaveLength(after);
  });

  it("normalises the guid a texture slot carries", () => {
    expect(albedoSlot({ "0": "0x8835FD83F2FB1673" })).toBe("0x8835fd83f2fb1673");
    expect(albedoSlot({ "0": "8835fd83f2fb1673" })).toBe("0x8835fd83f2fb1673");
    expect(albedoSlot({ "0": "0x1" })).toBe("0x0000000000000001");
    expect(albedoSlot({})).toBeNull();
    expect(albedoSlot({ "0": "not-a-guid" })).toBeNull();
    expect(albedoSlot({ "0": "0x1234567890abcdef01" })).toBeNull();
    // Slot 0 is the only albedo; a normal map must never stand in for one.
    expect(albedoSlot({ "1": "0x8c9647619e10e9a6" })).toBeNull();
  });

  it("keeps the host's material warnings and replaces only the renderer's", () => {
    const editor = editorWith(new RecordingCore());
    editor.warnMaterial("a", "no albedo");
    editor.warnMaterial("a", "no albedo");
    expect(editor.materialWarnings).toHaveLength(1);
    editor.setRendererMaterialWarnings([{ material: "b", message: "albedo texture is missing" }]);
    expect(editor.materialWarnings.map((w) => `${w.material}:${w.source}`)).toEqual(["a:host", "b:gpu"]);
    editor.setRendererMaterialWarnings([{ material: "b", message: "albedo texture is missing" }]);
    expect(editor.materialWarnings).toHaveLength(2);
    editor.setRendererMaterialWarnings([]);
    expect(editor.materialWarnings.map((w) => w.material)).toEqual(["a"]);
  });
});

describe("x-ray surface pick", () => {
  function boxMesh(min: Vec3, max: Vec3): MeshBuffers {
    const corners: Vec3[] = [
      [min[0], min[1], min[2]], [max[0], min[1], min[2]], [max[0], max[1], min[2]], [min[0], max[1], min[2]],
      [min[0], min[1], max[2]], [max[0], min[1], max[2]], [max[0], max[1], max[2]], [min[0], max[1], max[2]],
    ];
    const faces = [
      [0, 1, 2, 3], [5, 4, 7, 6], [4, 0, 3, 7], [1, 5, 6, 2], [3, 2, 6, 7], [4, 5, 1, 0],
    ];
    const positions: number[] = [];
    const normals: number[] = [];
    const uvs: number[] = [];
    const indices: number[] = [];
    for (const f of faces) {
      const base = positions.length / 3;
      for (let i = 0; i < 4; i++) {
        positions.push(corners[f[i]][0], corners[f[i]][1], corners[f[i]][2]);
        normals.push(0, 0, 1);
        uvs.push(0, 0);
      }
      indices.push(base, base + 1, base + 2, base, base + 2, base + 3);
    }
    return {
      positions: new Float32Array(positions),
      normals: new Float32Array(normals),
      uvs: new Float32Array(uvs),
      indices: new Uint32Array(indices),
      ranges: [{ material: "world/dev/dev_gray_512", start: 0, count: indices.length }],
    };
  }

  function stackedEditor(): { editor: Editor; near: Id; far: Id } {
    const editor = editorWith(new RecordingCore());
    const near = uuid();
    const far = uuid();
    const meshes = new Map([
      [near, boxMesh([0, 0, 0], [8, 8, 8])],
      [far, boxMesh([0, 0, -16], [8, 8, -8])],
    ]);
    const source: SceneSource = {
      objectMesh: (id: Id) => meshes.get(id) ?? null,
      objectEdges: () => null,
      modelEdges: () => null,
      model: () => null,
      bounds: () => null,
    };
    const docOf = (id: string): DocObject => ({
      id,
      name: "brush",
      layer: "l1",
      parent: null,
      transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
      hidden: false,
      locked: false,
      kind: { type: "brush", brush: { faces: [], contents: 1, detail: false } },
    });
    const asObject = (doc: DocObject): SceneObject => ({
      doc,
      world: fromRotationTranslationScale(mat4(), doc.transform.rotation, doc.transform.position, doc.transform.scale),
      worldBounds: null,
      hidden: false,
      ancestors: [],
    });
    const options: SceneOptions = { ...defaultSceneOptions(), invalidate: new Set(), selection: new Set(), hover: null };
    editor.scene = new SceneBuilder(source).build([asObject(docOf(near)), asObject(docOf(far))], options);
    return { editor, near, far };
  }

  it("picks the front object off and the occluded one on", () => {
    const { editor, near, far } = stackedEditor();
    const origin: Vec3 = [4, 4, 64];
    const dir: Vec3 = [0, 0, -1];
    expect(editor.pickSurface(origin, dir)?.id).toBe(near);
    expect(editor.pickSurface(origin, dir, undefined, undefined, 1)?.id).toBe(far);
  });

  it("pickWorld follows the same rule", () => {
    const { editor } = stackedEditor();
    const origin: Vec3 = [4, 4, 64];
    const dir: Vec3 = [0, 0, -1];
    expect(editor.pickWorld(origin, dir)?.[2]).toBeCloseTo(8, 6);
    expect(editor.pickWorld(origin, dir, undefined, 1)?.[2]).toBeCloseTo(-8, 6);
  });
});
