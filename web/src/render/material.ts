import { CATEGORY_COLORS, type Category, categoryFor } from "./atlas";
import { decodeBlocksToRgba } from "./bc";
import { DdsError, type DdsInfo, mipLayout, parseDdsHeader, ddsHeaderSize } from "./dds";
import { hasBcSupport } from "./device";

/** A material as the host reports it, plus the state the renderer needs. */
export interface MaterialSlot {
  /** Key used to dedupe: the game material path, or a synthetic name. */
  key: string;
  baseColor: [number, number, number, number];
  metallic: number;
  roughness: number;
  /** Texture GUIDs, keyed by the channel the shader binds. */
  albedoGuid: string | null;
  normalGuid: string | null;
  /** Discard texels whose albedo alpha is under half, as foliage cards need. */
  alphaTest?: boolean;
}

export function defaultSlot(key: string): MaterialSlot {
  return {
    key,
    baseColor: [0.62, 0.62, 0.64, 1],
    metallic: 0,
    roughness: 0.8,
    albedoGuid: null,
    normalGuid: null,
  };
}

export interface MaterialWarning {
  material: string;
  message: string;
}

/** A neutral grey 1x1 RGBA8 texture used when a channel has no image. */
export const NEUTRAL_GREY: readonly number[] = [152, 152, 158, 255];
export const FLAT_NORMAL: readonly number[] = [128, 128, 255, 255];

const COMPRESSED_FORMATS = new Set(["bc1", "bc3", "bc4", "bc5", "bc6h", "bc6hs", "bc7"]);

export interface GpuFormat {
  format: GPUTextureFormat;
  bytesPerBlock: number;
  blockWidth: number;
  blockHeight: number;
  compressed: boolean;
}

/** WebGPU format for a decoded DDS format, or null when the GPU cannot take it. */
export function gpuFormatFor(info: DdsInfo): GpuFormat | null {
  switch (info.format) {
    case "bc1":
      return { format: "bc1-rgba-unorm", bytesPerBlock: 8, blockWidth: 4, blockHeight: 4, compressed: true };
    case "bc3":
      return { format: "bc3-rgba-unorm", bytesPerBlock: 16, blockWidth: 4, blockHeight: 4, compressed: true };
    case "bc4":
      return { format: "bc4-r-unorm", bytesPerBlock: 8, blockWidth: 4, blockHeight: 4, compressed: true };
    case "bc5":
      return { format: "bc5-rg-unorm", bytesPerBlock: 16, blockWidth: 4, blockHeight: 4, compressed: true };
    // HDR skies and cubemaps; the BC feature covers them as it covers BC7.
    case "bc6h":
      return { format: "bc6h-rgb-ufloat", bytesPerBlock: 16, blockWidth: 4, blockHeight: 4, compressed: true };
    case "bc6hs":
      return { format: "bc6h-rgb-float", bytesPerBlock: 16, blockWidth: 4, blockHeight: 4, compressed: true };
    case "bc7":
      return { format: "bc7-rgba-unorm", bytesPerBlock: 16, blockWidth: 4, blockHeight: 4, compressed: true };
    case "rgba8":
      return { format: "rgba8unorm", bytesPerBlock: 4, blockWidth: 1, blockHeight: 1, compressed: false };
    case "bgra8":
      return { format: "bgra8unorm", bytesPerBlock: 4, blockWidth: 1, blockHeight: 1, compressed: false };
    case "r8":
      return { format: "r8unorm", bytesPerBlock: 1, blockWidth: 1, blockHeight: 1, compressed: false };
    case "rg8":
      return { format: "rg8unorm", bytesPerBlock: 2, blockWidth: 1, blockHeight: 1, compressed: false };
    default:
      return null;
  }
}

/** What the renderer ended up doing with one DDS, for the asset panel. */
export interface TextureUpload {
  texture: GPUTexture;
  view: GPUTextureView;
  width: number;
  height: number;
  /** True when the blocks went to the GPU untouched. */
  passthrough: boolean;
  /** Set when the CPU decoded the blocks because the GPU cannot take them. */
  decoded?: boolean;
  mipLevels: number;
}

