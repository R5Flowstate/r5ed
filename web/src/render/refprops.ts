import type { Vec3 } from "../core/api";
import { fromRotationTranslationScale, invert, mat4, type Mat4 } from "../math/mat4";
import { quatFromSourceAngles } from "../math/quat";
import type { Aabb } from "../math/vec3";
import type { Frustum } from "./frustum";
import { VERTEX_FLOATS, type GeometryChunk } from "./scene";

/**
 * The static props of an imported map, drawn straight from the reference's
 * prop table: a shipped map carries hundreds of thousands, far more than the
 * document can hold as objects. A prop joins the document only when edited.
 */
export interface RefPropTable {
  models: string[];
  count: number;
  model: Uint32Array;
  skin: Uint32Array;
  flags: Uint32Array;
  origin: Float32Array;
  angles: Float32Array;
  scale: Float32Array;
  fade: Float32Array;
}

const MAGIC = 0x50525052; // "RPRP"
const RECORD = 48;
export const PROP_FLAG_SOLID = 1;
export const REF_PROP_TABLE = "reference/props.bin";

/** Pick ids at and above this belong to reference props, by record index. */
export const REF_PICK_BASE = 1 << 22;
const REF_ID_PREFIX = "ref:prop:";

export function refPropId(index: number): string {
  return `${REF_ID_PREFIX}${index}`;
}

/** The record index a reference prop id names, or null for any other id. */
export function refPropIndex(id: string): number | null {
  if (!id.startsWith(REF_ID_PREFIX)) return null;
  const n = Number(id.slice(REF_ID_PREFIX.length));
  return Number.isInteger(n) && n >= 0 ? n : null;
}

export function parseRefProps(bytes: Uint8Array): RefPropTable {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const fail = (why: string): never => {
    throw new Error(`prop table: ${why}`);
  };
  if (bytes.byteLength < 16 || view.getUint32(0, true) !== MAGIC) fail("not a prop table");
  if (view.getUint32(4, true) !== 1) fail(`version ${view.getUint32(4, true)} is not one this page reads`);
  const modelCount = view.getUint32(8, true);
  const count = view.getUint32(12, true);
  const decoder = new TextDecoder();
  const models: string[] = [];
  let o = 16;
  for (let i = 0; i < modelCount; i++) {
    if (o + 2 > bytes.byteLength) fail("the model names run past the end");
    const n = view.getUint16(o, true);
    if (o + 2 + n > bytes.byteLength) fail("a model name runs past the end");
    models.push(decoder.decode(bytes.subarray(o + 2, o + 2 + n)));
    o += 2 + n;
  }
  o = (o + 3) & ~3;
  if (o + count * RECORD !== bytes.byteLength) fail(`${count} records do not fill the ${bytes.byteLength - o} bytes left`);
  const t: RefPropTable = {
    models,
    count,
    model: new Uint32Array(count),
    skin: new Uint32Array(count),
    flags: new Uint32Array(count),
    origin: new Float32Array(count * 3),
    angles: new Float32Array(count * 3),
    scale: new Float32Array(count),
    fade: new Float32Array(count),
  };
  for (let i = 0; i < count; i++, o += RECORD) {
    const m = view.getUint32(o, true);
    t.model[i] = m < modelCount ? m : 0;
    t.skin[i] = view.getUint32(o + 4, true);
    t.flags[i] = view.getUint32(o + 8, true);
    for (let k = 0; k < 3; k++) {
      t.origin[i * 3 + k] = view.getFloat32(o + 12 + k * 4, true);
      t.angles[i * 3 + k] = view.getFloat32(o + 24 + k * 4, true);
    }
    t.scale[i] = view.getFloat32(o + 36, true);
    t.fade[i] = view.getFloat32(o + 40, true);
  }
  return t;
}

/** Cell edge in units: small enough to cull a map in pieces, big enough to stay a few thousand cells. */
const CELL = 2048;
/** A run whose props subtend less than this (radius over distance) is not drawn. */
const MIN_ANGULAR_SIZE = 0.0015;
export const REF_INSTANCE_FLOATS = 24;

/** One model's props inside one cell: a contiguous slice of the instance buffer. */
interface Run {
  model: number;
  first: number;
  count: number;
  maxScale: number;
}

