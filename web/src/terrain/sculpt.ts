/**
 * TypeScript mirror of the terrain sculpt stroke. The field names and types
 * match the serde shape the core deserializes: points are `[x, y, pressure]`,
 * `falloff` is a 0..1 blend between a flat disc and a smoothstep cone, and the
 * stamp tool carries its brush as a heightfield.
 */

import type { BlendMode } from "./recipe";

export type SculptTool =
  | "raise" | "lower" | "smooth" | "flatten" | "set_height"
  | "noise" | "erode" | "terrace" | "ramp" | "stamp";

export const SCULPT_TOOLS: SculptTool[] = [
  "raise", "lower", "smooth", "flatten", "set_height",
  "noise", "erode", "terrace", "ramp", "stamp",
];

/**
 * Named falloffs, each the `falloff` scalar the core expects. The core blends a
 * flat disc into a smoothstep cone, so these are points on that one axis.
 */
export type Falloff = "hard" | "sharp" | "smooth" | "linear" | "soft";

export const FALLOFFS: Falloff[] = ["hard", "sharp", "smooth", "linear", "soft"];

export const FALLOFF_VALUES: Record<Falloff, number> = {
  hard: 0,
  sharp: 0.25,
  smooth: 0.5,
  linear: 0.75,
  soft: 1,
};

export const STAMP_BLEND_MODES: BlendMode[] = [
  "replace", "add", "subtract", "multiply", "screen", "overlay", "max", "min", "mix",
];

/** A regular grid of heights, row-major, as the stamp brush is carried. */
export interface Heightfield {
  width: number;
  height: number;
  cell_size: number;
  data: number[];
}

export interface StrokePoint {
  /** Object-space terrain X, in inches. */
  x: number;
  /** Object-space terrain Y, in inches. */
  y: number;
  /** 0..1. */
  pressure: number;
}

export interface Stroke {
  tool: SculptTool;
  points: StrokePoint[];
  /** Brush radius in inches. */
  radius: number;
  /** Inches of effect at full pressure for raise and lower. */
  strength: number;
  /** 0 is a flat disc, 1 is a smoothstep cone. */
  falloff: number;
  /** Height the flatten, set-height and terrace tools aim at. */
  target_height: number;
  /** Stamp brush; only the stamp tool reads it. */
  brush: Heightfield | null;
  /** Clockwise rotation of the stamp brush, in radians. */
  rotation: number;
  /** How the stamp brush combines with the surface. */
  blend: BlendMode;
  /** Benches per inch for the terrace tool. */
  step: number;
  /** Seed for the noise and erode tools. */
  seed: number;
  /** Noise scale in cycles per inch for the noise tool. */
  noise_frequency: number;
}

export function defaultStroke(tool: SculptTool = "raise"): Stroke {
  return {
    tool,
    points: [],
    radius: 128,
    strength: 32,
    falloff: FALLOFF_VALUES.smooth,
    target_height: 0,
    brush: null,
    rotation: 0,
    blend: "add",
    step: 24,
    seed: 0,
    noise_frequency: 0.01,
  };
}

export class StrokeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "StrokeError";
  }
}

const BOUNDS = 1 << 22;

function num(v: unknown, fallback: number): number {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && v.trim() !== "") {
    const n = Number(v);
    if (Number.isFinite(n)) return n;
  }
  return fallback;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

function parseHeightfield(v: unknown): Heightfield | null {
  if (!isRecord(v)) return null;
  const width = Math.max(1, Math.round(num(v.width, 0)));
  const height = Math.max(1, Math.round(num(v.height, 0)));
  const cell = num(v.cell_size, 0);
  const data = Array.isArray(v.data) ? v.data.map((n) => num(n, 0)) : [];
  if (width <= 0 || height <= 0 || !(cell > 0)) return null;
  if (data.length !== width * height) return null;
  if (data.some((n) => !Number.isFinite(n))) return null;
  return { width, height, cell_size: cell, data };
}

