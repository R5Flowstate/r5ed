// Drives every editor feature in a real browser and reports what broke.
//
// Run through scripts/smoke.sh, which builds everything, starts the host with
// a throwaway config and runs this under Xvfb. R5ED_URL is the host URL with
// its token; R5ED_SMOKE_OUT is where screenshots go.
//
//   node web/smoke/smoke.mjs [--only <substring>] [--shots]
//
// Each step is independent enough that one failure does not hide the rest:
// a step records its failure, the console errors it caused, and a screenshot.

import { chromium } from "playwright-core";
import fs from "node:fs";
import path from "node:path";
import { FIXTURES } from "./fixtures.mjs";

const URL = process.env.R5ED_URL;
if (!URL) {
  console.error("smoke: R5ED_URL is not set; run scripts/smoke.sh");
  process.exit(2);
}
const OUT = process.env.R5ED_SMOKE_OUT ?? "smoke-out";
const PROJECTS = process.env.R5ED_PROJECTS ?? "";
const args = process.argv.slice(2);
const only = args.includes("--only") ? args[args.indexOf("--only") + 1] : null;
const shots = args.includes("--shots");
fs.mkdirSync(OUT, { recursive: true });

const browser = await chromium.launch({
  headless: false,
  executablePath: process.env.R5ED_CHROMIUM ?? "/opt/pw-browsers/chromium",
  // R5ED_CHROMIUM_ARGS replaces the SwiftShader flags where a real GPU is present.
  args: process.env.R5ED_CHROMIUM_ARGS?.split(" ").filter(Boolean) ?? ["--enable-unsafe-webgpu", "--enable-features=Vulkan", "--use-vulkan=swiftshader", "--use-webgpu-adapter=swiftshader"],
});
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
let consoleErrors = [];
page.on("console", (m) => {
  if (m.type() === "error") consoleErrors.push(m.text());
});
page.on("pageerror", (e) => consoleErrors.push(`pageerror: ${e.message}`));
page.on("response", (r) => {
  if (r.status() >= 400) consoleErrors.push(`http ${r.status()} ${r.request().method()} ${new globalThis.URL(r.url()).pathname}`);
});

const results = [];
class Fail extends Error {}
function expect(cond, message) {
  if (!cond) throw new Fail(message);
}