interface Cell {
  runs: Run[];
  /** Box of the prop origins, grown by the largest loaded model in the cell. */
  min: Vec3;
  max: Vec3;
  originMin: Vec3;
  originMax: Vec3;
  reach: number;
}

export interface RefDraw {
  chunk: number;
  first: number;
  count: number;
}

export class RefPropLayer {
  readonly table: RefPropTable;
  /** Per-instance floats in cell-then-model order, the layout the renderer uploads as is. */
  readonly instances: Float32Array<ArrayBuffer>;
  /** Record index to its slot in `instances`. */
  readonly slotOf: Uint32Array;
  /** Slot to record index. */
  readonly recordAt: Uint32Array;
  private cells: Cell[] = [];
  private chunkOf: Int32Array;
  private radiusOf: Float32Array;
  /** Model indices, most used first. */
  private byUse: number[];
  private hidden = new Set<number>();
  /** Slots whose instance floats changed since the renderer last read them. */
  dirtySlots: number[] = [];
  /** Bumped when the drawable set changes: a model arrived or a prop was hidden. */
  version = 0;
  /** Bumped when the whole instance buffer must be uploaded again. */
  uploadVersion = 0;

  constructor(table: RefPropTable) {
    this.table = table;
    const n = table.count;
    this.chunkOf = new Int32Array(table.models.length).fill(-1);
    this.radiusOf = new Float32Array(table.models.length);
    const uses = new Uint32Array(table.models.length);
    for (let i = 0; i < n; i++) uses[table.model[i]]++;
    this.byUse = [...uses.keys()].filter((m) => uses[m] > 0).sort((a, b) => uses[b] - uses[a]);
    const cellKey = new Map<string, number>();
    const cellOf = new Uint32Array(n);
    const keys: [number, number, number][] = [];
    for (let i = 0; i < n; i++) {
      const cx = Math.floor(table.origin[i * 3] / CELL);
      const cy = Math.floor(table.origin[i * 3 + 1] / CELL);
      const cz = Math.floor(table.origin[i * 3 + 2] / CELL);
      const key = `${cx},${cy},${cz}`;
      let c = cellKey.get(key);
      if (c === undefined) {
        c = keys.length;
        cellKey.set(key, c);
        keys.push([cx, cy, cz]);
      }
      cellOf[i] = c;
    }
    const order = new Uint32Array(n);
    for (let i = 0; i < n; i++) order[i] = i;
    order.sort((a, b) => cellOf[a] - cellOf[b] || table.model[a] - table.model[b]);
    this.recordAt = order;
    this.slotOf = new Uint32Array(n);
    this.instances = new Float32Array(n * REF_INSTANCE_FLOATS);
    const m = mat4();
    for (let s = 0; s < n; s++) {
      const i = order[s];
      this.slotOf[i] = s;
      const q = quatFromSourceAngles([table.angles[i * 3], table.angles[i * 3 + 1], table.angles[i * 3 + 2]]);
      const k = table.scale[i] > 0 ? table.scale[i] : 1;
      fromRotationTranslationScale(m, q, [table.origin[i * 3], table.origin[i * 3 + 1], table.origin[i * 3 + 2]], [k, k, k]);
      const at = s * REF_INSTANCE_FLOATS;
      this.instances.set(m, at);
      this.instances[at + 16] = 1;
      this.instances[at + 17] = 1;
      this.instances[at + 18] = 1;
      this.instances[at + 19] = REF_PICK_BASE + i;
    }
    for (let s = 0; s < n; ) {
      const cell = cellOf[order[s]];
      const runs: Run[] = [];
      const lo: Vec3 = [Infinity, Infinity, Infinity];
      const hi: Vec3 = [-Infinity, -Infinity, -Infinity];
      while (s < n && cellOf[order[s]] === cell) {
        const model = table.model[order[s]];
        const run: Run = { model, first: s, count: 0, maxScale: 0 };
        while (s < n && cellOf[order[s]] === cell && table.model[order[s]] === model) {
          const i = order[s];
          run.maxScale = Math.max(run.maxScale, table.scale[i] > 0 ? table.scale[i] : 1);
          for (let a = 0; a < 3; a++) {
            lo[a] = Math.min(lo[a], table.origin[i * 3 + a]);
            hi[a] = Math.max(hi[a], table.origin[i * 3 + a]);
          }
          run.count++;
          s++;
        }
        runs.push(run);
      }
      this.cells.push({ runs, min: [...lo] as Vec3, max: [...hi] as Vec3, originMin: lo, originMax: hi, reach: 0 });
    }
    this.uploadVersion++;
  }

