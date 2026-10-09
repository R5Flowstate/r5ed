// Box geometry for the brush tool, kept apart from the viewport so the
// dimensions the user asked for are the ones that reach the core.

import type { Vec3 } from "../core/api";
import { type GridSize, snapToGrid } from "./grid";

/** Thickness the brush gets along the axis a drag cannot reach. */
export const DEFAULT_BRUSH_HEIGHT = 128;
export const MIN_BRUSH_HEIGHT = 1;

export interface BoxBounds {
  min: Vec3;
  max: Vec3;
}

/** The axis a plane normal points along: 0 = x, 1 = y, 2 = z. */
export function normalAxis(normal: Vec3): 0 | 1 | 2 {
  if (normal[0] !== 0) return 0;
  if (normal[1] !== 0) return 1;
  return 2;
}

/** Clamps a height to a range the core can build a solid from. */
export function brushHeight(value: number, fallback = DEFAULT_BRUSH_HEIGHT): number {
  if (!Number.isFinite(value)) return fallback;
  return Math.max(MIN_BRUSH_HEIGHT, Math.min(1_000_000, Math.round(value)));
}

function ordered(a: Vec3, b: Vec3): { lo: Vec3; hi: Vec3 } {
  return {
    lo: [Math.min(a[0], b[0]), Math.min(a[1], b[1]), Math.min(a[2], b[2])],
    hi: [Math.max(a[0], b[0]), Math.max(a[1], b[1]), Math.max(a[2], b[2])],
  };
}

/**
 * The box a drag describes. Two axes come from the snapped cursor points and
 * the axis the view plane normal points along gets `height`; a top or
 * perspective drag grows upward, a front or side drag grows both ways so the
 * box stays centred on the plane.
 */
export function dragBox(start: Vec3, end: Vec3, planeNormal: Vec3, grid: GridSize, height: number): BoxBounds {
  const a = snapToGrid(start, grid);
  const b = snapToGrid(end, grid);
  const { lo, hi } = ordered(a, b);
  const axis = normalAxis(planeNormal);
  const depth = Math.max(grid, brushHeight(height));
  if (axis === 2) {
    hi[2] = lo[2] + depth;
  } else {
    lo[axis] -= depth / 2;
    hi[axis] += depth / 2;
  }
  const span = Math.max(1, grid);
  for (let i = 0; i < 3; i++) if (hi[i] - lo[i] < span) hi[i] = lo[i] + span;
  return { min: lo, max: hi };
}

/** A box of `size` centred on `at`. */
export function centredBox(at: Vec3, size: Vec3): BoxBounds {
  const half: Vec3 = [size[0] / 2, size[1] / 2, size[2] / 2];
  return {
    min: [at[0] - half[0], at[1] - half[1], at[2] - half[2]],
    max: [at[0] + half[0], at[1] + half[1], at[2] + half[2]],
  };
}
