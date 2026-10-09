/**
 * TypeScript mirror of the terrain splat auto-paint rule. Field names and types
 * match the serde shape the core deserializes, so a rule built from these specs
 * is accepted by `terrainAutoPaint`.
 */

export type LayerDriver = "constant" | "height" | "slope" | "curvature" | "flow" | "noise";
export const LAYER_DRIVERS: LayerDriver[] = ["constant", "height", "slope", "curvature", "flow", "noise"];

export type FieldType = "float" | "int" | "enum" | "string";

export interface LayerRule {
  /** Layer name, so the core can match a rule to the layer it paints. */
  name: string;
  driver: LayerDriver;
  /** Lower edge of the window, in the driver's own units. */
  range_lo: number;
  /** Upper edge of the window. */
  range_hi: number;
  /** Weight before normalisation, 0..1. */
  weight: number;
  /** Seed for the `noise` driver. */
  seed: number;
  /** Noise frequency in cycles per inch, for the `noise` driver. */
  frequency: number;
  /** Blur radius applied to the finished weight map, in cells. */
  blur: number;
  /** Score exponent applied before normalisation. */
  contrast: number;
}

export interface RuleSpec {
  path: keyof Omit<LayerRule, "name" | "driver"> | "driver" | "name";
  type: FieldType;
  label: string;
  def: number | string;
  min?: number;
  max?: number;
  step?: number;
  choices?: string[];
}

const INCH = 1e6;
const U32 = 4294967295;

export const RULE_SPECS: RuleSpec[] = [
  { path: "name", type: "string", label: "Layer name", def: "" },
  { path: "driver", type: "enum", label: "Driver", def: "height", choices: [...LAYER_DRIVERS] },
  { path: "range_lo", type: "float", label: "Range low", def: 0, min: -INCH, max: INCH, step: 1 },
  { path: "range_hi", type: "float", label: "Range high", def: 1, min: -INCH, max: INCH, step: 1 },
  { path: "weight", type: "float", label: "Weight", def: 1, min: 0, max: 1, step: 0.01 },
  { path: "seed", type: "int", label: "Noise seed", def: 0, min: 0, max: U32 },
  { path: "frequency", type: "float", label: "Noise frequency", def: 0.01, min: 0.000001, max: 16, step: 0.0001 },
  { path: "blur", type: "int", label: "Blur cells", def: 0, min: 0, max: 64 },
  { path: "contrast", type: "float", label: "Contrast", def: 1, min: 0, max: 16, step: 0.01 },
];

/** Largest stack of layers the core will paint. */
export const MAX_LAYERS = 16;

export function defaultLayerRule(name = "", driver: LayerDriver = "height"): LayerRule {
  const rule: LayerRule = { name, driver, range_lo: 0, range_hi: 0, weight: 0, seed: 0, frequency: 0, blur: 0, contrast: 0 };
  for (const spec of RULE_SPECS) {
    if (spec.path === "name" || spec.path === "driver") continue;
    applySpec(rule, spec, spec.def);
  }
  return rule;
}

function applySpec(rule: LayerRule, spec: RuleSpec, value: unknown): void {
  if (spec.path === "driver") {
    rule.driver = typeof value === "string" && LAYER_DRIVERS.includes(value as LayerDriver) ? (value as LayerDriver) : rule.driver;
  } else if (spec.path === "name") {
    rule.name = typeof value === "string" ? value : rule.name;
  } else if (spec.type === "int") {
    const n = typeof value === "number" && Number.isFinite(value) ? value : 0;
    rule[spec.path] = Math.round(Math.min(spec.max ?? U32, Math.max(spec.min ?? 0, n)));
  } else if (spec.type === "float") {
    const n = typeof value === "number" && Number.isFinite(value) ? value : 0;
    rule[spec.path] = Math.min(spec.max ?? INCH, Math.max(spec.min ?? -INCH, n));
  }
}

/** One spec's value on a rule, so a form can read and write every field. */
export function getRuleField(rule: LayerRule, spec: RuleSpec): string | number {
  if (spec.path === "driver") return rule.driver;
  if (spec.path === "name") return rule.name;
  return rule[spec.path];
}

/** Writes one field immutably, clamped to the spec's range. */
export function setRuleField(rule: LayerRule, spec: RuleSpec, value: string | number): LayerRule {
  const next: LayerRule = { ...rule };
  applySpec(next, spec, value);
  return next;
}

export class SplatError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SplatError";
  }
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

/** Validates a rule and returns it, so the caller can hand it to the core. */
export function validateLayerRule(rule: LayerRule): LayerRule {
  if (!LAYER_DRIVERS.includes(rule.driver)) throw new SplatError(`unknown splat driver ${rule.driver}`);
  if (!Number.isFinite(rule.range_lo) || !Number.isFinite(rule.range_hi)) {
    throw new SplatError("the range is not finite");
  }
  if (!Number.isFinite(rule.weight) || rule.weight < 0 || rule.weight > 1) {
    throw new SplatError("weight must be 0..1");
  }
  if (!Number.isInteger(rule.seed) || rule.seed < 0 || rule.seed > U32) throw new SplatError("seed must be a u32");
  if (!(rule.frequency > 0)) throw new SplatError("frequency must be positive");
  if (!Number.isInteger(rule.blur) || rule.blur < 0) throw new SplatError("blur must be a cell count");
  if (!Number.isFinite(rule.contrast)) throw new SplatError("contrast is not finite");
  return rule;
}

/** Serialises a rule stack the way `terrainAutoPaint` expects it. */
export function rulesJson(rules: LayerRule[]): string {
  if (rules.length === 0) throw new SplatError("auto-paint needs at least one rule");
  if (rules.length > MAX_LAYERS) throw new SplatError(`auto-paint takes at most ${MAX_LAYERS} rules`);
  return JSON.stringify(rules.map(validateLayerRule));
}

/** Parses a rule stack, filling in defaults and dropping entries it cannot read. */
export function parseRules(text: string): LayerRule[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new SplatError("auto-paint rules do not parse");
  }
  const raw = Array.isArray(parsed) ? parsed : isRecord(parsed) && Array.isArray(parsed.rules) ? parsed.rules : null;
  if (!raw) throw new SplatError("auto-paint rules are not a list");
  const out: LayerRule[] = [];
  for (const entry of raw) {
    if (!isRecord(entry)) continue;
    const rule = defaultLayerRule();
    for (const spec of RULE_SPECS) {
      const value = entry[spec.path];
      if (value !== undefined) applySpec(rule, spec, value);
    }
    out.push(rule);
  }
  return out.slice(0, MAX_LAYERS);
}
