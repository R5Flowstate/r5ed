import { describe, expect, it } from "vitest";
import type { Id, MeshBuffers, Vec3 } from "../core/api";
import { DEV_MATERIALS, type DocObject } from "../doc/model";
import { fromRotationTranslationScale, mat4 } from "../math/mat4";
import { quatFromSourceAngles } from "../math/quat";
import {
  SceneBuilder,
  type ModelMesh,
  type SceneObject,
  type SceneOptions,
  type SceneSource,
  composeWorld,
  defaultSceneOptions,
  instanceBounds,
  isTranslucentMaterial,
  localZDirection,
  transformPointBy,
  worldTranslation,
} from "./scene";
import { SKY_CAMERA_ABOVE_VISTA, SKY_SCALE, VERTEX_FLOATS, skyTransform } from "./scene";
import { boundsOf } from "./overlay";
import { CATEGORY_ORDER, defaultAtlasOptions, layoutAtlas } from "./atlas";
import type { Aabb } from "../math/vec3";

function boxMesh(min: Vec3, max: Vec3, material = "props/concrete/rock"): MeshBuffers {
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
    const n = faceNormal(corners[f[0]], corners[f[1]], corners[f[2]]);
    const base = positions.length / 3;
    for (let i = 0; i < 4; i++) {
      positions.push(corners[f[i]][0], corners[f[i]][1], corners[f[i]][2]);
      normals.push(n[0], n[1], n[2]);
      uvs.push(i === 1 || i === 2 ? 1 : 0, i >= 2 ? 1 : 0);
    }
    indices.push(base, base + 1, base + 2, base, base + 2, base + 3);
  }
  const count = indices.length;
  return {
    positions: new Float32Array(positions),
    normals: new Float32Array(normals),
    uvs: new Float32Array(uvs),
    indices: new Uint32Array(indices),
    ranges: [{ material, start: 0, count }],
  };
}

function faceNormal(a: Vec3, b: Vec3, c: Vec3): Vec3 {
  const u: Vec3 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
  const v: Vec3 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
  const n: Vec3 = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
  const l = Math.hypot(n[0], n[1], n[2]) || 1;
  return [n[0] / l, n[1] / l, n[2] / l];
}

let idCounter = 0;
function nextId(): string {
  idCounter += 1;
  return `00000000-0000-4000-8000-${idCounter.toString().padStart(12, "0")}`;
}

function brush(id: string, name = "brush"): DocObject {
  return {
    id,
    name,
    layer: "layer-1",
    parent: null,
    transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    hidden: false,
    locked: false,
    kind: { type: "brush", brush: { faces: [], contents: 1, detail: false } },
  };
}

