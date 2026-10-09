// The shape catalogue behind `core.meshCreate`. The wire form is plain JSON and
// must match `rdoc::meshgen::MeshSpec`, which is tagged on `shape`.

import type { Vec3 } from "../core/api";

export type MeshShapeId =
  | "box" | "cylinder" | "cone" | "sphere" | "torus" | "plane" | "stairs"
  | "ramp" | "arch" | "extrude" | "lathe" | "loft" | "raw";

export type Points2 = [number, number][];
export type Points3 = [number, number, number][];

export type MeshSpec =
  | { shape: "box"; size: [number, number, number] }
  | { shape: "cylinder"; radius: number; height: number; segments: number; caps: boolean }
  | { shape: "cone"; radius: number; height: number; segments: number }
  | { shape: "sphere"; radius: number; segments: number; rings: number }
  | { shape: "torus"; major: number; minor: number; segments: number; sides: number }
  | { shape: "plane"; size: [number, number]; divisions: [number, number] }
  | { shape: "stairs"; steps: number; run: number; rise: number; width: number }
  | { shape: "ramp"; length: number; width: number; height: number }
  | { shape: "arch"; width: number; height: number; thickness: number; depth: number; segments: number }
  | { shape: "extrude"; outline: Points2; height: number }
  | { shape: "lathe"; profile: Points2; segments: number }
  | { shape: "loft"; section: Points2; path: Points3; closed_section: boolean }
  | { shape: "raw"; positions: Points3; indices: number[] };

export type MeshFieldKind = "number" | "integer" | "boolean" | "vector2" | "vector3" | "points2" | "points3" | "numbers";

export interface MeshFieldSpec {
  name: string;
  label: string;
  kind: MeshFieldKind;
  /** Inclusive bounds; the core refuses anything outside them, so the form does too. */
  min: number;
  max: number;
  step: number;
  /** A shape cannot be built without this field, so an empty value is an error. */
  required?: boolean;
}

export interface MeshShapeEntry {
  id: MeshShapeId;
  label: string;
  fields: MeshFieldSpec[];
}

function num(name: string, label: string, max = 4096, step = 8): MeshFieldSpec {
  return { name, label, kind: "number", min: 0, max, step };
}

function count(name: string, label: string, min: number, max = 256): MeshFieldSpec {
  return { name, label, kind: "integer", min, max, step: 1 };
}

const SEGMENT_MIN = 3;
const COUNT_MIN = 1;

