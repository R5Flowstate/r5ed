import { describe, expect, it } from "vitest";
import {
  generateRequestJson,
  heightmapFromImage,
  BLEND_MODES,
  DETAIL_FIELDS,
  FILTER_OP_FIELDS,
  FILTER_OP_KINDS,
  FRACTAL_KINDS,
  GENERATOR_FIELDS,
  GENERATOR_KINDS,
  MASK_FIELDS,
  MASK_KINDS,
  NODE_KINDS,
  SDF_OP_FIELDS,
  SDF_OP_KINDS,
  SDF_SHAPE_FIELDS,
  SDF_SHAPE_KINDS,
  VOLUME_FIELDS,
  type FieldSpec,
  type GeneratorKind,
  type NodeKind,
  type Recipe,
  RecipeError,
  defaultGenerator,
  defaultMask,
  defaultNode,
  defaultRecipe,
  getField,
  inputsOf,
  nextNodeId,
  nodeFields,
  parseRecipe,
  recipeJson,
  setField,
  storedRecipeJson,
  validateRecipe,
} from "./recipe";

/** Every spec table, so one loop can prove the whole mirror is well formed. */
function allSpecTables(): { name: string; specs: FieldSpec[] }[] {
  return [
    ...GENERATOR_KINDS.map((k) => ({ name: `generator ${k}`, specs: GENERATOR_FIELDS[k] })),
    ...FILTER_OP_KINDS.map((k) => ({ name: `filter op ${k}`, specs: FILTER_OP_FIELDS[k] })),
    ...MASK_KINDS.map((k) => ({ name: `mask ${k}`, specs: MASK_FIELDS[k] })),
    ...SDF_SHAPE_KINDS.map((k) => ({ name: `sdf shape ${k}`, specs: SDF_SHAPE_FIELDS[k] })),
    ...SDF_OP_KINDS.map((k) => ({ name: `sdf op ${k}`, specs: SDF_OP_FIELDS[k] })),
    { name: "detail", specs: DETAIL_FIELDS },
    { name: "volume", specs: VOLUME_FIELDS },
  ];
}