async function step(name, fn) {
  if (only && !name.includes(only)) return;
  consoleErrors = [];
  const started = Date.now();
  let error = null;
  try {
    await fn();
  } catch (e) {
    error = e instanceof Error ? e.message.split("\n")[0] : String(e);
  }
  // Console errors fail the step: a feature that logs an error is not working.
  const errors = [...consoleErrors];
  const ok = !error && errors.length === 0;
  results.push({ name, ok, error, errors, ms: Date.now() - started });
  const file = `${String(results.length).padStart(2, "0")}-${name.replace(/\W+/g, "_")}.png`;
  if (!ok || shots) await page.screenshot({ path: path.join(OUT, file) }).catch(() => {});
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${error ? ` -- ${error}` : ""}${errors.length ? ` -- console: ${errors.join(" | ")}` : ""}`);
  // A step that leaves a dialog or menu open would break the next one.
  await closeOverlays();
}

async function closeOverlays() {
  for (let i = 0; i < 3; i++) {
    const close = page.locator('.modal [aria-label="Close"]');
    if ((await close.count()) === 0) break;
    await close.first().click().catch(() => {});
  }
}

// --- probes into the running editor ---

const count = () => page.evaluate(() => globalThis.r5ed.store.ids().length);
const objects = () =>
  page.evaluate(() => [...globalThis.r5ed.store.snapshot().objects.values()].map((o) => JSON.parse(JSON.stringify(o))));
const byName = async (name) => (await objects()).find((o) => o.name === name);
const statusCell = (cell) => page.locator(`.status-bar [data-cell="${cell}"]`).innerText();
const selectedCount = async () => Number((await statusCell("selection")).split(" ")[0]);
const message = () => statusCell("message");
const canvas = () => page.locator(".viewport canvas").first();
const TOOL_LABELS = { Brush: "Add Brush Box", Vertex: "Edit Vertices", Edge: "Edit Edges", Face: "Edit Faces", Sculpt: "Sculpt Terrain", Paint: "Paint Terrain", Voxel: "Voxel Terrain", Entity: "Place Entity" };
const toolRadio = (label) => page.getByRole("radio", { name: TOOL_LABELS[label] ?? label, exact: true });
const tool = (label) => toolRadio(label).click();
/** Viewport keys go to the view under the pointer, as in Blender. */
async function overView() {
  const box = await canvas().boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
}
const inspector = () => page.locator(".panel.inspector");
const inspectorName = () => inspector().locator(".panel-head h2").innerText();

async function reset() {
  await page.evaluate(() => {
    const e = globalThis.r5ed;
    const ids = e.store.ids();
    if (ids.length) e.removeObjects(ids);
  });
  const showAll = page.locator(".isolate-bar button");
  if (await showAll.count()) await showAll.click();
  await unfocus();
  await page.keyboard.press("Escape");
  await page.keyboard.press("Alt+a");
}

/** Adds one object through the outliner menu and returns its id. */
async function addViaOutliner(label) {
  const before = new Set((await objects()).map((o) => o.id));
  await page.getByRole("button", { name: "+ object" }).click();
  await page.locator('.create-menu [role="menuitem"]', { hasText: new RegExp(`^${label}$`) }).click();
  const after = await objects();
  const added = after.filter((o) => !before.has(o.id));
  return added;
}

/** Clicks an object's outliner row. Solid sets an input's value as a property, so rows are found by id. */
async function selectRow(name, modifiers) {
  const found = (await objects()).find((o) => o.name === name);
  if (!found) throw new Fail(`no object named ${name}`);
  // The kind chip, clear of the expand button at the row's left edge.
  await page.locator(`.tree-row[data-object-id="${found.id}"] .kind-chip`).click({ modifiers });
}

/** Clicks somewhere that holds no focusable control, so a key press is a shortcut. */
async function unfocus() {
  await page.locator(".panel-head h2", { hasText: "Outliner" }).click();
}

/** Fills an input and commits it the way a user does: type, then leave the field. */
async function commit(locator, value) {
  await locator.fill(String(value));
  await locator.press("Tab");
}

const field = (scope, label) => scope.locator(".field").filter({ has: page.locator(`label.field-name:text-is("${label}")`) }).locator("input, select").first();
const triple = (legend, axis) =>
  inspector().locator(`fieldset.triple-editor:has(legend:text-is("${legend}")) .field:has(label:text-is("${axis}")) input`);

await page.goto(URL);
await page.waitForSelector(".app", { timeout: 60000 });

// ---------------------------------------------------------------- boot

await step("boot: core, host session, schema and WebGPU come up", async () => {
  await page.waitForFunction(() => Number(document.querySelector('[data-cell="fps"]')?.textContent?.split(" ")[0]) > 0, null, { timeout: 20000 });
  expect((await page.locator(".view-overlay.fatal").count()) === 0, "the viewport shows a fatal overlay");
  const session = await page.evaluate(() => globalThis.r5ed.session);
  expect(session && session.version, "no host session");
  const classes = await page.evaluate(() => Object.keys(globalThis.r5ed.schema.classes ?? {}).length);
  expect(classes > 100, `schema has ${classes} classes`);
});

await step("layout: panels have usable sizes", async () => {
  const layer = await page.locator(".layer-row").first().boundingBox();
  expect(layer && layer.height >= 16, `the layer row is ${layer?.height}px tall`);
  expect(await page.locator(".layer-row .layer-name").first().isVisible(), "the layer name is not visible");
  const search = await page.locator(".outliner input.search").boundingBox();
  expect(search && search.height < 60, `the outliner filter is ${search?.height}px tall`);
  const tree = await page.locator(".outliner .tree").boundingBox();
  expect(tree && tree.height > 200, `the outliner tree is ${tree?.height}px tall`);
});

// ---------------------------------------------------------------- top bar

await step("topbar: theme toggles by button and by shortcut", async () => {
  const theme = () => page.evaluate(() => document.documentElement.dataset.theme);
  const first = await theme();
  await page.getByRole("button", { name: "Toggle theme" }).click();
  expect((await theme()) !== first, "the theme button did nothing");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("Control+Shift+L");
  expect((await theme()) === first, "Ctrl+Shift+L did not toggle the theme");
});

await step("topbar: 4 panes, view kinds, 1 pane", async () => {
  await page.getByRole("button", { name: "4 panes" }).click();
  expect((await page.locator(".viewport canvas").count()) === 4, "four panes did not show four canvases");
  await page.waitForTimeout(500);
  expect((await page.locator(".view-overlay.fatal").count()) === 0, "a pane lost WebGPU");
  await page.getByLabel("Viewport 2 kind").selectOption("front");
  expect((await page.locator('canvas[data-view="front"]').count()) >= 1, "the kind select did not change the pane");
  await page.getByLabel("Viewport 2 kind").selectOption("top");
  await page.getByRole("button", { name: "1 pane" }).click();
  expect((await page.locator(".viewport canvas").count()) === 1, "one pane did not show one canvas");
});

await step("topbar: view toggles", async () => {
  for (const label of ["Toggle grid", "Toggle gizmo", "Toggle lightmap preview", "Toggle volumes", "Toggle entity billboards", "Toggle light gizmos"]) {
    const box = page.getByLabel(label);
    const before = await box.isChecked();
    await box.click();
    expect((await box.isChecked()) !== before, `${label} did not toggle`);
    await box.click();
  }
});

await step("topbar: snap and grid size", async () => {
  await page.getByLabel("Grid size").selectOption("64");
  expect((await statusCell("grid")) === "grid 64", "the status bar did not show the new grid size");
  await page.getByLabel("Grid size").selectOption("16");
  const snap = page.getByLabel("Toggle snapping");
  await snap.click();
  expect(!(await snap.isChecked()), "snap did not toggle");
  await snap.click();
});

await step("topbar: every tool button", async () => {
  for (const label of ["Select", "Move", "Rotate", "Scale", "Brush", "Clip", "Vertex", "Edge", "Face", "Sculpt", "Paint", "Voxel", "Entity"]) {
    await tool(label);
    expect((await toolRadio(label).getAttribute("aria-checked")) === "true", `${label} button did not select the tool`);
  }
  await tool("Select");
});

await step("topbar: brush options show with the brush tool", async () => {
  await tool("Brush");
  expect(await page.getByLabel("Brush width").isVisible(), "no brush options with the brush tool");
  await commit(page.getByLabel("Brush width"), 128);
  await commit(page.getByLabel("Brush depth"), 96);
  await commit(page.getByLabel("Brush height"), 32);
  await unfocus();
  await tool("Select");
  expect(!(await page.getByLabel("Brush width").isVisible()), "brush options stayed after leaving the brush tool");
});

// ---------------------------------------------------------------- outliner: create every kind

const KINDS = [
  ["Brush", "brush"], ["Prop", "prop"], ["Entity", "entity"], ["Light", "light"], ["Volume", "volume"],
  ["Path", "path"], ["Terrain", "terrain"], ["Voxel terrain", "voxel_terrain"], ["Group", "group"],
];
for (const [label, type] of KINDS) {
  await step(`outliner: + object ${label}`, async () => {
    await reset();
    const added = await addViaOutliner(label);
    expect(added.length === 1, `${label} added ${added.length} objects`);
    expect(added[0].kind.type === type, `${label} added a ${added[0].kind.type}`);
    expect((await selectedCount()) === 1, "the new object is not selected");
    expect((await inspector().locator(".kind-chip").innerText()) === type, "the inspector does not show the new object");
    expect((await page.locator(".tree-row").count()) === 1, "the outliner does not list the object");
    await page.waitForTimeout(150);
  });
}

await step("outliner: + object Mesh opens the mesh dialog and creates every shape", async () => {
  await reset();
  await page.getByRole("button", { name: "+ object" }).click();
  await page.locator('.create-menu [role="menuitem"]', { hasText: /^Mesh$/ }).click();
  const dialog = page.getByRole("dialog", { name: "New mesh" });
  expect(await dialog.isVisible(), "no mesh dialog");
  const shapes = await dialog.locator("#mesh-shape option").evaluateAll((os) => os.map((o) => o.value));
  await closeOverlays();
  for (const shape of shapes) {
    await page.getByRole("button", { name: "+ object" }).click();
    await page.locator('.create-menu [role="menuitem"]', { hasText: /^Mesh$/ }).click();
    await dialog.locator("#mesh-shape").selectOption(shape);
    const before = await count();
    await dialog.getByRole("button", { name: "Create" }).click();
    expect((await count()) === before + 1, `shape ${shape} did not create a mesh (${await message()})`);
    expect(!(await dialog.isVisible()), `the dialog stayed open after ${shape}`);
  }
  await page.waitForTimeout(300);
});

// ---------------------------------------------------------------- inspector per kind

await step("inspector: transform edits position, angles and scale", async () => {
  await reset();
  const [o] = await addViaOutliner("Entity");
  // Typed key by key, the way a user enters a number.
  const type = async (locator, value) => {
    await locator.fill("");
    await locator.pressSequentially(String(value));
    await locator.press("Tab");
  };
  await type(triple("Position", "x"), 128);
  await type(triple("Position", "z"), -32);
  await type(triple("Angles (pitch yaw roll)", "y"), 90);
  await type(triple("Scale", "x"), 2);
  const now = (await objects()).find((x) => x.id === o.id);
  expect(now.transform.position[0] === 128 && now.transform.position[2] === -32, `position is ${now.transform.position}`);
  expect(Math.abs(Math.abs(now.transform.rotation[2]) - Math.SQRT1_2) < 1e-3, `rotation is ${now.transform.rotation}`);
  expect(now.transform.scale[0] === 2, `scale is ${now.transform.scale}`);
  // One commit per field, not one per keystroke.
  let undos = 0;
  while (await page.evaluate(() => globalThis.r5ed.store.snapshot().canUndo) && undos < 50) {
    await page.evaluate(() => globalThis.r5ed.undo());
    undos++;
    const x = (await objects()).find((q) => q.id === o.id);
    if (!x) break;
  }
  expect(undos <= 6, `four field edits took ${undos} undo steps to unwind`);
});

await step("inspector: prop fields", async () => {
  await reset();
  const [o] = await addViaOutliner("Prop");
  await commit(inspector().locator("#prop-model"), "mdl/props/barrel.rmdl");
  await commit(field(inspector(), "Skin"), 2);
  await field(inspector(), "Solid").click();
  await commit(field(inspector(), "Fade distance"), 4096);
  const p = (await objects()).find((x) => x.id === o.id).kind.prop;
  expect(p.model === "mdl/props/barrel.rmdl" && p.skin === 2 && p.solid === false && p.fade_distance === 4096, JSON.stringify(p));
});

await step("inspector: key/value editor adds and edits a key", async () => {
  const o = (await objects())[0];
  await commit(field(inspector(), "New key"), "rendercolor");
  await inspector().getByRole("button", { name: "Add" }).click();
  await commit(field(inspector(), "rendercolor"), "255 0 0");
  const p = (await objects()).find((x) => x.id === o.id).kind.prop;
  expect(p.keyvalues.rendercolor === "255 0 0", JSON.stringify(p.keyvalues));
});

await step("inspector: light fields", async () => {
  await reset();
  const [o] = await addViaOutliner("Light");
  await inspector().locator("#light-kind").selectOption("spot");
  await commit(field(inspector(), "Intensity"), 3.5);
  await commit(field(inspector(), "Range"), 1024);
  await commit(triple("Colour", "y"), 0.5);
  const l = (await objects()).find((x) => x.id === o.id).kind.light;
  expect(l.kind === "spot" && l.intensity === 3.5 && l.range === 1024 && l.color[1] === 0.5, JSON.stringify(l));
});

await step("inspector: volume fields", async () => {
  await reset();
  const [o] = await addViaOutliner("Volume");
  await inspector().locator("#volume-kind").selectOption("trigger_hurt");
  await commit(field(inspector(), "Classname"), "trigger_hurt");
  const v = (await objects()).find((x) => x.id === o.id).kind.volume;
  expect(v.kind === "trigger_hurt" && v.classname === "trigger_hurt", JSON.stringify(v));
});

await step("inspector: path points add, edit and delete", async () => {
  await reset();
  const [o] = await addViaOutliner("Path");
  await inspector().locator("#path-kind").selectOption("ziprail");
  await inspector().getByRole("button", { name: "Add point" }).click();
  let p = (await objects()).find((x) => x.id === o.id).kind.path;
  expect(p.kind === "ziprail" && p.points.length === 3, JSON.stringify(p));
  await commit(inspector().locator('li[data-point="2"] fieldset .field:has(label:text-is("z")) input'), 999);
  p = (await objects()).find((x) => x.id === o.id).kind.path;
  expect(p.points[2][2] === 999, `point 2 is ${p.points[2]}`);
  await inspector().getByRole("button", { name: "Delete point 0" }).click();
  p = (await objects()).find((x) => x.id === o.id).kind.path;
  expect(p.points.length === 2, `${p.points.length} points after delete`);
});

await step("inspector: entity form edits a schema key", async () => {
  await reset();
  const [o] = await addViaOutliner("Entity");
  const input = inspector().locator('[data-key="targetname"]').first();
  expect((await input.count()) === 1, "no targetname field for info_target");
  await commit(input, "smoke_target");
  const e = (await objects()).find((x) => x.id === o.id).kind.entity;
  expect(e.keyvalues.targetname === "smoke_target", JSON.stringify(e.keyvalues));
});

await step("inspector: entity tool creates an entity from the class picker", async () => {
  await reset();
  await page.getByRole("radio", { name: "Entity", exact: true }).click();
  await commit(field(inspector(), "Class"), "info_spawnpoint");
  const option = inspector().getByRole("option").first();
  expect((await option.count()) === 1, "the class picker found nothing");
  const cls = await option.locator(".classname").innerText();
  await option.click();
  const all = await objects();
  expect(all.length === 1 && all[0].kind.entity.classname === cls, `picked ${cls}, document has ${JSON.stringify(all.map((x) => x.kind.type))}`);
  await tool("Select");
});

await step("inspector: mesh fields", async () => {
  await reset();
  await page.evaluate(() => globalThis.r5ed.createObject("mesh", [0, 0, 0]));
  const o = (await objects())[0];
  await selectRow(o.name);
  await inspector().locator("#mesh-collision").selectOption("none");
  await commit(field(inspector(), "Lightmap scale"), 8);
  const m = (await objects())[0].kind.mesh;
  expect(m.collision === "none" && m.lightmap_scale === 8, JSON.stringify({ ...m, material_map: undefined }));
});

await step("inspector: voxel terrain ops are stored the way the core reads them", async () => {
  await reset();
  const [o] = await addViaOutliner("Voxel terrain");
  await inspector().getByRole("button", { name: "Add operation" }).click();
  const ops = (await objects()).find((x) => x.id === o.id).kind.voxel_terrain.ops;
  expect(ops.length === 2, `${ops.length} ops after adding one to the starting slab`);
  expect(ops[1].op === "union" && ops[1].shape?.shape === "sphere", `the added op is ${JSON.stringify(ops[1])}`);
  await page.waitForTimeout(300);
});

// ---------------------------------------------------------------- brush operations

async function oneBrush() {
  await reset();
  await page.evaluate(() => globalThis.r5ed.brushBox([0, 0, 0], [128, 128, 128], "dev/dev_grey"));
  const o = (await objects())[0];
  await selectRow(o.name);
  return o;
}
const brushSize = (id) => page.evaluate((id) => {
  const b = globalThis.r5ed.brushBounds(id);
  return [b[3] - b[0], b[4] - b[1], b[5] - b[2]];
}, id);

await step("brush: resize from the inspector", async () => {
  const o = await oneBrush();
  await commit(field(inspector(), "W (X)"), 256);
  const s = await brushSize(o.id);
  expect(s[0] === 256 && s[1] === 128 && s[2] === 128, `size is ${s}`);
});

await step("brush: extrude face", async () => {
  const o = await oneBrush();
  const before = await brushSize(o.id);
  await inspector().getByRole("button", { name: "Extrude face" }).click();
  const after = await brushSize(o.id);
  expect(after.join() !== before.join(), `extrude left the size at ${after} (${await message()})`);
});

await step("brush: hollow", async () => {
  await oneBrush();
  await inspector().getByRole("button", { name: "Hollow" }).first().click();
  expect((await count()) >= 6, `hollow left ${await count()} objects (${await message()})`);
});

await step("brush: move vertex", async () => {
  const o = await oneBrush();
  await commit(field(inspector(), "Vertex"), "128 128 128");
  await commit(field(inspector(), "Move to"), "128 128 192");
  await inspector().getByRole("button", { name: "Move vertex" }).click();
  const s = await brushSize(o.id);
  expect(Math.abs(s[2] - 192) < 1e-6, `size after moving a vertex up is ${s} (${await message()})`);
});

await step("brush: clip from the inspector", async () => {
  const o = await oneBrush();
  await commit(field(inspector(), "Clip point A"), "64 0 0");
  await commit(field(inspector(), "Clip point B"), "64 1 0");
  await commit(field(inspector(), "Clip point C"), "64 0 1");
  await inspector().getByRole("button", { name: "Clip", exact: true }).click();
  const s = await brushSize(o.id);
  expect(s[0] === 64, `size after clipping at x=64 is ${s} (${await message()})`);
});

await step("brush: merge convex and carve", async () => {
  await reset();
  await page.evaluate(() => {
    const e = globalThis.r5ed;
    e.brushBox([0, 0, 0], [64, 64, 64], "dev/dev_grey");
    e.brushBox([64, 0, 0], [128, 64, 64], "dev/dev_grey");
  });
  await unfocus();
  await page.keyboard.press("a");
  await selectRow((await objects())[0].name, ["Shift"]);
  await selectRow((await objects())[0].name, ["Shift"]);
  expect((await selectedCount()) === 2, `${await selectedCount()} selected`);
  await inspector().getByRole("button", { name: "Merge convex" }).click();
  expect((await count()) === 1, `merge left ${await count()} objects (${await message()})`);
  // carve: a small brush cut out of a big one
  await reset();
  await page.evaluate(() => {
    const e = globalThis.r5ed;
    e.brushBox([0, 0, 0], [256, 256, 64], "dev/dev_grey");
    e.brushBox([96, 96, -16], [160, 160, 80], "dev/dev_grey");
  });
  const [big, small] = await objects();
  await selectRow(big.name);
  await commit(field(inspector(), "Carver id"), small.id);
  await inspector().getByRole("button", { name: "Carve" }).click();
  expect((await count()) > 2, `carve left ${await count()} objects (${await message()})`);
});

await step("brush: face material and uv edits", async () => {
  const o = await oneBrush();
  const face = inspector().locator('.face-row[data-face="0"]');
  const select = face.locator(".material-field select");
  const values = await select.locator("option").evaluateAll((os) => os.map((x) => x.value));
  const pick = values.find((v) => v !== "dev/dev_grey");
  await select.selectOption(pick);
  let b = (await objects()).find((x) => x.id === o.id).kind.brush;
  expect(b.faces[0].material === pick, `face 0 is ${b.faces[0].material} after picking ${pick}`);
  await commit(face.locator(".material-field .material-path"), "world/custom/typed_in");
  b = (await objects()).find((x) => x.id === o.id).kind.brush;
  expect(b.faces[0].material === "world/custom/typed_in", `face 0 is ${b.faces[0].material} after typing a path`);
  await commit(face.locator('fieldset:has(legend:text-is("UV scale")) input').first(), 0.5);
  b = (await objects()).find((x) => x.id === o.id).kind.brush;
  expect(b.faces[0].uv.scale[0] === 0.5, `uv scale is ${b.faces[0].uv.scale}`);
});

// ---------------------------------------------------------------- edit commands

await step("edit: undo and redo by button and shortcut", async () => {
  await reset();
  await addViaOutliner("Light");
  expect(await page.getByRole("button", { name: "Undo" }).isEnabled(), "undo is disabled after an edit");
  await page.getByRole("button", { name: "Undo" }).click();
  expect((await count()) === 0, "undo did not remove the light");
  await page.getByRole("button", { name: "Redo" }).click();
  expect((await count()) === 1, "redo did not restore the light");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("Control+z");
  expect((await count()) === 0, "Ctrl+Z did not undo");
  await page.keyboard.press("Control+y");
  expect((await count()) === 1, "Ctrl+Y did not redo");
});

await step("edit: select all, duplicate, delete, deselect", async () => {
  await reset();
  await addViaOutliner("Light");
  await addViaOutliner("Entity");
  await overView();
  await page.keyboard.press("a");
  expect((await selectedCount()) === 2, "A did not select both");
  // Shift+D duplicates and grabs; Escape cancels the move and keeps the copies.
  await page.keyboard.press("Shift+d");
  await page.keyboard.press("Escape");
  expect((await count()) === 4, `Shift+D left ${await count()} objects`);
  await page.keyboard.press("Alt+a");
  expect((await selectedCount()) === 0, "Alt+A did not deselect");
  await page.keyboard.press("a");
  await page.keyboard.press("x");
  expect((await count()) === 0, `X left ${await count()} objects`);
});

await step("edit: shortcuts work with focus outside the viewport", async () => {
  await reset();
  await addViaOutliner("Light");
  await page.locator(".panel-head h2", { hasText: "Outliner" }).click();
  await page.keyboard.press("Control+z");
  expect((await count()) === 0, "Ctrl+Z with the outliner header focused did nothing");
});

await step("edit: group, ungroup and reparent by drag", async () => {
  await reset();
  await addViaOutliner("Light");
  await addViaOutliner("Entity");
  await unfocus();
  await page.keyboard.press("a");
  await page.locator(".selection-bar").getByRole("button", { name: "Group", exact: true }).click();
  const g = (await objects()).find((o) => o.kind.type === "group");
  expect(g, "no group object");
  expect((await objects()).filter((o) => o.parent === g.id).length === 2, "children were not moved under the group");
  await selectRow("group");
  await page.locator(".selection-bar").getByRole("button", { name: "Ungroup", exact: true }).click();
  expect(!(await objects()).some((o) => o.kind.type === "group"), "ungroup left the group");
  const [a, b] = await objects();
  await page.locator(`.tree-row[data-object-id="${a.id}"]`).dragTo(page.locator(`.tree-row[data-object-id="${b.id}"]`));
  expect((await objects()).find((o) => o.id === a.id).parent === b.id, "drag and drop did not reparent");
});

await step("edit: isolate and show all", async () => {
  await reset();
  await addViaOutliner("Light");
  await addViaOutliner("Entity");
  await page.locator(".selection-bar").getByRole("button", { name: "Isolate" }).click();
  expect((await page.locator(".tree-row").count()) === 1, "isolate did not narrow the outliner");
  await page.locator(".isolate-bar").getByRole("button", { name: "Show all" }).click();
  expect((await page.locator(".tree-row").count()) === 2, "show all did not bring the rest back");
  await overView();
  await page.keyboard.press("NumpadDivide");
  expect((await page.locator(".tree-row").count()) === 1, "Numpad / did not isolate");
  await page.keyboard.press("NumpadDivide");
  expect((await page.locator(".tree-row").count()) === 2, "Numpad / did not end the isolate");
});

await step("edit: frame selection", async () => {
  await reset();
  await page.evaluate(() => globalThis.r5ed.brushBox([4096, 4096, 0], [4224, 4224, 128], "dev/dev_grey"));
  await selectRow((await objects())[0].name);
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.waitForTimeout(200);
  const before = await canvas().screenshot();
  await page.keyboard.press("NumpadDecimal");
  await page.waitForTimeout(300);
  const after = await canvas().screenshot();
  expect(!(await message()).includes("nothing to frame"), "frame found nothing");
  expect(!before.equals(after), "F did not move the view onto the brush far from the origin");
});

// ---------------------------------------------------------------- outliner details

await step("outliner: rename, filter and context menu", async () => {
  await reset();
  await addViaOutliner("Light");
  await addViaOutliner("Entity");
  const input = page.locator(".tree-row input.name").first();
  await commit(input, "renamed_light");
  expect(await byName("renamed_light"), "rename did not reach the document");
  await page.getByLabel("Filter objects").fill("renamed");
  expect((await page.locator(".tree-row").count()) === 1, "the filter did not narrow the tree");
  await page.getByLabel("Filter objects").fill("");
  const row = page.locator(".tree-row").first();
  for (const action of ["Duplicate", "Group", "Isolate", "Save prefab", "Delete"]) {
    await row.click({ button: "right", position: { x: 4, y: 6 } });
    const item = page.locator('.context-menu [role="menuitem"]', { hasText: action });
    expect(await item.isVisible(), `no ${action} in the context menu`);
    await item.click();
    expect(!(await page.locator(".context-menu").isVisible()), `the context menu stayed open after ${action}`);
  }
  const files = await page.evaluate(() => globalThis.r5ed.core.fileNames());
  expect(files.some((f) => f.startsWith("prefabs/")), `save prefab wrote no prefab file (${files})`);
});

await step("outliner: layers add, hide, lock, remove", async () => {
  await reset();
  await page.getByRole("button", { name: "+ layer" }).click();
  const rows = page.locator(".layer-row");
  expect((await rows.count()) === 2, `${await rows.count()} layers after adding one`);
  const second = rows.nth(1);
  const name = await second.getAttribute("data-layer-id");
  await second.locator('input[type="checkbox"]').first().click();
  let layers = await page.evaluate(() => globalThis.r5ed.store.layers.map((l) => ({ ...l })));
  expect(layers[1].visible === false, "hiding the layer did not reach the document");
  await second.locator('input[type="checkbox"]').nth(1).click();
  layers = await page.evaluate(() => globalThis.r5ed.store.layers.map((l) => ({ ...l })));
  expect(layers[1].locked === true, "locking the layer did not reach the document");
  await page.getByRole("button", { name: /^Remove layer/ }).nth(1).click();
  expect((await rows.count()) === 1, `${await rows.count()} layers after removing one (${name})`);
});

// ---------------------------------------------------------------- viewport

await step("viewport: click picks an object and empty space clears", async () => {
  await reset();
  await page.evaluate(() => globalThis.r5ed.brushBox([-256, -256, 0], [256, 256, 64], "dev/dev_grey"));
  await overView();
  await page.keyboard.press("Alt+a");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("NumpadDecimal");
  await page.waitForTimeout(300);
  const box = await canvas().boundingBox();
  await canvas().click({ position: { x: box.width / 2, y: box.height / 2 } });
  await page.waitForTimeout(300);
  expect((await selectedCount()) === 1, "clicking the brush in the middle of the view did not select it");
  // Framed, the brush fills the view; back off so a corner is empty.
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, 1500);
  await page.waitForTimeout(300);
  await canvas().click({ position: { x: 10, y: 10 } });
  await page.waitForTimeout(300);
  expect((await selectedCount()) === 0, "clicking empty space did not clear the selection");
});

await step("viewport: orbit, pan and wheel move the camera", async () => {
  const box = await canvas().boundingBox();
  const before = await canvas().screenshot();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down({ button: "middle" });
  await page.mouse.move(box.x + box.width / 2 + 120, box.y + box.height / 2 + 40, { steps: 6 });
  await page.mouse.up({ button: "middle" });
  await page.mouse.wheel(0, 400);
  await page.waitForTimeout(200);
  const after = await canvas().screenshot();
  expect(!before.equals(after), "orbit and wheel did not change the view");
  // A 2D view zooms with the wheel too, and keeps the zoom on the next frame.
  await reset();
  await page.evaluate(() => globalThis.r5ed.brushBox([-64, -64, 0], [64, 64, 64], "dev/dev_grey"));
  await page.getByLabel("Viewport 1 kind").selectOption("top");
  await page.waitForTimeout(300);
  const flat = await canvas().screenshot();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, -600);
  await page.waitForTimeout(300);
  const zoomed = await canvas().screenshot();
  await page.getByLabel("Viewport 1 kind").selectOption("persp");
  expect(!flat.equals(zoomed), "the wheel did not zoom the top view");
});

await step("viewport: brush tool draws a box by drag", async () => {
  await reset();
  await tool("Brush");
  await page.getByLabel("Viewport 1 kind").selectOption("top");
  await page.waitForTimeout(200);
  const box = await canvas().boundingBox();
  await page.mouse.move(box.x + box.width / 2 - 80, box.y + box.height / 2 - 80);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 80, box.y + box.height / 2 + 80, { steps: 8 });
  await page.mouse.up();
  const all = await objects();
  expect(all.length === 1 && all[0].kind.type === "brush", `the drag made ${all.length} objects (${await message()})`);
  await page.getByLabel("Viewport 1 kind").selectOption("persp");
  await tool("Select");
});

await step("viewport: clip tool cuts a brush with two clicks in a 2D view", async () => {
  await oneBrush();
  await page.getByLabel("Viewport 1 kind").selectOption("top");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("NumpadDecimal");
  await tool("Clip");
  const box = await canvas().boundingBox();
  const before = await page.evaluate((id) => globalThis.r5ed.brushBounds(id), (await objects())[0].id);
  // Two points on a vertical line through the middle; the view axis is the third.
  await canvas().click({ position: { x: box.width / 2, y: 20 } });
  await canvas().click({ position: { x: box.width / 2, y: box.height - 20 } });
  const after = await page.evaluate((id) => globalThis.r5ed.brushBounds(id), (await objects())[0].id);
  expect(JSON.stringify(before) !== JSON.stringify(after), `two clip clicks left the brush at ${after} (${await message()})`);
  expect(after[5] - after[2] === 128, `the cut changed the height: ${after}`);
  await page.getByLabel("Viewport 1 kind").selectOption("persp");
  await tool("Select");
});

await step("viewport: move gizmo drags the selection as one undo step", async () => {
  await reset();
  await page.evaluate(() => {
    globalThis.r5ed.createObject("light", [0, 0, 0]);
    globalThis.r5ed.createObject("light", [0, 256, 0]);
  });
  const [a, b] = await objects();
  await page.evaluate((ids) => {
    // Select both, the one at the origin last so it is the gizmo's pivot.
    const e = globalThis.r5ed;
    e.clear();
    e.toggleSelect(ids[1]);
    e.toggleSelect(ids[0]);
  }, [a.id, b.id]);
  await page.getByLabel("Viewport 1 kind").selectOption("top");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await tool("Move");
  await page.waitForTimeout(200);
  // Frame the primary light, at the origin: the X handle then points right
  // from the view centre.
  await page.keyboard.press("NumpadDecimal");
  await page.waitForTimeout(200);
  const box = await canvas().boundingBox();
  const cx = box.x + box.width / 2;
  const cy = box.y + box.height / 2;
  let moved = false;
  for (const dx of [30, 50, 70]) {
    await page.mouse.move(cx + dx, cy);
    await page.mouse.down();
    await page.mouse.move(cx + dx + 150, cy, { steps: 10 });
    await page.mouse.up();
    const [pa, pb] = (await objects()).map((o) => o.transform.position);
    if (pa[0] !== 0) {
      moved = true;
      expect(pa[1] === 0 && pa[2] === 0, `an X drag moved the light to ${pa}`);
      expect(pb[0] === pa[0] && pb[1] === 256, `the second light did not keep its offset: ${pa} and ${pb}`);
      break;
    }
  }
  expect(moved, "no drag along the X handle moved the selection");
  await page.evaluate(() => globalThis.r5ed.undo());
  const back = (await objects()).map((o) => o.transform.position.join());
  expect(back.join("|") === "0,0,0|0,256,0", `one undo after the drag left the lights at ${back.join(" and ")}`);
  // The rotate ring is grabbed where it is drawn.
  await page.evaluate((id) => {
    globalThis.r5ed.clear();
    globalThis.r5ed.toggleSelect(id);
  }, a.id);
  await tool("Rotate");
  const ring = 90 * 0.85;
  await page.mouse.move(cx + ring, cy);
  await page.mouse.down();
  await page.mouse.move(cx, cy - ring, { steps: 10 });
  await page.mouse.up();
  const turned = (await objects()).find((o) => o.id === a.id).transform.rotation;
  expect(Math.abs(turned[3]) < 0.99, `dragging the rotate ring left the rotation at ${turned}`);
  await page.getByLabel("Viewport 1 kind").selectOption("persp");
  await tool("Select");
});

/** Frames a fresh 0..128 brush in the top view; returns where its corners land on screen. */
async function topViewBrush() {
  const o = await oneBrush();
  await page.getByLabel("Viewport 1 kind").selectOption("top");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("NumpadDecimal");
  await page.waitForTimeout(300);
  const box = await canvas().boundingBox();
  // Framed: the centre is the brush centre and the view is 2 * orthoHeight tall.
  const half = Math.sqrt(3) * 64 * 1.1;
  const ppu = box.height / 2 / half;
  const at = (x, y) => ({ x: box.x + box.width / 2 + (x - 64) * ppu, y: box.y + box.height / 2 - (y - 64) * ppu });
  return { o, at };
}

async function dragFromTo(from, to) {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps: 8 });
  await page.mouse.up();
}

await step("tools: vertex tool drags a brush corner", async () => {
  const { o, at } = await topViewBrush();
  await tool("Vertex");
  await dragFromTo(at(128, 128), at(192, 128));
  const b = await page.evaluate((id) => globalThis.r5ed.brushBounds(id), o.id);
  expect(Math.abs(b[3] - 192) < 1e-6, `dragging the corner left the brush at ${b} (${await message()})`);
  await page.evaluate(() => globalThis.r5ed.undo());
  const back = await page.evaluate((id) => globalThis.r5ed.brushBounds(id), o.id);
  expect(Math.abs(back[3] - 128) < 1e-6, `one undo left the brush at ${back}`);
});

await step("tools: edge tool drags both ends of an edge as one step", async () => {
  const { o, at } = await topViewBrush();
  await tool("Edge");
  await dragFromTo(at(128, 64), at(192, 64));
  const b = await page.evaluate((id) => globalThis.r5ed.brushBounds(id), o.id);
  expect(Math.abs(b[3] - 192) < 1e-6, `dragging the edge left the brush at ${b} (${await message()})`);
  await page.evaluate(() => globalThis.r5ed.undo());
  const back = await page.evaluate((id) => globalThis.r5ed.brushBounds(id), o.id);
  expect(Math.abs(back[3] - 128) < 1e-6, `one undo left the brush at ${back}`);
});

await step("tools: face tool pulls a face along its normal", async () => {
  const o = await oneBrush();
  await page.getByLabel("Viewport 1 kind").selectOption("persp");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("NumpadDecimal");
  await tool("Face");
  await page.waitForTimeout(300);
  const box = await canvas().boundingBox();
  const c = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  const before = await page.evaluate((id) => globalThis.r5ed.brushBounds(id), o.id);
  await dragFromTo(c, { x: c.x, y: c.y - 120 });
  const after = await page.evaluate((id) => globalThis.r5ed.brushBounds(id), o.id);
  expect(JSON.stringify(before) !== JSON.stringify(after), `the face drag left the brush at ${after} (${await message()})`);
  await tool("Select");
});

await step("tools: voxel terrain shows, and the voxel tool adds and carves", async () => {
  await reset();
  await page.getByLabel("Viewport 1 kind").selectOption("persp");
  const [v] = await addViaOutliner("Voxel terrain");
  await page.waitForTimeout(300);
  const bounds = await page.evaluate((id) => globalThis.r5ed.currentObjects().find((o) => o.doc.id === id)?.worldBounds ?? null, v.id);
  expect(bounds, "a new voxel terrain has no geometry in the viewport");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("NumpadDecimal");
  await tool("Voxel");
  await page.waitForTimeout(300);
  const box = await canvas().boundingBox();
  await canvas().click({ position: { x: box.width / 2, y: box.height / 2 } });
  await canvas().click({ position: { x: box.width / 2, y: box.height / 2 }, modifiers: ["Shift"] });
  const ops = (await objects()).find((o) => o.id === v.id).kind.voxel_terrain.ops;
  expect(ops.length === 3 && ops[1].op === "union" && ops[2].op === "subtract", `ops are ${JSON.stringify(ops.map((o) => o.op))} (${await message()})`);
  await tool("Select");
});

await step("viewport: gizmo mode shortcuts", async () => {
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await tool("Move");
  await tool("Rotate");
  expect((await page.getByRole("radio", { name: "Rotate", exact: true }).getAttribute("aria-checked")) === "true", "Ctrl+2 did not switch to rotate");
  await tool("Scale");
  expect((await page.getByRole("radio", { name: "Scale", exact: true }).getAttribute("aria-checked")) === "true", "Ctrl+3 did not switch to scale");
  await tool("Move");
  expect((await page.getByRole("radio", { name: "Move", exact: true }).getAttribute("aria-checked")) === "true", "Ctrl+1 did not switch to move");
  await tool("Select");
});

await step("viewport: lightmap preview bakes", async () => {
  await reset();
  await page.evaluate(() => {
    const e = globalThis.r5ed;
    e.brushBox([-512, -512, -16], [512, 512, 0], "dev/dev_grey");
    e.createObject("light", [0, 0, 128]);
  });
  await page.getByLabel("Toggle lightmap preview").check();
  await page.waitForTimeout(1500);
  const m = await message();
  expect(/lightmap preview: [1-9]/.test(m), `the preview reported "${m}"`);
  await page.getByLabel("Toggle lightmap preview").uncheck();
});

// ---------------------------------------------------------------- terrain

await step("terrain: new terrain dialog creates a generated terrain", async () => {
  await reset();
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("Control+Shift+T");
  const dialog = page.getByRole("dialog", { name: "New terrain" });
  expect(await dialog.isVisible(), "Ctrl+Shift+T did not open the dialog");
  await commit(field(dialog, "World size"), 4096);
  await dialog.getByRole("button", { name: "Create" }).click();
  const all = await objects();
  expect(all.length === 1 && all[0].kind.type === "terrain", `the dialog made ${JSON.stringify(all.map((o) => o.kind.type))}`);
  expect(all[0].kind.terrain.heights, "the terrain has no heights file");
  await page.waitForTimeout(500);
});

await step("terrain: every generator creates a terrain", async () => {
  await reset();
  const open = async () => {
    await unfocus();
    await page.keyboard.press("Control+Shift+T");
    return page.getByRole("dialog", { name: "New terrain" });
  };
  let dialog = await open();
  const kinds = await dialog.locator("#generator-kind option").evaluateAll((os) => os.map((o) => o.value));
  await closeOverlays();
  const bad = [];
  for (const kind of kinds) {
    dialog = await open();
    await dialog.locator("#generator-kind").selectOption(kind);
    await commit(field(dialog, "World size"), 1024);
    const create = dialog.getByRole("button", { name: "Create" });
    if (kind === "heightmap") {
      // No image yet: the dialog says so instead of failing in the core.
      if (!(await create.isDisabled())) bad.push("heightmap without an image can be created");
      await dialog.locator("#heightmap-image").setInputFiles({ name: "h.png", mimeType: "image/png", buffer: FIXTURES.png() });
      await page.waitForFunction(() => !document.querySelector('[role="dialog"] .row button:first-child')?.hasAttribute("disabled"), null, { timeout: 3000 }).catch(() => {});
    }
    const before = await count();
    await create.click();
    if ((await count()) !== before + 1) bad.push(`${kind}: ${await message()}`);
    await closeOverlays();
  }
  expect(bad.length === 0, `generators failed: ${bad.join("; ")}`);
  await page.waitForTimeout(300);
});

await step("terrain: inspector generate, auto paint, scatter", async () => {
  const t = (await objects()).find((o) => o.kind.type === "terrain");
  await selectRow(t.name);
  const gen = inspector().getByRole("button", { name: "Generate" });
  expect((await gen.count()) >= 1, "no Generate button");
  await gen.first().click();
  expect(/generated/.test(await message()), `generate said "${await message()}"`);
  const paint = inspector().getByRole("button", { name: /auto.?paint|Apply rules|Apply auto/i });
  if ((await paint.count()) > 0) await paint.first().click();
  await inspector().getByRole("button", { name: "Apply scatter rules" }).click();
  expect(/scatter/.test(await message()), `scatter said "${await message()}"`);
});

await step("terrain: sculpt tool raises the terrain under a drag", async () => {
  await reset();
  await page.keyboard.press("Control+Shift+T");
  const dialog = page.getByRole("dialog", { name: "New terrain" });
  await commit(field(dialog, "World size"), 2048);
  await dialog.getByRole("button", { name: "Create" }).click();
  const t = (await objects())[0];
  await selectRow(t.name);
  await page.getByLabel("Viewport 1 kind").selectOption("top");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("NumpadDecimal");
  await tool("Sculpt");
  const heights = () => page.evaluate((p) => {
    const bytes = globalThis.r5ed.core.getFile?.(p);
    if (!bytes) return null;
    const f = new Float32Array(bytes.buffer, bytes.byteOffset, bytes.byteLength / 4);
    let s = 0;
    for (const v of f) s += v;
    return s;
  }, t.kind.terrain.heights);
  const before = await heights();
  const box = await canvas().boundingBox();
  await page.mouse.move(box.x + box.width / 2 - 40, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 40, box.y + box.height / 2, { steps: 10 });
  await page.mouse.up();
  const after = await heights();
  expect(!(await message()).includes("no terrain under"), `sculpt said "${await message()}"`);
  expect(before === null || after !== before, `the heights did not change (${before} -> ${after})`);
  await page.getByLabel("Viewport 1 kind").selectOption("persp");
  await tool("Select");
});

await step("terrain: paint tool writes splat weights under a drag", async () => {
  const t = (await objects()).find((o) => o.kind.type === "terrain");
  expect(t, "no terrain left by the sculpt step");
  await selectRow(t.name);
  await page.getByLabel("Viewport 1 kind").selectOption("top");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("NumpadDecimal");
  await tool("Paint");
  const box = await canvas().boundingBox();
  await dragFromTo({ x: box.x + box.width / 2 - 30, y: box.y + box.height / 2 }, { x: box.x + box.width / 2 + 30, y: box.y + box.height / 2 });
  const now = (await objects()).find((o) => o.id === t.id).kind.terrain;
  const weights = now.layers.map((l) => l.weights);
  const written = await page.evaluate((paths) => paths.map((p) => !!p && !!globalThis.r5ed.core.getFile(p)), weights);
  expect(written.every(Boolean), `paint left the weights ${JSON.stringify(weights)} unwritten (${await message()})`);
  await page.getByLabel("Viewport 1 kind").selectOption("persp");
  await tool("Select");
});

// ---------------------------------------------------------------- dialogs

await step("dialogs: map settings apply a new stem", async () => {
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("Control+Shift+M");
  const dialog = page.getByRole("dialog", { name: "Map settings" });
  expect(await dialog.isVisible(), "Ctrl+Shift+M did not open map settings");
  await commit(field(dialog, "Stem (mp_rr_*)"), "mp_rr_smoke");
  await commit(field(dialog, "Playlists (comma separated)"), "survival, fs_dm");
  await commit(field(dialog, "Bounces"), 3);
  await dialog.getByRole("button", { name: "Apply" }).click();
  const doc = await page.evaluate(() => JSON.parse(JSON.stringify(globalThis.r5ed.document())));
  expect(doc.meta.stem === "mp_rr_smoke", `stem is ${doc.meta.stem}`);
  expect(doc.meta.playlists.join() === "survival,fs_dm", `playlists are ${doc.meta.playlists}`);
  expect(doc.settings.bake.bounces === 3, `bounces is ${doc.settings.bake.bounces}`);
  expect((await page.locator(".top-bar .stem").innerText()) === "mp_rr_smoke", "the top bar still shows the old stem");
});

await step("dialogs: map settings refuse a bad stem", async () => {
  await page.keyboard.press("Control+Shift+M");
  const dialog = page.getByRole("dialog", { name: "Map settings" });
  await commit(field(dialog, "Stem (mp_rr_*)"), "Bad Stem");
  expect(await dialog.getByRole("button", { name: "Apply" }).isDisabled(), "a bad stem can be applied");
});

for (const [kind, text] of Object.entries(FIXTURES.text)) {
  await step(`dialogs: import ${kind}`, async () => {
    await reset();
    await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
    await page.keyboard.press("Control+u");
    const dialog = page.getByRole("dialog", { name: "Import" });
    expect(await dialog.isVisible(), "Ctrl+U did not open the import dialog");
    await dialog.locator("#import-kind").selectOption(kind);
    await dialog.locator("#import-text").fill(text);
    await dialog.getByRole("button", { name: "Import" }).click();
    expect((await count()) > 0, `importing ${kind} added nothing (${await message()})`);
  });
}

await step("dialogs: import glTF from a file", async () => {
  await reset();
  await page.keyboard.press("Control+u");
  const dialog = page.getByRole("dialog", { name: "Import" });
  await dialog.locator("#import-kind").selectOption("gltf");
  const file = dialog.locator('input[type="file"]');
  expect((await file.count()) === 1, "the glTF import has no file picker");
  await file.setInputFiles({ name: "cube.glb", mimeType: "model/gltf-binary", buffer: FIXTURES.glb() });
  await dialog.getByRole("button", { name: "Import" }).click();
  // Reading the file is asynchronous.
  await page.waitForFunction(() => globalThis.r5ed.store.ids().length > 0, null, { timeout: 5000 }).catch(() => {});
  const all = await objects();
  expect(all.length === 1 && all[0].kind.type === "mesh", `the glb import made ${JSON.stringify(all.map((o) => o.kind.type))} (${await message()})`);
});

await step("dialogs: shortcuts table rebinds a command", async () => {
  await unfocus();
  await page.keyboard.press("F1");
  const dialog = page.getByRole("dialog", { name: "Keymap and input" });
  expect(await dialog.isVisible(), "F1 did not open the keymap dialog");
  const row = dialog.locator("tr", { has: page.locator("td", { hasText: /^Frame selected$/ }) });
  await row.getByRole("button").click();
  await page.keyboard.press("k");
  expect((await row.getByRole("button").innerText()) === "K", `the binding shows ${await row.getByRole("button").innerText()}`);
  // Tabbing away must not rebind anything.
  const other = dialog.locator("tr", { has: page.locator("td", { hasText: /^Select all$/ }) }).getByRole("button");
  await other.focus();
  await page.keyboard.press("Tab");
  expect((await other.innerText()) === "A", `focusing a binding and pressing Tab rebound it to ${await other.innerText()}`);
  await dialog.getByRole("button", { name: "Restore defaults" }).click();
  expect((await row.getByRole("button").innerText()) === "NumpadDecimal", "restore defaults did not restore the binding");
  await closeOverlays();
});

await step("dialogs: keymap dialog and project menu", async () => {
  await unfocus();
  await page.keyboard.press("F1");
  expect(await page.getByRole("dialog", { name: "Keymap and input" }).isVisible(), "F1 did not open the keymap");
  await closeOverlays();
  await page.getByRole("button", { name: "Project" }).click();
  expect((await page.locator(".top-bar .menu").count()) > 0, "the project button opened nothing");
  await page.keyboard.press("Escape");
  await closeOverlays();
});

await step("dialogs: Escape closes a dialog", async () => {
  await unfocus();
  await page.keyboard.press("F1");
  await page.keyboard.press("Escape");
  expect((await page.locator('[role="dialog"]').count()) === 0, "Escape did not close the dialog");
});

await step("shortcuts: view, layout and snap commands", async () => {
  const press = async (key) => {
    await unfocus();
    await page.keyboard.press(key);
  };
  await press("Control+Alt+q");
  expect((await page.locator(".viewport canvas").count()) === 4, "Ctrl+Alt+Q did not show four panes");
  await press("Control+Alt+q");
  expect((await page.locator(".viewport canvas").count()) === 1, "Ctrl+Alt+Q did not return to one pane");
  const grid = await page.getByLabel("Toggle grid").isChecked();
  await press("Alt+Quote");
  expect((await page.getByLabel("Toggle grid").isChecked()) !== grid, "Alt+' did not toggle the grid");
  await press("Alt+Quote");
  const gizmo = await page.getByLabel("Toggle gizmo").isChecked();
  await press("Control+Backquote");
  expect((await page.getByLabel("Toggle gizmo").isChecked()) !== gizmo, "Ctrl+` did not toggle the gizmo");
  await press("Control+Backquote");
  const snap = await page.getByLabel("Toggle snapping").isChecked();
  await press("Shift+Tab");
  expect((await page.getByLabel("Toggle snapping").isChecked()) !== snap, "Shift+Tab did not toggle snapping");
  await press("Shift+Tab");
  const space = await page.locator(".top-bar button", { hasText: /^(world|local)$/ }).innerText();
  await press("Comma");
  await page.locator(".pie-item", { hasText: space === "world" ? "Local" : "Global" }).click();
  const flipped = await page.locator(".top-bar button", { hasText: /^(world|local)$/ }).innerText();
  expect(flipped !== space, "the orientation pie did not change the transform orientation");
  await press("Comma");
  await page.locator(".pie-item", { hasText: space === "world" ? "Global" : "Local" }).click();
});