/** A DDS whose blocks the GPU cannot take is decoded to RGBA8 on the CPU. */
export function needsCpuDecode(info: DdsInfo, bc: boolean): boolean {
  if (!COMPRESSED_FORMATS.has(info.format)) return false;
  if (bc) return false;
  return info.format === "bc1" || info.format === "bc3" || info.format === "bc4" || info.format === "bc5";
}

/**
 * Uploads a DDS as a mipped texture. Compressed formats pass through when the
 * device supports them; otherwise BC1/BC3/BC4/BC5 are decoded to RGBA8.
 */
export function uploadDds(device: GPUDevice, bytes: Uint8Array, label: string, bc: boolean): TextureUpload {
  const info = parseDdsHeader(bytes);
  const gpuFormat = gpuFormatFor(info);
  if (!gpuFormat) throw new DdsError(`no GPU format for ${info.format}`);
  const decode = needsCpuDecode(info, bc) || !gpuFormat.compressed;
  if (!decode && !bc) throw new DdsError(`compressed ${info.format} needs texture-compression-bc`);
  const dataStart = ddsHeaderSize(bytes);
  const mipLevels = Math.min(info.mipCount, mipLevelsFor(info.width, info.height));
  const usage = GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST;
  if (decode) {
    const texture = device.createTexture({
      label,
      size: [info.width, info.height, 1],
      format: "rgba8unorm",
      mipLevelCount: mipLevels,
      usage,
    });
    for (let level = 0; level < mipLevels; level++) {
      const layout = mipLayout(info, level);
      const raw = bytes.subarray(dataStart + layout.offset, dataStart + layout.offset + layout.size);
      let pixels: Uint8Array;
      if (info.uncompressed) {
        pixels = decodeBlocksToRgba(info.format, raw, layout.width, layout.height);
      } else if (info.format === "bc1" || info.format === "bc3" || info.format === "bc4" || info.format === "bc5") {
        pixels = decodeBlocksToRgba(info.format, raw, layout.width, layout.height);
      } else {
        throw new DdsError(`no CPU decoder for ${info.format}`);
      }
      device.queue.writeTexture(
        { texture, mipLevel: level },
        uploadData(pixels),
        { bytesPerRow: layout.width * 4, rowsPerImage: layout.height },
        [layout.width, layout.height, 1],
      );
    }
    return { texture, view: texture.createView(), width: info.width, height: info.height, passthrough: false, decoded: true, mipLevels };
  }
  const texture = device.createTexture({
    label,
    size: [info.width, info.height, 1],
    format: gpuFormat.format,
    mipLevelCount: mipLevels,
    usage,
  });
  for (let level = 0; level < mipLevels; level++) {
    const layout = mipLayout(info, level);
    const raw = bytes.subarray(dataStart + layout.offset, dataStart + layout.offset + layout.size);
    const rows = Math.max(1, Math.ceil(layout.height / gpuFormat.blockHeight));
    device.queue.writeTexture(
      { texture, mipLevel: level },
      uploadData(raw),
      { bytesPerRow: layout.size / rows, rowsPerImage: rows },
      [layout.width, layout.height, 1],
    );
  }
  return { texture, view: texture.createView(), width: info.width, height: info.height, passthrough: true, mipLevels };
}

export function mipLevelsFor(width: number, height: number): number {
  let count = 1;
  let w = Math.max(1, width);
  let h = Math.max(1, height);
  while (w > 1 || h > 1) {
    w = Math.max(1, w >> 1);
    h = Math.max(1, h >> 1);
    count++;
  }
  return count;
}

/** `writeTexture` accepts any buffer source, which the typings do not re-export. */
type UploadData = Parameters<GPUQueue["writeTexture"]>[1];

function uploadData(view: Uint8Array): UploadData {
  return view as UploadData;
}

/** A 1x1 unorm texture, the stand-in for a channel with no image. */
export function solidTexture(device: GPUDevice, rgba: readonly number[], label: string): GPUTexture {
  const texture = device.createTexture({
    label,
    size: [1, 1, 1],
    format: "rgba8unorm",
    usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
  });
  device.queue.writeTexture({ texture }, uploadData(new Uint8Array(rgba)), { bytesPerRow: 4, rowsPerImage: 1 }, [1, 1, 1]);
  return texture;
}

