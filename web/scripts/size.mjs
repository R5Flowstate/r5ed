// Fails the build when the gzipped production JS exceeds the budget.
// Also prints the emitted asset table in plain ASCII, because the Vite build
// summary it replaces uses box-drawing and check-mark glyphs.
import { gzipSync } from "node:zlib";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const BUDGET = 400 * 1024;
const dist = join(process.cwd(), "dist");

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else out.push(full);
  }
  return out;
}

let files;
try {
  files = walk(dist);
} catch {
  console.error("size: dist not found");
  process.exit(1);
}

const rows = [];
let total = 0;
for (const file of files) {
  const bytes = readFileSync(file);
  const gz = gzipSync(bytes, { level: 9 }).length;
  const name = relative(dist, file).replace(/\\/g, "/");
  if (/\.(js|mjs)$/i.test(name)) total += gz;
  rows.push([name, bytes.length, gz]);
}
rows.sort((a, b) => b[2] - a[2]);
for (const [name, bytes, gz] of rows) {
  console.log(`${(bytes / 1024).toFixed(1).padStart(8)} KB  ${(gz / 1024).toFixed(1).padStart(7)} KB  ${name}`);
}
console.log(`total js (gzip): ${(total / 1024).toFixed(1)} KB of ${(BUDGET / 1024).toFixed(0)} KB budget`);
if (total > BUDGET) {
  console.error("size: production JS exceeds the 400 KB gzip budget");
  process.exit(1);
}
