import type { Core } from "./api";

/** The wasm-bindgen module shape, kept loose so the glue stays minimal. */
interface WasmModule {
  default?: (input?: unknown) => Promise<unknown>;
  init?: (input?: unknown) => Promise<unknown>;
  initSync?: (input?: unknown) => unknown;
  initCore?: () => Promise<Core> | Core;
  Core?: new () => Core;
  memory?: WebAssembly.Memory;
}

export class CoreLoadError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CoreLoadError";
  }
}

/** Where the built wasm package is served from; never bundled. */
export const WASM_URL = "/pkg/r5ed_wasm.js";

function isCore(v: unknown): v is Core {
  return typeof v === "object" && v !== null && typeof (v as Core).apply === "function" && typeof (v as Core).pick === "function";
}

/**
 * Loads the wasm core. The module is fetched at runtime from `url` so nothing
 * wasm-related ends up in the JS bundle.
 */
export async function loadCore(url: string = WASM_URL): Promise<Core> {
  let mod: WasmModule;
  try {
    mod = (await import(/* @vite-ignore */ url)) as WasmModule;
  } catch (e) {
    throw new CoreLoadError(`failed to load ${url}: ${e instanceof Error ? e.message : String(e)}`);
  }
  const init = mod.default ?? mod.init;
  if (typeof init === "function") {
    const result = await init();
    const candidate = result && typeof result === "object" ? (result as WasmModule) : mod;
    if (isCore(candidate)) return candidate;
    const fromResult = pickCore(candidate);
    if (fromResult) return fromResult;
  } else if (typeof mod.initSync === "function") {
    mod.initSync();
  }
  return pickCore(mod) ?? fail(url);
}

function pickCore(mod: WasmModule): Core | null {
  if (isCore(mod)) return mod;
  if (typeof mod.initCore === "function") {
    const made = mod.initCore();
    if (isCore(made)) return made;
  }
  if (typeof mod.Core === "function") {
    const made = new mod.Core();
    if (isCore(made)) return made;
  }
  return null;
}

function fail(url: string): never {
  throw new CoreLoadError(`${url} did not export a Core; the wasm package is not built`);
}
