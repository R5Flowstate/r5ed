export class BufferPoolError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "BufferPoolError";
  }
}

/** GPUBufferUsage.COPY_DST: every pool is re-uploaded with writeBuffer after the first frame. */
function align4(n: number): number {
  return (n + 3) & ~3;
}

/**
 * A growable float pool uploaded through one buffer. The CPU side is a single
 * contiguous `Float32Array`; the GPU buffer is created at the high-water mark
 * and re-uploaded whenever the frame wrote more than last time.
 */
export class FloatPool {
  private cpu = new Float32Array(1024);
  private gpu: GPUBuffer | null = null;
  private written = 0;
  private live = 0;
  private dirty = false;

  constructor(
    private device: GPUDevice,
    private label: string,
    readonly usage: GPUBufferUsageFlags,
  ) {}

  get elements(): number {
    return this.written;
  }

  get liveElements(): number {
    return this.live;
  }

  /** Byte length of the live range. */
  get bytes(): number {
    return this.live * 4;
  }

  begin(): void {
    this.written = 0;
    this.live = 0;
  }

  private ensure(elements: number): void {
    if (elements <= this.cpu.length) return;
    let size = this.cpu.length;
    while (size < elements) size *= 2;
    const grown = new Float32Array(size);
    grown.set(this.cpu.subarray(0, this.written));
    this.cpu = grown;
    this.gpu?.destroy();
    this.gpu = null;
  }

  /** Reserves `elements` floats and returns their offset. */
  alloc(elements: number): number {
    if (!Number.isInteger(elements) || elements <= 0) {
      throw new BufferPoolError(`allocation of ${elements} elements is not a positive integer`);
    }
    this.ensure(this.written + elements);
    const at = this.written;
    this.written += elements;
    this.dirty = true;
    if (this.written > this.live) this.live = this.written;
    return at;
  }

  /** Writable view of a reserved range; invalidated by the next `alloc`. */
  view(at: number, elements: number): Float32Array {
    if (at < 0 || elements < 0 || at + elements > this.written) {
      throw new BufferPoolError(`range ${at}..${at + elements} is outside the written area`);
    }
    return this.cpu.subarray(at, at + elements);
  }

  markDirty(): void {
    this.dirty = true;
  }

  /** Uploads and returns the buffer, or null when nothing was written. */
  upload(): GPUBuffer | null {
    if (this.live === 0) return null;
    if (!this.gpu) {
      this.gpu = this.device.createBuffer({
        label: this.label,
        size: Math.max(16, align4(this.live * 4)),
        usage: this.usage | 0x0008,
        mappedAtCreation: true,
      });
      new Float32Array(this.gpu.getMappedRange()).set(this.cpu.subarray(0, this.live));
      this.gpu.unmap();
      this.dirty = false;
      return this.gpu;
    }
    if (!this.dirty && this.gpu.size >= align4(this.live * 4)) return this.gpu;
    if (this.gpu.size < align4(this.live * 4)) {
      this.gpu.destroy();
      this.gpu = this.device.createBuffer({
        label: this.label,
        size: Math.max(16, align4(this.live * 4)),
        usage: this.usage | 0x0008,
      });
    }
    this.device.queue.writeBuffer(this.gpu, 0, this.cpu.buffer as ArrayBuffer, this.cpu.byteOffset, this.live * 4);
    this.dirty = false;
    return this.gpu;
  }

  destroy(): void {
    this.gpu?.destroy();
    this.gpu = null;
    this.written = 0;
    this.live = 0;
  }
}

/** The same growth policy for 32-bit index data. */
export class IndexPool {
  private cpu = new Uint32Array(1024);
  private gpu: GPUBuffer | null = null;
  private written = 0;
  private live = 0;
  private dirty = false;

  constructor(
    private device: GPUDevice,
    private label: string,
    private usage: GPUBufferUsageFlags,
  ) {}

  get elements(): number {
    return this.written;
  }

  get liveElements(): number {
    return this.live;
  }

  begin(): void {
    this.written = 0;
    this.live = 0;
  }

  alloc(elements: number): number {
    if (!Number.isInteger(elements) || elements <= 0) {
      throw new BufferPoolError(`allocation of ${elements} elements is not a positive integer`);
    }
    if (this.written + elements > this.cpu.length) {
      let size = this.cpu.length;
      while (size < this.written + elements) size *= 2;
      const grown = new Uint32Array(size);
      grown.set(this.cpu.subarray(0, this.written));
      this.cpu = grown;
      this.gpu?.destroy();
      this.gpu = null;
    }
    const at = this.written;
    this.written += elements;
    this.dirty = true;
    if (this.written > this.live) this.live = this.written;
    return at;
  }

  view(at: number, elements: number): Uint32Array {
    if (at < 0 || elements < 0 || at + elements > this.written) {
      throw new BufferPoolError(`range ${at}..${at + elements} is outside the written area`);
    }
    return this.cpu.subarray(at, at + elements);
  }

  markDirty(): void {
    this.dirty = true;
  }

  upload(): GPUBuffer | null {
    if (this.live === 0) return null;
    const size = Math.max(16, align4(this.live * 4));
    if (!this.gpu) {
      this.gpu = this.device.createBuffer({
        label: this.label,
        size,
        usage: this.usage | 0x0008,
        mappedAtCreation: true,
      });
      new Uint32Array(this.gpu.getMappedRange()).set(this.cpu.subarray(0, this.live));
      this.gpu.unmap();
      this.dirty = false;
      return this.gpu;
    }
    if (!this.dirty && this.gpu.size >= size) return this.gpu;
    if (this.gpu.size < size) {
      this.gpu.destroy();
      this.gpu = this.device.createBuffer({
        label: this.label,
        size,
        usage: this.usage | 0x0008,
      });
    }
    this.device.queue.writeBuffer(this.gpu, 0, this.cpu.buffer as ArrayBuffer, this.cpu.byteOffset, this.live * 4);
    this.dirty = false;
    return this.gpu;
  }

  destroy(): void {
    this.gpu?.destroy();
    this.gpu = null;
    this.written = 0;
    this.live = 0;
  }
}

export interface DrawGroup {
  /** Index range in the frame's index pool, in elements. */
  firstIndex: number;
  indexCount: number;
  /** Instance range in the frame's instance pool; count 1 for translucent. */
  firstInstance: number;
  instanceCount: number;
  material: string;
  translucent: boolean;
  /** Pick id the group writes, or 0 when it draws nothing selectable. */
  pick: number;
  /** The instances live in the reference prop buffer, not the frame's pool. */
  refs?: boolean;
}

/** Groups this frame's draws, split by the pass that consumes them. */
export class DrawList {
  private groups: DrawGroup[] = [];

  clear(): void {
    this.groups.length = 0;
  }

  push(group: DrawGroup): void {
    this.groups.push(group);
  }

  all(): readonly DrawGroup[] {
    return this.groups;
  }

  byPass(translucent: boolean): DrawGroup[] {
    return this.groups.filter((g) => g.translucent === translucent);
  }

  /** Opaque groups with one material, in insertion order, for batching. */
  byMaterial(): Map<string, DrawGroup[]> {
    const out = new Map<string, DrawGroup[]>();
    for (const g of this.groups) {
      if (g.translucent) continue;
      const list = out.get(g.material);
      if (list) list.push(g);
      else out.set(g.material, [g]);
    }
    return out;
  }
}
