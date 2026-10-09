import { describe, expect, it } from "vitest";
import type { Core } from "../core/api";
import { parseOpResult, rerunMeshOp, rerunRequest, type MeshOpResult } from "./meshEdit";
import type { LastMeshOp } from "./meshEdit";

const LAST: LastMeshOp = {
  request: {
    op: "translate",
    object: "abc",
    sel: { mode: "vertex", ids: [0, 1] },
    params: { translate: [1, 0, 0] },
  },
  report: { verts: 8, edges: 18, faces: 12, warnings: [] },
  depth: 4,
};

function opJson(result: MeshOpResult): string {
  return JSON.stringify({
    command: { op: "batch", label: "mesh", commands: [] },
    selection: result.selection,
    report: result.report,
  });
}

function fakeCore(answer: MeshOpResult, seen: string[]): Core {
  return {
    meshOp: (request: string) => {
      seen.push(request);
      return opJson(answer);
    },
  } as unknown as Core;
}

describe("redo panel re-run", () => {
  it("keeps op, object and selection and swaps only the params", () => {
    const params = { translate: [0, 4, 0] };
    expect(JSON.parse(rerunRequest(LAST, params))).toEqual({ op: "translate", object: "abc", sel: { mode: "vertex", ids: [0, 1] }, params });
  });

  it("re-runs the last op against the core with the edited params", () => {
    const answer: MeshOpResult = { selection: { mode: "vertex", ids: [0, 1] }, report: { verts: 8, edges: 18, faces: 12, warnings: [] } };
    const seen: string[] = [];
    const out = rerunMeshOp(fakeCore(answer, seen), LAST, { translate: [0, 4, 0] });
    expect(out.result).toEqual(answer);
    expect(JSON.parse(out.commandJson)).toEqual({ op: "batch", label: "mesh", commands: [] });
    expect(seen).toHaveLength(1);
    expect(JSON.parse(seen[0] as string)).toEqual({
      op: "translate",
      object: "abc",
      sel: { mode: "vertex", ids: [0, 1] },
      params: { translate: [0, 4, 0] },
    });
  });

  it("surfaces a core refusal instead of a stale panel", () => {
    const core = { meshOp: () => JSON.stringify({ nope: true }) } as unknown as Core;
    expect(() => rerunMeshOp(core, LAST, {})).toThrow();
  });

  it("rejects a malformed op result without touching the document", () => {
    expect(() => parseOpResult("not json")).toThrow();
    expect(() => parseOpResult(JSON.stringify({ selection: { mode: "corner", ids: [] }, report: {} }))).toThrow();
  });
});
