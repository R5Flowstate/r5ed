import { describe, expect, it } from "vitest";
import {
  LAYER_DRIVERS,
  MAX_LAYERS,
  RULE_SPECS,
  SplatError,
  defaultLayerRule,
  getRuleField,
  parseRules,
  rulesJson,
  setRuleField,
  validateLayerRule,
  type RuleSpec,
} from "./splat";

function spec(path: string): RuleSpec {
  const found = RULE_SPECS.find((s) => s.path === path);
  if (!found) throw new Error(`no spec for ${path}`);
  return found;
}

describe("splat rules", () => {
  it("names every driver the core declares", () => {
    expect(LAYER_DRIVERS).toEqual(["constant", "height", "slope", "curvature", "flow", "noise"]);
  });

  it("declares every field of a layer rule, with a default in range", () => {
    expect(RULE_SPECS.map((s) => s.path)).toEqual([
      "name", "driver", "range_lo", "range_hi", "weight", "seed", "frequency", "blur", "contrast",
    ]);
    for (const spec of RULE_SPECS) {
      expect(spec.label.length).toBeGreaterThan(0);
      if (spec.type === "float" || spec.type === "int") {
        expect(typeof spec.def).toBe("number");
        expect(spec.min as number).toBeLessThanOrEqual(spec.def as number);
        expect(spec.def as number).toBeLessThanOrEqual(spec.max as number);
      }
      if (spec.type === "enum") expect(spec.choices).toContain(spec.def);
    }
  });

  it("starts at the defaults the core's own Default impl uses", () => {
    const rule = defaultLayerRule("rock");
    expect(rule).toEqual({
      name: "rock",
      driver: "height",
      range_lo: 0,
      range_hi: 1,
      weight: 1,
      seed: 0,
      frequency: 0.01,
      blur: 0,
      contrast: 1,
    });
  });

  it("reads and writes every field immutably", () => {
    const rule = defaultLayerRule("sand");
    const next = setRuleField(rule, spec("range_hi"), 512);
    expect(getRuleField(next, spec("range_hi"))).toBe(512);
    expect(getRuleField(rule, spec("range_hi"))).toBe(1);
    expect(next).not.toBe(rule);
    expect(getRuleField(setRuleField(rule, spec("driver"), "slope"), spec("driver"))).toBe("slope");
    expect(getRuleField(setRuleField(rule, spec("name"), "gravel"), spec("name"))).toBe("gravel");
  });

  it("clamps a value to its spec range and rounds an int", () => {
    const rule = defaultLayerRule();
    expect(getRuleField(setRuleField(rule, spec("weight"), 5), spec("weight"))).toBe(1);
    expect(getRuleField(setRuleField(rule, spec("weight"), -5), spec("weight"))).toBe(0);
    expect(getRuleField(setRuleField(rule, spec("blur"), 3.7), spec("blur"))).toBe(4);
    expect(getRuleField(setRuleField(rule, spec("seed"), 1e12), spec("seed"))).toBe(4294967295);
  });

  it("keeps a driver the core does not declare out", () => {
    expect(getRuleField(setRuleField(defaultLayerRule(), spec("driver"), "swirl"), spec("driver"))).toBe("height");
  });
});

describe("splat json", () => {
  it("emits exactly the fields the core deserializes", () => {
    const rule = { ...defaultLayerRule("rock"), driver: "slope" as const, range_lo: 5, range_hi: 45, contrast: 2 };
    const out = JSON.parse(rulesJson([rule])) as Record<string, unknown>[];
    expect(out).toHaveLength(1);
    expect(Object.keys(out[0]).sort()).toEqual([
      "blur", "contrast", "driver", "frequency", "name", "range_hi", "range_lo", "seed", "weight",
    ]);
    expect(out[0].driver).toBe("slope");
  });

  it("round-trips through parse", () => {
    const rules = [
      { ...defaultLayerRule("rock"), driver: "slope" as const, range_lo: 5, range_hi: 45, weight: 0.8, blur: 2 },
      { ...defaultLayerRule("sand"), driver: "height" as const, range_lo: 0, range_hi: 64, weight: 1, seed: 9 },
      { ...defaultLayerRule("noise"), driver: "noise" as const, frequency: 0.05, contrast: 1.5 },
    ];
    const first = rulesJson(rules);
    expect(rulesJson(parseRules(first))).toBe(first);
  });

  it("accepts a wrapper object and a bare list", () => {
    const rule = defaultLayerRule("rock");
    const body = JSON.stringify([rule]);
    expect(parseRules(body)).toHaveLength(1);
    expect(parseRules(JSON.stringify({ rules: [rule] }))).toHaveLength(1);
  });

  it("drops an entry it cannot read and caps the stack", () => {
    const rule = defaultLayerRule("rock");
    const many = new Array(MAX_LAYERS + 5).fill(rule);
    expect(parseRules(JSON.stringify(many))).toHaveLength(MAX_LAYERS);
    const mixed = parseRules(JSON.stringify([rule, "junk", null, 7, rule]));
    expect(mixed).toHaveLength(2);
  });

  it("rejects an empty stack and one that is too deep", () => {
    expect(() => rulesJson([])).toThrow(SplatError);
    expect(() => rulesJson(new Array(MAX_LAYERS + 1).fill(defaultLayerRule("x")))).toThrow(/at most/);
  });

  it("rejects json that is not a rule stack", () => {
    expect(() => parseRules("nope")).toThrow(SplatError);
    expect(() => parseRules("{}")).toThrow(/not a list/);
  });
});

describe("splat validation", () => {
  it("rejects a bad driver, weight, seed, frequency and blur", () => {
    expect(() => validateLayerRule({ ...defaultLayerRule(), driver: "swirl" as never })).toThrow(/unknown splat driver/);
    expect(() => validateLayerRule({ ...defaultLayerRule(), weight: 2 })).toThrow(/weight must be 0..1/);
    expect(() => validateLayerRule({ ...defaultLayerRule(), seed: -1 })).toThrow(/seed/);
    expect(() => validateLayerRule({ ...defaultLayerRule(), frequency: 0 })).toThrow(/frequency/);
    expect(() => validateLayerRule({ ...defaultLayerRule(), blur: -1 })).toThrow(/blur/);
    expect(() => validateLayerRule({ ...defaultLayerRule(), range_lo: Number.NaN })).toThrow(/range/);
  });
});