  /** Models the layer still has no geometry for, the most used first. */
  pendingModels(): string[] {
    return this.byUse.filter((m) => this.chunkOf[m] < 0).map((m) => this.table.models[m]);
  }

  /**
   * Binds loaded geometry to the models that were waiting for it. `chunkFor`
   * answers a model path with its chunk index, or null while it is not loaded.
   */
  bindModels(chunkFor: (path: string) => number | null, chunks: readonly GeometryChunk[]): void {
    let bound = false;
    for (let m = 0; m < this.table.models.length; m++) {
      if (this.chunkOf[m] >= 0) continue;
      const c = chunkFor(this.table.models[m]);
      if (c === null) continue;
      this.chunkOf[m] = c;
      const b = chunks[c].bounds;
      this.radiusOf[m] = Math.hypot(
        Math.max(Math.abs(b.min[0]), Math.abs(b.max[0])),
        Math.max(Math.abs(b.min[1]), Math.abs(b.max[1])),
        Math.max(Math.abs(b.min[2]), Math.abs(b.max[2])),
      );
      bound = true;
    }
    if (!bound) return;
    for (const cell of this.cells) {
      let reach = 0;
      for (const r of cell.runs) if (this.chunkOf[r.model] >= 0) reach = Math.max(reach, this.radiusOf[r.model] * r.maxScale);
      cell.reach = reach;
      for (let a = 0; a < 3; a++) {
        cell.min[a] = cell.originMin[a] - reach;
        cell.max[a] = cell.originMax[a] + reach;
      }
    }
    this.version++;
  }

  /** Stops drawing the records the document now holds as objects of its own. */
  setHidden(records: ReadonlySet<number>): void {
    const changed: number[] = [];
    for (const i of this.hidden) if (!records.has(i)) changed.push(i);
    for (const i of records) if (!this.hidden.has(i) && i < this.table.count) changed.push(i);
    if (changed.length === 0) return;
    this.hidden = new Set([...records].filter((i) => i < this.table.count));
    const m = mat4();
    for (const i of changed) {
      const s = this.slotOf[i];
      const at = s * REF_INSTANCE_FLOATS;
      if (this.hidden.has(i)) {
        // A zero matrix collapses every vertex to one point, which rasterises nothing.
        this.instances.fill(0, at, at + 16);
      } else {
        const q = quatFromSourceAngles([this.table.angles[i * 3], this.table.angles[i * 3 + 1], this.table.angles[i * 3 + 2]]);
        const k = this.table.scale[i] > 0 ? this.table.scale[i] : 1;
        fromRotationTranslationScale(m, q, [this.table.origin[i * 3], this.table.origin[i * 3 + 1], this.table.origin[i * 3 + 2]], [k, k, k]);
        this.instances.set(m, at);
      }
      this.dirtySlots.push(s);
    }
    this.version++;
  }

  isHidden(record: number): boolean {
    return this.hidden.has(record);
  }

  /** The runs a view draws: cells in the frustum, runs big enough to see from the eye. */
  cull(frustum: Frustum, eye: Vec3, out: RefDraw[]): number {
    let props = 0;
    for (const cell of this.cells) {
      if (cell.reach === 0 || !boxVisible(frustum, cell.min, cell.max)) continue;
      const d = Math.max(1, boxDistance(cell.min, cell.max, eye));
      if (cell.reach / d < MIN_ANGULAR_SIZE) continue;
      for (const r of cell.runs) {
        const chunk = this.chunkOf[r.model];
        if (chunk < 0) continue;
        if ((this.radiusOf[r.model] * r.maxScale) / d < MIN_ANGULAR_SIZE) continue;
        out.push({ chunk, first: r.first, count: r.count });
        props += r.count;
      }
    }
    return props;
  }