/** Every shape the core can build, in the order the picker lists them. */
export const MESH_SHAPES: readonly MeshShapeEntry[] = [
  {
    id: "box",
    label: "Box",
    fields: [
      { name: "size", label: "size x y z", kind: "vector3", min: 1, max: 4096, step: 8, required: true },
    ],
  },
  {
    id: "cylinder",
    label: "Cylinder",
    fields: [
      num("radius", "radius", 2048, 4),
      num("height", "height"),
      count("segments", "segments", SEGMENT_MIN),
      { name: "caps", label: "caps", kind: "boolean", min: 0, max: 1, step: 1 },
    ],
  },
  {
    id: "cone",
    label: "Cone",
    fields: [num("radius", "radius", 2048, 4), num("height", "height"), count("segments", "segments", SEGMENT_MIN)],
  },
  {
    id: "sphere",
    label: "Sphere",
    fields: [num("radius", "radius", 2048, 4), count("segments", "segments", SEGMENT_MIN), count("rings", "rings", 2)],
  },
  {
    id: "torus",
    label: "Torus",
    fields: [
      num("major", "major", 2048, 4),
      num("minor", "minor", 2048, 2),
      count("segments", "segments", SEGMENT_MIN),
      count("sides", "sides", SEGMENT_MIN),
    ],
  },
  {
    id: "plane",
    label: "Plane",
    fields: [
      { name: "size", label: "size w d", kind: "vector2", min: 1, max: 8192, step: 8, required: true },
      { name: "divisions", label: "divisions", kind: "vector2", min: 1, max: 256, step: 1, required: true },
    ],
  },
  {
    id: "stairs",
    label: "Stairs",
    fields: [
      count("steps", "steps", COUNT_MIN, 128),
      num("run", "run"),
      num("rise", "rise"),
      num("width", "width"),
    ],
  },
  {
    id: "ramp",
    label: "Ramp",
    fields: [num("length", "length"), num("width", "width"), num("height", "height")],
  },
  {
    id: "arch",
    label: "Arch",
    fields: [
      num("width", "width"),
      num("height", "height"),
      num("thickness", "thickness"),
      num("depth", "depth"),
      count("segments", "segments", 2),
    ],
  },
  {
    id: "extrude",
    label: "Extrusion",
    fields: [
      { name: "outline", label: "outline x,y x,y", kind: "points2", min: -4096, max: 4096, step: 8, required: true },
      num("height", "height"),
    ],
  },
  {
    id: "lathe",
    label: "Lathe",
    fields: [
      { name: "profile", label: "profile x,z x,z", kind: "points2", min: -4096, max: 4096, step: 4, required: true },
      count("segments", "segments", SEGMENT_MIN),
    ],
  },
  {
    id: "loft",
    label: "Loft",
    fields: [
      { name: "section", label: "section u,v u,v", kind: "points2", min: -4096, max: 4096, step: 4, required: true },
      { name: "path", label: "path x,y,z x,y,z", kind: "points3", min: -65536, max: 65536, step: 16, required: true },
      { name: "closed_section", label: "closed section", kind: "boolean", min: 0, max: 1, step: 1 },
    ],
  },
  {
    id: "raw",
    label: "Raw triangles",
    fields: [
      { name: "positions", label: "positions x,y,z x,y,z", kind: "points3", min: -65536, max: 65536, step: 8, required: true },
      { name: "indices", label: "indices 0 1 2", kind: "numbers", min: 0, max: 4_000_000, step: 1, required: true },
    ],
  },
];

/** A dimension the core accepts: strictly positive and below a million units. */
const MAX_DIMENSION = 1_000_000;
const MIN_DIMENSION = 1e-4;

function positive(raw: string): number | null {
  const n = Number(raw);
  if (!Number.isFinite(n) || n < MIN_DIMENSION || n >= MAX_DIMENSION) return null;
  return n;
}

function countIn(raw: string, field: MeshFieldSpec): number | null {
  const n = Number(raw);
  if (!Number.isInteger(n) || n < field.min || n > field.max) return null;
  return n;
}

function scalar(raw: string, field: MeshFieldSpec): number | null {
  const n = Number(raw);
  if (!Number.isFinite(n) || n < field.min || n > field.max) return null;
  return n;
}

/** Splits a point list on whitespace, commas or semicolons. */
function tokens(raw: string): string[] {
  return raw.split(/[\s,;]+/).filter((t) => t.length > 0);
}

function parseFixed(raw: string, dims: 2 | 3, field: MeshFieldSpec): number[] | null {
  const parts = tokens(raw);
  if (parts.length !== dims) return null;
  const out: number[] = [];
  for (const p of parts) {
    const n = scalar(p, field);
    if (n === null) return null;
    out.push(n);
  }
  return out;
}

function parsePoints(raw: string, dims: 2 | 3, field: MeshFieldSpec): number[][] | null {
  const groups = raw.split(/[;|]/).map((g) => g.trim()).filter((g) => g.length > 0);
  if (groups.length === 0) return null;
  const out: number[][] = [];
  for (const g of groups) {
    const row = parseFixed(g, dims, field);
    if (!row) return null;
    out.push(row);
  }
  return out;
}

function parseNumbers(raw: string, field: MeshFieldSpec): number[] | null {
  const parts = tokens(raw);
  if (parts.length < 1) return null;
  const out: number[] = [];
  for (const p of parts) {
    const n = countIn(p, field);
    if (n === null) return null;
    out.push(n);
  }
  return out;
}

export type ParseResult = { ok: true; spec: MeshSpec } | { ok: false; error: string };

/** The message a rejected parse carries, or the empty string when it passed. */
export function specErrorOf(result: ParseResult): string {
  return result.ok ? "" : result.error;
}

