// Test reporter that keeps `vitest run` output inside ASCII.
//
// The built-in default reporter draws a U+2713 check per file, which a parent
// process that captures stdout and replays it on a legacy Windows code page
// (cp1252) cannot encode. This reporter prints one ASCII line per test file, a
// failure block for anything that did not pass, and the usual summary.
const MAX_ERROR_CHARS = 3000;

function rel(file) {
  const abs = String(file).replace(/\\/g, "/");
  const base = process.cwd().replace(/\\/g, "/");
  return abs.startsWith(base + "/") ? abs.slice(base.length + 1) : abs;
}

function short(value) {
  const text = typeof value === "string" ? value : (JSON.stringify(value) ?? String(value));
  return text.length > 400 ? `${text.slice(0, 400)}...` : text;
}

function describeError(err) {
  if (!err) return "unknown error";
  const head = (err.name ? `${err.name}: ` : "") + String(err.message ?? err);
  const parts = [head];
  let stack = err.stack ? String(err.stack) : "";
  if (stack.startsWith(head)) stack = stack.slice(head.length).replace(/^\n/, "");
  if (stack.trim()) parts.push(stack);
  if (err.actual !== undefined || err.expected !== undefined) {
    parts.push(`expected: ${short(err.expected)}`);
    parts.push(`actual:   ${short(err.actual)}`);
  }
  const text = parts.join("\n");
  return text.length > MAX_ERROR_CHARS ? `${text.slice(0, MAX_ERROR_CHARS)}\n(truncated)` : text;
}

function tally() {
  return { tests: 0, passed: 0, failed: 0, skipped: 0, ms: 0 };
}

export default class AsciiReporter {
  constructor() {
    this.total = tally();
    this.files = new Map();
    this.failures = [];
  }

  onTestModuleQueued(testModule) {
    const id = testModule?.moduleId;
    if (typeof id === "string" && !this.files.has(id)) this.files.set(id, tally());
  }

  onTestCaseResult(testCase) {
    const result = testCase.result();
    const id = testCase.module?.moduleId;
    const buckets = [this.total];
    if (typeof id === "string") {
      if (!this.files.has(id)) this.files.set(id, tally());
      buckets.push(this.files.get(id));
    }
    const ms = Math.round(testCase.diagnostic()?.duration ?? result.duration ?? 0);
    for (const bucket of buckets) {
      bucket.tests += 1;
      bucket.ms += ms;
      if (result.state in bucket) bucket[result.state] += 1;
    }
    if (result.state === "failed") {
      this.failures.push({
        name: `${rel(id ?? "?")} > ${testCase.fullName}`,
        errors: result.errors?.length ? result.errors : [result],
      });
    }
  }

  onTestRunEnd(_testModules, unhandledErrors, reason) {
    const out = (line) => process.stdout.write(`${line}\n`);
    for (const [id, b] of this.files) {
      if (b.tests === 0) continue;
      const mark = b.failed ? "FAIL" : b.passed ? "PASS" : "SKIP";
      const name = rel(id);
      const plural = b.tests === 1 ? "" : "s";
      out(` ${mark}  ${name} (${b.tests} test${plural}) ${b.ms}ms`);
    }
    for (const failure of this.failures) {
      out("");
      out(`FAILED  ${failure.name}`);
      for (const err of failure.errors) out(describeError(err));
    }
    for (const err of unhandledErrors) {
      out("");
      out("UNHANDLED ERROR");
      out(describeError(err));
    }
    const t = this.total;
    out("");
    out(` Test files  ${this.fileLine()} (${this.files.size})`);
    out(`      tests  ${t.passed} passed${t.failed ? `, ${t.failed} failed` : ""}` +
      `${t.skipped ? `, ${t.skipped} skipped` : ""} (${t.tests})`);
    out(`    result  ${reason}`);
  }

  fileLine() {
    const pass = [...this.files.values()].filter((b) => b.tests > 0 && !b.failed).length;
    const fail = [...this.files.values()].filter((b) => b.failed).length;
    return `${pass} passed${fail ? `, ${fail} failed` : ""}`;
  }
}