  /** The world box every drawable prop spans, or null before any model arrived. */
  bounds(): Aabb | null {
    const min: Vec3 = [Infinity, Infinity, Infinity];
    const max: Vec3 = [-Infinity, -Infinity, -Infinity];
    let any = false;
    for (const cell of this.cells) {
      if (cell.reach === 0) continue;
      any = true;
      for (let a = 0; a < 3; a++) {
        min[a] = Math.min(min[a], cell.min[a]);
        max[a] = Math.max(max[a], cell.max[a]);
      }
    }
    return any ? { min, max } : null;
  }

  /** The nearest prop surface a ray hits: cells by entry distance, then each prop's triangles. */
  pick(chunks: readonly GeometryChunk[], origin: Vec3, dir: Vec3, maxDist: number): { record: number; position: Vec3; normal: Vec3; distance: number } | null {
    const len = Math.hypot(dir[0], dir[1], dir[2]);
    if (!(len > 0)) return null;
    const d: Vec3 = [dir[0] / len, dir[1] / len, dir[2] / len];
    const inv: Vec3 = [1 / d[0], 1 / d[1], 1 / d[2]];
    const cells: [number, Cell][] = [];
    for (const cell of this.cells) {
      if (cell.reach === 0) continue;
      const t = rayBox(origin, inv, cell.min, cell.max, maxDist);
      if (t !== Infinity) cells.push([t, cell]);
    }
    cells.sort((a, b) => a[0] - b[0]);
    let best: { record: number; position: Vec3; normal: Vec3; distance: number } | null = null;
    const local = mat4();
    const world = mat4();
    const lo: Vec3 = [0, 0, 0];
    const hi: Vec3 = [0, 0, 0];
    for (const [entry, cell] of cells) {
      if (best && entry > best.distance) break;
      for (const run of cell.runs) {
        const c = this.chunkOf[run.model];
        if (c < 0) continue;
        const chunk = chunks[c];
        const reach = this.radiusOf[run.model];
        for (let s = run.first; s < run.first + run.count; s++) {
          const record = this.recordAt[s];
          if (this.hidden.has(record)) continue;
          const at = s * REF_INSTANCE_FLOATS;
          const r = reach * (this.table.scale[record] > 0 ? this.table.scale[record] : 1);
          for (let a = 0; a < 3; a++) {
            lo[a] = this.instances[at + 12 + a] - r;
            hi[a] = this.instances[at + 12 + a] + r;
          }
          const t = rayBox(origin, inv, lo, hi, best ? best.distance : maxDist);
          if (t === Infinity) continue;
          for (let k = 0; k < 16; k++) world[k] = this.instances[at + k];
          if (!invert(local, world)) continue;
          const hit = rayChunk(chunk, local, world, origin, d);
          if (hit && hit.distance <= maxDist && (!best || hit.distance < best.distance)) best = { record, ...hit };
        }
      }
    }
    return best;
  }
}

function boxVisible(f: Frustum, min: Vec3, max: Vec3): boolean {
  for (const [nx, ny, nz, dd] of f.planes) {
    if (nx * (nx >= 0 ? max[0] : min[0]) + ny * (ny >= 0 ? max[1] : min[1]) + nz * (nz >= 0 ? max[2] : min[2]) + dd < 0) return false;
  }
  return true;
}

function boxDistance(min: Vec3, max: Vec3, p: Vec3): number {
  let s = 0;
  for (let a = 0; a < 3; a++) {
    const v = p[a] < min[a] ? min[a] - p[a] : p[a] > max[a] ? p[a] - max[a] : 0;
    s += v * v;
  }
  return Math.sqrt(s);
}

function rayBox(o: Vec3, inv: Vec3, min: Vec3, max: Vec3, limit: number): number {
  let lo = 0;
  let hi = limit;
  for (let a = 0; a < 3; a++) {
    let t0 = (min[a] - o[a]) * inv[a];
    let t1 = (max[a] - o[a]) * inv[a];
    if (t0 > t1) [t0, t1] = [t1, t0];
    if (t0 > lo) lo = t0;
    if (t1 < hi) hi = t1;
    if (lo > hi) return Infinity;
  }
  return lo;
}

