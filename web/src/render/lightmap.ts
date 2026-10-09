import type { Id } from "../core/api";

/** One object's lightmap, as `bakePreview` returns it. */
export interface LightmapPreview {
  width: number;
  height: number;
  rgba: Uint8Array;
}

export type LightmapPreviews = Record<string, LightmapPreview>;

export interface PackedLightmap {
  width: number;
  height: number;
  /** RGBA8 atlas, row-major, ready to upload. */
  pixels: Uint8Array;
  /** UV rect per object id in atlas space, with a half-texel inset. */
  rects: Map<Id, [number, number, number, number]>;
}

export interface PackOptions {
  /** Longest edge of the atlas; the layout shrinks to fit. */
  maxSize: number;
  /** Fraction of a cell kept as padding so bilinear taps do not bleed. */
  pad: number;
}

export const DEFAULT_PACK: PackOptions = { maxSize: 2048, pad: 0.02 };

/**
 * Packs the per-object lightmaps into one atlas so a single texture serves
 * every instance. Rectangles are laid out in rows, tallest first, which keeps
 * the row count low for the mixed sizes a bake produces.
 */
export function packLightmaps(previews: LightmapPreviews, options: PackOptions = DEFAULT_PACK): PackedLightmap {
  const entries = Object.entries(previews).filter(([, p]) => p.width > 0 && p.height > 0 && p.rgba.length >= p.width * p.height * 4);
  if (entries.length === 0) return { width: 1, height: 1, pixels: new Uint8Array(4), rects: new Map() };
  const maxSize = Math.max(64, Math.floor(options.maxSize));
  const pad = Math.min(0.25, Math.max(0, options.pad));

  // Shrink uniformly until the row layout fits the atlas budget.
  let scale = 1;
  let layout = layoutRows(entries, maxSize, scale, pad);
  while (layout.width > maxSize && scale > 0.05) {
    scale /= 2;
    layout = layoutRows(entries, maxSize, scale, pad);
  }
  const { width, height, cells } = layout;
  const pixels = new Uint8Array(width * height * 4);
  cells.forEach((cell, i) => {
    const preview = entries[i][1];
    copyInto(pixels, width, height, cell.x, cell.y, cell.w, cell.h, preview.rgba, preview.width, preview.height, pad);
  });
  const uv = new Map<Id, [number, number, number, number]>();
  cells.forEach((cell, i) => {
    const inset = 0.5 / Math.max(1, Math.min(cell.w, cell.h));
    uv.set(entries[i][0], [
      (cell.x + inset) / width,
      (cell.y + inset) / height,
      (cell.w - inset * 2) / width,
      (cell.h - inset * 2) / height,
    ]);
  });
  return { width, height, pixels, rects: uv };
}

interface Cell {
  x: number;
  y: number;
  w: number;
  h: number;
}

interface Layout {
  width: number;
  height: number;
  cells: Cell[];
  rects: Map<Id, [number, number, number, number]>;
}

function layoutRows(entries: [string, LightmapPreview][], maxSize: number, scale: number, pad: number): Layout {
  const sizes = entries
    .map(([, p]) => ({ w: Math.max(1, Math.round(p.width * scale)), h: Math.max(1, Math.round(p.height * scale)) }))
    .sort((a, b) => b.h - a.h);
  const cells: Cell[] = [];
  let x = 0;
  let y = 0;
  let rowHeight = 0;
  let width = 1;
  for (const size of sizes) {
    const w = size.w + 2;
    const h = size.h + 2;
    if (x + w > maxSize && x > 0) {
      x = 0;
      y += rowHeight;
      rowHeight = 0;
    }
    cells.push({ x, y, w: size.w, h: size.h });
    x += w;
    rowHeight = Math.max(rowHeight, h);
    width = Math.max(width, x);
  }
  void pad;
  return { width, height: y + rowHeight, cells, rects: new Map() };
}

/** Box-filters a lightmap into its cell, keeping the border off its neighbours. */
function copyInto(
  target: Uint8Array,
  targetWidth: number,
  targetHeight: number,
  x: number,
  y: number,
  w: number,
  h: number,
  source: Uint8Array,
  sourceWidth: number,
  sourceHeight: number,
  pad: number,
): void {
  const insetX = Math.floor(w * pad);
  const insetY = Math.floor(h * pad);
  const dstX0 = x + insetX;
  const dstY0 = y + insetY;
  const dstW = Math.max(1, w - insetX * 2);
  const dstH = Math.max(1, h - insetY * 2);
  for (let dy = 0; dy < dstH; dy++) {
    const sy = Math.min(sourceHeight - 1, Math.floor(((dy + 0.5) * sourceHeight) / dstH));
    for (let dx = 0; dx < dstW; dx++) {
      const sx = Math.min(sourceWidth - 1, Math.floor(((dx + 0.5) * sourceWidth) / dstW));
      const s = (sy * sourceWidth + sx) * 4;
      const d = ((dstY0 + dy) * targetWidth + (dstX0 + dx)) * 4;
      if (d < 0 || d + 3 >= target.length) continue;
      target[d] = source[s];
      target[d + 1] = source[s + 1];
      target[d + 2] = source[s + 2];
      target[d + 3] = 255;
    }
  }
  void targetHeight;
}

/** The UV rect for an object, or a zero rect when it has no lightmap. */
export function lightmapRect(packed: PackedLightmap | null, id: Id): [number, number, number, number] {
  if (!packed) return [0, 0, 0, 0];
  return packed.rects.get(id) ?? [0, 0, 0, 0];
}
