export class GpuError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "GpuError";
  }
}

export interface GpuContext {
  device: GPUDevice;
  context: GPUCanvasContext;
  format: GPUTextureFormat;
  canvas: HTMLCanvasElement;
  /** True when `texture-compression-bc` was granted, so compressed uploads are legal. */
  bc: boolean;
}

export const BC_FEATURE = "texture-compression-bc" as const;

/** Browsers known to ship WebGPU; named so the fallback page can list them. */
export const SUPPORTED_BROWSERS = [
  "Chrome or Edge 113+ on Windows, macOS or ChromeOS",
  "Chrome 121+ on Android",
  "Firefox 141+ on Windows",
  "Safari 26+ on macOS and iOS",
];

/** The device limits this renderer actually depends on. */
export interface GpuLimits {
  maxTextureSize: number;
  maxBufferSize: number;
  maxVertexBuffers: number;
  maxBindGroups: number;
  maxSampledTexturesPerStage: number;
  maxStorageBuffersPerStage: number;
}


/** Descriptors of a device, or null when WebGPU is not usable at all. */
export function gpuAdapterOptions(): GPURequestAdapterOptions | null {
  const nav = globalThis.navigator as Navigator | undefined;
  const gpu = nav?.gpu as GPU | undefined;
  if (!gpu) return null;
  return { powerPreference: "high-performance" };
}

/** Names the browsers that work when `navigator.gpu` is missing. */
export function unsupportedMessage(detail?: string): string {
  const base = "r5ed needs WebGPU, which this browser does not expose.";
  const list = `Supported browsers: ${SUPPORTED_BROWSERS.join("; ")}.`;
  return detail ? `${base} ${detail} ${list}` : `${base} ${list}`;
}


/**
 * Requests an adapter and device, asking for BC block compression when the
 * adapter offers it. Throws `GpuError` with browser names when unavailable.
 */
export async function requestDevice(): Promise<GPUDevice> {
  const nav = globalThis.navigator as Navigator | undefined;
  const gpu = nav?.gpu as GPU | undefined;
  if (!gpu) throw new GpuError(unsupportedMessage("navigator.gpu is missing."));
  let adapter: GPUAdapter | null = null;
  try {
    adapter = await gpu.requestAdapter(gpuAdapterOptions() ?? {});
  } catch (e) {
    throw new GpuError(unsupportedMessage(e instanceof Error ? e.message : String(e)));
  }
  if (!adapter) throw new GpuError(unsupportedMessage("no WebGPU adapter was returned."));
  const features: GPUFeatureName[] = [];
  if (adapter.features.has(BC_FEATURE)) features.push(BC_FEATURE);
  try {
    return await adapter.requestDevice({ requiredFeatures: features, label: "r5ed" });
  } catch (e) {
    // A driver that advertises the feature can still refuse it; retry without.
    if (features.length === 0) throw new GpuError(unsupportedMessage(e instanceof Error ? e.message : String(e)));
    return adapter.requestDevice({ label: "r5ed" });
  }
}

/** True when the device may upload BC1/BC3/BC4/BC5/BC7 blocks directly. */
export function hasBcSupport(device: GPUDevice): boolean {
  return device.features.has(BC_FEATURE);
}


/** Resolves when the device is lost, so the app can show a full-page message. */
export interface GpuInit {
  context: GpuContext;
  /** Resolves to a device-lost message string when the device goes away. */
  lost: Promise<string>;
}

/** Configures the canvas and acquires a device, with a depth pre-pass format. */
export async function initGpu(canvas: HTMLCanvasElement): Promise<GpuInit> {
  const nav = globalThis.navigator as Navigator | undefined;
  if (!(nav?.gpu)) throw new GpuError(unsupportedMessage());
  const context = canvas.getContext("webgpu");
  if (!context) throw new GpuError(unsupportedMessage("canvas.getContext(\"webgpu\") returned null."));
  const device = await requestDevice();
  const format = navigator.gpu.getPreferredCanvasFormat();
  context.configure({ device, format, alphaMode: "opaque" });
  const lost = device.lost.then((info) => info?.message || "device lost");
  return {
    context: { device, context, format, canvas, bc: hasBcSupport(device) },
    lost,
  };
}

/** Binds a `?` comparison, the size a depth attachment must be created with. */
export const DEPTH_FORMAT: GPUTextureFormat = "depth24plus";

/** Byte size a 2D texture of a compressed format needs, from its block shape. */
export function compressedTextureBytes(
  width: number,
  height: number,
  bytesPerBlock: number,
  blockWidth: number,
  blockHeight: number,
): number {
  const blocksX = Math.ceil(Math.max(1, width) / blockWidth);
  const blocksY = Math.ceil(Math.max(1, height) / blockHeight);
  return blocksX * blocksY * bytesPerBlock;
}

/** Mip chain length down to 1x1, matching the DDS/DXT rules. */
export function mipChainCount(width: number, height: number): number {
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