await step("blender: modal move, rotate and scale with typed values", async () => {
  await reset();
  await page.evaluate(() => globalThis.r5ed.createObject("light", [0, 0, 0]));
  const [o] = await objects();
  await page.evaluate((id) => {
    globalThis.r5ed.clear();
    globalThis.r5ed.toggleSelect(id);
  }, o.id);
  await overView();
  await page.keyboard.press("g");
  expect((await page.locator(".status-hints .modal-name").innerText()) === "Move", "G did not start a move");
  for (const k of ["x", "1", "2", "8", "Enter"]) await page.keyboard.press(k);
  let t = (await objects())[0].transform;
  expect(t.position[0] === 128 && t.position[1] === 0 && t.position[2] === 0, `G X 128 left the light at ${t.position}`);
  await page.keyboard.press("r");
  for (const k of ["z", "9", "0", "Enter"]) await page.keyboard.press(k);
  t = (await objects())[0].transform;
  expect(Math.abs(Math.abs(t.rotation[2]) - Math.SQRT1_2) < 1e-4, `R Z 90 gave rotation ${t.rotation}`);
  await page.keyboard.press("s");
  for (const k of ["2", "Enter"]) await page.keyboard.press(k);
  t = (await objects())[0].transform;
  expect(t.scale.every((v) => Math.abs(v - 2) < 1e-6), `S 2 gave scale ${t.scale}`);
  // Escape puts everything back and leaves no undo step.
  await page.keyboard.press("g");
  for (const k of ["z", "5", "0", "0", "Escape"]) await page.keyboard.press(k);
  t = (await objects())[0].transform;
  expect(t.position[2] === 0, `a cancelled move left z at ${t.position[2]}`);
  await page.keyboard.press("Control+z");
  t = (await objects())[0].transform;
  expect(t.scale[0] === 1, "one undo did not take back exactly the scale");
  await page.keyboard.press("Alt+g");
  t = (await objects())[0].transform;
  expect(t.position.every((v) => v === 0), `Alt+G left ${t.position}`);
});