describe("recipe mirror covers the core's serde types", () => {
  it("names every generator, filter op, mask, sdf shape and sdf op the core declares", () => {
    expect(GENERATOR_KINDS).toHaveLength(17);
    expect(GENERATOR_KINDS).toContain("pits");
    expect(GENERATOR_KINDS).toContain("heightmap");
    expect(FILTER_OP_KINDS).toHaveLength(13);
    expect(FILTER_OP_KINDS).toContain("scale_about");
    expect(FILTER_OP_KINDS).toContain("resample");
    expect(MASK_KINDS).toEqual(["constant", "radial", "noise", "from_field"]);
    expect(SDF_SHAPE_KINDS).toEqual(["sphere", "capsule", "box", "cylinder", "cone", "torus", "half_space"]);
    expect(SDF_OP_KINDS).toEqual(["add", "union", "subtract", "intersect", "noise", "shift", "scale"]);
    expect(NODE_KINDS).toEqual(["generator", "filter", "blend", "modulate", "detail", "volume"]);
    expect(BLEND_MODES).toEqual([
      "replace", "add", "subtract", "multiply", "screen", "overlay", "max", "min", "mix",
    ]);
  });

  it("gives every generator a non-empty field list except the analytic ones", () => {
    for (const kind of GENERATOR_KINDS) {
      expect(GENERATOR_FIELDS[kind].length).toBeGreaterThan(0);
    }
    // The generators whose shape comes from a formula carry no ShapeParams.
    for (const kind of ["volcanic", "craters", "farmland", "arena", "pits", "heightmap"] as GeneratorKind[]) {
      expect(GENERATOR_FIELDS[kind].some((s) => s.path.startsWith("shape."))).toBe(false);
    }
    for (const kind of ["rolling_hills", "mountains", "alpine", "plateau", "canyon", "badlands",
      "dunes", "islands", "glacial", "rivers", "coastline"] as GeneratorKind[]) {
      expect(GENERATOR_FIELDS[kind].some((s) => s.path === "shape.kind")).toBe(true);
    }
  });

  it("uses the core's own defaults and sane ranges in every spec", () => {
    for (const { name, specs } of allSpecTables()) {
      for (const spec of specs) {
        expect(spec.path, name).toMatch(/^[a-z][a-z0-9_]*(\.[a-z0-9_]+)*$/);
        expect(spec.label.length, spec.path).toBeGreaterThan(0);
        if (spec.type === "float" || spec.type === "int") {
          const def = spec.def as number;
          expect(typeof def, spec.path).toBe("number");
          expect(typeof spec.min, spec.path).toBe("number");
          expect(typeof spec.max, spec.path).toBe("number");
          expect(spec.min as number).toBeLessThanOrEqual(def);
          expect(def).toBeLessThanOrEqual(spec.max as number);
          expect((spec.min as number) < (spec.max as number)).toBe(true);
        }
        if (spec.type === "enum") {
          expect((spec.choices ?? []).length, spec.path).toBeGreaterThan(0);
          expect(spec.choices, spec.path).toContain(spec.def);
        }
        if (spec.type === "vec2") expect((spec.def as number[]).length).toBe(2);
        if (spec.type === "vec3") expect((spec.def as number[]).length).toBe(3);
        if (spec.type === "bool") expect(typeof spec.def).toBe("boolean");
        if (spec.type === "string") expect(typeof spec.def).toBe("string");
      }
    }
  });

  it("starts every generator at the default its parameter struct declares", () => {
    expect(getField(defaultNode("generator", "rolling_hills"), GENERATOR_FIELDS.rolling_hills[0])).toBe(64);
    expect(getField(defaultNode("generator", "rolling_hills"), GENERATOR_FIELDS.rolling_hills[1])).toBe(320);
    expect(getField(defaultNode("generator", "mountains"), GENERATOR_FIELDS.mountains[4])).toBe(0.45);
    expect(getField(defaultNode("generator", "alpine"), GENERATOR_FIELDS.alpine[5])).toBe(12);
    expect(getField(defaultNode("generator", "plateau"), GENERATOR_FIELDS.plateau[3])).toBe(5);
    expect(getField(defaultNode("generator", "canyon"), GENERATOR_FIELDS.canyon[3])).toBe(2);
    expect(getField(defaultNode("generator", "dunes"), GENERATOR_FIELDS.dunes[3])).toBe("transverse");
    expect(getField(defaultNode("generator", "volcanic"), GENERATOR_FIELDS.volcanic[6])).toBe(5);
    expect(getField(defaultNode("generator", "craters"), GENERATOR_FIELDS.craters[3])).toBe(40);
    expect(getField(defaultNode("generator", "pits"), GENERATOR_FIELDS.pits[7])).toBe(true);
    expect(getField(defaultNode("generator", "heightmap"), GENERATOR_FIELDS.heightmap[3])).toBe(4096);
  });

  it("copies array defaults so two nodes do not share one array", () => {
    const a = defaultGenerator("dunes");
    const b = defaultGenerator("dunes");
    expect(a.shape?.fractal).toEqual(b.shape?.fractal);
    expect(a.shape?.fractal).not.toBe(b.shape?.fractal);
  });

  it("gives every node a unique id", () => {
    const ids = new Set(Array.from({ length: 32 }, () => nextNodeId()));
    expect(ids.size).toBe(32);
  });
});

