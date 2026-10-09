/** Console errors since the last drain, for the control socket's state reply. */
const buffer: string[] = [];
let installed = false;

function push(text: string): void {
  if (buffer.length >= 100) buffer.shift();
  buffer.push(text.slice(0, 500));
}

function textOf(args: unknown[]): string {
  return args
    .map((a) => {
      if (typeof a === "string") return a;
      if (a instanceof Error) return `${a.name}: ${a.message}`;
      try {
        return JSON.stringify(a) ?? String(a);
      } catch {
        return String(a);
      }
    })
    .join(" ");
}

/** Records console errors and page errors without printing anything new. */
export function installErrorTrap(): void {
  if (installed) return;
  installed = true;
  const g = globalThis as { console?: Console; addEventListener?: typeof globalThis.addEventListener };
  const original = g.console?.error.bind(g.console);
  if (original && g.console) {
    g.console.error = (...args: unknown[]) => {
      push(textOf(args));
      original(...(args as Parameters<typeof original>));
    };
  }
  try {
    g.addEventListener?.("error", (e) => {
      const err = e as ErrorEvent;
      push(err.message || "page error");
    });
    g.addEventListener?.("unhandledrejection", (e) => {
      const reason = (e as PromiseRejectionEvent).reason;
      push(`unhandled rejection: ${reason instanceof Error ? reason.message : String(reason)}`);
    });
  } catch {
    // No window to listen on; console wrapping above is enough.
  }
}

/** Empties the buffer. */
export function takeErrors(): string[] {
  const out = buffer.slice();
  buffer.length = 0;
  return out;
}