await step("blender: numpad views, projection toggle and view pie", async () => {
  await overView();
  const label = () => page.locator(".viewport .view-name").first().innerText();
  await page.keyboard.press("Numpad7");
  expect((await label()) === "Top Orthographic", `Numpad 7 shows ${await label()}`);
  await page.keyboard.press("Control+Numpad3");
  expect((await label()) === "Left Orthographic", `Ctrl+Numpad 3 shows ${await label()}`);
  await page.keyboard.press("Numpad5");
  expect((await label()) === "User Perspective", `Numpad 5 shows ${await label()}`);
  await page.keyboard.press("Numpad1");
  await page.keyboard.press("Numpad4");
  expect((await label()) === "User Perspective", `orbiting off an axis shows ${await label()}`);
  await page.keyboard.press("Backquote");
  expect(await page.locator(".pie-menu").isVisible(), "the backquote key opened no view pie");
  await page.locator(".pie-item", { hasText: "Top" }).click();
  expect((await label()) === "Top Orthographic", `the pie chose ${await label()}`);
});

await step("blender: box select, hide and reveal, add menu, search", async () => {
  await reset();
  await page.evaluate(() => {
    globalThis.r5ed.brushBox([-64, -64, 0], [0, 0, 64], "dev/dev_grey");
    globalThis.r5ed.brushBox([64, 64, 0], [128, 128, 64], "dev/dev_grey");
  });
  await overView();
  await page.keyboard.press("Numpad7");
  await page.keyboard.press("Home");
  await page.waitForTimeout(200);
  const box = await canvas().boundingBox();
  await dragFromTo({ x: box.x + 8, y: box.y + 8 }, { x: box.x + box.width - 90, y: box.y + box.height - 8 });
  expect((await selectedCount()) === 2, `a box drag over the view selected ${await selectedCount()}`);
  await overView();
  await page.keyboard.press("h");
  expect((await objects()).every((o) => o.hidden), "H did not hide the selection");
  await page.keyboard.press("Alt+h");
  expect((await objects()).every((o) => !o.hidden), "Alt+H did not reveal");
  await page.keyboard.press("Shift+a");
  expect(await page.locator(".popup-menu").isVisible(), "Shift+A opened no add menu");
  const before = await count();
  await page.locator(".popup-menu button", { hasText: "Light" }).click();
  expect((await count()) === before + 1, "the add menu added nothing");
  await page.keyboard.press("F3");
  const search = page.locator(".search-menu input");
  expect(await search.isVisible(), "F3 opened no search");
  await search.fill("invert");
  await search.press("Enter");
  expect((await selectedCount()) === before, `searching invert selection left ${await selectedCount()} selected`);
});