describe("field access", () => {
  it("reads and writes a dotted path immutably", () => {
    const node = defaultNode("generator", "rolling_hills");
    const spec = GENERATOR_FIELDS.rolling_hills.find((s) => s.path === "shape.fractal.octaves") as FieldSpec;
    const next = setField(node, spec, 9);
    expect(getField(next, spec)).toBe(9);
    expect(getField(node, spec)).toBe(6);
    expect(next).not.toBe(node);
  });

  it("clamps a number to the spec range and rounds an int", () => {
    const spec = GENERATOR_FIELDS.rolling_hills.find((s) => s.path === "base") as FieldSpec;
    expect(getField(setField(defaultNode("generator"), spec, 1e12), spec)).toBe(spec.max);
    expect(getField(setField(defaultNode("generator"), spec, -1e12), spec)).toBe(spec.min);
    const seed = GENERATOR_FIELDS.rolling_hills.find((s) => s.path === "seed") as FieldSpec;
    expect(getField(setField(defaultNode("generator"), seed, 3.7), seed)).toBe(4);
  });

  it("falls back to the spec default for a missing or wrong-typed value", () => {
    const node = defaultNode("generator", "rolling_hills");
    const spec = GENERATOR_FIELDS.rolling_hills.find((s) => s.path === "amplitude") as FieldSpec;
    const empty = setField(node, spec, 5);
    (empty.generator as unknown as Record<string, unknown>).amplitude = "not a number";
    expect(getField(empty, spec)).toBe(320);
  });

  it("keeps an enum inside its choice list", () => {
    const spec = GENERATOR_FIELDS.dunes.find((s) => s.path === "pattern") as FieldSpec;
    expect(getField(setField(defaultNode("generator", "dunes"), spec, "spiral"), spec)).toBe("transverse");
    expect(getField(setField(defaultNode("generator", "dunes"), spec, "star"), spec)).toBe("star");
  });

  it("lists the fields each node kind shows", () => {
    expect(nodeFields(defaultNode("generator", "canyon")).map((s) => s.path)).toContain("channel_width");
    expect(nodeFields(defaultNode("filter", "hydraulic")).map((s) => s.path)).toEqual([
      "droplets", "seed", "inertia", "capacity", "erode_speed", "deposit_speed", "evaporation", "radius",
    ]);
    expect(nodeFields(defaultNode("detail")).map((s) => s.path)).toEqual(["seed", "frequency", "octaves", "amplitude"]);
    expect(nodeFields(defaultNode("volume")).map((s) => s.path)).toContain("voxel_size");
  });
});

/** A graph that touches every node kind with a value on every field. */
function richRecipe(): Recipe {
  const generator = defaultNode("generator", "mountains");
  const filter = defaultNode("filter", "hydraulic");
  filter.source = generator.id;
  const detail = defaultNode("detail");
  const blend = defaultNode("blend");
  blend.terms = [
    { source: filter.id, weight: 0.75, mask: defaultMask("radial") },
    { source: detail.id, weight: -0.25, mask: null },
  ];
  blend.mask = defaultMask("from_field");
  blend.mask.source = generator.id;
  const modulate = defaultNode("modulate");
  modulate.source = blend.id;
  modulate.mask = defaultMask("noise");
  const volume = defaultNode("volume");
  volume.terrain.ops = [
    { op: "add", shape: { kind: "box", center: [0, 0, 0], half: [64, 64, 64], rotation: [0, 0, 0] } },
    { op: "subtract", shape: { kind: "sphere", center: [0, 0, 0], radius: 32 }, k: 4 },
    { op: "noise", amplitude: 8, frequency: 0.02, seed: 4, kind: "simplex" },
    { op: "shift", offset: [16, 0, 0] },
    { op: "scale", factor: 0.5 },
  ];
  for (const node of [generator, filter, detail, blend, modulate, volume]) {
    for (const spec of nodeFields(node)) setField(node, spec, spec.def);
  }
  return { resolution: [256, 128], cell_size: 32, nodes: [generator, filter, detail, blend, modulate, volume], output: blend.id };
}


