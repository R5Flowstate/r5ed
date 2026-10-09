import type { Id, Vec3 } from "../core/api";
import { type Mat4 } from "../math/mat4";
import { type Aabb, aabbCenter, expandAabb } from "../math/vec3";
import { aabbInFrustum, cullBvh, frustumFromViewProj } from "./frustum";
import { DEPTH_FORMAT, type GpuContext, mipChainCount } from "./device";
import { MaterialTable, type MaterialSlot, type TextureUpload, defaultSlot, uploadDds } from "./material";
import { BOX_SHADER, BILLBOARD_SHADER, LINE_SHADER, SURFACE_SHADER } from "./shaders";
import { type Scene, type SceneInstance, VERTEX_FLOATS } from "./scene";
import { isHiddenMaterial, isTranslucentMaterial, skyTransform } from "./scene";
import { type DrawGroup, DrawList, FloatPool, IndexPool } from "./buffers";
import type { GlyphAtlas } from "./atlas";
import type { PackedLightmap } from "./lightmap";
import { REF_INSTANCE_FLOATS, REF_PICK_BASE, type RefDraw, type RefPropLayer, refPropId } from "./refprops";
import { paddedRowBytes, unpackRows } from "./png";
import { defaultShading, drawsWire, solidColorIndex, type ShadingState } from "./shading";

export class RendererError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "RendererError";
  }
}

const FRAME_FLOATS = 32 + 4 * 10;
const INSTANCE_FLOATS = 24;
const LINE_FLOATS = 8;
const BILLBOARD_FLOATS = 12;
const BOX_FLOATS = 12;
const GRID_FLOATS = 4;
/** Pick ids are written as a float, so they are limited to exact integers. */
const MAX_PICK_ID = 1 << 23;

type Upload = Parameters<GPUQueue["writeTexture"]>[1];

export interface Lighting {
  sunDir: Vec3;
  sunColor: [number, number, number, number];
  skyColor: [number, number, number, number];
  groundColor: [number, number, number, number];
  fogColor: [number, number, number];
  fogDensity: number;
  fogStart: number;
  fogEnd: number;
  showLightmaps: boolean;
  gridMajor: [number, number, number];
  gridMinor: [number, number, number];
  gridFade: number;
}

export function defaultLighting(): Lighting {
  return {
    sunDir: [0.42, -0.55, 0.72],
    sunColor: [1.05, 0.98, 0.86, 1],
    skyColor: [0.35, 0.44, 0.6, 1],
    groundColor: [0.16, 0.15, 0.14, 1],
    fogColor: [0.08, 0.09, 0.11],
    fogDensity: 0.000008,
    fogStart: 2400,
    fogEnd: 12000,
    showLightmaps: false,
    gridMajor: [0.42, 0.46, 0.52],
    gridMinor: [0.3, 0.33, 0.38],
    gridFade: 2400,
  };
}

export interface ViewMatrices {
  viewProj: Mat4;
  invViewProj: Mat4;
  eye: Vec3;
  width: number;
  height: number;
}

export interface FrameStats {
  opaqueGroups: number;
  translucentGroups: number;
  overlayDraws: number;
  culled: number;
  instances: number;
  chunks: number;
}

/** A geometry chunk placed in the frame's index pool. */
interface ChunkPack {
  chunk: number;
  indexBase: number;
  indexCount: number;
  subBases: number[];
}

/**
 * Owns every GPU resource and issues the frame. Geometry is uploaded once per
 * chunk per frame, and every instance of a chunk shares it through one
 * instanced draw, so repeated props cost one upload and one draw.
 */
export class Renderer {
  readonly context: GpuContext;
  readonly materials: MaterialTable;

  private device: GPUDevice;
  private frameBuffer: GPUBuffer;
  private frameData = new Float32Array(FRAME_FLOATS);
  private frameBindGroup: GPUBindGroup;
  private frameLayout: GPUBindGroupLayout;
  private atlasLayout: GPUBindGroupLayout;
  private atlasBindGroup: GPUBindGroup | null = null;
  private atlasTexture: GPUTexture | null = null;
  private lightmapTexture: GPUTexture | null = null;

  private vertices!: FloatPool;
  private indices!: IndexPool;
  private instances!: FloatPool;
  private prepared: { instances: SceneInstance[]; chunkCount: number; refs: number; eye: Vec3; viewProj: number[]; result: { packs: ChunkPack[]; stats: FrameStats } } | null = null;
  private refBuffer: GPUBuffer | null = null;
  private refUploaded: { layer: RefPropLayer; version: number } | null = null;
  private refDraws: RefDraw[] = [];
  /** Where each chunk's geometry already sits in the vertex and index pools. */
  private resident = new Map<Scene["chunks"][number], ChunkPack>();
  private lines!: FloatPool;
  private billboards!: FloatPool;
  private boxes!: FloatPool;
  private grid!: FloatPool;
  private draws = new DrawList();
  /** The sky vista's draws, which a pass of their own puts behind the world. */
  private skyDraws: DrawGroup[] = [];

  private surfaceModule: GPUShaderModule;
  private lineModule: GPUShaderModule;
  private billboardModule: GPUShaderModule;
  private boxModule: GPUShaderModule;
  private surfaceLayouts: GPUVertexBufferLayout[] = [];
  private instanceLayout: GPUVertexBufferLayout;
  private surfaceCache = new Map<string, { opaque: GPURenderPipeline; translucent: GPURenderPipeline }>();
  private linePipelines = new Map<GPUTextureFormat, GPURenderPipeline>();
  private gridPipelines = new Map<GPUTextureFormat, GPURenderPipeline>();
  private billboardPipelines = new Map<GPUTextureFormat, GPURenderPipeline>();
  private boxPipelines = new Map<GPUTextureFormat, GPURenderPipeline>();

  private depthTexture: GPUTexture | null = null;
  private depthView: GPUTextureView | null = null;
  private depthSize = [0, 0];
  private pickTexture: GPUTexture | null = null;
  private pickView: GPUTextureView | null = null;
  private pickBuffer: GPUBuffer | null = null;
  private pickDepth: GPUTexture | null = null;
  private pickDepthView: GPUTextureView | null = null;
  private pickSize = [0, 0];
  private clear: [number, number, number, number] = [0.055, 0.06, 0.07, 1];
  private stats: FrameStats = { opaqueGroups: 0, translucentGroups: 0, overlayDraws: 0, culled: 0, instances: 0, chunks: 0 };

