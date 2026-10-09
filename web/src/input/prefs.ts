import { type InputPrefs, type Keymap, type PresetId, defaultInputPrefs, defaultKeymap } from "./keymap";

const KEYMAP_KEY = "r5ed.keymap";
const INPUT_KEY = "r5ed.input";

function read(key: string): unknown {
  try {
    const raw = globalThis.localStorage?.getItem(key);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

function write(key: string, value: unknown): void {
  try {
    globalThis.localStorage?.setItem(key, JSON.stringify(value));
  } catch {
    // Storage can be blocked or full; the setting just lasts this session.
  }
}

/** The saved keymap, or the preset default when none is saved or it is malformed. */
export function loadKeymap(): Keymap {
  const saved = read(KEYMAP_KEY) as Partial<Keymap> | null;
  const preset: PresetId = saved?.preset === "legacy" ? "legacy" : "blender";
  if (!saved || !Array.isArray(saved.bindings)) return defaultKeymap(preset);
  const bindings = saved.bindings.filter(
    (b) => b && typeof b.chord === "string" && typeof b.command === "string" && (b.context === "window" || b.context === "viewport" || b.context === "edit_mesh"),
  );
  return { preset, bindings };
}

export function saveKeymap(keymap: Keymap): void {
  write(KEYMAP_KEY, keymap);
}

export function loadInputPrefs(): InputPrefs {
  const saved = read(INPUT_KEY) as Partial<InputPrefs> | null;
  const base = defaultInputPrefs();
  if (!saved) return base;
  const out = { ...base };
  for (const k of Object.keys(base) as (keyof InputPrefs)[]) {
    if (typeof saved[k] === typeof base[k]) (out as Record<string, unknown>)[k] = saved[k];
  }
  return out;
}

export function saveInputPrefs(prefs: InputPrefs): void {
  write(INPUT_KEY, prefs);
}
