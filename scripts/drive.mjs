// Drives the open editor through its control handle in a real browser.
// Reuses the smoke harness launch logic (R5ED_URL, R5ED_CHROMIUM,
// R5ED_CHROMIUM_ARGS), so flags that work for smoke work here.
//
//   node scripts/drive.mjs --out <dir> [messages.json]
//
// messages.json holds a JSON array of control messages ({verb, params}); with
// no file they are read from stdin. Each reply prints as one JSON line, and a
// reply carrying `png` is also written to <dir> as NN-<verb>.png.

import { chromium } from "playwright-core";
import fs from "node:fs";
import path from "node:path";

function usage() {
  console.error("usage: node scripts/drive.mjs --out <dir> [messages.json]");
  process.exit(2);
}

const args = process.argv.slice(2);
const outAt = args.indexOf("--out");
if (outAt < 0 || !args[outAt + 1]) usage();
const OUT = args[outAt + 1];
const rest = args.filter((_, i) => i !== outAt && i !== outAt + 1);
if (rest.length > 1 || rest.includes("--out")) usage();
fs.mkdirSync(OUT, { recursive: true });

function readMessages() {
  if (rest.length === 1) return fs.readFileSync(rest[0], "utf8");
  return fs.readFileSync(0, "utf8");
}

let messages;
try {
  const parsed = JSON.parse(readMessages());
  messages = Array.isArray(parsed) ? parsed : [parsed];
} catch (e) {
  console.error(`drive: messages are not JSON: ${e.message}`);
  process.exit(2);
}

let URL = process.env.R5ED_URL;
if (!URL) {
  console.error("drive: R5ED_URL is not set");
  process.exit(2);
}
if (URL.includes("#")) URL += "&control=1";
else URL += "#control=1";

const browser = await chromium.launch({
  headless: false,
  executablePath: process.env.R5ED_CHROMIUM ?? "/opt/pw-browsers/chromium",
  // R5ED_CHROMIUM_ARGS replaces the SwiftShader flags where a real GPU is present.
  args: process.env.R5ED_CHROMIUM_ARGS?.split(" ").filter(Boolean) ?? ["--enable-unsafe-webgpu", "--enable-features=Vulkan", "--use-vulkan=swiftshader", "--use-webgpu-adapter=swiftshader"],
});
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
const consoleErrors = [];
page.on("console", (m) => {
  if (m.type() === "error") consoleErrors.push(m.text());
});
page.on("pageerror", (e) => consoleErrors.push(`pageerror: ${e.message}`));

let failed = false;
try {
  await page.goto(URL);
  await page.waitForSelector(".app", { timeout: 60000 });
  await page.waitForFunction(() => globalThis.__r5edControl, null, { timeout: 60000 });
  for (let i = 0; i < messages.length; i++) {
    const msg = messages[i];
    const verb = msg?.verb ?? `message${i}`;
    let reply;
    try {
      reply = await page.evaluate((m) => globalThis.__r5edControl.handle(m), msg);
    } catch (e) {
      failed = true;
      console.log(JSON.stringify({ index: i, verb, error: `transport: ${e.message}` }));
      continue;
    }
    const line = { index: i, verb, reply };
    if (reply && typeof reply.png === "string") {
      const file = path.join(OUT, `${String(i).padStart(2, "0")}-${String(verb).replace(/\W+/g, "_")}.png`);
      fs.writeFileSync(file, Buffer.from(reply.png, "base64"));
      line.file = file;
    }
    if (reply && typeof reply.error === "string") failed = true;
    console.log(JSON.stringify(line));
  }
} catch (e) {
  console.error(`drive: ${e.message}`);
  failed = true;
} finally {
  await browser.close();
}
if (consoleErrors.length > 0) {
  for (const text of consoleErrors) console.error(`drive console: ${text}`);
}
process.exit(failed ? 1 : 0);