function prop(id: string, model: string, at: Vec3): DocObject {
  return {
    ...brush(id, "prop"),
    transform: { position: at, rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    kind: { type: "prop", prop: { model, skin: 0, static_prop: true, solid: true, cast_shadows: true, fade_distance: 0, keyvalues: {} } },
  };
}

function light(id: string, at: Vec3, range: number, kind: "point" | "spot" | "sun" = "point"): DocObject {
  return {
    ...brush(id, "light"),
    transform: { position: at, rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    kind: {
      type: "light",
      light: { kind, color: [1, 1, 1], intensity: 1, range, inner_angle: 30, outer_angle: 45, size: [0, 0], cast_shadows: true, baked: true, realtime: false, keyvalues: {} },
    },
  };
}

function path(id: string, points: Vec3[]): DocObject {
  return {
    ...brush(id, "path"),
    kind: { type: "path", path: { kind: "zipline", points, keyvalues: {} } },
  };
}

function volume(id: string, kind: string, at: Vec3): DocObject {
  return {
    ...brush(id, "volume"),
    transform: { position: at, rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    kind: { type: "volume", volume: { kind: kind as "trigger", classname: kind, brushes: [], keyvalues: {} } },
  };
}

function entity(id: string, classname: string, at: Vec3): DocObject {
  return {
    ...brush(id, "entity"),
    transform: { position: at, rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    kind: { type: "entity", entity: { classname, keyvalues: {}, partition: null, links: {} } },
  };
}

/** A source that answers meshes and models out of maps, with a bounds hook. */
function makeSource(meshes: Map<string, MeshBuffers>, models: Map<string, ModelMesh>, bounds?: Map<string, Aabb>): SceneSource {
  return {
    objectMesh(id: Id) {
      return meshes.get(id) ?? null;
    },
    objectEdges() {
      return null;
    },
    modelEdges() {
      return null;
    },
    model(path: string) {
      return models.get(path) ?? null;
    },
    bounds(object: DocObject) {
      return bounds?.get(object.id) ?? null;
    },
  };
}

function worldOf(object: DocObject): Float32Array {
  return fromRotationTranslationScale(mat4(), object.transform.rotation, object.transform.position, object.transform.scale);
}

function asSceneObject(object: DocObject, source: SceneSource): SceneObject {
  return { doc: object, world: worldOf(object), worldBounds: source.bounds(object, worldOf(object)), hidden: false, ancestors: [] };
}

function options(over: Partial<SceneOptions> = {}): SceneOptions {
  return { ...defaultSceneOptions(), invalidate: new Set(), selection: new Set(), hover: null, ...over };
}

describe("scene building", () => {
  it("turns brushes into chunks with material sub-ranges", () => {
    const id = nextId();
    const meshes = new Map([[id, boxMesh([-16, -16, 0], [16, 16, 32])]]);
    const builder = new SceneBuilder(makeSource(meshes, new Map()));
    const scene = builder.build([asSceneObject(brush(id), makeSource(meshes, new Map()))], options());
    expect(scene.instances).toHaveLength(1);
    expect(scene.instances[0].id).toBe(id);
    expect(scene.instances[0].pick).toBe(1);
    expect(scene.instances[0].materials).toEqual(["props/concrete/rock"]);
    const chunk = scene.chunks[scene.instances[0].chunk];
    expect(chunk.vertices.length % VERTEX_FLOATS).toBe(0);
    expect(chunk.vertices.length / VERTEX_FLOATS).toBe(24);
    expect(chunk.indices).toHaveLength(36);
    expect(chunk.bounds).toEqual({ min: [-16, -16, 0], max: [16, 16, 32] });
  });

  it("skips objects with no geometry rather than emitting empty draws", () => {
    const builder = new SceneBuilder(makeSource(new Map(), new Map()));
    const scene = builder.build([asSceneObject(brush(nextId()), makeSource(new Map(), new Map()))], options());
    expect(scene.instances).toHaveLength(0);
    expect(scene.bvhBoxes).toHaveLength(0);
  });

  it("skips hidden objects and objects on an invisible layer", () => {
    const id = nextId();
    const meshes = new Map([[id, boxMesh([0, 0, 0], [8, 8, 8])]]);
    const source = makeSource(meshes, new Map());
    const hidden = asSceneObject(brush(id), source);
    hidden.doc.hidden = true;
    const builder = new SceneBuilder(source);
    expect(builder.build([hidden], options()).instances).toHaveLength(0);
    const onHiddenLayer = asSceneObject(brush(id), source);
    onHiddenLayer.hidden = true;
    expect(builder.build([onHiddenLayer], options()).instances).toHaveLength(0);
  });

  it("shares one chunk between props with the same model", () => {
    const models = new Map<string, ModelMesh>([
      [
        "mdl/props/crate.rmdl",
        {
          parts: [
            { positions: new Float32Array([0, 0, 0, 8, 0, 0, 8, 8, 0]), normals: new Float32Array(9), uvs: new Float32Array(6), indices: new Uint32Array([0, 1, 2]), material: "props/wood/crate" },
            { positions: new Float32Array([0, 0, 0, 0, 8, 0, 0, 0, 8]), normals: new Float32Array(9), uvs: new Float32Array(6), indices: new Uint32Array([0, 1, 2]), material: "props/wood/crate_side" },
          ],
          bounds: { min: [0, 0, 0], max: [8, 8, 8] },
        },
      ],
    ]);
    const source = makeSource(new Map(), models);
    const a = prop(nextId(), "mdl/props/crate.rmdl", [0, 0, 0]);
    const b = prop(nextId(), "mdl/props/crate.rmdl", [64, 0, 0]);
    const builder = new SceneBuilder(source);
    const scene = builder.build([asSceneObject(a, source), asSceneObject(b, source)], options());
    expect(scene.instances).toHaveLength(2);
    expect(scene.instances[0].chunk).toBe(scene.instances[1].chunk);
    expect(builder.chunkCount).toBe(1);
    const chunk = scene.chunks[scene.instances[0].chunk];
    expect(chunk.subRanges.map((r) => r.material)).toEqual(["props/wood/crate", "props/wood/crate_side"]);
    expect(chunk.subRanges[1].start).toBe(3);
    // The second part's indices are offset past the first part's vertices.
    expect(chunk.indices[3]).toBe(3);
  });

  it("bounds each instance in world space from its local box", () => {
    const models = new Map<string, ModelMesh>([
      [
        "mdl/props/crate.rmdl",
        { parts: [{ positions: new Float32Array([0, 0, 0, 8, 0, 0, 8, 8, 0]), normals: new Float32Array(9), uvs: new Float32Array(6), indices: new Uint32Array([0, 1, 2]), material: "props/wood/crate" }], bounds: { min: [-4, -4, 0], max: [4, 4, 8] } },
      ],
    ]);
    const source = makeSource(new Map(), models);
    const rotated = prop(nextId(), "mdl/props/crate.rmdl", [100, 0, 0]);
    rotated.transform.rotation = quatFromSourceAngles([0, 90, 0]);
    const builder = new SceneBuilder(source);
    const scene = builder.build([asSceneObject(rotated, source)], options());
    const b = scene.instances[0].bounds;
    expect(b.min[0]).toBeCloseTo(96, 4);
    expect(b.max[0]).toBeCloseTo(104, 4);
  });

  it("rebuilds a core chunk only when the id is invalidated", () => {
    const id = nextId();
    const meshes = new Map([[id, boxMesh([0, 0, 0], [8, 8, 8])]]);
    const source = makeSource(meshes, new Map());
    const builder = new SceneBuilder(source);
    const object = asSceneObject(brush(id), source);
    builder.build([object], options());
    const before = builder.chunkCount;
    builder.build([object], options());
    expect(builder.chunkCount).toBe(before);
    meshes.set(id, boxMesh([0, 0, 0], [16, 16, 16]));
    builder.build([object], options({ invalidate: new Set([id]) }));
    expect(builder.chunkCount).toBeGreaterThan(before);
  });

  it("puts overlays in the scene: light gizmos, paths, volumes and billboards", () => {
    const id = nextId();
    const meshes = new Map<string, MeshBuffers>();
    const source = makeSource(meshes, new Map(), new Map());
    const vol = volume(nextId(), "trigger", [0, 0, 0]);
    const bounds = new Map<string, Aabb>([[vol.id, { min: [-8, -8, 0], max: [8, 8, 8] }]]);
    const withBounds: SceneSource = { ...source, bounds: (o) => bounds.get(o.id) ?? null };
    const objects = [
      asSceneObject(light(nextId(), [0, 0, 64], 128), withBounds),
      asSceneObject(light(nextId(), [0, 0, 64], 128, "spot"), withBounds),
      asSceneObject(path(nextId(), [[0, 0, 0], [64, 0, 64], [128, 0, 32]]), withBounds),
      asSceneObject(vol, withBounds),
    ];
    const scene = new SceneBuilder(withBounds).build(objects, options());
    expect(scene.overlay.lines.count).toBeGreaterThan(0);
    expect(scene.overlay.boxes).toHaveLength(1);
    expect(scene.overlay.boxes[0].color[3]).toBeLessThan(1);
    // No atlas was supplied, so no billboards are produced.
    expect(scene.overlay.billboards).toHaveLength(0);
    void id;
  });

  it("draws a selection outline and skips it for unselected objects", () => {
    const a = nextId();
    const b = nextId();
    const bounds = new Map<string, Aabb>([
      [a, { min: [0, 0, 0], max: [16, 16, 16] }],
      [b, { min: [32, 0, 0], max: [48, 16, 16] }],
    ]);
    const source: SceneSource = { ...makeSource(new Map(), new Map()), bounds: (o) => bounds.get(o.id) ?? null };
    const objects = [asSceneObject(brush(a), source), asSceneObject(brush(b), source)];
    const plain = new SceneBuilder(source).build(objects, options());
    const selected = new SceneBuilder(source).build(objects, options({ selection: new Set([a]) }));
    expect(selected.overlay.lines.count).toBeGreaterThan(plain.overlay.lines.count);
    // A box wireframe is twelve segments, so twenty-four vertices.
    expect(selected.overlay.lines.count - plain.overlay.lines.count).toBe(24);
  });

  it("builds a culling BVH over the instance bounds", () => {
    const ids = Array.from({ length: 20 }, () => nextId());
    const meshes = new Map<string, MeshBuffers>(ids.map((id, i) => [id, boxMesh([i * 64, 0, 0], [i * 64 + 16, 16, 16])]));
    const source = makeSource(meshes, new Map());
    const scene = new SceneBuilder(source).build(ids.map((id) => asSceneObject(brush(id), source)), options());
    expect(scene.bvhBoxes).toHaveLength(20);
    expect(scene.bvh.nodes.length).toBeGreaterThan(1);
    const near = scene.bvhBoxes[0];
    expect(near.min[0]).toBe(0);
  });

  it("draws one billboard per entity, coloured by category", () => {
    const source = makeSource(new Map(), new Map());
    const atlas = layoutAtlas([...CATEGORY_ORDER], defaultAtlasOptions(CATEGORY_ORDER.length));
    const objects = [
      asSceneObject(entity(nextId(), "trigger_multiple", [0, 0, 0]), source),
      asSceneObject(entity(nextId(), "info_target", [64, 0, 0]), source),
    ];
    const scene = new SceneBuilder(source).build(objects, options({ atlas }));
    expect(scene.overlay.billboards).toHaveLength(2);
    const [trigger, target] = scene.overlay.billboards;
    expect(trigger.rect).toEqual(atlas.entries.get("trigger")?.uv);
    expect(target.rect).toEqual(atlas.entries.get("info_target")?.uv);
    expect(trigger.color).not.toEqual(target.color);
    expect(trigger.size).toBeGreaterThan(0);
  });

  it("does not emit geometry for a group or a prefab instance", () => {
    const group: DocObject = { ...brush(nextId()), kind: { type: "group" } };
    const prefab: DocObject = { ...brush(nextId()), kind: { type: "prefab_instance", prefab_instance: { prefab: "prefabs/room.r5prefab" } } };
    const source = makeSource(new Map(), new Map());
    const scene = new SceneBuilder(source).build([asSceneObject(group, source), asSceneObject(prefab, source)], options());
    expect(scene.instances).toHaveLength(0);
  });

  it("builds budgeted wireframe lines from edge tables, nearest first", () => {
    const corners: Vec3[] = [
      [0, 0, 0], [8, 0, 0], [0, 8, 0], [8, 8, 0],
      [0, 0, 8], [8, 0, 8], [0, 8, 8], [8, 8, 8],
    ];
    const pairs: [number, number][] = [
      [0, 1], [1, 3], [3, 2], [2, 0],
      [4, 5], [5, 7], [7, 6], [6, 4],
      [0, 4], [1, 5], [2, 6], [3, 7],
    ];
    const wire = { positions: new Float32Array(corners.flat()), edges: new Uint32Array(pairs.flat()) };
    const a = nextId();
    const b = nextId();
    const meshes = new Map([[a, boxMesh([0, 0, 0], [8, 8, 8])], [b, boxMesh([64, 0, 0], [72, 8, 8])]]);
    const source: SceneSource = { ...makeSource(meshes, new Map()), objectEdges: () => wire, modelEdges: () => null };
    const objects = [asSceneObject(brush(a), source), asSceneObject(brush(b), source)];
    const builder = new SceneBuilder(source);
    const wireCount = (built: { wire: { count: number }[] }): number => built.wire.reduce((n, w) => n + w.count, 0);
    expect(builder.build(objects, options()).wire).toEqual([]);
    // Twelve segments per box in one buffer per chunk: the full budget draws
    // both, a half budget the nearer one.
    const full = builder.build(objects, options({ wire: { eye: [4, 4, 64], budget: 100, only: null } }));
    expect(full.wire).toHaveLength(2);
    expect(wireCount(full)).toBe(48);
    const half = builder.build(objects, options({ wire: { eye: [4, 4, 64], budget: 12, only: null } }));
    expect(half.wire).toHaveLength(1);
    expect(wireCount(half)).toBe(24);
  });

  it("scopes the solid-mode wire overlay to the edited object", () => {
    const corners: Vec3[] = [
      [0, 0, 0], [8, 0, 0], [0, 8, 0], [8, 8, 0],
      [0, 0, 8], [8, 0, 8], [0, 8, 8], [8, 8, 8],
    ];
    const pairs: [number, number][] = [
      [0, 1], [1, 3], [3, 2], [2, 0],
      [4, 5], [5, 7], [7, 6], [6, 4],
      [0, 4], [1, 5], [2, 6], [3, 7],
    ];
    const wire = { positions: new Float32Array(corners.flat()), edges: new Uint32Array(pairs.flat()) };
    const a = nextId();
    const b = nextId();
    const meshes = new Map([[a, boxMesh([0, 0, 0], [8, 8, 8])], [b, boxMesh([64, 0, 0], [72, 8, 8])]]);
    const source: SceneSource = { ...makeSource(meshes, new Map()), objectEdges: () => wire, modelEdges: () => null };
    const objects = [asSceneObject(brush(a), source), asSceneObject(brush(b), source)];
    const builder = new SceneBuilder(source);
    const scoped = builder.build(objects, options({ wire: { eye: [4, 4, 64], budget: 100, only: b } }));
    expect(scoped.wire).toHaveLength(1);
    expect(scoped.wire.reduce((n, w) => n + w.count, 0)).toBe(24);
  });
});

describe("boolean cutters", () => {
  it("draw as wire only, in every shading mode", () => {
    const corners: Vec3[] = [
      [0, 0, 0], [8, 0, 0], [0, 8, 0], [8, 8, 0],
      [0, 0, 8], [8, 0, 8], [0, 8, 8], [8, 8, 8],
    ];
    const pairs: [number, number][] = [
      [0, 1], [1, 3], [3, 2], [2, 0],
      [4, 5], [5, 7], [7, 6], [6, 4],
      [0, 4], [1, 5], [2, 6], [3, 7],
    ];
    const wire = { positions: new Float32Array(corners.flat()), edges: new Uint32Array(pairs.flat()) };
    const solid = nextId();
    const cut = nextId();
    // An editable mesh is drawn from the core, the same way a brush is.
    const meshes = new Map([
      [solid, boxMesh([0, 0, 0], [8, 8, 8])],
      [cut, boxMesh([0, 0, 0], [8, 8, 8])],
    ]);
    const source: SceneSource = { ...makeSource(meshes, new Map()), objectEdges: () => wire, modelEdges: () => null };
    const cutter: DocObject = {
      ...brush(cut, "cutter"),
      transform: { position: [100, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
      kind: {
        type: "mesh",
        mesh: { source: "meshes/cut.r5mesh", material_map: {}, collision: "none", contents: 0, cast_shadows: false, lightmap_scale: 0, role: "cutter" },
      },
    };
    const objects = [asSceneObject(brush(solid), source), asSceneObject(cutter, source)];
    const builder = new SceneBuilder(source);
    const built = builder.build(objects, options());
    expect(built.instances.find((i) => i.id === cut)?.wireOnly).toBe(true);
    // The core already placed it in the world: the instance must not move it again.
    expect(Array.from(built.instances.find((i) => i.id === cut)?.transform ?? [])).toEqual(Array.from(mat4()));
    // Its wire is object space, so it keeps the object's placement.
    expect(worldTranslation(built.instances.find((i) => i.id === cut)?.wireTransform ?? mat4())).toEqual([100, 0, 0]);
    expect(built.instances.find((i) => i.id === solid)?.wireOnly).toBe(false);
    // No wireframe drawing asked for: the cutter's twelve edges are the only wire.
    expect(built.wire.reduce((n, w) => n + w.count, 0)).toBe(24);
    const all = builder.build(objects, options({ wire: { eye: [4, 4, 64], budget: 100, only: null } }));
    expect(all.wire.reduce((n, w) => n + w.count, 0)).toBe(48);
  });
});

describe("transform helpers", () => {
  it("composes a child with its ancestors", () => {
    const parent = { position: [100, 0, 0] as Vec3, rotation: [0, 0, 0, 1] as [number, number, number, number], scale: [2, 2, 2] as Vec3 };
    const child = { position: [10, 0, 0] as Vec3, rotation: [0, 0, 0, 1] as [number, number, number, number], scale: [1, 1, 1] as Vec3 };
    const world = composeWorld(child, [parent]);
    expect(worldTranslation(world)).toEqual([120, 0, 0]);
    const unparented = composeWorld(child, []);
    expect(worldTranslation(unparented)).toEqual([10, 0, 0]);
  });

  it("reads the world direction of a local axis", () => {
    const m = fromRotationTranslationScale(mat4(), quatFromSourceAngles([0, 90, 0]), [0, 0, 0], [1, 1, 1]);
    const z = localZDirection(m);
    expect(Math.hypot(z[0], z[1], z[2])).toBeCloseTo(1, 5);
    const point = transformPointBy(m, [1, 0, 0]);
    expect(point[0]).toBeCloseTo(0, 5);
    expect(point[1]).toBeCloseTo(1, 5);
  });

  it("transforms an axis-aligned box conservatively", () => {
    const m = fromRotationTranslationScale(mat4(), quatFromSourceAngles([45, 45, 0]), [0, 0, 0], [1, 1, 1]);
    const b = instanceBounds({ min: [-8, -8, -8], max: [8, 8, 8] }, m);
    for (let c = 0; c < 3; c++) {
      expect(b.max[c] - b.min[c]).toBeGreaterThan(16);
    }
  });

  it("classifies translucent materials by name", () => {
    expect(isTranslucentMaterial("glass/panel")).toBe(true);
    expect(isTranslucentMaterial("props/water/lake")).toBe(true);
    expect(isTranslucentMaterial("props/concrete/rock")).toBe(false);
  });

  it("draws the whole dev set opaque, whatever the host says about it", () => {
    // The host reports `translucent: true` for each of these, because it reads
    // the `blendStateMask` bit rather than the blend state. That bit cannot
    // decide the pass, so the classification must not follow it.
    for (const material of DEV_MATERIALS) {
      expect(isTranslucentMaterial(material), material).toBe(false);
    }
  });

  it("does not change when the host's blend mask flips", () => {
    const opaque = { path: "world/dev/dev_gray_512", blendStateMask: "0x4", translucent: true };
    const clear = { path: "world/dev/dev_gray_512", blendStateMask: "0x0", translucent: false };
    expect(isTranslucentMaterial(opaque.path)).toBe(isTranslucentMaterial(clear.path));
  });
});

describe("bounds helper", () => {
  it("returns a zero box for no positions", () => {
    expect(boundsOf(new Float32Array(0))).toEqual({ min: [0, 0, 0], max: [0, 0, 0] });
  });

  it("ignores non-finite coordinates", () => {
    const positions = new Float32Array([0, 0, 0, Number.NaN, 5, 5, 4, 4, 4]);
    const b = boundsOf(positions);
    // Only the non-finite component is dropped; the rest of that vertex counts.
    expect(b.max).toEqual([4, 5, 5]);
  });
});

describe("skyTransform", () => {
  it("keeps the vista around the eye, below the sky camera, turned a quarter", () => {
    const m = skyTransform([1000, -2000, 3000]);
    // The view's offset from the sky camera is the eye over the sky scale.
    expect(m[12]).toBeCloseTo(1000 - 1000 / SKY_SCALE, 3);
    expect(m[13]).toBeCloseTo(-2000 + 2000 / SKY_SCALE, 3);
    expect(m[14]).toBeCloseTo(3000 - 3000 / SKY_SCALE - SKY_CAMERA_ABOVE_VISTA, 3);
    // A quarter turn about z sends the model's +x to world +y.
    expect(m[0]).toBeCloseTo(0, 6);
    expect(m[1]).toBeCloseTo(1, 6);
    expect(m[10]).toBe(1);
  });
});
