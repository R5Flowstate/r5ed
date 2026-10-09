import type { Vec3 } from "../core/api";

/** Snap sizes the status bar and the grid offer, in inches. */
export const GRID_SIZES = [1, 4, 8, 16, 32, 64, 128, 256, 512, 1024] as const;
export type GridSize = (typeof GRID_SIZES)[number];

export interface GridSettings {
  /** Minor line spacing in inches. */
  minor: number;
  /** Major line every `majorEvery` minor cells. */
  majorEvery: number;
  /** Half-extent of the drawn square, in inches. */
  extent: number;
  /** Distance at which lines fade out entirely. */
  fadeDistance: number;
  /** XY point the grid is centred on (snapped to the major spacing). */
  center?: [number, number];
}

export function defaultGrid(): GridSettings {
  return { minor: 16, majorEvery: 4, extent: 4096, fadeDistance: 2400 };
}

/** Grid settings for a snap size at a fixed scale. */
export function gridForSize(size: GridSize): GridSettings {
  return { minor: size, majorEvery: 8, extent: Math.max(4096, size * 256), fadeDistance: Math.max(2400, size * 160) };
}

/**
 * A grid that follows the camera: spacing grows by 4x as the view pulls back
 * so a few hundred lines stay on screen, and the drawn area and fade reach
 * scale with the view distance. Map-scale views (tens of thousands of units)
 * therefore still show a grid.
 */
export function adaptiveGrid(snap: number, viewDistance: number, target: Vec3): GridSettings {
  const dist = Math.max(64, viewDistance);
  let minor = Math.max(1, snap);
  while (dist / minor > 160) minor *= 4;
  const major = minor * 8;
  const extent = Math.min(minor * 1800, Math.max(4096, dist * 6));
  return {
    minor,
    majorEvery: 8,
    extent,
    fadeDistance: Math.max(2400, dist * 5),
    center: [Math.round(target[0] / major) * major, Math.round(target[1] / major) * major],
  };
}

export interface GridLine {
  start: Vec3;
  end: Vec3;
  major: boolean;
}

export interface GridMesh {
  /** xyz per vertex. */
  positions: Float32Array;
  /** 1 for major lines, 0 for minor. */
  weight: Float32Array;
  count: number;
}

const MAX_LINES = 4096;

/**
 * Line list for the XY grid centred on the origin, plus the two world axes
 * drawn on top. Vertices carry a major-line weight for the shader.
 */
export function buildGridLines(settings: GridSettings): GridMesh {
  const minor = Math.max(0.01, settings.minor);
  const majorEvery = Math.max(1, Math.round(settings.majorEvery));
  const extent = Math.max(minor, settings.extent);
  const half = Math.min(Math.ceil(extent / minor), Math.floor((MAX_LINES - 2) / 4)) * minor;
  const cx = settings.center?.[0] ?? 0;
  const cy = settings.center?.[1] ?? 0;
  const lines: GridLine[] = [];
  // Two lines per step on each side of the centre.
  const steps = Math.min(Math.floor((MAX_LINES - 2) / 4), Math.floor(half / minor));
  for (let i = -steps; i <= steps; i++) {
    if (steps <= 0) break;
    const v = i * minor;
    const major = i % majorEvery === 0;
    if (lines.length >= MAX_LINES) break;
    lines.push({ start: [cx + v, cy - half, 0], end: [cx + v, cy + half, 0], major });
    if (lines.length >= MAX_LINES) break;
    lines.push({ start: [cx - half, cy + v, 0], end: [cx + half, cy + v, 0], major });
  }
  lines.push({ start: [cx - half, 0, 0], end: [cx + half, 0, 0], major: true });
  lines.push({ start: [0, cy - half, 0], end: [0, cy + half, 0], major: true });
  const positions = new Float32Array(lines.length * 6);
  const weight = new Float32Array(lines.length * 2);
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    positions[i * 6 + 0] = l.start[0];
    positions[i * 6 + 1] = l.start[1];
    positions[i * 6 + 2] = l.start[2];
    positions[i * 6 + 3] = l.end[0];
    positions[i * 6 + 4] = l.end[1];
    positions[i * 6 + 5] = l.end[2];
    weight[i * 2] = l.major ? 1 : 0;
    weight[i * 2 + 1] = l.major ? 1 : 0;
  }
  return { positions, weight, count: lines.length * 2 };
}

/** Alpha for a grid line at `distance` from the eye, fading to zero at the end. */
export function gridFade(distance: number, fadeDistance: number): number {
  if (!(fadeDistance > 0)) return 1;
  if (distance >= fadeDistance) return 0;
  const t = distance / fadeDistance;
  return 1 - t * t;
}

/** Snaps a world position to the grid, preserving Z. */
export function snapToGrid(p: Vec3, size: GridSize): Vec3 {
  return [Math.round(p[0] / size) * size, Math.round(p[1] / size) * size, p[2]];
}