function rayChunk(chunk: GeometryChunk, local: Mat4, world: Mat4, origin: Vec3, d: Vec3): { position: Vec3; normal: Vec3; distance: number } | null {
  const lo: Vec3 = [
    local[0] * origin[0] + local[4] * origin[1] + local[8] * origin[2] + local[12],
    local[1] * origin[0] + local[5] * origin[1] + local[9] * origin[2] + local[13],
    local[2] * origin[0] + local[6] * origin[1] + local[10] * origin[2] + local[14],
  ];
  const ld: Vec3 = [
    local[0] * d[0] + local[4] * d[1] + local[8] * d[2],
    local[1] * d[0] + local[5] * d[1] + local[9] * d[2],
    local[2] * d[0] + local[6] * d[1] + local[10] * d[2],
  ];
  const v = chunk.vertices;
  const ix = chunk.indices;
  let hitT = Infinity;
  let hitTri = -1;
  for (let t = 0; t + 2 < ix.length; t += 3) {
    const a = ix[t] * VERTEX_FLOATS, b = ix[t + 1] * VERTEX_FLOATS, c = ix[t + 2] * VERTEX_FLOATS;
    const e1x = v[b] - v[a], e1y = v[b + 1] - v[a + 1], e1z = v[b + 2] - v[a + 2];
    const e2x = v[c] - v[a], e2y = v[c + 1] - v[a + 1], e2z = v[c + 2] - v[a + 2];
    const px = ld[1] * e2z - ld[2] * e2y, py = ld[2] * e2x - ld[0] * e2z, pz = ld[0] * e2y - ld[1] * e2x;
    const det = e1x * px + e1y * py + e1z * pz;
    if (Math.abs(det) < 1e-12) continue;
    const f = 1 / det;
    const sx = lo[0] - v[a], sy = lo[1] - v[a + 1], sz = lo[2] - v[a + 2];
    const u = f * (sx * px + sy * py + sz * pz);
    if (u < 0 || u > 1) continue;
    const qx = sy * e1z - sz * e1y, qy = sz * e1x - sx * e1z, qz = sx * e1y - sy * e1x;
    const w = f * (ld[0] * qx + ld[1] * qy + ld[2] * qz);
    if (w < 0 || u + w > 1) continue;
    const tt = f * (e2x * qx + e2y * qy + e2z * qz);
    if (tt > 1e-6 && tt < hitT) {
      hitT = tt;
      hitTri = t;
    }
  }
  if (hitTri < 0) return null;
  const p: Vec3 = [lo[0] + ld[0] * hitT, lo[1] + ld[1] * hitT, lo[2] + ld[2] * hitT];
  const position: Vec3 = [
    world[0] * p[0] + world[4] * p[1] + world[8] * p[2] + world[12],
    world[1] * p[0] + world[5] * p[1] + world[9] * p[2] + world[13],
    world[2] * p[0] + world[6] * p[1] + world[10] * p[2] + world[14],
  ];
  const a = ix[hitTri] * VERTEX_FLOATS, b = ix[hitTri + 1] * VERTEX_FLOATS, c = ix[hitTri + 2] * VERTEX_FLOATS;
  const e1: Vec3 = [v[b] - v[a], v[b + 1] - v[a + 1], v[b + 2] - v[a + 2]];
  const e2: Vec3 = [v[c] - v[a], v[c + 1] - v[a + 1], v[c + 2] - v[a + 2]];
  const cx = e1[1] * e2[2] - e1[2] * e2[1], cy = e1[2] * e2[0] - e1[0] * e2[2], cz = e1[0] * e2[1] - e1[1] * e2[0];
  let n: Vec3 = [
    world[0] * cx + world[4] * cy + world[8] * cz,
    world[1] * cx + world[5] * cy + world[9] * cz,
    world[2] * cx + world[6] * cy + world[10] * cz,
  ];
  const nl = Math.hypot(n[0], n[1], n[2]) || 1;
  n = [n[0] / nl, n[1] / nl, n[2] / nl];
  if (n[0] * d[0] + n[1] * d[1] + n[2] * d[2] > 0) n = [-n[0], -n[1], -n[2]];
  const distance = Math.hypot(position[0] - origin[0], position[1] - origin[1], position[2] - origin[2]);
  return { position, normal: n, distance };
}
