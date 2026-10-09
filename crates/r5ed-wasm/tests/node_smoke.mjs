// Smoke test of the built wasm core, run under node.
//
//   node crates/r5ed-wasm/tests/node_smoke.mjs
//
// R5ED_WASM names the module to load. The nodejs variant of the package is
// generated into a scratch directory so the test never disturbs the browser
// build under web/public/pkg.
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const MATERIAL = "world/dev/dev_gray_512";
const recipe = {
  nodes: [
    {
      id: "hills",
      node: "generator",
      generator: {
        kind: "rolling_hills",
        base: 64,
        amplitude: 320,
        seed: 1,
        shape: {
          fractal: { octaves: 5, frequency: 1.1, lacunarity: 2, gain: 0.45, offset: 1, weight: 1 },
          kind: "fbm",
          anisotropy: 0,
          rotation_deg: 0,
          warp: 0,
          warp_seed: 0,
        },
      },
    },
  ],
  output: "hills",
};

function fail(message) {
  console.error("node_smoke: " + message);
  process.exit(1);
}

function check(condition, message) {
  if (!condition) fail(message);
}

function require(condition, message) {
  if (!condition) fail(message);
}

const wasm = resolve(process.env.R5ED_WASM ?? "");
if (!process.env.R5ED_WASM || !existsSync(wasm)) {
  fail("R5ED_WASM must name the built r5ed_wasm.wasm");
}

const scratch = mkdtempSync(join(tmpdir(), "r5ed-node-smoke-"));
try {
  const bindgen = spawnSync("wasm-bindgen", ["--target", "nodejs", "--out-dir", scratch, wasm], {
    encoding: "utf8",
  });
  if (bindgen.status !== 0) {
    fail("wasm-bindgen failed: " + (bindgen.stderr || bindgen.stdout || "").slice(-800));
  }
  const load = createRequire(import.meta.url);
  const wasmModule = load(join(scratch, "r5ed_wasm.js"));

  const core = new wasmModule.Core();
  check(typeof core.apply === "function", "the package exports a Core");
  check(typeof core.pick === "function", "the Core carries the whole api");

  // A box room: floor, ceiling and two walls, so the room is sealed.
  const room = [
    [[-256, -256, -32], [256, 256, 0]],
    [[-256, -256, 256], [256, 256, 288]],
    [[-288, -288, -32], [-256, 288, 288]],
    [[256, -288, -32], [288, 288, 288]],
  ];
  for (const [min, max] of room) {
    core.apply(core.brushBox(min, max, MATERIAL));
  }
  const afterRoom = core.objectIds().length;
  check(afterRoom === room.length, `the room holds ${room.length} brushes, got ${afterRoom}`);

  // Undo takes the last brush away and redo puts it back, which is the whole
  // history contract the editor depends on.
  const undone = core.undo();
  check(core.objectIds().length === room.length - 1, "undo removed the last brush");
  check(core.canUndo(), "there is still something to undo");
  check(core.canRedo(), "the redo is waiting");
  const redone = core.redo();
  check(core.objectIds().length === room.length, "redo put the brush back");
  check(Array.isArray(undone) && Array.isArray(redone), "undo and redo report the ids they touched");

  // A light, so the bake has something to solve and the compile a scene.
  const document = JSON.parse(core.save());
  core.apply(
    JSON.stringify({
      op: "add_objects",
      objects: [
        {
          id: "00000000-0000-4000-8000-000000000001",
          name: "sun",
          layer: document.layers[0].id,
          parent: null,
          transform: { position: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
          hidden: false,
          locked: false,
          kind: {
            type: "light",
            kind: "sun",
            color: [1, 0.95, 0.9],
            intensity: 1,
            range: 0,
            inner_angle: 0,
            outer_angle: 0,
            size: [0, 0],
            cast_shadows: true,
            baked: true,
            realtime: false,
            keyvalues: {},
          },
        },
      ],
    }),
  );
  const saved = JSON.parse(core.save());

  // Terrain: the recipe evaluates into a heightfield of the requested size.
  const heights = core.terrainGenerate(
    JSON.stringify({ recipe, resolution: [32, 32], cell_size: 64 }),
  );
  check(heights.length === 32 * 32, `the heightfield is 32x32, got ${heights.length}`);
  let varied = false;
  for (let i = 1; i < heights.length; i += 1) {
    if (heights[i] !== heights[0]) {
      varied = true;
      break;
    }
  }
  check(varied, "the recipe produced a heightfield that is not flat");

  // The compile, in the browser, with no install and no asset tables.
  const result = core.compile(JSON.stringify(["client", "dedi"]), JSON.stringify({}));
  check(result.ok, "the compile succeeded: " + JSON.stringify(result.report.errors));
  check(Array.isArray(result.report.warnings), "the report carries warnings");
  check(result.report.stats.triangles > 0, "the room is in the map: " + JSON.stringify(result.report.stats));
  check(Object.keys(result.report.timings_ms).length > 0, "the report carries stage timings");

  for (const [target, version] of [
    ["client", 51],
    ["dedi", 47],
  ]) {
    const files = result[target];
    require(files, `the ${target} half is missing`);
    const stem = saved.meta.stem;
    const header = files[`maps/${stem}.bsp`];
    require(header, `the ${target} map has no header file: ${Object.keys(files).join(", ")}`);
    check(header.length > 64, `the ${target} header is ${header.length} bytes`);
    check(
      String.fromCharCode(header[0], header[1], header[2], header[3]) === "rBSP",
      `the ${target} header is an rBSP container`,
    );
    const got = header[4] | (header[5] << 8);
    check(got === version, `the ${target} map is version ${got}, not ${version}`);
    const lumps = Object.keys(files).filter((n) => n.includes(".bsp_lump"));
    check(lumps.length > 4, `the ${target} map carries ${lumps.length} lumps`);
    const ents = Object.keys(files).filter((n) => n.endsWith(".ent"));
    check(ents.length > 0, `the ${target} map carries ${ents.length} entity partitions`);
  }

  // The lightmap preview is what the viewport lights itself with.
  const previews = core.bakePreview(JSON.stringify({}));
  const ids = Object.keys(previews);
  check(ids.length > 0, "the preview bake produced images");
  const first = previews[ids[0]];
  check(first.width > 0 && first.height > 0, "a preview image has an extent");
  check(first.rgba.length === first.width * first.height * 4, "a preview image is RGBA8");

  // A bad command is an error, not a panic and not a silent no-op.
  let threw = false;
  try {
    core.apply("{ not a command }");
  } catch {
    threw = true;
  }
  check(threw, "a malformed command is refused");

  console.log("node_smoke: ok, " + ids.length + " preview images, " + result.report.stats.triangles + " triangles");
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
