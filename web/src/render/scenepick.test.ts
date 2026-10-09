import { describe, expect, it } from "vitest";
import type { Id, MeshBuffers, Vec3 } from "../core/api";
import type { DocObject } from "../doc/model";
import { fromRotationTranslationScale, mat4 } from "../math/mat4";
import { SceneBuilder, defaultSceneOptions, type SceneObject, type SceneOptions, type SceneSource } from "./scene";
import { pickFarthest, pickScene, pickSceneAll } from "./scenepick";

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

let idCounter = 1000;
function nextId(): string {
  idCounter += 1;
  return `00000000-0000-4000-8000-${idCounter.toString().padStart(12, "0")}`;
}

function brush(id: string): DocObject {
  return {
    id,
    name: "brush",
    layer: "layer-1",
    parent: null,
    transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
    hidden: false,
    locked: false,
    kind: { type: "brush", brush: { faces: [], contents: 1, detail: false } },
  };
}

/** Two boxes on one ray: `near` in front of `far` from above. */
function stackedScene(): { scene: ReturnType<SceneBuilder["build"]>; near: Id; far: Id } {
  const near = nextId();
  const far = nextId();
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
  const worldOf = (object: DocObject): Float32Array =>
    fromRotationTranslationScale(mat4(), object.transform.rotation, object.transform.position, object.transform.scale);
  const asObject = (object: DocObject): SceneObject => ({ doc: object, world: worldOf(object), worldBounds: null, hidden: false, ancestors: [] });
  const options: SceneOptions = { ...defaultSceneOptions(), invalidate: new Set(), selection: new Set(), hover: null };
  const scene = new SceneBuilder(source).build([asObject(brush(near)), asObject(brush(far))], options);
  return { scene, near, far };
}

const ORIGIN: Vec3 = [4, 4, 64];
const DIR: Vec3 = [0, 0, -1];

describe("object pick without x-ray", () => {
  it("picks the front object", () => {
    const { scene, near } = stackedScene();
    expect(pickScene(scene, ORIGIN, DIR)?.id).toBe(near);
  });
});

describe("object pick with x-ray", () => {
  it("lists every hit nearest first", () => {
    const { scene, near, far } = stackedScene();
    const hits = pickSceneAll(scene, ORIGIN, DIR);
    expect(hits.map((h) => h.id)).toEqual([near, far]);
    expect(hits[0].distance).toBeLessThan(hits[1].distance);
  });

  it("resolves the occluded object", () => {
    const { scene, far } = stackedScene();
    expect(pickFarthest(pickSceneAll(scene, ORIGIN, DIR))?.id).toBe(far);
  });

  it("resolves nothing when the ray hits nothing", () => {
    const { scene } = stackedScene();
    expect(pickSceneAll(scene, [512, 512, 64], DIR)).toEqual([]);
    expect(pickFarthest([])).toBeNull();
  });

  it("honours the accept filter on every hit", () => {
    const { scene, far } = stackedScene();
    const hits = pickSceneAll(scene, ORIGIN, DIR, 1 << 22, (id) => id === far);
    expect(hits.map((h) => h.id)).toEqual([far]);
  });
});