export interface MaterialTexture {
  view: GPUTextureView;
  width: number;
  height: number;
  passthrough: boolean;
}

/** What the material table keeps per slot, plus the warnings it emits. */
export interface CompiledMaterial {
  slot: MaterialSlot;
  uniform: GPUBuffer;
  bindGroup: GPUBindGroup;
}

const MATERIAL_FLOATS = 8;

/**
 * Owns one uniform buffer and bind group per material. Textures arrive
 * separately so a missing one can fall back to neutral grey and warn.
 */
export class MaterialTable {
  private device: GPUDevice;
  private layout: GPUBindGroupLayout;
  private sampler: GPUSampler;
  private byKey = new Map<string, CompiledMaterial>();
  private textures = new Map<string, MaterialTexture>();
  private owned: GPUTexture[] = [];
  warnings: MaterialWarning[] = [];

  constructor(device: GPUDevice) {
    this.device = device;
    this.sampler = device.createSampler({
      magFilter: "linear",
      minFilter: "linear",
      mipmapFilter: "linear",
      addressModeU: "repeat",
      addressModeV: "repeat",
      maxAnisotropy: hasBcSupport(device) ? 8 : 1,
    });
    this.layout = device.createBindGroupLayout({
      label: "material",
      entries: [
        { binding: 0, visibility: GPUShaderStage.FRAGMENT, sampler: {} },
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } },
        { binding: 2, visibility: GPUShaderStage.FRAGMENT, sampler: {} },
        { binding: 3, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } },
        { binding: 4, visibility: GPUShaderStage.FRAGMENT, sampler: {} },
        { binding: 5, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } },
        { binding: 6, visibility: GPUShaderStage.FRAGMENT, buffer: { type: "uniform" } },
      ],
    });
    const grey = solidTexture(device, NEUTRAL_GREY, "neutral-grey");
    const flat = solidTexture(device, FLAT_NORMAL, "flat-normal");
    this.owned.push(grey, flat);
    this.grey = { view: grey.createView(), width: 1, height: 1, passthrough: false };
    this.flat = { view: flat.createView(), width: 1, height: 1, passthrough: false };
  }

  readonly grey: MaterialTexture;
  readonly flat: MaterialTexture;
  private lightmapView: GPUTextureView | null = null;

  bindGroupLayout(): GPUBindGroupLayout {
    return this.layout;
  }

  /** Points the lightmap channel at the baked atlas, or back to grey. */
  setLightmapView(view: GPUTextureView | null): void {
    this.lightmapView = view;
    for (const [key, compiled] of this.byKey) this.byKey.set(key, this.rebind(compiled, compiled.slot));
  }

  private rebind(compiled: CompiledMaterial, slot: MaterialSlot): CompiledMaterial {
    const uniform = compiled.uniform;
    const data = new Float32Array(MATERIAL_FLOATS);
    data.set(slot.baseColor, 0);
    data[4] = 1 - Math.min(1, Math.max(0, slot.roughness));
    data[5] = slot.albedoGuid && this.textures.has(slot.albedoGuid) ? 1 : 0;
    data[6] = slot.normalGuid && this.textures.has(slot.normalGuid) ? 1 : 0;
    data[7] = 0.55;
    this.device.queue.writeBuffer(uniform, 0, data);
    const bindGroup = this.device.createBindGroup({
      label: `material:${slot.key}`,
      layout: this.layout,
      entries: [
        { binding: 0, resource: this.sampler },
        { binding: 1, resource: (slot.albedoGuid ? this.textures.get(slot.albedoGuid)?.view : undefined) ?? this.grey.view },
        { binding: 2, resource: this.sampler },
        { binding: 3, resource: (slot.normalGuid ? this.textures.get(slot.normalGuid)?.view : undefined) ?? this.flat.view },
        { binding: 4, resource: this.sampler },
        { binding: 5, resource: this.lightmapView ?? this.grey.view },
        { binding: 6, resource: { buffer: uniform } },
      ],
    });
    return { slot, uniform, bindGroup };
  }

  /** Registers a DDS by GUID so slots that reference it can bind it. */
  addTexture(guid: string, upload: TextureUpload): void {
    this.textures.set(guid, { view: upload.view, width: upload.width, height: upload.height, passthrough: upload.passthrough });
  }

  /** Drops a texture and destroys it; slots fall back to grey on the next build. */
  removeTexture(guid: string): boolean {
    const had = this.textures.delete(guid);
    return had;
  }

  has(guid: string): boolean {
    return this.textures.has(guid);
  }

  /**
   * Builds (or rebuilds) one material. Missing textures fall back to neutral
   * grey or a flat normal and are reported through `warnings`.
   */
  compile(slot: MaterialSlot): CompiledMaterial {
    const albedo = slot.albedoGuid ? this.textures.get(slot.albedoGuid) : undefined;
    const normal = slot.normalGuid ? this.textures.get(slot.normalGuid) : undefined;
    if (slot.albedoGuid && !albedo) this.warn(slot, `albedo texture ${slot.albedoGuid} is missing`);
    if (slot.normalGuid && !normal) this.warn(slot, `normal texture ${slot.normalGuid} is missing`);
    const uniform = this.device.createBuffer({
      label: `material:${slot.key}`,
      size: MATERIAL_FLOATS * 4,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    const data = new Float32Array(MATERIAL_FLOATS);
    data.set(slot.baseColor, 0);
    // x: specular strength, y/z: has albedo / has normal, w: ambient scale.
    data[4] = 1 - Math.min(1, Math.max(0, slot.roughness));
    data[5] = albedo ? (slot.alphaTest ? 2 : 1) : 0;
    data[6] = normal ? 1 : 0;
    data[7] = 0.55;
    this.device.queue.writeBuffer(uniform, 0, data);
    const bindGroup = this.device.createBindGroup({
      label: `material:${slot.key}`,
      layout: this.layout,
      entries: [
        { binding: 0, resource: this.sampler },
        { binding: 1, resource: (albedo ?? this.grey).view },
        { binding: 2, resource: this.sampler },
        { binding: 3, resource: (normal ?? this.flat).view },
        { binding: 4, resource: this.sampler },
        { binding: 5, resource: this.lightmapView ?? this.grey.view },
        { binding: 6, resource: { buffer: uniform } },
      ],
    });
    const compiled: CompiledMaterial = { slot, uniform, bindGroup };
    this.byKey.set(slot.key, compiled);
    return compiled;
  }

  private warn(slot: MaterialSlot, message: string): void {
    if (this.warnings.some((w) => w.material === slot.key && w.message === message)) return;
    this.warnings.push({ material: slot.key, message });
  }

  get(key: string): CompiledMaterial | null {
    return this.byKey.get(key) ?? null;
  }

  keys(): string[] {
    return [...this.byKey.keys()];
  }

  clearWarnings(): void {
    this.warnings = [];
  }

  destroy(): void {
    for (const m of this.byKey.values()) m.uniform.destroy();
    this.byKey.clear();
    for (const t of this.owned) t.destroy();
    this.owned = [];
  }
}

/** Translucent volume and window colours, one per `VolumeKind`. */
export const VOLUME_COLORS: Record<string, [number, number, number, number]> = {
  trigger: [0.95, 0.45, 0.2, 0.16],
  trigger_hurt: [0.9, 0.2, 0.25, 0.2],
  trigger_soundscape: [0.5, 0.4, 0.9, 0.14],
  envmap_volume: [0.4, 0.8, 0.9, 0.1],
  light_environment_volume: [0.95, 0.85, 0.5, 0.12],
  fog_volume: [0.7, 0.75, 0.8, 0.18],
  player_clip: [0.95, 0.8, 0.2, 0.14],
  out_of_bounds: [0.9, 0.3, 0.4, 0.18],
  custom: [0.6, 0.7, 0.8, 0.12],
};

export function volumeColor(kind: string): [number, number, number, number] {
  return VOLUME_COLORS[kind] ?? VOLUME_COLORS.custom;
}

/** Light gizmo colour by kind, matching the billboard category colour. */
export function lightColor(kind: string): [number, number, number] {
  const category: Category = kind === "sun" || kind === "sky" ? "light" : "env";
  return CATEGORY_COLORS[category];
}

export function entityCategory(classname: string): Category {
  return categoryFor(classname);
}