/** Turns the form's text values into a spec, refusing anything the core would. */
export function parseSpec(id: MeshShapeId, values: Readonly<Record<string, string>>): ParseResult {
  const shape = MESH_SHAPES.find((s) => s.id === id);
  if (!shape) return { ok: false, error: `${id} is not a shape the core can build` };
  const read = (name: string): string => (values[name] ?? "").trim();
  const fail = (field: MeshFieldSpec): ParseResult => {
    const empty = read(field.name).length === 0;
    return { ok: false, error: empty ? `${field.label} is required` : `${field.label} is out of range` };
  };
  const bool = (name: string): boolean => read(name) === "1" || read(name).toLowerCase() === "true";
  const dimension = (field: MeshFieldSpec): number | null => positive(read(field.name));
  switch (id) {
    case "box": {
      const size = parseFixed(read("size"), 3, shape.fields[0]);
      if (!size) return fail(shape.fields[0]);
      if (size.some((v) => v < MIN_DIMENSION || v >= MAX_DIMENSION)) return fail(shape.fields[0]);
      return { ok: true, spec: { shape: "box", size: size as [number, number, number] } };
    }
    case "cylinder": {
      const radius = dimension(shape.fields[0]);
      const height = dimension(shape.fields[1]);
      const segments = countIn(read("segments"), shape.fields[2]);
      if (radius === null) return fail(shape.fields[0]);
      if (height === null) return fail(shape.fields[1]);
      if (segments === null) return fail(shape.fields[2]);
      return { ok: true, spec: { shape: "cylinder", radius, height, segments, caps: bool("caps") } };
    }
    case "cone": {
      const radius = dimension(shape.fields[0]);
      const height = dimension(shape.fields[1]);
      const segments = countIn(read("segments"), shape.fields[2]);
      if (radius === null) return fail(shape.fields[0]);
      if (height === null) return fail(shape.fields[1]);
      if (segments === null) return fail(shape.fields[2]);
      return { ok: true, spec: { shape: "cone", radius, height, segments } };
    }
    case "sphere": {
      const radius = dimension(shape.fields[0]);
      const segments = countIn(read("segments"), shape.fields[1]);
      const rings = countIn(read("rings"), shape.fields[2]);
      if (radius === null) return fail(shape.fields[0]);
      if (segments === null) return fail(shape.fields[1]);
      if (rings === null) return fail(shape.fields[2]);
      return { ok: true, spec: { shape: "sphere", radius, segments, rings } };
    }
    case "torus": {
      const major = dimension(shape.fields[0]);
      const minor = dimension(shape.fields[1]);
      const segments = countIn(read("segments"), shape.fields[2]);
      const sides = countIn(read("sides"), shape.fields[3]);
      if (major === null) return fail(shape.fields[0]);
      if (minor === null) return fail(shape.fields[1]);
      if (segments === null) return fail(shape.fields[2]);
      if (sides === null) return fail(shape.fields[3]);
      return { ok: true, spec: { shape: "torus", major, minor, segments, sides } };
    }
    case "plane": {
      const size = parseFixed(read("size"), 2, shape.fields[0]);
      const divisions = parseFixed(read("divisions"), 2, shape.fields[1]);
      if (!size || size.some((v) => v < MIN_DIMENSION || v >= MAX_DIMENSION)) return fail(shape.fields[0]);
      if (!divisions || divisions.some((v) => !Number.isInteger(v) || v < 1 || v > 1024)) return fail(shape.fields[1]);
      return { ok: true, spec: { shape: "plane", size: size as [number, number], divisions: divisions as [number, number] } };
    }
    case "stairs": {
      const steps = countIn(read("steps"), shape.fields[0]);
      const run = dimension(shape.fields[1]);
      const rise = dimension(shape.fields[2]);
      const width = dimension(shape.fields[3]);
      if (steps === null) return fail(shape.fields[0]);
      if (run === null) return fail(shape.fields[1]);
      if (rise === null) return fail(shape.fields[2]);
      if (width === null) return fail(shape.fields[3]);
      return { ok: true, spec: { shape: "stairs", steps, run, rise, width } };
    }
    case "ramp": {
      const length = dimension(shape.fields[0]);
      const width = dimension(shape.fields[1]);
      const height = dimension(shape.fields[2]);
      if (length === null) return fail(shape.fields[0]);
      if (width === null) return fail(shape.fields[1]);
      if (height === null) return fail(shape.fields[2]);
      return { ok: true, spec: { shape: "ramp", length, width, height } };
    }
    case "arch": {
      const width = dimension(shape.fields[0]);
      const height = dimension(shape.fields[1]);
      const thickness = dimension(shape.fields[2]);
      const depth = dimension(shape.fields[3]);
      const segments = countIn(read("segments"), shape.fields[4]);
      if (width === null) return fail(shape.fields[0]);
      if (height === null) return fail(shape.fields[1]);
      if (thickness === null) return fail(shape.fields[2]);
      if (depth === null) return fail(shape.fields[3]);
      if (segments === null) return fail(shape.fields[4]);
      // The core rejects an arch that is not taller than half its width.
      if (height <= width / 2) return { ok: false, error: "height must be more than half the width" };
      return { ok: true, spec: { shape: "arch", width, height, thickness, depth, segments } };
    }
    case "extrude": {
      const outline = parsePoints(read("outline"), 2, shape.fields[0]);
      const height = dimension(shape.fields[1]);
      if (!outline) return fail(shape.fields[0]);
      if (outline.length < 3) return { ok: false, error: "outline needs three or more points" };
      if (Math.abs(signedArea(outline as Points2)) < 1e-6) return { ok: false, error: "outline is degenerate" };
      if (height === null) return fail(shape.fields[1]);
      return { ok: true, spec: { shape: "extrude", outline: outline as Points2, height } };
    }
    case "lathe": {
      const profile = parsePoints(read("profile"), 2, shape.fields[0]);
      const segments = countIn(read("segments"), shape.fields[1]);
      if (!profile) return fail(shape.fields[0]);
      if (profile.length < 2) return { ok: false, error: "profile needs two or more points" };
      if (profile.some((p) => p[0] < 0)) return { ok: false, error: "profile x must not be negative" };
      if (segments === null) return fail(shape.fields[1]);
      return { ok: true, spec: { shape: "lathe", profile: profile as Points2, segments } };
    }
    case "loft": {
      const section = parsePoints(read("section"), 2, shape.fields[0]);
      const path = parsePoints(read("path"), 3, shape.fields[1]);
      if (!section) return fail(shape.fields[0]);
      if (section.length < 2) return { ok: false, error: "section needs two or more points" };
      if (!path) return fail(shape.fields[1]);
      if (path.length < 2) return { ok: false, error: "path needs two or more points" };
      return { ok: true, spec: { shape: "loft", section: section as Points2, path: path as Points3, closed_section: bool("closed_section") } };
    }
    case "raw": {
      const positions = parsePoints(read("positions"), 3, shape.fields[0]);
      const indices = parseNumbers(read("indices"), shape.fields[1]);
      if (!positions) return fail(shape.fields[0]);
      if (!indices) return fail(shape.fields[1]);
      if (indices.length % 3 !== 0) return { ok: false, error: "indices must be a triangle list" };
      if (indices.some((i) => i >= positions.length)) return { ok: false, error: "an index is out of range" };
      return { ok: true, spec: { shape: "raw", positions: positions as Points3, indices } };
    }
  }
}