describe("recipe json", () => {
  it("emits the serde tags the core deserializes", () => {
    const recipe = richRecipe();
    const out = JSON.parse(recipeJson(recipe)) as Record<string, unknown>;
    expect(out.output).toBe(recipe.output);
    const nodes = out.nodes as Record<string, unknown>[];
    expect(nodes[0].node).toBe("generator");
    const generator = nodes[0].generator as Record<string, unknown>;
    expect(generator.kind).toBe("mountains");
    const shape = generator.shape as Record<string, unknown>;
    expect(Object.keys(shape).sort()).toEqual([
      "anisotropy", "fractal", "kind", "rotation_deg", "warp", "warp_seed",
    ]);
    expect(Object.keys(shape.fractal as object).sort()).toEqual([
      "frequency", "gain", "lacunarity", "octaves", "offset", "weight",
    ]);
    expect(nodes[1].node).toBe("filter");
    expect((nodes[1].op as Record<string, unknown>).op).toBe("hydraulic");
    expect(nodes[3].node).toBe("blend");
    expect((nodes[3].terms as unknown[]).length).toBe(2);
    expect(((nodes[3].terms as Record<string, unknown>[])[0].mask as Record<string, unknown>).mask).toBe("radial");
    expect(((nodes[3].terms as Record<string, unknown>[])[0].mask as Record<string, unknown>).center).toEqual([0, 0]);
    expect((nodes[3].mask as Record<string, unknown>).mask).toBe("from_field");
    expect((nodes[3].mask as Record<string, unknown>).source).toBe(recipe.nodes[0].id);
    expect((nodes[3].mask as Record<string, unknown>).kind).toBe("slope");
    expect(nodes[4].node).toBe("modulate");
    expect((nodes[4].mask as Record<string, unknown>).mask).toBe("noise");
    expect(nodes[5].node).toBe("volume");
    const terrain = nodes[5].terrain as Record<string, unknown>;
    expect(terrain.chunks).toEqual([32, 32, 32]);
    const ops = terrain.ops as Record<string, unknown>[];
    expect(ops.map((o) => o.op)).toEqual(["add", "subtract", "noise", "shift", "scale"]);
    expect((ops[0].shape as Record<string, unknown>).shape).toBe("box");
    expect((ops[1].shape as Record<string, unknown>).shape).toBe("sphere");
    expect(ops[2].kind).toBe("simplex");
  });

  it("emits a heightmap generator with its samples", () => {
    const node = defaultNode("generator", "heightmap");
    node.generator.samples = [0, 16384, 32768, 65535];
    const out = JSON.parse(recipeJson({ resolution: [4, 4], cell_size: 8, nodes: [node], output: node.id })) as {
      nodes: { generator: Record<string, unknown> }[];
    };
    expect(out.nodes[0].generator.kind).toBe("heightmap");
    expect(out.nodes[0].generator.samples).toEqual([0, 16384, 32768, 65535]);
    expect("shape" in out.nodes[0].generator).toBe(false);
  });

  it("survives a full round trip unchanged", () => {
    const first = recipeJson(richRecipe());
    expect(recipeJson(parseRecipe(first))).toBe(first);
    const back = parseRecipe(first);
    expect(back.resolution).toEqual([256, 128]);
    expect(back.cell_size).toBe(32);
    expect(back.nodes.map((n) => n.node)).toEqual([
      "generator", "filter", "detail", "blend", "modulate", "volume",
    ]);
    expect(back.nodes[3].terms[0].mask?.kind).toBe("radial");
    expect(back.nodes[5].terrain.ops[2].op).toBe("noise");
  });

  it("round-trips the default recipe", () => {
    const first = recipeJson(defaultRecipe());
    expect(recipeJson(parseRecipe(first))).toBe(first);
  });

  it("stores the graph without the grid the form owns", () => {
    const stored = JSON.parse(storedRecipeJson(richRecipe())) as Record<string, unknown>;
    expect(stored.resolution).toBeUndefined();
    expect(stored.cell_size).toBeUndefined();
    expect((stored.nodes as unknown[]).length).toBe(6);
    expect(stored.output).toBeTruthy();
  });

  it("fills in defaults for a partial or empty document", () => {
    const back = parseRecipe("{}");
    expect(back.resolution).toEqual([512, 512]);
    expect(back.cell_size).toBe(64);
    expect(back.nodes).toHaveLength(0);
    expect(back.output).toBe("");
    const partial = parseRecipe(JSON.stringify({ nodes: [{ node: "generator", generator: { kind: "rivers" } }] }));
    expect(partial.nodes).toHaveLength(1);
    expect(getField(partial.nodes[0], GENERATOR_FIELDS.rivers.find((s) => s.path === "incision") as FieldSpec)).toBe(0.5);
    expect(partial.output).toBe(partial.nodes[0].id);
  });

  it("drops a node with an unknown tag and coerces known fields to their type", () => {
    const back = parseRecipe(JSON.stringify({
      nodes: [
        { id: "a", node: "nonsense" },
        {
          id: "b",
          node: "generator",
          generator: { kind: "rivers", incision: "0.9", lakes: 0, seed: 3.7, nonsense: 5 },
        },
      ],
    }));
    expect(back.nodes).toHaveLength(1);
    const spec = (path: string): FieldSpec => GENERATOR_FIELDS.rivers.find((s) => s.path === path) as FieldSpec;
    expect(getField(back.nodes[0], spec("incision"))).toBe(0.9);
    expect(getField(back.nodes[0], spec("lakes"))).toBe(0);
    expect(getField(back.nodes[0], spec("seed"))).toBe(4);
    expect("nonsense" in back.nodes[0].generator).toBe(false);
  });

  it("rejects an enum value the core does not declare", () => {
    const back = parseRecipe(JSON.stringify({
      nodes: [{ id: "a", node: "generator", generator: { kind: "dunes", pattern: "spiral" } }],
    }));
    expect(getField(back.nodes[0], GENERATOR_FIELDS.dunes.find((s) => s.path === "pattern") as FieldSpec)).toBe("transverse");
    const badFractal = parseRecipe(JSON.stringify({
      nodes: [{ id: "a", node: "generator", generator: { kind: "dunes", shape: { kind: "swirl" } } }],
    }));
    expect((badFractal.nodes[0].generator.shape as { kind: string }).kind).toBe("fbm");
    expect(FRACTAL_KINDS).toContain("jordan_turbulence");
  });

  it("clamps a hostile grid", () => {
    const back = parseRecipe(JSON.stringify({ resolution: [0, -5], cell_size: 0 }));
    expect(back.resolution).toEqual([1, 1]);
    expect(back.cell_size).toBe(0.01);
  });

  it("rejects json that is not a recipe", () => {
    expect(() => parseRecipe("not json")).toThrow(RecipeError);
    expect(() => parseRecipe("[]")).toThrow(/not an object/);
  });
});