  constructor(context: GpuContext) {
    this.context = context;
    this.device = context.device;
    this.materials = new MaterialTable(this.device);
    this.vertices = new FloatPool(this.device, "scene-vertices", GPUBufferUsage.VERTEX);
    this.indices = new IndexPool(this.device, "scene-indices", GPUBufferUsage.INDEX | GPUBufferUsage.COPY_DST);
    this.instances = new FloatPool(this.device, "scene-instances", GPUBufferUsage.VERTEX);
    this.lines = new FloatPool(this.device, "overlay-lines", GPUBufferUsage.VERTEX);
    this.billboards = new FloatPool(this.device, "billboards", GPUBufferUsage.VERTEX);
    this.boxes = new FloatPool(this.device, "boxes", GPUBufferUsage.VERTEX);
    this.grid = new FloatPool(this.device, "grid", GPUBufferUsage.VERTEX);
    this.frameBuffer = this.device.createBuffer({
      label: "frame",
      size: FRAME_FLOATS * 4,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    this.frameLayout = this.device.createBindGroupLayout({
      label: "frame",
      entries: [{ binding: 0, visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT, buffer: { type: "uniform" } }],
    });
    this.frameBindGroup = this.device.createBindGroup({
      layout: this.frameLayout,
      entries: [{ binding: 0, resource: { buffer: this.frameBuffer } }],
    });
    this.atlasLayout = this.device.createBindGroupLayout({
      label: "atlas",
      entries: [
        { binding: 0, visibility: GPUShaderStage.FRAGMENT, sampler: {} },
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: {} },
      ],
    });
    this.surfaceModule = this.device.createShaderModule({ label: "surface", code: SURFACE_SHADER });
    this.lineModule = this.device.createShaderModule({ label: "line", code: LINE_SHADER });
    this.billboardModule = this.device.createShaderModule({ label: "billboard", code: BILLBOARD_SHADER });
    this.boxModule = this.device.createShaderModule({ label: "box", code: BOX_SHADER });
    this.surfaceLayouts = [
      {
        arrayStride: VERTEX_FLOATS * 4,
        attributes: [
          { shaderLocation: 0, offset: 0, format: "float32x3" },
          { shaderLocation: 1, offset: 12, format: "float32x3" },
          { shaderLocation: 2, offset: 24, format: "float32x2" },
        ],
      },
    ];
    this.instanceLayout = {
      arrayStride: INSTANCE_FLOATS * 4,
      stepMode: "instance",
      attributes: [
        { shaderLocation: 3, offset: 0, format: "float32x4" },
        { shaderLocation: 4, offset: 16, format: "float32x4" },
        { shaderLocation: 5, offset: 32, format: "float32x4" },
        { shaderLocation: 6, offset: 48, format: "float32x4" },
        { shaderLocation: 7, offset: 64, format: "float32x4" },
        { shaderLocation: 8, offset: 80, format: "float32x4" },
      ],
    };
  }

  /** Uploads the runtime-drawn glyph atlas the billboards sample. */
  setAtlas(atlas: GlyphAtlas): void {
    this.atlasTexture?.destroy();
    this.atlasTexture = this.device.createTexture({
      label: "glyph-atlas",
      size: [atlas.width, atlas.height, 1],
      format: "rgba8unorm",
      mipLevelCount: mipChainCount(atlas.width, atlas.height),
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });
    this.device.queue.writeTexture(
      { texture: this.atlasTexture },
      atlas.pixels as unknown as Upload,
      { bytesPerRow: atlas.width * 4, rowsPerImage: atlas.height },
      [atlas.width, atlas.height, 1],
    );
    this.atlasBindGroup = this.device.createBindGroup({
      layout: this.atlasLayout,
      entries: [
        { binding: 0, resource: this.device.createSampler({ magFilter: "linear", minFilter: "linear" }) },
        { binding: 1, resource: this.atlasTexture.createView() },
      ],
    });
  }

  setClearColor(rgba: [number, number, number, number]): void {
    this.clear = rgba;
  }

  /**
   * Uploads the packed per-object lightmap atlas the surface shader samples.
   * Passing null drops it and the shader falls back to analytic lighting.
   */
  setLightmapAtlas(atlas: PackedLightmap | null): void {
    this.lightmapTexture?.destroy();
    this.lightmapTexture = null;
    this.materials.setLightmapView(null);
    if (!atlas) return;
    const texture = this.device.createTexture({
      label: "lightmap-atlas",
      size: [atlas.width, atlas.height, 1],
      format: "rgba8unorm",
      mipLevelCount: mipChainCount(atlas.width, atlas.height),
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });
    this.device.queue.writeTexture(
      { texture },
      atlas.pixels as unknown as Upload,
      { bytesPerRow: atlas.width * 4, rowsPerImage: atlas.height },
      [atlas.width, atlas.height, 1],
    );
    this.lightmapTexture = texture;
    this.materials.setLightmapView(texture.createView());
  }

  private blend(): GPUBlendState {
    return {
      color: { srcFactor: "src-alpha", dstFactor: "one-minus-src-alpha", operation: "add" },
      alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
    };
  }

  private surfacePipelines(
    shading: ShadingState,
    format: GPUTextureFormat = this.context.format,
  ): { opaque: GPURenderPipeline; translucent: GPURenderPipeline } {
    const solid = shading.mode === "solid";
    const xray = shading.xray > 0;
    const cull = solid && shading.cullBackfaces;
    const key = `${format}|${solid ? "solid" : "textured"}|${xray ? "xray" : "opaque"}|${cull ? "cull" : "nocull"}`;
    const hit = this.surfaceCache.get(key);
    if (hit) return hit;
    const layout = this.device.createPipelineLayout({
      bindGroupLayouts: [this.frameLayout, this.materials.bindGroupLayout()],
    });
    // Engine triangles are CW (the one CCW->CW flip lives in rcomp), so a
    // culling pipeline names them front explicitly.
    const make = (translucent: boolean): GPURenderPipeline =>
      this.device.createRenderPipeline({
        label: translucent ? "surface-translucent" : "surface-opaque",
        layout,
        vertex: { module: this.surfaceModule, entryPoint: "vs", buffers: [this.surfaceLayouts[0], this.instanceLayout] },
        fragment: {
          module: this.surfaceModule,
          entryPoint: solid ? "fs_solid" : translucent ? "fs_translucent" : "fs_surface",
          targets: [{ format, blend: translucent || xray ? this.blend() : undefined }],
        },
        primitive: { topology: "triangle-list", cullMode: cull ? "back" : "none", ...(cull ? { frontFace: "cw" as const } : {}) },
        depthStencil: { format: DEPTH_FORMAT, depthWriteEnabled: !translucent && !xray, depthCompare: "less-equal" },
      });
    const made = { opaque: make(false), translucent: make(true) };
    this.surfaceCache.set(key, made);
    return made;
  }

  private pickPipelines = new Map<boolean, GPURenderPipeline>();

  private ensurePickPipeline(xray: boolean): GPURenderPipeline {
    const hit = this.pickPipelines.get(xray);
    if (hit) return hit;
    // The x-ray pass keeps the farthest id instead of the nearest: with a
    // greater test over a zeroed depth buffer, draw order stops mattering and
    // every draw only wins where it lies behind what won so far. This is what
    // the CPU `pickFarthest` agrees with.
    const made = this.device.createRenderPipeline({
      label: xray ? "pick-far" : "pick",
      layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.frameLayout] }),
      vertex: { module: this.surfaceModule, entryPoint: "vs", buffers: [this.surfaceLayouts[0], this.instanceLayout] },
      fragment: { module: this.surfaceModule, entryPoint: "fs_pick", targets: [{ format: "r32uint" }] },
      primitive: { topology: "triangle-list", cullMode: "none" },
      depthStencil: xray
        ? { format: DEPTH_FORMAT, depthWriteEnabled: true, depthCompare: "greater" }
        : { format: DEPTH_FORMAT, depthWriteEnabled: true, depthCompare: "less" },
    });
    this.pickPipelines.set(xray, made);
    return made;
  }

  private ensureLinePipeline(grid: boolean, format: GPUTextureFormat = this.context.format): GPURenderPipeline {
    const cache = grid ? this.gridPipelines : this.linePipelines;
    const hit = cache.get(format);
    if (hit) return hit;
    const pipeline = this.device.createRenderPipeline({
      label: grid ? "grid" : "line",
      layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.frameLayout] }),
      vertex: {
        module: this.lineModule,
        entryPoint: grid ? "vs_grid" : "vs",
        buffers: [
          grid
            ? {
                arrayStride: GRID_FLOATS * 4,
                attributes: [
                  { shaderLocation: 0, offset: 0, format: "float32x3" },
                  { shaderLocation: 1, offset: 12, format: "float32" },
                ],
              }
            : {
                arrayStride: LINE_FLOATS * 4,
                attributes: [
                  { shaderLocation: 0, offset: 0, format: "float32x3" },
                  { shaderLocation: 1, offset: 12, format: "float32" },
                  { shaderLocation: 2, offset: 16, format: "float32x4" },
                ],
              },
        ],
      },
      fragment: {
        module: this.lineModule,
        entryPoint: grid ? "fs_grid" : "fs_color",
        targets: [{ format, blend: this.blend() }],
      },
      primitive: { topology: "line-list" },
      depthStencil: { format: DEPTH_FORMAT, depthWriteEnabled: false, depthCompare: "less-equal" },
    });
    cache.set(format, pipeline);
    return pipeline;
  }

  private ensureBillboardPipeline(format: GPUTextureFormat = this.context.format): GPURenderPipeline {
    const hit = this.billboardPipelines.get(format);
    if (hit) return hit;
    const made = this.device.createRenderPipeline({
      label: "billboard",
      layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.frameLayout, this.atlasLayout] }),
      vertex: {
        module: this.billboardModule,
        entryPoint: "vs",
        buffers: [
          {
            arrayStride: BILLBOARD_FLOATS * 4,
            stepMode: "instance",
            attributes: [
              { shaderLocation: 0, offset: 0, format: "float32x4" },
              { shaderLocation: 1, offset: 16, format: "float32x4" },
              { shaderLocation: 2, offset: 32, format: "float32x4" },
            ],
          },
        ],
      },
      fragment: {
        module: this.billboardModule,
        entryPoint: "fs_billboard",
        targets: [{ format, blend: this.blend() }],
      },
      primitive: { topology: "triangle-list", cullMode: "none" },
      depthStencil: { format: DEPTH_FORMAT, depthWriteEnabled: false, depthCompare: "less-equal" },
    });
    this.billboardPipelines.set(format, made);
    return made;
  }

  private ensureBoxPipeline(format: GPUTextureFormat = this.context.format): GPURenderPipeline {
    const hit = this.boxPipelines.get(format);
    if (hit) return hit;
    const made = this.device.createRenderPipeline({
      label: "box",
      layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.frameLayout] }),
      vertex: {
        module: this.boxModule,
        entryPoint: "vs",
        buffers: [
          {
            arrayStride: BOX_FLOATS * 4,
            stepMode: "instance",
            attributes: [
              { shaderLocation: 0, offset: 0, format: "float32x4" },
              { shaderLocation: 1, offset: 16, format: "float32x4" },
              { shaderLocation: 2, offset: 32, format: "float32x4" },
            ],
          },
        ],
      },
      fragment: {
        module: this.boxModule,
        entryPoint: "fs_box",
        targets: [{ format, blend: this.blend() }],
      },
      primitive: { topology: "triangle-list", cullMode: "none" },
      depthStencil: { format: DEPTH_FORMAT, depthWriteEnabled: false, depthCompare: "less-equal" },
    });
    this.boxPipelines.set(format, made);
    return made;
  }

  private writeFrame(view: ViewMatrices, light: Lighting, shading: ShadingState): void {
    const d = this.frameData;
    d.set(view.viewProj, 0);
    d.set(view.invViewProj, 16);
    let o = 32;
    const put = (a: readonly number[], w: number): void => {
      d[o] = a[0];
      d[o + 1] = a[1];
      d[o + 2] = a[2];
      d[o + 3] = w;
      o += 4;
    };
    put(view.eye, 1);
    put(light.sunDir, 0);
    put(light.sunColor, light.sunColor[3]);
    put(light.skyColor, light.skyColor[3]);
    put(light.groundColor, light.groundColor[3]);
    put(light.fogColor, light.fogDensity);
    put(light.gridMajor, light.gridFade);
    put(light.gridMinor, light.showLightmaps ? 1 : 0);
    put([solidColorIndex(shading.color), Math.min(1, Math.max(0, shading.xray)), 0], 0);
    put(shading.single, 1);
    this.device.queue.writeBuffer(this.frameBuffer, 0, d);
  }

  private materialBind(material: string): GPUBindGroup {
    const hit = this.materials.get(material);
    if (hit) return hit.bindGroup;
    return this.materials.compile(defaultSlot(material)).bindGroup;
  }

  /**
   * Packs the scene for one view: each visible chunk is written once, its
   * sub-ranges become instanced draws, and translucent sub-ranges become one
   * draw per instance so they can be sorted back to front.
   */
  private prepare(scene: Scene, view: ViewMatrices): { packs: ChunkPack[]; stats: FrameStats } {
    // A still camera over an unchanged instance list draws what it drew last.
    const last = this.prepared;
    if (
      last &&
      last.instances === scene.instances &&
      last.chunkCount === scene.chunks.length &&
      last.refs === (scene.refs?.version ?? -1) &&
      last.eye[0] === view.eye[0] && last.eye[1] === view.eye[1] && last.eye[2] === view.eye[2] &&
      last.viewProj.every((v, i) => v === view.viewProj[i])
    ) {
      return last.result;
    }
    const result = this.prepareFresh(scene, view);
    this.prepared = { instances: scene.instances, chunkCount: scene.chunks.length, refs: scene.refs?.version ?? -1, eye: [...view.eye] as Vec3, viewProj: Array.from(view.viewProj), result };
    return result;
  }

  private prepareFresh(scene: Scene, view: ViewMatrices): { packs: ChunkPack[]; stats: FrameStats } {
    const frustum = frustumFromViewProj(view.viewProj);
    // Chunk geometry stays resident; the pools are only rewound once edits have
    // left more dead chunks in them than the scene holds live ones.
    if (this.resident.size > 2 * scene.chunks.length + 64) {
      this.vertices.begin();
      this.indices.begin();
      this.resident.clear();
    }
    this.instances.begin();
    this.draws.clear();
    this.skyDraws = [];
    const skyChunk = scene.sky ? scene.chunks[scene.sky.chunk] : undefined;
    if (skyChunk && skyChunk.indices.length > 0) {
      const pack = this.residentPack(scene, scene.sky!.chunk);
      const at = this.instances.alloc(INSTANCE_FLOATS);
      const floats = this.instances.view(at, INSTANCE_FLOATS);
      floats.set(skyTransform(view.eye), 0);
      floats[16] = 1;
      floats[17] = 1;
      floats[18] = 1;
      floats[19] = 0;
      for (const range of skyChunk.subRanges) {
        if (range.count === 0 || isHiddenMaterial(range.material)) continue;
        this.skyDraws.push({
          firstIndex: pack.indexBase + range.start,
          indexCount: range.count,
          firstInstance: at / INSTANCE_FLOATS,
          instanceCount: 1,
          material: range.material,
          translucent: isTranslucentMaterial(range.material),
          pick: 0,
        });
      }
    }
    const visible: SceneInstance[] = [];
    let culled = 0;
    const hits = scene.bvh.nodes.length > 0 && scene.bvh.order.length === scene.instances.length
      ? cullBvh(frustum, scene.bvh.nodes, scene.bvh.order, scene.bvh.boxes, 4).sort((a, b) => a - b)
      : scene.instances.map((_, i) => i).filter((i) => aabbInFrustum(frustum, expandAabb(scene.instances[i].bounds, 4)));
    culled = scene.instances.length - hits.length;
    for (const i of hits) {
      const instance = scene.instances[i];
      const chunk = scene.chunks[instance.chunk];
      if (!chunk || chunk.indices.length === 0 || instance.wireOnly) continue;
      visible.push(instance);
    }

    const packs: ChunkPack[] = [];
    const usersOf = new Map<number, SceneInstance[]>();
    for (const instance of visible) {
      const users = usersOf.get(instance.chunk);
      if (users) {
        users.push(instance);
        continue;
      }
      usersOf.set(instance.chunk, [instance]);
      packs.push({ ...this.residentPack(scene, instance.chunk), chunk: instance.chunk });
    }

    const writeInstance = (instance: SceneInstance): number => {
      const at = this.instances.alloc(INSTANCE_FLOATS);
      const viewFloats = this.instances.view(at, INSTANCE_FLOATS);
      viewFloats.set(instance.transform, 0);
      // A locked object draws but never answers a pick.
      const pick = instance.selectable && instance.pick <= MAX_PICK_ID ? instance.pick : 0;
      viewFloats[16] = 1;
      viewFloats[17] = 1;
      viewFloats[18] = 1;
      viewFloats[19] = pick;
      const rect = instance.lightmap;
      if (rect) {
        viewFloats[20] = rect[0];
        viewFloats[21] = rect[1];
        viewFloats[22] = rect[2];
        viewFloats[23] = rect[3];
      }
      return at / INSTANCE_FLOATS;
    };

    for (const pack of packs) {
      const chunk = scene.chunks[pack.chunk];
      const users = usersOf.get(pack.chunk) ?? [];
      for (let s = 0; s < chunk.subRanges.length; s++) {
        const range = chunk.subRanges[s];
        if (range.count === 0 || isHiddenMaterial(range.material)) continue;
        const translucent = isTranslucentMaterial(range.material);
        if (translucent) {
          const ordered = users
            .slice()
            .sort((a, b) => distanceTo(b.bounds, view.eye) - distanceTo(a.bounds, view.eye));
          for (const instance of ordered) {
            const first = writeInstance(instance);
            this.draws.push({
              firstIndex: pack.indexBase + range.start,
              indexCount: range.count,
              firstInstance: first,
              instanceCount: 1,
              material: range.material,
              translucent: true,
              pick: instance.selectable ? instance.pick : 0,
            });
          }
          continue;
        }
        const first = users.map(writeInstance);
        this.draws.push({
          firstIndex: pack.indexBase + range.start,
          indexCount: range.count,
          firstInstance: first[0] ?? 0,
          instanceCount: first.length,
          material: range.material,
          translucent: false,
          pick: 0,
        });
      }
    }

    let refProps = 0;
    if (scene.refs) {
      this.refDraws.length = 0;
      refProps = scene.refs.cull(frustum, view.eye, this.refDraws);
      for (const d of this.refDraws) {
        const chunk = scene.chunks[d.chunk];
        if (!chunk || chunk.indices.length === 0) continue;
        const pack = this.residentPack(scene, d.chunk);
        for (const range of chunk.subRanges) {
          if (range.count === 0 || isHiddenMaterial(range.material)) continue;
          this.draws.push({
            firstIndex: pack.indexBase + range.start,
            indexCount: range.count,
            firstInstance: d.first,
            instanceCount: d.count,
            material: range.material,
            translucent: isTranslucentMaterial(range.material),
            pick: 0,
            refs: true,
          });
        }
      }
    }

    const opaque = this.draws.byPass(false);
    const translucentGroups = this.draws.byPass(true);
    return {
      packs,
      stats: {
        opaqueGroups: opaque.length,
        translucentGroups: translucentGroups.length,
        overlayDraws: 0,
        culled,
        instances: visible.length + refProps,
        chunks: packs.length,
      },
    };
  }

  /** Where a chunk's geometry sits in the pools, uploading it the first time. */
  private residentPack(scene: Scene, index: number): ChunkPack {
    const chunk = scene.chunks[index];
    let pack = this.resident.get(chunk);
    if (!pack) {
      const vertexAt = this.vertices.alloc(chunk.vertices.length);
      this.vertices.view(vertexAt, chunk.vertices.length).set(chunk.vertices);
      const baseVertex = vertexAt / VERTEX_FLOATS;
      const indexAt = this.indices.alloc(chunk.indices.length);
      const indexView = this.indices.view(indexAt, chunk.indices.length);
      for (let i = 0; i < chunk.indices.length; i++) indexView[i] = chunk.indices[i] + baseVertex;
      pack = { chunk: index, indexBase: indexAt, indexCount: chunk.indices.length, subBases: chunk.subRanges.map((r) => r.start) };
      this.resident.set(chunk, pack);
    }
    return pack;
  }

  /**
   * The reference props' instance buffer: uploaded whole once per layer, then
   * only the slots a promotion or a restore changed.
   */
  private syncRefBuffer(layer: RefPropLayer | null): GPUBuffer | null {
    if (!layer || layer.table.count === 0) return null;
    const fresh = !this.refBuffer || !this.refUploaded || this.refUploaded.layer !== layer || this.refUploaded.version !== layer.uploadVersion;
    if (fresh) {
      this.refBuffer?.destroy();
      this.refBuffer = this.device.createBuffer({
        label: "reference-props",
        size: layer.instances.byteLength,
        usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
      });
      this.device.queue.writeBuffer(this.refBuffer, 0, layer.instances);
      this.refUploaded = { layer, version: layer.uploadVersion };
      layer.dirtySlots.length = 0;
    } else if (layer.dirtySlots.length > 0) {
      for (const slot of layer.dirtySlots) {
        this.device.queue.writeBuffer(this.refBuffer as GPUBuffer, slot * REF_INSTANCE_FLOATS * 4, layer.instances, slot * REF_INSTANCE_FLOATS, REF_INSTANCE_FLOATS);
      }
      layer.dirtySlots.length = 0;
    }
    return this.refBuffer;
  }

  /** Floats of scene wire at the head of the line buffer. */
  private wireLineElements = 0;

  private packOverlays(scene: Scene, eye: Vec3, shading: ShadingState): number {
    this.lines.begin();
    this.grid.begin();
    this.billboards.begin();
    this.boxes.begin();
    const grid = scene.grid;
    if (grid.count > 0) {
      const at = this.grid.alloc(grid.count * GRID_FLOATS);
      const v = this.grid.view(at, grid.count * GRID_FLOATS);
      for (let i = 0; i < grid.count; i++) {
        v[i * GRID_FLOATS] = grid.positions[i * 3];
        v[i * GRID_FLOATS + 1] = grid.positions[i * 3 + 1];
        v[i * GRID_FLOATS + 2] = grid.positions[i * 3 + 2];
        v[i * GRID_FLOATS + 3] = grid.weight[i];
      }
    }
    // Wireframe mode draws every chunk's budgeted wire buffer; solid draws
    // the scoped overlay buffer when the toggle is on (edit mode). Cutters
    // draw as wire in every mode.
    if (drawsWire(shading) || scene.instances.some((i) => i.wireOnly)) {
      for (const wire of scene.wire) {
        if (wire.count === 0) continue;
        const at = this.lines.alloc(wire.count * LINE_FLOATS);
        const v = this.lines.view(at, wire.count * LINE_FLOATS);
        for (let i = 0; i < wire.count; i++) {
          const o = i * LINE_FLOATS;
          v[o] = wire.positions[i * 3];
          v[o + 1] = wire.positions[i * 3 + 1];
          v[o + 2] = wire.positions[i * 3 + 2];
          v[o + 3] = 1;
          v[o + 4] = wire.colors[i * 4];
          v[o + 5] = wire.colors[i * 4 + 1];
          v[o + 6] = wire.colors[i * 4 + 2];
          v[o + 7] = wire.colors[i * 4 + 3];
        }
      }
    }
    // The scene wire leads the buffer so a capture without overlays can
    // still draw it: in wireframe mode the wire is the picture.
    this.wireLineElements = this.lines.liveElements;
    const lines = scene.overlay.lines;
    if (lines.count > 0) {
      const at = this.lines.alloc(lines.count * LINE_FLOATS);
      const v = this.lines.view(at, lines.count * LINE_FLOATS);
      for (let i = 0; i < lines.count; i++) {
        const o = i * LINE_FLOATS;
        v[o] = lines.positions[i * 3];
        v[o + 1] = lines.positions[i * 3 + 1];
        v[o + 2] = lines.positions[i * 3 + 2];
        v[o + 3] = 1;
        v[o + 4] = lines.colors[i * 4];
        v[o + 5] = lines.colors[i * 4 + 1];
        v[o + 6] = lines.colors[i * 4 + 2];
        v[o + 7] = lines.colors[i * 4 + 3];
      }
    }
    let billboards = 0;
    for (const b of scene.overlay.billboards) {
      const d = Math.hypot(b.center[0] - eye[0], b.center[1] - eye[1], b.center[2] - eye[2]);
      const at = this.billboards.alloc(BILLBOARD_FLOATS);
      const v = this.billboards.view(at, BILLBOARD_FLOATS);
      v[0] = b.center[0];
      v[1] = b.center[1];
      v[2] = b.center[2];
      v[3] = Math.max(8, b.size * Math.max(0.05, d / 900));
      v[4] = b.color[0];
      v[5] = b.color[1];
      v[6] = b.color[2];
      v[7] = b.color[3];
      v[8] = b.rect[0];
      v[9] = b.rect[1];
      v[10] = b.rect[2];
      v[11] = b.rect[3];
      billboards++;
    }
    for (const b of scene.overlay.boxes) {
      const at = this.boxes.alloc(BOX_FLOATS);
      const v = this.boxes.view(at, BOX_FLOATS);
      v[0] = b.center[0];
      v[1] = b.center[1];
      v[2] = b.center[2];
      v[3] = 1;
      v[4] = b.color[0];
      v[5] = b.color[1];
      v[6] = b.color[2];
      v[7] = b.color[3];
      v[8] = Math.max(0.5, Math.abs(b.half[0]));
      v[9] = Math.max(0.5, Math.abs(b.half[1]));
      v[10] = Math.max(0.5, Math.abs(b.half[2]));
      v[11] = 0;
    }
    return billboards;
  }

  private ensureDepth(width: number, height: number): GPUTextureView {
    if (!this.depthTexture || this.depthSize[0] !== width || this.depthSize[1] !== height) {
      this.depthTexture?.destroy();
      this.depthTexture = this.device.createTexture({
        label: "depth",
        size: [width, height, 1],
        format: DEPTH_FORMAT,
        usage: GPUTextureUsage.RENDER_ATTACHMENT,
      });
      this.depthView = this.depthTexture.createView();
      this.depthSize = [width, height];
    }
    return this.depthView as GPUTextureView;
  }

  private ensurePickTarget(width: number, height: number): void {
    if (this.pickTexture && this.pickSize[0] === width && this.pickSize[1] === height) return;
    this.pickTexture?.destroy();
    this.pickBuffer?.destroy();
    this.pickDepth?.destroy();
    this.pickDepth = this.device.createTexture({
      label: "pick-depth",
      size: [width, height, 1],
      format: DEPTH_FORMAT,
      usage: GPUTextureUsage.RENDER_ATTACHMENT,
    });
    this.pickDepthView = this.pickDepth.createView();
    this.pickTexture = this.device.createTexture({
      label: "pick-target",
      size: [width, height, 1],
      format: "r32uint",
      usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
    });
    this.pickView = this.pickTexture.createView();
    this.pickBuffer = this.device.createBuffer({
      label: "pick-readback",
      size: 256,
      usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
    });
    this.pickSize = [width, height];
  }

  /** Renders one view into the swap chain. */
  render(scene: Scene, view: ViewMatrices, light: Lighting, shading: ShadingState = defaultShading()): void {
    this.writeFrame(view, light, shading);
    const prepared = this.prepare(scene, view);
    this.packOverlays(scene, view.eye, shading);
    const vertexBuffer = this.vertices.upload();
    const indexBuffer = this.indices.upload();
    const instanceBuffer = this.instances.upload();
    const gridBuffer = this.grid.upload();
    const lineBuffer = this.lines.upload();
    const billboardBuffer = this.billboards.upload();
    const boxBuffer = this.boxes.upload();
    const refBuffer = this.syncRefBuffer(scene.refs);

    const width = Math.max(1, Math.floor(view.width));
    const height = Math.max(1, Math.floor(view.height));
    const swap = this.context.context.getCurrentTexture().createView();
    const depth = this.ensureDepth(width, height);
    const encoder = this.device.createCommandEncoder({ label: "frame" });
    const overlay = this.recordFrame(encoder, swap, depth, this.context.format, true, shading, vertexBuffer, indexBuffer, instanceBuffer, gridBuffer, lineBuffer, billboardBuffer, boxBuffer, refBuffer);
    this.stats = { ...prepared.stats, overlayDraws: overlay };
    this.device.queue.submit([encoder.finish()]);
  }

  /**
   * Renders one view into an offscreen RGBA texture at an exact size and reads
   * it back as tightly packed pixels. The swap chain is untouched, so the next
   * normal frame draws as if this never happened.
   */
  async capture(scene: Scene, view: ViewMatrices, light: Lighting, overlays: boolean, shading: ShadingState = defaultShading()): Promise<Uint8Array> {
    const width = Math.max(1, Math.floor(view.width));
    const height = Math.max(1, Math.floor(view.height));
    this.writeFrame(view, light, shading);
    this.prepare(scene, view);
    this.packOverlays(scene, view.eye, shading);
    const vertexBuffer = this.vertices.upload();
    const indexBuffer = this.indices.upload();
    const instanceBuffer = this.instances.upload();
    const gridBuffer = this.grid.upload();
    const lineBuffer = this.lines.upload();
    const billboardBuffer = this.billboards.upload();
    const boxBuffer = this.boxes.upload();
    const refBuffer = this.syncRefBuffer(scene.refs);
    const format: GPUTextureFormat = "rgba8unorm";
    const target = this.device.createTexture({
      label: "capture",
      size: [width, height, 1],
      format,
      usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
    });
    const depth = this.device.createTexture({
      label: "capture-depth",
      size: [width, height, 1],
      format: DEPTH_FORMAT,
      usage: GPUTextureUsage.RENDER_ATTACHMENT,
    });
    const encoder = this.device.createCommandEncoder({ label: "capture" });
    this.recordFrame(encoder, target.createView(), depth.createView(), format, overlays, shading, vertexBuffer, indexBuffer, instanceBuffer, gridBuffer, lineBuffer, billboardBuffer, boxBuffer, refBuffer);
    const stride = paddedRowBytes(width);
    const readback = this.device.createBuffer({
      label: "capture-readback",
      size: stride * height,
      usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
    });
    encoder.copyTextureToBuffer({ texture: target, origin: { x: 0, y: 0, z: 0 } }, { buffer: readback, bytesPerRow: stride, rowsPerImage: height }, [width, height, 1]);
    this.device.queue.submit([encoder.finish()]);
    await readback.mapAsync(GPUMapMode.READ, 0, stride * height);
    const tight = unpackRows(new Uint8Array(readback.getMappedRange(0, stride * height).slice(0)), width, height);
    readback.unmap();
    readback.destroy();
    target.destroy();
    depth.destroy();
    return tight;
  }

  private recordFrame(
    encoder: GPUCommandEncoder,
    color: GPUTextureView,
    depth: GPUTextureView,
    format: GPUTextureFormat,
    withOverlays: boolean,
    shading: ShadingState,
    vertexBuffer: GPUBuffer | null,
    indexBuffer: GPUBuffer | null,
    instanceBuffer: GPUBuffer | null,
    gridBuffer: GPUBuffer | null,
    lineBuffer: GPUBuffer | null,
    billboardBuffer: GPUBuffer | null,
    boxBuffer: GPUBuffer | null,
    refBuffer: GPUBuffer | null,
  ): number {
    const wireframe = shading.mode === "wireframe";
    // The sky is drawn first with a depth buffer of its own, which is then
    // cleared, so the world always draws over it. Wireframe skips surfaces.
    const sky = !wireframe && this.skyDraws.length > 0 && vertexBuffer && indexBuffer && instanceBuffer;
    if (sky) {
      const skyPass = encoder.beginRenderPass({
        colorAttachments: [{ view: color, clearValue: this.clear, loadOp: "clear", storeOp: "store" }],
        depthStencilAttachment: { view: depth, depthClearValue: 1, depthLoadOp: "clear", depthStoreOp: "store" },
      });
      skyPass.setBindGroup(0, this.frameBindGroup);
      skyPass.setIndexBuffer(indexBuffer as GPUBuffer, "uint32");
      skyPass.setVertexBuffer(0, vertexBuffer as GPUBuffer);
      skyPass.setVertexBuffer(1, instanceBuffer as GPUBuffer);
      const pipelines = this.surfacePipelines(shading, format);
      for (const translucent of [false, true]) {
        skyPass.setPipeline(translucent ? pipelines.translucent : pipelines.opaque);
        for (const g of this.skyDraws) {
          if (g.translucent !== translucent) continue;
          skyPass.setBindGroup(1, this.materialBind(g.material));
          skyPass.drawIndexed(g.indexCount, g.instanceCount, g.firstIndex, 0, g.firstInstance);
        }
      }
      skyPass.end();
    }
    const pass = encoder.beginRenderPass({
      colorAttachments: [{ view: color, clearValue: this.clear, loadOp: sky ? "load" : "clear", storeOp: "store" }],
      depthStencilAttachment: { view: depth, depthClearValue: 1, depthLoadOp: "clear", depthStoreOp: "store" },
    });
    pass.setBindGroup(0, this.frameBindGroup);
    if (!wireframe && vertexBuffer && indexBuffer && instanceBuffer) {
      pass.setIndexBuffer(indexBuffer, "uint32");
      pass.setVertexBuffer(0, vertexBuffer);
      pass.setVertexBuffer(1, instanceBuffer);
      const pipelines = this.surfacePipelines(shading, format);
      for (const translucent of [false, true]) {
        pass.setPipeline(translucent ? pipelines.translucent : pipelines.opaque);
        let current = "";
        let onRefs = false;
        for (const group of this.draws.byPass(translucent)) {
          const refs = group.refs === true;
          if (refs && !refBuffer) continue;
          if (refs !== onRefs) {
            pass.setVertexBuffer(1, refs ? (refBuffer as GPUBuffer) : instanceBuffer);
            onRefs = refs;
          }
          if (group.material !== current) {
            pass.setBindGroup(1, this.materialBind(group.material));
            current = group.material;
          }
          pass.drawIndexed(group.indexCount, group.instanceCount, group.firstIndex, 0, group.firstInstance);
        }
        if (onRefs) pass.setVertexBuffer(1, instanceBuffer);
      }
    }
    if (!withOverlays && lineBuffer && this.wireLineElements > 0) {
      pass.setPipeline(this.ensureLinePipeline(false, format));
      pass.setBindGroup(0, this.frameBindGroup);
      pass.setVertexBuffer(0, lineBuffer);
      pass.draw(this.wireLineElements / LINE_FLOATS);
    }
    const overlay = withOverlays ? this.drawOverlays(pass, format, gridBuffer, lineBuffer, billboardBuffer, boxBuffer) : 0;
    pass.end();
    return overlay;
  }

  private drawOverlays(
    pass: GPURenderPassEncoder,
    format: GPUTextureFormat,
    gridBuffer: GPUBuffer | null,
    lineBuffer: GPUBuffer | null,
    billboardBuffer: GPUBuffer | null,
    boxBuffer: GPUBuffer | null,
  ): number {
    let drawn = 0;
    if (gridBuffer) {
      pass.setPipeline(this.ensureLinePipeline(true, format));
      pass.setBindGroup(0, this.frameBindGroup);
      pass.setVertexBuffer(0, gridBuffer);
      pass.draw(this.grid.liveElements / GRID_FLOATS);
      drawn++;
    }
    if (lineBuffer) {
      pass.setPipeline(this.ensureLinePipeline(false, format));
      pass.setBindGroup(0, this.frameBindGroup);
      pass.setVertexBuffer(0, lineBuffer);
      pass.draw(this.lines.liveElements / LINE_FLOATS);
      drawn++;
    }
    if (boxBuffer) {
      pass.setPipeline(this.ensureBoxPipeline(format));
      pass.setBindGroup(0, this.frameBindGroup);
      pass.setVertexBuffer(0, boxBuffer);
      pass.draw(36, this.boxes.liveElements / BOX_FLOATS);
      drawn++;
    }
    if (billboardBuffer && this.atlasBindGroup) {
      pass.setPipeline(this.ensureBillboardPipeline(format));
      pass.setBindGroup(0, this.frameBindGroup);
      pass.setBindGroup(1, this.atlasBindGroup);
      pass.setVertexBuffer(0, billboardBuffer);
      pass.draw(6, this.billboards.liveElements / BILLBOARD_FLOATS);
      drawn++;
    }
    return drawn;
  }

  /**
   * Renders the id target and reads one pixel back, resolving it to an object
   * id. The readback happens once per call, so a click costs one map. Without
   * x-ray the pass keeps the nearest id with a less test, so the surface in
   * front answers; with x-ray it keeps the farthest with a greater test over
   * a zeroed depth buffer, so an occluded object answers however the draws
   * are ordered. That farthest rule is what the CPU `pickFarthest` agrees
   * with, so both picks select the same occluded object.
   */
  async pick(scene: Scene, view: ViewMatrices, light: Lighting, px: number, py: number, xray = 0): Promise<Id | null> {
    const width = Math.max(1, Math.floor(view.width));
    const height = Math.max(1, Math.floor(view.height));
    if (px < 0 || py < 0 || px >= width || py >= height) return null;
    this.writeFrame(view, light, defaultShading());
    this.prepare(scene, view);
    const vertexBuffer = this.vertices.upload();
    const indexBuffer = this.indices.upload();
    const instanceBuffer = this.instances.upload();
    const refBuffer = this.syncRefBuffer(scene.refs);
    if (!vertexBuffer || !indexBuffer || !instanceBuffer) return null;
    this.ensurePickTarget(width, height);
    const x = Math.floor(px);
    const y = Math.floor(py);
    const encoder = this.device.createCommandEncoder({ label: "pick" });
    const far = xray > 0;
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        { view: this.pickView as GPUTextureView, clearValue: { r: 0, g: 0, b: 0, a: 0 }, loadOp: "clear", storeOp: "store" },
      ],
      // Near keeps the front id, far keeps the occluded one.
      depthStencilAttachment: { view: this.pickDepthView as GPUTextureView, depthClearValue: far ? 0 : 1, depthLoadOp: "clear", depthStoreOp: "discard" },
    });
    pass.setPipeline(this.ensurePickPipeline(far));
    pass.setBindGroup(0, this.frameBindGroup);
    pass.setVertexBuffer(0, vertexBuffer);
    pass.setVertexBuffer(1, instanceBuffer);
    pass.setIndexBuffer(indexBuffer, "uint32");
    // Every group draws: the pick id is per instance, in the instance buffer,
    // and an opaque group batches many instances under one draw.
    let onRefs = false;
    for (const group of this.draws.all()) {
      const refs = group.refs === true;
      if (refs && !refBuffer) continue;
      if (refs !== onRefs) {
        pass.setVertexBuffer(1, refs ? (refBuffer as GPUBuffer) : instanceBuffer);
        onRefs = refs;
      }
      pass.drawIndexed(group.indexCount, group.instanceCount, group.firstIndex, 0, group.firstInstance);
    }
    pass.end();
    const buffer = this.pickBuffer as GPUBuffer;
    encoder.copyTextureToBuffer(
      { texture: this.pickTexture as GPUTexture, origin: { x, y, z: 0 } },
      { buffer, bytesPerRow: 256, rowsPerImage: 1 },
      [1, 1, 1],
    );
    this.device.queue.submit([encoder.finish()]);
    await buffer.mapAsync(GPUMapMode.READ, 0, 256);
    const read = new Uint32Array(buffer.getMappedRange(0, 4))[0] ?? 0;
    buffer.unmap();
    if (read === 0) return null;
    if (read >= REF_PICK_BASE) return refPropId(read - REF_PICK_BASE);
    for (const instance of scene.instances) if (instance.pick === read) return instance.id;
    return null;
  }

  /** Counts from the last rendered frame. */
  frameStats(): FrameStats {
    return { ...this.stats };
  }

  /** Uploads a DDS and registers it so material slots can bind it. */
  registerTexture(guid: string, upload: TextureUpload): void {
    this.materials.addTexture(guid, upload);
  }

  /** Decodes and registers DDS bytes under `guid`. */
  addDdsTexture(guid: string, bytes: Uint8Array): void {
    this.materials.addTexture(guid, uploadDds(this.device, bytes, guid, this.context.bc));
  }

  /** Rebuilds one material from a slot, e.g. once its textures have arrived. */
  setMaterial(slot: MaterialSlot): void {
    // A material can change pass (translucent) once its info arrives.
    this.prepared = null;
    this.materials.compile(slot);
  }

  destroy(): void {
    this.resident.clear();
    this.refBuffer?.destroy();
    this.vertices.destroy();
    this.indices.destroy();
    this.instances.destroy();
    this.lines.destroy();
    this.grid.destroy();
    this.billboards.destroy();
    this.boxes.destroy();
    this.depthTexture?.destroy();
    this.pickTexture?.destroy();
    this.pickBuffer?.destroy();
    this.pickDepth?.destroy();
    this.atlasTexture?.destroy();
    this.lightmapTexture?.destroy();
    this.frameBuffer.destroy();
    this.materials.destroy();
  }
}

function distanceTo(b: Aabb, p: Vec3): number {
  const c = aabbCenter(b);
  return Math.hypot(c[0] - p[0], c[1] - p[1], c[2] - p[2]);
}

/** World-space bounds of everything the scene draws, or null when empty. */
export function sceneBounds(scene: Scene): Aabb | null {
  if (scene.bvhBoxes.length === 0) return null;
  const out: Aabb = { min: [...scene.bvhBoxes[0].min] as Vec3, max: [...scene.bvhBoxes[0].max] as Vec3 };
  for (const b of scene.bvhBoxes) {
    for (let i = 0; i < 3; i++) {
      out.min[i] = Math.min(out.min[i], b.min[i]);
      out.max[i] = Math.max(out.max[i], b.max[i]);
    }
  }
  return out;
}