/** Validates a stroke and returns it, so the caller can hand it to the core. */
export function validateStroke(stroke: Stroke): Stroke {
  if (!SCULPT_TOOLS.includes(stroke.tool)) throw new StrokeError(`unknown sculpt tool ${stroke.tool}`);
  if (!Number.isFinite(stroke.falloff) || stroke.falloff < 0 || stroke.falloff > 1) {
    throw new StrokeError("falloff must be 0..1");
  }
  if (!Number.isFinite(stroke.radius) || stroke.radius <= 0 || stroke.radius > BOUNDS) {
    throw new StrokeError("radius is out of range");
  }
  if (!Number.isFinite(stroke.strength) || Math.abs(stroke.strength) > BOUNDS) {
    throw new StrokeError("strength is out of range");
  }
  if (!Number.isFinite(stroke.target_height) || Math.abs(stroke.target_height) > BOUNDS) {
    throw new StrokeError("target height is out of range");
  }
  if (!Number.isFinite(stroke.step) || stroke.step <= 0 || stroke.step > BOUNDS) {
    throw new StrokeError("terrace step must be positive");
  }
  if (!Number.isInteger(stroke.seed) || stroke.seed < 0 || stroke.seed > 0xffffffff) {
    throw new StrokeError("seed must be a u32");
  }
  if (!Number.isFinite(stroke.noise_frequency) || stroke.noise_frequency <= 0) {
    throw new StrokeError("noise frequency must be positive");
  }
  if (!Number.isFinite(stroke.rotation)) throw new StrokeError("rotation is not finite");
  if (stroke.points.length > 4096) throw new StrokeError("stroke has too many points");
  for (const p of stroke.points) {
    if (!Number.isFinite(p.x) || !Number.isFinite(p.y)) throw new StrokeError("stroke point is not finite");
    if (Math.abs(p.x) > BOUNDS || Math.abs(p.y) > BOUNDS) throw new StrokeError("stroke point is out of range");
    if (!Number.isFinite(p.pressure) || p.pressure < 0 || p.pressure > 1) {
      throw new StrokeError("pressure must be 0..1");
    }
  }
  if (stroke.tool === "ramp" && stroke.points.length < 2) {
    throw new StrokeError("a ramp stroke needs at least two points");
  }
  if (stroke.tool === "stamp") {
    if (!stroke.brush) throw new StrokeError("the stamp tool needs a brush heightfield");
  }
  return stroke;
}

/**
 * Weight at distance `d` from the brush centre, for a radius of `radius`.
 * This is the same curve the core applies: `falloff` 0 is a flat disc and 1 is
 * a smoothstep cone.
 */
export function falloffWeight(d: number, radius: number, falloff: number): number {
  if (!(radius > 0) || d >= radius) return 0;
  const t = 1 - d / radius;
  const f = Math.min(1, Math.max(0, falloff));
  return (1 - f) + f * (t * t * (3 - 2 * t));
}

/** The `falloff` scalar a named falloff stands for. */
export function falloffValue(falloff: Falloff): number {
  return FALLOFF_VALUES[falloff] ?? FALLOFF_VALUES.smooth;
}

/** The named falloff nearest a scalar, so a round trip keeps the name. */
export function falloffName(value: number): Falloff {
  let best: Falloff = "smooth";
  let bestDelta = Infinity;
  for (const name of FALLOFFS) {
    const delta = Math.abs(FALLOFF_VALUES[name] - value);
    if (delta < bestDelta) {
      bestDelta = delta;
      best = name;
    }
  }
  return best;
}

/** Samples a stroke into discrete dabs along the path. */
export function strokeDabs(stroke: Stroke, spacing = 16): StrokePoint[] {
  const s = validateStroke(stroke);
  if (s.points.length === 0) return [];
  const step = Math.max(1, spacing);
  const out: StrokePoint[] = [];
  for (let i = 0; i < s.points.length; i++) {
    const p = s.points[i];
    const prev = i > 0 ? s.points[i - 1] : null;
    if (!prev || prev.pressure === 0) {
      out.push({ x: p.x, y: p.y, pressure: p.pressure });
      continue;
    }
    const dx = p.x - prev.x;
    const dy = p.y - prev.y;
    const n = Math.max(1, Math.ceil(Math.hypot(dx, dy) / step));
    for (let k = 1; k <= n; k++) {
      const t = k / n;
      out.push({
        x: prev.x + dx * t,
        y: prev.y + dy * t,
        pressure: prev.pressure + (p.pressure - prev.pressure) * t,
      });
    }
  }
  return out;
}

