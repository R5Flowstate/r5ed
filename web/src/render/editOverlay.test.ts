import { describe, expect, it } from "vitest";
import { EDIT_SEAM, EDIT_SELECTED, EDIT_SHARP, EditOverlayCache, buildEditOverlay, type EditMeshSource } from "./editOverlay";

function quad(selEdge = -1, flags: number[] = [0, 0, 0, 0]): EditMeshSource {
  const edgeSel = new Uint8Array(4);
  if (selEdge >= 0) edgeSel[selEdge] = 1;
  return {
    verts: new Float32Array([0, 0, 0, 64, 0, 0, 64, 64, 0, 0, 64, 0]),
    polyEdges: new Uint32Array([0, 1, 1, 2, 2, 3, 3, 0]),
    faceCentres: new Float32Array([32, 32, 0]),
    vertSel: new Uint8Array(4),
    edgeSel,
    faceSel: new Uint8Array([0]),
    edgeFlags: new Uint8Array(flags),
    topoHash: "quad",
  };
}

describe("edit overlay", () => {
  it("draws polygon edges, vertex crosses and face dots", () => {
    const lines = buildEditOverlay(quad(), "vertex", 2).lines;
    // 4 edge segments plus 4 three-segment crosses, two vertices each.
    expect(lines.count).toBe((4 + 4 * 3) * 2);
    const face = buildEditOverlay(quad(), "face", 2).lines;
    // One twelve-segment dot box on top.
    expect(face.count).toBe((4 + 4 * 3 + 12) * 2);
  });

  it("colours selection, sharp and seam edges apart", () => {
    const lines = buildEditOverlay(quad(2, [1, 2, 0, 0]), "edge", 2).lines;
    const colorAt = (segment: number): number[] => [...lines.colors.slice(segment * 2 * 4, segment * 2 * 4 + 4)];
    for (const [segment, want] of [[0, EDIT_SHARP], [1, EDIT_SEAM], [2, EDIT_SELECTED]] as const) {
      colorAt(segment).slice(0, 3).forEach((c, i) => expect(c).toBeCloseTo(want[i] as number, 5));
    }
  });

  it("keeps the buffers across a positions-only drag", () => {
    const cache = new EditOverlayCache();
    const first = cache.update(quad(), "vertex", 2);
    const moved = quad();
    moved.verts = new Float32Array([1, 0, 0, 64, 0, 0, 64, 64, 0, 0, 64, 0]);
    const second = cache.update(moved, "vertex", 2);
    expect(second).toBe(first);
    expect(second.lines.positions[0]).toBeCloseTo(1, 5);
    const changed = quad();
    changed.topoHash = "quad-split";
    expect(cache.update(changed, "vertex", 2)).not.toBe(first);
    expect(cache.update(changed, "face", 2)).not.toBe(first);
  });
});
