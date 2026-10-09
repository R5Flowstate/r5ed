export interface AtlasEntry {
  /** Index of the sprite within the atlas image. */
  index: number;
  /** UV rect in [0,1]: u0, v0, u1, v1. */
  uv: [number, number, number, number];
  /** Pixel size within the atlas. */
  size: [number, number];
}

export interface GlyphAtlas {
  width: number;
  height: number;
  entries: Map<string, AtlasEntry>;
  /** RGBA8 pixels, row-major. */
  pixels: Uint8Array;
}

export interface AtlasOptions {
  cell: number;
  padding: number;
  columns: number;
}

export function defaultAtlasOptions(count: number): AtlasOptions {
  const columns = Math.max(1, Math.min(8, Math.ceil(Math.sqrt(Math.max(1, count)))));
  return { cell: 64, padding: 2, columns };
}

/** Grid slot and pixel rect for entry `index`. */
export function atlasSlot(index: number, options: AtlasOptions): { col: number; row: number; x: number; y: number } {
  const { cell, padding, columns } = options;
  const col = index % columns;
  const row = Math.floor(index / columns);
  return { col, row, x: col * (cell + padding), y: row * (cell + padding) };
}

export function atlasSize(count: number, options: AtlasOptions): { width: number; height: number } {
  const { cell, padding, columns } = options;
  const rows = Math.max(1, Math.ceil(count / columns));
  const width = columns * (cell + padding) - padding;
  const height = rows * (cell + padding) - padding;
  return { width: Math.max(1, width), height: Math.max(1, height) };
}

/** Packs glyph names into a grid atlas, one slot per name. */
export function layoutAtlas(names: readonly string[], options: AtlasOptions): GlyphAtlas {
  const { cell, padding } = options;
  const { width, height } = atlasSize(names.length, options);
  const entries = new Map<string, AtlasEntry>();
  const pixels = new Uint8Array(width * height * 4);
  names.forEach((name, i) => {
    const slot = atlasSlot(i, options);
    const u0 = slot.x / width;
    const v0 = slot.y / height;
    const u1 = (slot.x + cell) / width;
    const v1 = (slot.y + cell) / height;
    entries.set(name, { index: i, uv: [u0, v0, u1, v1], size: [cell, cell] });
    // A 1px border keeps bilinear sampling from bleeding between cells.
    for (let b = 0; b <= padding; b++) {
      const x0 = Math.max(0, slot.x - b);
      const y0 = Math.max(0, slot.y - b);
      const x1 = Math.min(width, slot.x + cell + b);
      const y1 = Math.min(height, slot.y + cell + b);
      for (let y = y0; y < y1; y++) {
        for (let x = x0; x < x1; x++) {
          const edge = x === x0 || y === y0 || x === x1 - 1 || y === y1 - 1;
          const o = (y * width + x) * 4;
          pixels[o] = edge ? 0 : 0;
          pixels[o + 1] = edge ? 0 : 0;
          pixels[o + 2] = edge ? 0 : 0;
          pixels[o + 3] = edge ? 0 : 0;
        }
      }
    }
  });
  return { width, height, entries, pixels };
}

/** The categories a classname maps to, in atlas order. */
export const CATEGORY_ORDER = [
  "trigger", "info_target", "light", "env", "func", "prop", "path",
  "player", "weapon", "vehicle", "npc", "gameplay", "debug", "other",
] as const;

export type Category = (typeof CATEGORY_ORDER)[number];

const PREFIX_CATEGORY: [string, Category][] = [
  ["trigger_", "trigger"],
  ["func_", "func"],
  ["env_", "env"],
  ["info_", "info_target"],
  ["light_", "light"],
  ["path_", "path"],
  ["prop_", "prop"],
  ["game_", "gameplay"],
  ["player_", "player"],
  ["weapon_", "weapon"],
  ["veh_", "vehicle"],
  ["npc_", "npc"],
];

/** Maps a classname to its billboard category. */
export function categoryFor(classname: string): Category {
  const name = classname.toLowerCase();
  if (name === "light" || name.startsWith("light_")) return "light";
  for (const [prefix, cat] of PREFIX_CATEGORY) if (name.startsWith(prefix)) return cat;
  if (name === "player" || name.startsWith("player_")) return "player";
  if (name.startsWith("zipline") || name.startsWith("path_")) return "path";
  return "other";
}

export const CATEGORY_COLORS: Record<Category, [number, number, number]> = {
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

/** Size of a category's glyph, in world units, for a given camera distance. */
export function billboardSize(distance: number, pixelsPerUnit: number): number {
  return (26 * Math.max(1, distance)) / Math.max(1e-3, pixelsPerUnit);
}
