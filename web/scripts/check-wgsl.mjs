// Validates every exported WGSL shader with naga (run: node scripts/check-wgsl.mjs).
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { build } from "vite";

const out = mkdtempSync(join(tmpdir(), "wgsl-"));
await build({
  logLevel: "silent",
  configFile: false,
  build: { lib: { entry: "src/render/shaders.ts", formats: ["es"], fileName: "shaders" }, outDir: out, emptyOutDir: true },
});
const mod = await import(pathToFileURL(join(out, "shaders.js")).href);
let bad = 0;
for (const [name, code] of Object.entries(mod)) {
  if (!name.endsWith("_SHADER")) continue;
  const f = join(out, name + ".wgsl");
  writeFileSync(f, code);
  try {
    execFileSync("naga", [f], { stdio: "pipe" });
    console.log("ok   " + name);
  } catch (e) {
    bad++;
    console.log("FAIL " + name + "\n" + (e.stderr?.toString() ?? e.message));
  }
}
process.exit(bad ? 1 : 0);
