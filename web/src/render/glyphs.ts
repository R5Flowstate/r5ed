import { CATEGORY_ORDER, type Category, type GlyphAtlas, defaultAtlasOptions, layoutAtlas } from "./atlas";

export interface GlyphCanvas {
  width: number;
  height: number;
  getContext(id: "2d"): CanvasRenderingContext2D | null;
}

export interface ReadbackContext extends CanvasRenderingContext2D {
  getImageData(x: number, y: number, w: number, h: number): ImageData;
}

export interface GlyphStyle {
  fill: string;
  stroke: string;
  font: string;
}

/** Fills the atlas cell with a rounded, category-coloured tile. */
function drawGlyph(ctx: CanvasRenderingContext2D, x: number, y: number, size: number, style: GlyphStyle, label: string): void {
  const pad = Math.max(2, Math.round(size * 0.08));
  const r = Math.round(size * 0.18);
  const w = size - pad * 2;
  ctx.save();
  ctx.clearRect(x, y, size, size);
  ctx.beginPath();
  ctx.moveTo(x + pad + r, y + pad);
  ctx.lineTo(x + pad + w - r, y + pad);
  ctx.quadraticCurveTo(x + pad + w, y + pad, x + pad + w, y + pad + r);
  ctx.lineTo(x + pad + w, y + pad + w - r);
  ctx.quadraticCurveTo(x + pad + w, y + pad + w, x + pad + w - r, y + pad + w);
  ctx.lineTo(x + pad + r, y + pad + w);
  ctx.quadraticCurveTo(x + pad, y + pad + w, x + pad, y + pad + w - r);
  ctx.lineTo(x + pad, y + pad + r);
  ctx.quadraticCurveTo(x + pad, y + pad, x + pad + r, y + pad);
  ctx.closePath();
  ctx.fillStyle = style.fill;
  ctx.fill();
  ctx.lineWidth = Math.max(1, size * 0.04);
  ctx.strokeStyle = style.stroke;
  ctx.stroke();
  ctx.fillStyle = style.stroke;
  ctx.font = style.font;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText(label, x + size / 2, y + size / 2);
  ctx.restore();
}

export function categoryStyle(category: Category, theme: "dark" | "light"): GlyphStyle {
  const lum = theme === "dark" ? 0.55 : 0.75;
  const tint = (v: number) => Math.round(v * 255 * lum);
  const [r, g, b] = CATEGORY_COLOR[category];
  return {
    fill: `rgb(${tint(r)},${tint(g)},${tint(b)})`,
    stroke: theme === "dark" ? "rgba(12,14,18,0.9)" : "rgba(255,255,255,0.85)",
    font: `${Math.round(64 * 0.5)}px sans-serif`,
  };
}

const CATEGORY_COLOR: Record<Category, [number, number, number]> = {
  trigger: [0.95, 0.45, 0.2],
  info_target: [0.4, 0.75, 0.95],
  light: [0.98, 0.9, 0.55],
  env: [0.55, 0.85, 0.45],
  func: [0.65, 0.65, 0.95],
  prop: [0.75, 0.75, 0.78],
  path: [0.9, 0.55, 0.9],
  player: [0.5, 0.9, 0.85],
  weapon: [0.85, 0.6, 0.55],
  vehicle: [0.7, 0.7, 0.6],
  npc: [0.8, 0.45, 0.6],
  gameplay: [0.6, 0.85, 0.7],
  debug: [0.9, 0.4, 0.4],
  other: [0.8, 0.8, 0.8],
};

/** The two-letter label drawn inside a category's cell. */
export function categoryLabel(category: Category): string {
  return category.slice(0, 2).toUpperCase();
}

/**
 * Builds the sprite atlas at runtime: one cell per category with its label
 * drawn by the 2D context, so the editor ships no image assets. The pixels are
 * read back when the context allows it, otherwise the canvas itself is the
 * source the texture is created from.
 */
export function buildGlyphAtlas(canvas: GlyphCanvas, theme: "dark" | "light" = "dark"): GlyphAtlas {
  const names = [...CATEGORY_ORDER];
  const options = defaultAtlasOptions(names.length);
  const layout = layoutAtlas(names, options);
  canvas.width = layout.width;
  canvas.height = layout.height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("glyph atlas needs a 2d context");
  for (const name of names) {
    const entry = layout.entries.get(name);
    if (!entry) continue;
    const col = entry.index % options.columns;
    const row = Math.floor(entry.index / options.columns);
    const x = col * (options.cell + options.padding);
    const y = row * (options.cell + options.padding);
    const style = categoryStyle(name as Category, theme);
    drawGlyph(ctx, x, y, options.cell, style, categoryLabel(name as Category));
  }
  const readback = ctx as Partial<ReadbackContext>;
  if (typeof readback.getImageData === "function") {
    const data = readback.getImageData(0, 0, layout.width, layout.height);
    layout.pixels.set(data.data.subarray(0, layout.pixels.length));
  }
  return layout;
}

/** UV rect of a category's glyph, falling back to the last entry. */
export function glyphRect(atlas: GlyphAtlas, category: Category): [number, number, number, number] {
  return atlas.entries.get(category)?.uv ?? [0, 0, 1, 1];
}