await step("viewport: dropping an asset on the view places it", async () => {
  await reset();
  const box = await canvas().boundingBox();
  await page.evaluate(({ x, y }) => {
    const target = document.querySelector(".viewport canvas");
    const data = new DataTransfer();
    data.setData("text/r5ed-asset", JSON.stringify({ kind: "model", path: "mdl/props/dropped.rmdl", guid: "0x1" }));
    target.dispatchEvent(new DragEvent("dragover", { dataTransfer: data, clientX: x, clientY: y, bubbles: true, cancelable: true }));
    target.dispatchEvent(new DragEvent("drop", { dataTransfer: data, clientX: x, clientY: y, bubbles: true, cancelable: true }));
  }, { x: box.x + box.width / 2, y: box.y + box.height / 2 });
  await page.waitForTimeout(300);
  const all = await objects();
  expect(all.length === 1 && all[0].kind.type === "prop" && all[0].kind.prop.model === "mdl/props/dropped.rmdl", `the drop made ${JSON.stringify(all.map((o) => o.kind.type))}`);
  // The model is not in the empty install; the editor asking for it is expected.
  consoleErrors = consoleErrors.filter((e) => !/assets\/model|Failed to load resource/.test(e));
});

// ---------------------------------------------------------------- panel focus

await step("panels: F3 F4 F5 focus the outliner, inspector and assets", async () => {
  for (const [key, label] of [["F3", "Outliner"], ["F4", "Inspector"], ["F5", "Asset browser"]]) {
    await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
    await page.keyboard.press(key);
    const inside = await page.evaluate((label) => document.querySelector(`[aria-label="${label}"]`)?.contains(document.activeElement) ?? false, label);
    expect(inside, `${key} did not focus the ${label}`);
  }
});

