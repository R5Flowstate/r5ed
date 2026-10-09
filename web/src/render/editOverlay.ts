import { LineBuilder, type LineMesh } from "./overlay";
import type { EditSelMode } from "../state/meshEdit";

/** The drawn polygon edges of one session, without triangulation diagonals. */
export interface EditMeshSource {
  verts: Float32Array;
  polyEdges: Uint32Array;
  faceCentres: Float32Array;
  vertSel: Uint8Array;
  edgeSel: Uint8Array;
  faceSel: Uint8Array;
  /** One byte per polygon edge: bit 0 sharp, bit 1 seam. */
  edgeFlags: Uint8Array;
  topoHash: string;
}

export const EDIT_EDGE: readonly [number, number, number, number] = [0.45, 0.5, 0.6, 1];
export const EDIT_POINT: readonly [number, number, number, number] = [0.7, 0.73, 0.8, 1];
export const EDIT_DOT: readonly [number, number, number, number] = [0.55, 0.58, 0.65, 1];
export const EDIT_SELECTED: readonly [number, number, number, number] = [1, 0.72, 0.15, 1];
export const EDIT_SHARP: readonly [number, number, number, number] = [0.25, 0.75, 1, 1];
export const EDIT_SEAM: readonly [number, number, number, number] = [1, 0.35, 0.2, 1];

export interface EditOverlay {
  mode: EditSelMode;
  topoHash: string;
  lines: LineMesh;
}

function edgeColor(sel: boolean, flags: number): readonly [number, number, number, number] {
  if (sel) return EDIT_SELECTED;
  if (flags & 2) return EDIT_SEAM;
  if (flags & 1) return EDIT_SHARP;
  return EDIT_EDGE;
}

function vert(i: number, verts: Float32Array): [number, number, number] {
  return [verts[i * 3], verts[i * 3 + 1], verts[i * 3 + 2]];
}

/** A vertex point as a 3-axis cross: cheaper than a box at high counts. */
function cross(into: LineBuilder, p: [number, number, number], h: number, color: readonly [number, number, number, number]): void {
  into.segment([p[0] - h, p[1], p[2]], [p[0] + h, p[1], p[2]], color);
  into.segment([p[0], p[1] - h, p[2]], [p[0], p[1] + h, p[2]], color);
  into.segment([p[0], p[1], p[2] - h], [p[0], p[1], p[2] + h], color);
}

/** A face-centre dot as a small box, the marker look the editor already uses. */
function dot(into: LineBuilder, p: [number, number, number], h: number, color: readonly [number, number, number, number]): void {
  const lo: [number, number, number] = [p[0] - h, p[1] - h, p[2] - h];
  const hi: [number, number, number] = [p[0] + h, p[1] + h, p[2] + h];
  const c: [number, number, number][] = [
    [lo[0], lo[1], lo[2]],
    [hi[0], lo[1], lo[2]],
    [lo[0], hi[1], lo[2]],
    [hi[0], hi[1], lo[2]],
    [lo[0], lo[1], hi[2]],
    [hi[0], lo[1], hi[2]],
    [lo[0], hi[1], hi[2]],
    [hi[0], hi[1], hi[2]],
  ];
  for (const [a, b] of [[0, 1], [1, 3], [3, 2], [2, 0], [4, 5], [5, 7], [7, 6], [6, 4], [0, 4], [1, 5], [2, 6], [3, 7]] as const) {
    into.segment(c[a], c[b], color);
  }
}

/**
 * Builds the edit-mode overlay from session buffers: polygon edges coloured
 * by selection, sharp and seam flags; vertex crosses; face dots in face mode.
 */
export function buildEditOverlay(src: EditMeshSource, mode: EditSelMode, size: number): EditOverlay {
  const into = new LineBuilder();
  const h = Math.max(0.5, size);
  const edges = src.polyEdges.length / 2;
  for (let i = 0; i < edges; i++) {
    const a = src.polyEdges[i * 2];
    const b = src.polyEdges[i * 2 + 1];
    if (a * 3 + 2 >= src.verts.length || b * 3 + 2 >= src.verts.length) continue;
    into.segment(vert(a, src.verts), vert(b, src.verts), edgeColor(src.edgeSel[i] === 1, src.edgeFlags[i] ?? 0));
  }
  const verts = src.verts.length / 3;
  for (let i = 0; i < verts; i++) {
    cross(into, vert(i, src.verts), h, src.vertSel[i] === 1 ? EDIT_SELECTED : EDIT_POINT);
  }
  if (mode === "face") {
    const faces = src.faceCentres.length / 3;
    for (let i = 0; i < faces; i++) {
      dot(into, [src.faceCentres[i * 3], src.faceCentres[i * 3 + 1], src.faceCentres[i * 3 + 2]], h, src.faceSel[i] === 1 ? EDIT_SELECTED : EDIT_DOT);
    }
  }
  return { mode, topoHash: src.topoHash, lines: into.build() };
}

/**
 * Caches the overlay by topology hash: a drag that only moves positions
 * rewrites the same buffers instead of rebuilding them.
 */
export class EditOverlayCache {
  private current: EditOverlay | null = null;

  update(src: EditMeshSource, mode: EditSelMode, size: number): EditOverlay {
    const hit = this.current;
    if (hit && hit.topoHash === src.topoHash && hit.mode === mode) {
      const fresh = buildEditOverlay(src, mode, size);
      if (fresh.lines.positions.length === hit.lines.positions.length) {
        hit.lines.positions.set(fresh.lines.positions);
        hit.lines.colors.set(fresh.lines.colors);
        return hit;
      }
    }
    const built = buildEditOverlay(src, mode, size);
    this.current = built;
    return built;
  }

  clear(): void {
    this.current = null;
  }
}