/** The height a ramp stroke reaches at the far end of its polyline. */
export function rampEnd(stroke: Stroke): StrokePoint | null {
  if (stroke.tool !== "ramp" || stroke.points.length < 2) return null;
  return { ...stroke.points[stroke.points.length - 1] };
}
/** Serialises a stroke in the shape `terrainSculpt` expects it. */
export function strokeJson(stroke: Stroke): string {
  const s = validateStroke(stroke);
  return JSON.stringify({
    tool: s.tool,
    points: s.points.map((p) => [p.x, p.y, p.pressure]),
    radius: s.radius,
    strength: s.strength,
    falloff: s.falloff,
    target_height: s.target_height,
    brush: s.brush,
    rotation: s.rotation,
    blend: s.blend,
    step: s.step,
    seed: s.seed,
    noise_frequency: s.noise_frequency,
  });
}

/** Parses stroke JSON, filling in defaults, so a stored stroke round-trips. */
export function parseStroke(text: string): Stroke {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new StrokeError("stroke JSON does not parse");
  }
  if (!isRecord(parsed)) throw new StrokeError("stroke is not an object");
  const tool = SCULPT_TOOLS.includes(parsed.tool as SculptTool) ? (parsed.tool as SculptTool) : "raise";
  const out = defaultStroke(tool);
  const points: StrokePoint[] = [];
  if (Array.isArray(parsed.points)) {
    for (const raw of parsed.points) {
      if (Array.isArray(raw) && raw.length >= 3) {
        points.push({ x: num(raw[0], 0), y: num(raw[1], 0), pressure: num(raw[2], 1) });
      } else if (isRecord(raw)) {
        points.push({ x: num(raw.x, 0), y: num(raw.y, 0), pressure: num(raw.pressure, 1) });
      }
    }
  }
  out.points = points.slice(0, 4096);
  out.radius = num(parsed.radius, out.radius);
  out.strength = num(parsed.strength, out.strength);
  out.falloff = Math.min(1, Math.max(0, num(parsed.falloff, out.falloff)));
  out.target_height = num(parsed.target_height, out.target_height);
  out.brush = parseHeightfield(parsed.brush);
  out.rotation = num(parsed.rotation, out.rotation);
  out.blend = STAMP_BLEND_MODES.includes(parsed.blend as BlendMode) ? (parsed.blend as BlendMode) : out.blend;
  out.step = num(parsed.step, out.step);
  out.seed = Math.round(num(parsed.seed, out.seed));
  out.noise_frequency = num(parsed.noise_frequency, out.noise_frequency);
  return out;
}

export interface Rect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/**
 * Sample-space rectangle a stroke dirties, so the viewport re-meshes only the
 * affected chunks. Samples are `cell_size` apart, X being the fast axis.
 */
export function strokeDirtyRect(stroke: Stroke, resolution: [number, number], cellSize: number): Rect | null {
  const s = validateStroke(stroke);
  if (s.points.length === 0 || !(cellSize > 0)) return null;
  const [w, h] = resolution;
  if (!(w > 0) || !(h > 0)) return null;
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const p of s.points) {
    minX = Math.min(minX, p.x);
    maxX = Math.max(maxX, p.x);
    minY = Math.min(minY, p.y);
    maxY = Math.max(maxY, p.y);
  }
  const r = s.radius + 1;
  const x0 = Math.max(0, Math.floor((minX - r) / cellSize));
  const x1 = Math.min(w - 1, Math.ceil((maxX + r) / cellSize));
  const y0 = Math.max(0, Math.floor((minY - r) / cellSize));
  const y1 = Math.min(h - 1, Math.ceil((maxY + r) / cellSize));
  if (x0 > x1 || y0 > y1) return null;
  return { x0, y0, x1: x1 + 1, y1: y1 + 1 };
}