function signedArea(points: Points2): number {
  let total = 0;
  for (let i = 0; i < points.length; i++) {
    const a = points[i];
    const b = points[(i + 1) % points.length];
    total += a[0] * b[1] - b[0] * a[1];
  }
  return total / 2;
}

function formatFixed(values: readonly number[]): string {
  return values.join(" ");
}

function formatPoints(points: readonly (readonly number[])[]): string {
  return points.map((p) => p.join(",")).join(";");
}

function boolText(value: boolean): string {
  return value ? "1" : "0";
}

/** The starting text values for a shape, in field order. */
export function defaultValues(id: MeshShapeId): Record<string, string> {
  const defaults: Record<MeshShapeId, Record<string, string>> = {
    box: { size: "64 64 64" },
    cylinder: { radius: "32", height: "64", segments: "16", caps: "1" },
    cone: { radius: "32", height: "64", segments: "16" },
    sphere: { radius: "32", segments: "16", rings: "8" },
    torus: { major: "32", minor: "12", segments: "16", sides: "8" },
    plane: { size: "128 128", divisions: "4 4" },
    stairs: { steps: "8", run: "128", rise: "64", width: "64" },
    ramp: { length: "128", width: "64", height: "64" },
    arch: { width: "96", height: "128", thickness: "16", depth: "32", segments: "12" },
    extrude: { outline: formatPoints([[0, 0], [96, 0], [96, 32], [32, 32], [32, 96], [0, 96]]), height: "32" },
    lathe: { profile: formatPoints([[0, 0], [24, 0], [16, 48]]), segments: "16" },
    loft: {
      section: formatPoints([[-8, -8], [8, -8], [8, 8], [-8, 8]]),
      path: formatPoints([[0, 0, 0], [0, 0, 128], [0, 96, 192]]),
      closed_section: "1",
    },
    raw: {
      positions: formatPoints([[0, 0, 0], [64, 0, 0], [64, 64, 0], [0, 64, 0]]),
      indices: "0 1 2 0 2 3",
    },
  };
  return { ...defaults[id] };
}