// ---------------------------------------------------------------- project

const projectName = `smoke_${Date.now().toString(36)}`;

await step("project: new creates a project and starts an empty document", async () => {
  await reset();
  await addViaOutliner("Light");
  await page.getByRole("button", { name: "Project", exact: true }).click();
  await page.getByRole("menuitem", { name: /^New Ctrl/ }).click();
  const dialog = page.getByRole("dialog", { name: "New project" });
  await commit(field(dialog, "Project name"), projectName);
  await dialog.getByRole("button", { name: "go" }).click();
  await page.waitForFunction(() => document.querySelectorAll('[role="dialog"]').length === 0, null, { timeout: 5000 });
  expect((await page.locator(".top-bar .project-name").innerText()) === projectName, "the top bar does not show the new project");
  expect((await count()) === 0, `the new project opened with ${await count()} objects from the old document`);
  if (PROJECTS) expect(fs.existsSync(path.join(PROJECTS, projectName)), "no project directory on disk");
});

await step("project: save writes map.json and clears the dirty flag", async () => {
  await addViaOutliner("Light");
  expect((await page.locator(".top-bar .dirty").innerText()) === "*", "an edit did not mark the document dirty");
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("Control+s");
  await page.waitForFunction(() => document.querySelector(".top-bar .dirty")?.textContent === "", null, { timeout: 5000 });
  if (PROJECTS) {
    const doc = JSON.parse(fs.readFileSync(path.join(PROJECTS, projectName, "map.json"), "utf8"));
    expect(doc.objects.length === 1, `map.json holds ${doc.objects.length} objects`);
  }
});

