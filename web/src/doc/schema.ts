export type KeyType =
  | "int" | "float" | "bool" | "string" | "vec3" | "angles"
  | "color" | "model" | "material" | "sound" | "target" | "flags" | "enum";

export const KEY_TYPES: KeyType[] = [
  "int", "float", "bool", "string", "vec3", "angles",
  "color", "model", "material", "sound", "target", "flags", "enum",
];

export interface KeyDef {
  name: string;
  ty: KeyType;
  /** Most common values in shipped maps, most frequent first. */
  observed_values: string[];
  default: string | null;
  required: boolean;
  sources: string[];
}

export type EntityKind = "point" | "brush" | "both";

export interface ClassDef {
  classname: string;
  cpp_class: string;
  base_chain: string[];
  kind: EntityKind;
  partitions: Record<string, number>;
  keys: Record<string, KeyDef>;
  inputs: string[];
  outputs: string[];
  sources: { engine_factory: boolean; engine_datamap: boolean; maps: number; scripts: number };
  example_count: number;
}

export interface EntitySchema {
  classes: Record<string, ClassDef>;
}

export class SchemaError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SchemaError";
  }
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

function keyType(v: unknown): KeyType {
  return typeof v === "string" && (KEY_TYPES as string[]).includes(v) ? (v as KeyType) : "string";
}

function stringList(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

function numberMap(v: unknown): Record<string, number> {
  if (!isRecord(v)) return {};
  return Object.fromEntries(Object.entries(v).map(([k, n]) => [k, typeof n === "number" ? n : 0]));
}

/** Parses `schema/entities.json`; unknown key types fall back to `string`. */
export function parseSchema(text: string): EntitySchema {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new SchemaError("entity schema does not parse");
  }
  if (!isRecord(parsed) || !isRecord(parsed.classes)) throw new SchemaError("entity schema has no classes");
  const classes: Record<string, ClassDef> = {};
  for (const [name, raw] of Object.entries(parsed.classes)) {
    if (!isRecord(raw)) continue;
    const keys: Record<string, KeyDef> = {};
    if (isRecord(raw.keys)) {
      for (const [keyName, keyRaw] of Object.entries(raw.keys)) {
        if (!isRecord(keyRaw)) continue;
        keys[keyName] = {
          name: typeof keyRaw.name === "string" ? keyRaw.name : keyName,
          ty: keyType(keyRaw.ty),
          observed_values: stringList(keyRaw.observed_values),
          default: typeof keyRaw.default === "string" ? keyRaw.default : null,
          required: keyRaw.required === true,
          sources: stringList(keyRaw.sources),
        };
      }
    }
    const sources = isRecord(raw.sources) ? raw.sources : {};
    classes[name] = {
      classname: typeof raw.classname === "string" ? raw.classname : name,
      cpp_class: typeof raw.cpp_class === "string" ? raw.cpp_class : "",
      base_chain: stringList(raw.base_chain),
      kind: raw.kind === "brush" || raw.kind === "both" ? raw.kind : "point",
      partitions: numberMap(raw.partitions),
      keys,
      inputs: stringList(raw.inputs),
      outputs: stringList(raw.outputs),
      sources: {
        engine_factory: sources.engine_factory === true,
        engine_datamap: sources.engine_datamap === true,
        maps: typeof sources.maps === "number" ? sources.maps : 0,
        scripts: typeof sources.scripts === "number" ? sources.scripts : 0,
      },
      example_count: typeof raw.example_count === "number" ? raw.example_count : 0,
    };
  }
  return { classes };
}

/** An empty schema, used until the core answers. */
export function emptySchema(): EntitySchema {
  return { classes: {} };
}

/** Classnames matching a case-insensitive substring, best matches first. */
export function searchClasses(schema: EntitySchema, query: string, limit = 50): ClassDef[] {
  const q = query.trim().toLowerCase();
  const all = Object.values(schema.classes);
  if (!q) {
    return all
      .slice()
      .sort((a, b) => b.example_count - a.example_count || a.classname.localeCompare(b.classname))
      .slice(0, limit);
  }
  const scored: { def: ClassDef; score: number }[] = [];
  for (const def of all) {
    const name = def.classname.toLowerCase();
    let score = -1;
    if (name === q) score = 0;
    else if (name.startsWith(q)) score = 1;
    else if (name.includes(q)) score = 2;
    else if (def.cpp_class.toLowerCase().includes(q)) score = 3;
    if (score >= 0) scored.push({ def, score });
  }
  scored.sort((a, b) => a.score - b.score || b.def.example_count - a.def.example_count);
  return scored.slice(0, limit).map((s) => s.def);
}

/** Keys of a class, required first then alphabetical. */
export function orderedKeys(def: ClassDef): KeyDef[] {
  return Object.values(def.keys).sort((a, b) => {
    if (a.required !== b.required) return a.required ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
}

export interface FlagBit {
  bit: number;
  value: number;
  label: string;
}

/**
 * Bits a flags key is observed to use. The generator records whole observed
 * values, so the bits are the union of set positions across them.
 */
export function observedFlagBits(key: KeyDef): FlagBit[] {
  const bits = new Set<number>();
  for (const value of key.observed_values) {
    const n = Number(value);
    if (!Number.isFinite(n) || !Number.isInteger(n) || n < 0) continue;
    for (let b = 0; b < 32; b++) if ((n >>> b) & 1) bits.add(b);
  }
  return [...bits]
    .sort((a, b) => a - b)
    .map((bit) => ({ bit, value: 1 << bit, label: `1 << ${bit} = ${1 << bit}` }));
}