/** The spec the form's text values describe, for callers that only need a shape. */
export function specTextValues(spec: MeshSpec): Record<string, string> {
  switch (spec.shape) {
    case "box":
      return { size: formatFixed(spec.size) };
    case "cylinder":
      return { radius: String(spec.radius), height: String(spec.height), segments: String(spec.segments), caps: boolText(spec.caps) };
    case "cone":
      return { radius: String(spec.radius), height: String(spec.height), segments: String(spec.segments) };
    case "sphere":
      return { radius: String(spec.radius), segments: String(spec.segments), rings: String(spec.rings) };
    case "torus":
      return { major: String(spec.major), minor: String(spec.minor), segments: String(spec.segments), sides: String(spec.sides) };
    case "plane":
      return { size: formatFixed(spec.size), divisions: formatFixed(spec.divisions) };
    case "stairs":
      return { steps: String(spec.steps), run: String(spec.run), rise: String(spec.rise), width: String(spec.width) };
    case "ramp":
      return { length: String(spec.length), width: String(spec.width), height: String(spec.height) };
    case "arch":
      return {
        width: String(spec.width),
        height: String(spec.height),
        thickness: String(spec.thickness),
        depth: String(spec.depth),
        segments: String(spec.segments),
      };
    case "extrude":
      return { outline: formatPoints(spec.outline), height: String(spec.height) };
    case "lathe":
      return { profile: formatPoints(spec.profile), segments: String(spec.segments) };
    case "loft":
      return { section: formatPoints(spec.section), path: formatPoints(spec.path), closed_section: boolText(spec.closed_section) };
    case "raw":
      return { positions: formatPoints(spec.positions), indices: spec.indices.join(" ") };
  }
}

export interface MeshRequestOptions {
  material: string;
  name?: string;
  position?: Vec3;
  /** Texture repeats per world unit; omitted uses the core's 512-texel tile. */
  uvScale?: number;
  collision?: "triangles" | "convex_hulls" | "none";
}

/** The `core.meshCreate` payload for a spec plus its placement. */
export function meshRequestJson(spec: MeshSpec, options: MeshRequestOptions): string {
  const request: Record<string, unknown> = {
    spec,
    material: options.material,
    collision: options.collision ?? "triangles",
  };
  if (options.name) request.name = options.name;
  if (options.uvScale !== undefined && Number.isFinite(options.uvScale) && options.uvScale > 0) {
    request.uv_scale = options.uvScale;
  }
  const at = options.position;
  if (at) request.transform = { position: [...at], rotation: [0, 0, 0, 1], scale: [1, 1, 1] };
  return JSON.stringify(request);
}