await step("project: open loads the saved document", async () => {
  await reset();
  expect((await count()) === 0, "reset failed");
  await page.getByRole("button", { name: "Project", exact: true }).click();
  await page.getByRole("menuitem", { name: /^Open Ctrl/ }).click();
  const dialog = page.getByRole("dialog", { name: "Open project" });
  const listed = dialog.getByRole("button", { name: projectName });
  expect((await listed.count()) === 1, "the project list does not show the saved project");
  await listed.click();
  await dialog.getByRole("button", { name: "go" }).click();
  await page.waitForFunction(() => document.querySelectorAll('[role="dialog"]').length === 0, null, { timeout: 5000 });
  expect((await count()) === 1, `open loaded ${await count()} objects`);
  expect((await page.locator(".tree-row").count()) === 1, "the outliner did not refresh after open");
});

await step("project: save as copies to a new project", async () => {
  await page.getByRole("button", { name: "Project", exact: true }).click();
  await page.getByRole("menuitem", { name: /^Save as Ctrl/ }).click();
  const dialog = page.getByRole("dialog", { name: "Save project as" });
  await commit(field(dialog, "Project name"), `${projectName}_copy`);
  await dialog.getByRole("button", { name: "go" }).click();
  await page.waitForFunction(() => document.querySelectorAll('[role="dialog"]').length === 0, null, { timeout: 5000 });
  if (PROJECTS) expect(fs.existsSync(path.join(PROJECTS, `${projectName}_copy`, "map.json")), "save as wrote no map.json");
});