describe("recipe validation", () => {
  it("accepts the default and a full graph", () => {
    expect(validateRecipe(defaultRecipe())).toEqual([]);
    expect(validateRecipe(richRecipe())).toEqual([]);
  });

  it("reports a missing input, a self reference and a duplicate id", () => {
    const a = defaultNode("generator");
    const b = defaultNode("filter");
    b.source = "nope";
    const c = defaultNode("filter", "terrace");
    c.id = a.id;
    c.source = c.id;
    const issues = validateRecipe({ resolution: [64, 64], cell_size: 8, nodes: [a, b, c], output: a.id });
    const messages = issues.map((i) => i.message);
    expect(messages).toContain("duplicate node id");
    expect(messages).toContain("input nope does not exist");
    expect(messages).toContain("node reads from itself");
  });

  it("reports a source that comes after the node reading it", () => {
    const filter = defaultNode("filter");
    const generator = defaultNode("generator", "dunes");
    filter.source = generator.id;
    const issues = validateRecipe({ resolution: [64, 64], cell_size: 8, nodes: [filter, generator], output: generator.id });
    expect(issues.map((i) => i.message)).toContain(`input ${generator.id} comes after it in the list`);
  });

  it("reports a filter with no source, a one-term blend and a maskless modulate", () => {
    const modulate = defaultNode("modulate");
    modulate.mask = null;
    const issues = validateRecipe({
      resolution: [64, 64],
      cell_size: 8,
      nodes: [defaultNode("generator"), defaultNode("filter"), defaultNode("blend"), modulate],
      output: "",
    });
    const messages = issues.map((i) => i.message);
    expect(messages).toContain("filter has no source node");
    expect(messages).toContain("a blend needs at least two terms");
    expect(messages).toContain("a modulate needs a mask");
    expect(messages).toContain("the recipe has no output node");
  });

  it("reports an empty recipe, a bad output and an oversized grid", () => {
    expect(validateRecipe({ resolution: [64, 64], cell_size: 8, nodes: [], output: "" }).map((i) => i.message))
      .toContain("recipe has no nodes");
    const a = defaultNode("generator");
    expect(validateRecipe({ resolution: [64, 64], cell_size: 8, nodes: [a], output: "nope" }).map((i) => i.message))
      .toContain("output nope does not exist");
    const huge = validateRecipe({ resolution: [65536, 65536], cell_size: 1, nodes: [a], output: a.id });
    expect(huge.map((i) => i.message)).toContain("resolution is larger than 268435456 samples");
  });

  it("reports more nodes than the evaluator will walk", () => {
    const nodes: Recipe["nodes"] = [];
    for (let k = 0; k < 300; k++) nodes.push(defaultNode("generator"));
    const issues = validateRecipe({ resolution: [8, 8], cell_size: 8, nodes, output: nodes[0].id });
    expect(issues.map((i) => i.message)).toContain("recipe has more than 256 nodes");
  });

  it("reports every node id a node reads from, masks included", () => {
    const generator = defaultNode("generator");
    const detail = defaultNode("detail");
    const blend = defaultNode("blend");
    blend.terms = [
      { source: generator.id, weight: 1, mask: defaultMask("from_field") },
      { source: detail.id, weight: 1, mask: defaultMask("noise") },
    ];
    blend.terms[0].mask!.source = generator.id;
    blend.mask = defaultMask("from_field");
    blend.mask.source = detail.id;
    expect(inputsOf(blend)).toEqual([generator.id, detail.id, generator.id, detail.id]);
    const filter = defaultNode("filter");
    filter.source = generator.id;
    expect(inputsOf(filter)).toEqual([generator.id]);
    expect(inputsOf(detail)).toEqual([]);
  });

  it("keeps every node kind reachable from a default node", () => {
    for (const kind of NODE_KINDS as NodeKind[]) {
      const node = defaultNode(kind);
      expect(node.node).toBe(kind);
      expect(node.id.length).toBeGreaterThan(0);
    }
  });
});

