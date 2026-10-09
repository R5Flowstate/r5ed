// Native == wasm identical-bytes gate for the mesh edit core.
//
// Replays the exact op script the native CLI ran to produce
// testdata/mesh_parity.json (editable box -> translate -> extrude along +Z)
// through the real wasm core and asserts byte-identical .r5mesh files plus
// identical selections and reports. Regenerate the goldens with the native
// CLI after any intentional format change; a mismatch otherwise means the
// builds diverged.
//
// Skipped when web/public/pkg is not built (run scripts/build_wasm.py first).
import { beforeAll, describe, expect, it } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const pkgDir = join(here, "..", "..", "public", "pkg");
const gluePath = join(pkgDir, "r5ed_wasm.js");
const wasmPath = join(pkgDir, "r5ed_wasm_bg.wasm");
const goldenPath = join(here, "testdata", "mesh_parity.json");

const hasPkg = existsSync(gluePath) && existsSync(wasmPath);
const describeWithPkg = hasPkg ? describe : describe.skip;

function bytesOf(core, id) {
  const object = JSON.parse(core.object(id));
  const file = core.getFile(object.kind.source);
  if (!file) throw new Error(`wasm core has no file for ${id}`);
  return Buffer.from(file).toString("base64");
}

describeWithPkg("wasm mesh core produces the native bytes", () => {
  let core;
  let id;
  let goldens;

  beforeAll(async () => {
    const glue = await import(gluePath);
    await glue.default(readFileSync(wasmPath));
    goldens = JSON.parse(readFileSync(goldenPath, "utf8")).steps;
    core = new glue.Core();
    const created = JSON.parse(core.meshCreate(JSON.stringify({ spec: { shape: "box", size: [64, 64, 64] }, editable: true })));
    core.apply(JSON.stringify(created));
    id = core.objectIds()[0];
  });

  it("makes the editable box byte-identical", () => {
    expect(bytesOf(core, id)).toBe(goldens[0].r5mesh_base64);
  });

  it("translates to identical bytes, selection and report", () => {
    const out = JSON.parse(
      core.meshOp(JSON.stringify({ op: "translate", object: id, sel: { mode: "face", ids: [] }, params: { translate: [16.0, 0.0, 8.0] } })),
    );
    core.apply(JSON.stringify(out.command));
    expect(bytesOf(core, id)).toBe(goldens[1].r5mesh_base64);
    expect(out.selection).toEqual({ mode: "face", ids: [0, 1, 2, 3, 4, 5] });
    expect(out.report).toEqual({ verts: 8, edges: 12, faces: 6, warnings: [] });
  });

  it("extrudes to identical bytes, selection and report", () => {
    const out = JSON.parse(
      core.meshOp(
        JSON.stringify({
          op: "extrude_region",
          object: id,
          sel: { mode: "face", query: { kind: "normal", dir: [0, 0, 1], max_angle_deg: 20.0 } },
          params: { distance: 16.0 },
        }),
      ),
    );
    core.apply(JSON.stringify(out.command));
    expect(bytesOf(core, id)).toBe(goldens[2].r5mesh_base64);
    expect(out.selection).toEqual({ mode: "face", ids: [5, 6, 7, 8, 9] });
    expect(out.report).toEqual({ verts: 12, edges: 20, faces: 10, warnings: [] });
  });
});

if (!hasPkg) {
  it.skip("wasm pkg not built; run scripts/build_wasm.py for the parity proof", () => {});
}