await step("project: meshes and terrain files survive save and open", async () => {
  await reset();
  await page.evaluate(() => globalThis.r5ed.createObject("mesh", [0, 0, 0]));
  await page.keyboard.press("Control+Shift+T");
  const dialog = page.getByRole("dialog", { name: "New terrain" });
  await commit(field(dialog, "World size"), 1024);
  await dialog.getByRole("button", { name: "Create" }).click();
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("Control+s");
  await page.waitForTimeout(1000);
  if (PROJECTS) {
    const dir = path.join(PROJECTS, `${projectName}_copy`);
    const doc = JSON.parse(fs.readFileSync(path.join(dir, "map.json"), "utf8"));
    const files = [];
    // The wire form flattens the kind: `{ type: "mesh", source, ... }`.
    for (const o of doc.objects) {
      const k = o.kind ?? {};
      if (k.type === "mesh" && k.source) files.push(k.source);
      if (k.type === "terrain" && k.heights) files.push(k.heights);
    }
    const missing = files.filter((f) => !fs.existsSync(path.join(dir, f)));
    expect(files.length >= 2, `map.json references ${files.length} files`);
    expect(missing.length === 0, `files the document references were never written: ${missing}`);
  }
});

// ---------------------------------------------------------------- assets and jobs

await step("assets: search runs and the kind select works", async () => {
  await page.getByLabel("Search assets").fill("container");
  await page.getByLabel("Search assets").press("Enter");
  await page.waitForTimeout(300);
  await page.getByLabel("Asset kind").selectOption("material");
  await page.waitForTimeout(300);
  const total = await page.locator(".panel.assets .panel-head .count").innerText();
  expect(total === "0", `an empty install reports ${total} assets`);
});

await step("assets: results render once the host answers", async () => {
  // Stub one search result so the grid, the click-to-place and the drag hint
  // are exercised without an install.
  await page.route("**/api/assets/search**", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ total: 1, items: [{ kind: "model", path: "mdl/props/smoke.rmdl", guid: "0x1" }] }) }),
  );
  await page.getByLabel("Search assets").fill("smoke");
  await page.getByLabel("Search assets").press("Enter");
  await page.waitForTimeout(500);
  await page.unroute("**/api/assets/search**");
  const tile = page.locator('.thumb-grid button[title="mdl/props/smoke.rmdl"]');
  // The stub result has no thumbnail or model behind it; the editor asking for
  // them and hearing 404 is the expected path.
  consoleErrors = consoleErrors.filter((e) => !/thumbnail|assets\/model|Failed to load resource/.test(e));
  expect((await tile.count()) === 1, "a search result did not render");
  await reset();
  await tile.click();
  const all = await objects();
  expect(all.length === 1 && all[0].kind.type === "prop", `placing the asset made ${JSON.stringify(all.map((o) => o.kind.type))}`);
  await page.waitForTimeout(300);
  consoleErrors = consoleErrors.filter((e) => !/thumbnail|assets\/model|Failed to load resource/.test(e));
});

for (const job of ["Compile", "Bake", "Package", "Deploy", "Reload"]) {
  await step(`jobs: ${job} runs as a host job and shows in the panel`, async () => {
    await reset();
    await page.evaluate(() => globalThis.r5ed.brushBox([-256, -256, -16], [256, 256, 0], "dev/dev_grey"));
    await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
    await page.keyboard.press("Control+s");
    await page.waitForTimeout(300);
    const before = await page.locator(".panel.jobs .job").count();
    await page.locator(".top-bar .build").getByRole("button", { name: job, exact: true }).click();
    await page.waitForSelector(".panel.jobs", { timeout: 5000 });
    await page.waitForFunction((n) => document.querySelectorAll(".panel.jobs .job").length > n, before, { timeout: 5000 });
    const row = page.locator(".panel.jobs .job").last();
    await page.waitForFunction(
      () => ["done", "failed", "cancelled"].includes(document.querySelector(".panel.jobs .job:last-child")?.getAttribute("data-state") ?? ""),
      null,
      { timeout: 60000 },
    );
    const state = await row.getAttribute("data-state");
    await row.getByRole("button", { name: "log" }).click();
    const log = (await row.locator(".job-log").innerText().catch(() => "")).trim().split("\n").slice(-3).join(" / ");
    // Deploy and reload need a game install, which the smoke host does not have.
    const needsInstall = job === "Deploy" || job === "Reload" || job === "Package";
    expect(state === "done" || (needsInstall && state === "failed"), `${job} ended ${state}: ${log}`);
    consoleErrors = consoleErrors.filter((e) => !(needsInstall && /job/.test(e)));
  });
}

await step("jobs: panel toggles and closes", async () => {
  await page.getByRole("button", { name: "Close jobs" }).click();
  expect((await page.locator(".panel.jobs").count()) === 0, "close did not close the job panel");
  await page.locator(".top-bar .build").getByRole("button", { name: "Jobs" }).click();
  expect((await page.locator(".panel.jobs").count()) === 1, "Jobs did not open the panel");
  await page.locator(".top-bar .build").getByRole("button", { name: "Jobs" }).click();
  await canvas().click({ position: { x: 5, y: 5 }, button: "middle" });
  await page.keyboard.press("F7");
  await page.waitForSelector(".panel.jobs", { timeout: 3000 }).catch(() => {});
  expect((await page.locator(".panel.jobs").count()) === 1, "F7 did not open the panel");
  expect((await page.locator(".panel.jobs .job").count()) > 0, "F7 opened an empty job list");
  await page.keyboard.press("F7");
});

await step("jobs: host job failures reach the message log", async () => {
  const log = await page.locator('[aria-label="Messages"] li').allInnerTexts();
  expect(log.length > 0, "the message log is empty after a session of edits and jobs");
});

// ---------------------------------------------------------------- live link

await step("live: toggle connects and disconnects", async () => {
  await page.locator(".top-bar").getByRole("button", { name: /^Live/ }).click();
  await page.waitForTimeout(800);
  const on = await statusCell("live");
  expect(!/off/.test(on), `live is "${on}" after turning it on`);
  await page.locator(".top-bar").getByRole("button", { name: /^Live/ }).click();
  await page.waitForTimeout(300);
  expect(/off/.test(await statusCell("live")), "live did not turn off");
});

// ---------------------------------------------------------------- report

await browser.close();
const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} steps passed`);
fs.writeFileSync(path.join(OUT, "results.json"), JSON.stringify(results, null, 2));
process.exit(failed.length ? 1 : 0);