describe("generate request", () => {
  it("nests the recipe the way the core's terrain_generate reads it", () => {
    const recipe = defaultRecipe();
    const out = JSON.parse(generateRequestJson(recipe)) as { recipe: { nodes: unknown[]; output: string }; resolution: number[]; cell_size: number };
    expect(out.resolution).toEqual(recipe.resolution);
    expect(out.cell_size).toBe(recipe.cell_size);
    expect(out.recipe.output).toBe(recipe.output);
    expect(out.recipe.nodes).toEqual(JSON.parse(storedRecipeJson(recipe)).nodes);
  });
});

describe("heightmap generator", () => {
  it("is refused until an image supplies its samples, then accepted", () => {
    const node = defaultNode("generator", "heightmap");
    const recipe = { ...defaultRecipe(), nodes: [node], output: node.id };
    expect(validateRecipe(recipe).map((i) => i.message)).toContain("the heightmap has no image; load one");
    const rgba = new Uint8Array([0, 0, 0, 255, 255, 0, 0, 255, 128, 0, 0, 255, 1, 0, 0, 255]);
    const grid = heightmapFromImage(2, 2, rgba);
    expect(grid.samples).toEqual([0, 65535, 128 * 257, 257]);
    const loaded = { ...recipe, nodes: [{ ...node, generator: { ...node.generator, ...grid } }] };
    expect(validateRecipe(loaded).map((i) => i.message)).toContain("the heightmap is 2 x 2 but the grid is 512 x 512");
    expect(validateRecipe({ ...loaded, resolution: [2, 2] })).toEqual([]);
  });
});
