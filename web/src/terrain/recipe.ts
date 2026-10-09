/**
 * TypeScript mirror of the terrain recipe graph. Every field name and tag here
 * matches the serde shape the core deserializes, so a form built from these
 * specs emits JSON the core accepts: `Generator` is tagged on `kind`, `NodeKind`
 * on `node`, `FilterOp` and `SdfOp` on `op`, `MaskSource` on `mask` and
 * `SdfShape` on `shape`.
 *
 * A field spec is addressed by a dotted path into the node, so one table drives
 * every widget without a hand-written control per parameter. `def` is the value
 * the core's own `Default` impl uses, so a new node starts where the core would.
 */

export type FractalKind = "fbm" | "ridged" | "billow" | "hybrid" | "swiss_turbulence" | "jordan_turbulence";
export const FRACTAL_KINDS: FractalKind[] = ["fbm", "ridged", "billow", "hybrid", "swiss_turbulence", "jordan_turbulence"];

export type BlendMode = "replace" | "add" | "subtract" | "multiply" | "screen" | "overlay" | "max" | "min" | "mix";
export const BLEND_MODES: BlendMode[] = ["replace", "add", "subtract", "multiply", "screen", "overlay", "max", "min", "mix"];

export type ResampleFilter = "nearest" | "bilinear" | "bicubic";
export const RESAMPLE_FILTERS: ResampleFilter[] = ["nearest", "bilinear", "bicubic"];

export type ToneCurve = "lift" | "contrast" | "flatten" | "invert";
export const TONE_CURVES: ToneCurve[] = ["lift", "contrast", "flatten", "invert"];

export type FieldMaskKind = "slope" | "height" | "curvature" | "flow" | "dry";
export const FIELD_MASK_KINDS: FieldMaskKind[] = ["slope", "height", "curvature", "flow", "dry"];

export type MaskKind = "constant" | "radial" | "noise" | "from_field";
export const MASK_KINDS: MaskKind[] = ["constant", "radial", "noise", "from_field"];

export type DunePattern = "transverse" | "barchan" | "star";
export const DUNE_PATTERNS: DunePattern[] = ["transverse", "barchan", "star"];

export type NoiseKind3 = "perlin" | "simplex" | "value" | "symmetric";
export const NOISE_KINDS_3: NoiseKind3[] = ["perlin", "simplex", "value", "symmetric"];

export type SdfShapeKind = "sphere" | "capsule" | "box" | "cylinder" | "cone" | "torus" | "half_space";
export const SDF_SHAPE_KINDS: SdfShapeKind[] = ["sphere", "capsule", "box", "cylinder", "cone", "torus", "half_space"];

export type SdfOpKind = "add" | "union" | "subtract" | "intersect" | "noise" | "shift" | "scale";
export const SDF_OP_KINDS: SdfOpKind[] = ["add", "union", "subtract", "intersect", "noise", "shift", "scale"];

export type FilterOpKind =
  | "smooth" | "sharpen" | "terrace" | "clamp" | "curve" | "tone" | "offset"
  | "scale_about" | "thermal" | "hydraulic" | "pipe" | "noise" | "resample";
export const FILTER_OP_KINDS: FilterOpKind[] = [
  "smooth", "sharpen", "terrace", "clamp", "curve", "tone", "offset",
  "scale_about", "thermal", "hydraulic", "pipe", "noise", "resample",
];

export type NodeKind = "generator" | "filter" | "blend" | "modulate" | "detail" | "volume";
export const NODE_KINDS: NodeKind[] = ["generator", "filter", "blend", "modulate", "detail", "volume"];

export type GeneratorKind =
  | "rolling_hills" | "mountains" | "alpine" | "plateau" | "canyon" | "badlands"
  | "dunes" | "islands" | "volcanic" | "craters" | "glacial" | "rivers"
  | "coastline" | "farmland" | "arena" | "pits" | "heightmap";
export const GENERATOR_KINDS: GeneratorKind[] = [
  "rolling_hills", "mountains", "alpine", "plateau", "canyon", "badlands",
  "dunes", "islands", "volcanic", "craters", "glacial", "rivers",
  "coastline", "farmland", "arena", "pits", "heightmap",
];

export type FieldType = "float" | "int" | "bool" | "enum" | "vec2" | "vec3" | "string";
export type FieldValue = number | boolean | string | number[];

export interface FieldSpec {
  /** Dotted path into the node, e.g. `base` or `shape.fractal.octaves`. */
  path: string;
  type: FieldType;
  label: string;
  def: FieldValue;
  min?: number;
  max?: number;
  step?: number;
  choices?: string[];
}

const f = (path: string, label: string, def: number, min: number, max: number, step = 0.01): FieldSpec =>
  ({ path, type: "float", label, def, min, max, step });
const i = (path: string, label: string, def: number, min: number, max: number): FieldSpec =>
  ({ path, type: "int", label, def, min, max, step: 1 });
const b = (path: string, label: string, def = false): FieldSpec => ({ path, type: "bool", label, def });
const e = (path: string, label: string, choices: string[], def = choices[0]): FieldSpec =>
  ({ path, type: "enum", label, def, choices });
const v2 = (path: string, label: string, def: [number, number] = [0, 0]): FieldSpec =>
  ({ path, type: "vec2", label, def });
const v3 = (path: string, label: string, def: [number, number, number] = [0, 0, 0]): FieldSpec =>
  ({ path, type: "vec3", label, def });
const s = (path: string, label: string, def = ""): FieldSpec => ({ path, type: "string", label, def });

const INCH = 1e6;
const CELLS = 4096;
const U32 = 4294967295;

/** The noise shape every generator but the analytic ones carries. */
const SHAPE_FIELDS: FieldSpec[] = [
  e("shape.kind", "Fractal", FRACTAL_KINDS, "fbm"),
  i("shape.fractal.octaves", "Octaves", 6, 1, 24),
  f("shape.fractal.frequency", "Frequency", 1.6, 0.000001, 64, 0.001),
  f("shape.fractal.lacunarity", "Lacunarity", 2, 1, 8),
  f("shape.fractal.gain", "Gain", 0.5, 0, 1),
  f("shape.fractal.offset", "Offset", 1, -4, 4),
  f("shape.fractal.weight", "Weight", 1, -4, 4),
  f("shape.anisotropy", "Anisotropy", 0, -1, 1),
  f("shape.rotation_deg", "Rotation (deg)", 0, -360, 360, 1),
  f("shape.warp", "Domain warp", 0, 0, 8),
  i("shape.warp_seed", "Warp seed", 0, 0, U32),
];

/** Every generator's own fields, in the order the form shows them. */
export const GENERATOR_FIELDS: Record<GeneratorKind, FieldSpec[]> = {
  rolling_hills: [
    f("base", "Base", 64, -INCH, INCH, 1), f("amplitude", "Amplitude", 320, -INCH, INCH, 1), i("seed", "Seed", 1, 0, U32),
    ...SHAPE_FIELDS,
  ],
  mountains: [
    f("base", "Base", 0, -INCH, INCH, 1), f("amplitude", "Amplitude", 2400, -INCH, INCH, 1), i("seed", "Seed", 2, 0, U32),
    f("ridged_mix", "Ridged mix", 0.7, 0, 1), f("massif", "Massif", 0.45, -1, 2), ...SHAPE_FIELDS,
  ],
  alpine: [
    f("base", "Base", 0, -INCH, INCH, 1), f("amplitude", "Amplitude", 3000, -INCH, INCH, 1), i("seed", "Seed", 3, 0, U32),
    f("ridged_mix", "Ridged mix", 0.75, 0, 1), f("massif", "Massif", 0.5, -1, 2),
    i("thermal_iterations", "Thermal passes", 12, 0, 256), f("talus_angle", "Talus angle", 38, 0, 90, 0.1),
    i("erosion_droplets", "Erosion droplets", 20000, 0, 4000000), ...SHAPE_FIELDS,
  ],
  plateau: [
    f("base", "Base", 200, -INCH, INCH, 1), f("amplitude", "Amplitude", 900, -INCH, INCH, 1), i("seed", "Seed", 4, 0, U32),
    f("steps", "Steps", 5, 0.01, 512), f("sharpness", "Sharpness", 0.85, 0, 1), f("tilt", "Tilt", 0.6, -8, 8),
    ...SHAPE_FIELDS,
  ],
  canyon: [
    f("base", "Base", 400, -INCH, INCH, 1), f("amplitude", "Amplitude", 1200, -INCH, INCH, 1), i("seed", "Seed", 5, 0, U32),
    i("channels", "Channels", 2, 0, 32), f("channel_width", "Channel width", 6, 0.01, CELLS, 0.01),
    f("channel_depth", "Channel depth", 0.55, 0, 4), f("meander", "Meander", 3, 0, 64, 0.01),
    f("steps", "Steps", 7, 0.01, 512), ...SHAPE_FIELDS,
  ],
  badlands: [
    f("base", "Base", 100, -INCH, INCH, 1), f("amplitude", "Amplitude", 1000, -INCH, INCH, 1), i("seed", "Seed", 6, 0, U32),
    f("drainage", "Drainage", 3, 0, 64, 0.01), f("steps", "Steps", 9, 0.01, 512), ...SHAPE_FIELDS,
  ],
  dunes: [
    f("base", "Base", 0, -INCH, INCH, 1), f("amplitude", "Amplitude", 260, -INCH, INCH, 1), i("seed", "Seed", 7, 0, U32),
    e("pattern", "Pattern", DUNE_PATTERNS, "transverse"), f("wind_deg", "Wind angle", 35, -360, 360, 1),
    f("wavelength", "Wavelength", 24, 0.01, CELLS, 0.01), f("crest_sharpness", "Crest sharpness", 0.6, 0, 1),
    ...SHAPE_FIELDS,
  ],
  islands: [
    f("base", "Base", 0, -INCH, INCH, 1), f("amplitude", "Amplitude", 700, -INCH, INCH, 1), i("seed", "Seed", 8, 0, U32),
    i("centres", "Centres", 3, 1, 64), f("radius", "Radius", 34, 0.01, CELLS, 0.01), f("shelf", "Shelf", 18, 0, CELLS, 0.01),
    f("water_level", "Water level", 0.18, 0, 2), ...SHAPE_FIELDS,
  ],
  volcanic: [
    f("base", "Base", 0, -INCH, INCH, 1), f("amplitude", "Amplitude", 1800, -INCH, INCH, 1), i("seed", "Seed", 9, 0, U32),
    f("radius", "Radius", 40, 0.01, CELLS, 0.01), f("caldera", "Caldera", 12, 0, CELLS, 0.01),
    f("caldera_depth", "Caldera depth", 0.35, 0, 2), i("flows", "Flows", 5, 0, 64), f("flank_noise", "Flank noise", 0.25, 0, 1),
  ],
  craters: [
    f("base", "Base", 100, -INCH, INCH, 1), f("amplitude", "Amplitude", 0, -INCH, INCH, 1), i("seed", "Seed", 10, 0, U32),
    i("count", "Count", 40, 1, 4096), f("max_radius", "Max radius", 22, 0.01, CELLS, 0.01), f("size_decay", "Size decay", 0.55, 0.01, 8),
    f("depth", "Depth", 0.35, 0, 2), f("rim", "Rim", 0.22, 0, 2), f("background", "Background", 120, -INCH, INCH, 1),
  ],
  glacial: [
    f("base", "Base", 200, -INCH, INCH, 1), f("amplitude", "Amplitude", 900, -INCH, INCH, 1), i("seed", "Seed", 11, 0, U32),
    f("ridge_height", "Ridge height", 0.6, 0, 2), f("floor_width", "Floor width", 5, 0.01, CELLS, 0.01),
    f("wall_width", "Wall width", 12, 0.01, CELLS, 0.01), i("smoothing", "Smoothing", 4, 0, 64), ...SHAPE_FIELDS,
  ],
  rivers: [
    f("base", "Base", 300, -INCH, INCH, 1), f("amplitude", "Amplitude", 1000, -INCH, INCH, 1), i("seed", "Seed", 12, 0, U32),
    f("incision", "Incision", 0.5, 0, 2), f("width", "Width", 3, 0.01, CELLS, 0.01), f("lakes", "Lakes", 0.02, 0, 1),
    i("smoothing", "Smoothing", 2, 0, 64), ...SHAPE_FIELDS,
  ],
  coastline: [
    f("base", "Base", 0, -INCH, INCH, 1), f("amplitude", "Amplitude", 700, -INCH, INCH, 1), i("seed", "Seed", 13, 0, U32),
    f("bearing_deg", "Bearing (deg)", 20, -360, 360, 1), f("meander", "Meander", 5, 0, 64, 0.01),
    f("water_level", "Water level", 0.22, 0, 2), f("cliff", "Cliff", 0.8, 0, 2), ...SHAPE_FIELDS,
  ],
  farmland: [
    f("base", "Base", 100, -INCH, INCH, 1), f("amplitude", "Amplitude", 400, -INCH, INCH, 1), i("seed", "Seed", 14, 0, U32),
    f("step", "Step", 24, 0.01, CELLS, 0.01), f("flatness", "Flatness", 0.9, 0, 1), f("bund", "Bund", 4, 0, CELLS, 0.01),
  ],
  arena: [
    f("base", "Base", 0, -INCH, INCH, 1), f("amplitude", "Amplitude", 500, -INCH, INCH, 1), i("seed", "Seed", 15, 0, U32),
    f("rim_start", "Rim start", 0.78, 0, 1), f("berm", "Berm", 260, 0, INCH, 1), f("berm_sharpness", "Berm sharpness", 0.7, 0, 1),
    f("floor_relief", "Floor relief", 0.08, 0, 1),
  ],
  pits: [
    f("base", "Base", 100, -INCH, INCH, 1), f("amplitude", "Amplitude", 500, -INCH, INCH, 1), i("seed", "Seed", 16, 0, U32),
    i("count", "Count", 24, 1, 4096), f("max_radius", "Max radius", 18, 0.01, CELLS, 0.01), f("depth", "Depth", 0.7, 0, 2),
    f("lip", "Lip", 0.1, 0, 2), b("radius_in_cells", "Radius in cells", true),
  ],
  heightmap: [
    i("width", "Width", 2, 2, 8192), i("height", "Height", 2, 2, 8192),
    f("cell_size", "Cell size", 8, 0.001, INCH, 0.001), f("z_scale", "Z scale", 4096, -INCH, INCH, 1),
    f("z_bias", "Z bias", 0, -INCH, INCH, 1), i("seed", "Seed", 0, 0, U32),
  ],
};

export const FILTER_OP_FIELDS: Record<FilterOpKind, FieldSpec[]> = {
  smooth: [i("radius", "Radius", 1, 0, 256), i("passes", "Passes", 1, 1, 64)],
  sharpen: [f("amount", "Amount", 0.5, -8, 8), i("radius", "Radius", 1, 0, 256)],
  terrace: [f("step", "Step", 24, 0.01, CELLS, 0.01), f("sharpness", "Sharpness", 0.5, 0, 1)],
  clamp: [f("lo", "Low", 0, -INCH, INCH, 1), f("hi", "High", 1024, -INCH, INCH, 1)],
  curve: [f("gamma", "Gamma", 1, 0.01, 16, 0.01), b("invert", "Invert")],
  tone: [e("curve", "Curve", TONE_CURVES, "lift")],
  offset: [f("delta", "Delta", 0, -INCH, INCH, 1)],
  scale_about: [f("base", "Base", 0, -INCH, INCH, 1), f("factor", "Factor", 1, -16, 16, 0.01)],
  thermal: [
    i("iterations", "Iterations", 8, 1, 512), f("talus_angle", "Talus angle", 34, 0, 90, 0.1),
    f("rate", "Rate", 0.5, 0, 1), f("talus", "Talus", 0, 0, CELLS, 0.01),
  ],
  hydraulic: [
    i("droplets", "Droplets", 40000, 1, 4000000), i("seed", "Seed", 0, 0, U32),
    f("inertia", "Inertia", 0.05, 0, 1), f("capacity", "Capacity", 4, 0.001, 100, 0.001),
    f("erode_speed", "Erode speed", 0.3, 0, 4, 0.001), f("deposit_speed", "Deposit speed", 0.3, 0, 4, 0.001),
    f("evaporation", "Evaporation", 0.02, 0, 1), i("radius", "Radius", 3, 1, 64),
  ],
  pipe: [
    i("iterations", "Iterations", 4, 1, 256), f("rain", "Rain", 1, 0, 4, 0.001),
    f("solubility", "Solubility", 1, 0, 4, 0.001), f("deposition", "Deposition", 1, 0, 4, 0.001),
  ],
  noise: [
    i("seed", "Seed", 0, 0, U32), f("frequency", "Frequency", 0.01, 0.000001, 16, 0.0001),
    i("octaves", "Octaves", 5, 1, 24), f("gain", "Gain", 0.5, 0, 1), f("lacunarity", "Lacunarity", 2, 1, 8),
    f("amplitude", "Amplitude", 64, -INCH, INCH, 1),
  ],
  resample: [i("width", "Width", 2, 2, 8192), i("height", "Height", 2, 2, 8192), e("filter", "Filter", RESAMPLE_FILTERS, "bilinear")],
};

export const MASK_FIELDS: Record<MaskKind, FieldSpec[]> = {
  constant: [f("value", "Value", 1, -INCH, INCH, 0.01)],
  radial: [
    v2("center", "Centre"), f("inner", "Inner", 0, 0, CELLS, 1), f("outer", "Outer", 1024, 1, CELLS, 1),
    b("invert", "Invert"),
  ],
  noise: [
    i("seed", "Seed", 0, 0, U32), f("frequency", "Frequency", 0.01, 0.000001, 16, 0.0001),
    i("octaves", "Octaves", 4, 1, 24), f("contrast", "Contrast", 1, 0, 16, 0.01), b("invert", "Invert"),
  ],
  from_field: [s("source", "Source node"), e("field", "Field", FIELD_MASK_KINDS, "slope")],
};

export const SDF_SHAPE_FIELDS: Record<SdfShapeKind, FieldSpec[]> = {
  sphere: [v3("center", "Centre"), f("radius", "Radius", 64, 0.01, CELLS, 0.01)],
  capsule: [v3("a", "From"), v3("b", "To"), f("radius", "Radius", 32, 0.01, CELLS, 0.01)],
  box: [v3("center", "Centre"), v3("half", "Half"), v3("rotation", "Pitch yaw roll")],
  cylinder: [v3("base", "Base"), f("height", "Height", 128, 0.01, CELLS, 0.01), f("radius", "Radius", 64, 0.01, CELLS, 0.01)],
  cone: [v3("base", "Base"), f("height", "Height", 128, 0.01, CELLS, 0.01), f("radius", "Radius", 64, 0.01, CELLS, 0.01)],
  torus: [v3("center", "Centre"), f("major", "Major", 96, 0.01, CELLS, 0.01), f("minor", "Minor", 24, 0.01, CELLS, 0.01)],
  half_space: [v3("point", "Point"), v3("normal", "Normal")],
};

export const SDF_OP_FIELDS: Record<SdfOpKind, FieldSpec[]> = {
  add: [],
  union: [f("k", "Blend radius", 0, 0, CELLS, 0.01)],
  subtract: [f("k", "Blend radius", 0, 0, CELLS, 0.01)],
  intersect: [f("k", "Blend radius", 0, 0, CELLS, 0.01)],
  noise: [
    f("amplitude", "Amplitude", 32, -INCH, INCH, 0.01), f("frequency", "Frequency", 0.01, 0.000001, 16, 0.0001),
    i("seed", "Seed", 0, 0, U32), e("kind", "Noise", NOISE_KINDS_3, "perlin"),
  ],
  shift: [v3("offset", "Offset")],
  scale: [f("factor", "Factor", 1, -16, 16, 0.01)],
};

export const DETAIL_FIELDS: FieldSpec[] = [
  i("seed", "Seed", 0, 0, U32),
  f("frequency", "Frequency", 0.01, 0.000001, 16, 0.0001),
  i("octaves", "Octaves", 5, 1, 24),
  f("amplitude", "Amplitude", 64, -INCH, INCH, 1),
];

export const VOLUME_FIELDS: FieldSpec[] = [
  i("chunks.0", "Chunks x", 32, 2, 512),
  i("chunks.1", "Chunks y", 32, 2, 512),
  i("chunks.2", "Chunks z", 32, 2, 512),
  f("voxel_size", "Voxel size", 16, 0.01, INCH, 0.01),
  v3("origin", "Origin"),
];

export const BLEND_FIELDS: FieldSpec[] = [e("mode", "Mode", BLEND_MODES, "mix")];
export const MODULATE_FIELDS: FieldSpec[] = [f("strength", "Strength", 1, 0, 4, 0.01)];

/**
 * The object a node's field paths are relative to. A generator's specs name
 * its own fields, a filter's name its operator's, and the rest name the node.
 */
export function fieldRoot(node: RecipeNode): Record<string, unknown> {
  if (node.node === "generator") return node.generator as unknown as Record<string, unknown>;
  if (node.node === "filter") return node.op as unknown as Record<string, unknown>;
  return node as unknown as Record<string, unknown>;
}

/** Reads one field of a mask, whose own tag lives on `kind`. */
export function getMaskField(mask: MaskNode, spec: FieldSpec): FieldValue {
  const raw = mask[spec.path];
  if (typeof raw === "number") return spec.type === "int" ? Math.round(raw) : raw;
  if (typeof raw === "boolean" || typeof raw === "string") return raw;
  if (spec.type === "vec2" || spec.type === "vec3") {
    const want = spec.type === "vec2" ? 2 : 3;
    const arr = Array.isArray(raw) ? raw : [];
    const out: number[] = [];
    for (let k = 0; k < want; k++) out.push(num(arr[k], 0));
    return out;
  }
  return defaultFor(spec);
}

/** Writes one field of a mask immutably, clamped to the spec's range. */
export function setMaskField(mask: MaskNode, spec: FieldSpec, value: FieldValue): MaskNode {
  const next: MaskNode = { ...mask };
  if (spec.type === "bool") next[spec.path] = value === true || value === 1 || value === "1";
  else if (spec.type === "int" || spec.type === "float") {
    const lo = spec.min ?? -INCH;
    const hi = spec.max ?? INCH;
    const n = num(value, 0);
    next[spec.path] = spec.type === "int" ? Math.round(clamp(n, lo, hi)) : clamp(n, lo, hi);
  } else if (spec.type === "vec2" || spec.type === "vec3") next[spec.path] = tripleOf(value, spec.type === "vec2" ? 2 : 3);
  else if (spec.type === "enum") next[spec.path] = enumOr(value, spec.choices ?? [], String(defaultFor(spec)));
  else next[spec.path] = typeof value === "string" ? value : String(value ?? "");
  return next;
}

/** Reads one field of an SDF operation. */
export function getSdfOpField(op: SdfOpNode, spec: FieldSpec): FieldValue {
  const raw = op[spec.path];
  if (typeof raw === "number") return spec.type === "int" ? Math.round(raw) : raw;
  if (typeof raw === "boolean" || typeof raw === "string") return raw;
  if (spec.type === "vec3") return parseTriple(raw, [0, 0, 0]);
  return defaultFor(spec);
}

/** Writes one field of an SDF operation immutably. */
export function setSdfOpField(op: SdfOpNode, spec: FieldSpec, value: FieldValue): SdfOpNode {
  const next: SdfOpNode = { ...op };
  if (spec.type === "bool") next[spec.path] = value === true || value === 1 || value === "1";
  else if (spec.type === "int" || spec.type === "float") {
    const lo = spec.min ?? -INCH;
    const hi = spec.max ?? INCH;
    const n = num(value, 0);
    next[spec.path] = spec.type === "int" ? Math.round(clamp(n, lo, hi)) : clamp(n, lo, hi);
  } else if (spec.type === "vec3") next[spec.path] = parseTriple(value, [0, 0, 0]);
  else next[spec.path] = typeof value === "string" ? value : String(value ?? "");
  return next;
}

/** Reads one field of an SDF shape. */
export function getSdfShapeField(shape: SdfShapeNode, spec: FieldSpec): FieldValue {
  return getSdfOpField(shape as unknown as SdfOpNode, spec);
}

/** Writes one field of an SDF shape immutably. */
export function setSdfShapeField(shape: SdfShapeNode, spec: FieldSpec, value: FieldValue): SdfShapeNode {
  return setSdfOpField(shape as unknown as SdfOpNode, spec, value) as unknown as SdfShapeNode;
}

/** The scalar fields a node shows, from its own sub-kind. */
export function nodeFields(node: RecipeNode): FieldSpec[] {
  switch (node.node) {
    case "generator":
      return GENERATOR_FIELDS[node.generator.kind] ?? [];
    case "filter":
      return FILTER_OP_FIELDS[node.op.kind] ?? [];
    case "blend":
      return BLEND_FIELDS;
    case "modulate":
      return MODULATE_FIELDS;
    case "detail":
      return DETAIL_FIELDS;
    case "volume":
      return VOLUME_FIELDS;
    default:
      return [];
  }
}

function segments(path: string): string[] {
  return path.split(".");
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

function num(v: unknown, fallback: number): number {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  // A hand-edited recipe may quote a number; accept that, reject the rest.
  if (typeof v === "string" && v.trim() !== "") {
    const n = Number(v);
    if (Number.isFinite(n)) return n;
  }
  return fallback;
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}

function enumOr(v: unknown, choices: string[], fallback: string): string {
  return typeof v === "string" && choices.includes(v) ? v : fallback;
}

/** The spec's own default, with array defaults copied. */
export function defaultFor(spec: FieldSpec): FieldValue {
  return Array.isArray(spec.def) ? ([...spec.def] as number[]) : spec.def;
}

function tripleOf(value: unknown, want: 2 | 3): number[] {
  const arr = Array.isArray(value) ? value : [];
  const out: number[] = [];
  for (let k = 0; k < want; k++) out.push(num(arr[k], 0));
  return out;
}

/** Reads a dotted path out of the node's own sub-object, clamped to the spec. */
export function getField(node: RecipeNode, spec: FieldSpec): FieldValue {
  let current: unknown = fieldRoot(node);
  for (const key of segments(spec.path)) {
    if (!isRecord(current)) return defaultFor(spec);
    current = current[key];
  }
  if (typeof current === "number") return spec.type === "int" ? Math.round(current) : current;
  if (typeof current === "boolean") return spec.type === "bool" ? current : defaultFor(spec);
  if (typeof current === "string") {
    if (spec.type === "int" || spec.type === "float") return num(current, num(defaultFor(spec), 0));
    if (spec.type === "enum") return enumOr(current, spec.choices ?? [], String(defaultFor(spec)));
    return current;
  }
  if (spec.type === "vec2" || spec.type === "vec3") {
    const want = spec.type === "vec2" ? 2 : 3;
    const arr = Array.isArray(current) ? current : [];
    const out: number[] = [];
    for (let k = 0; k < want; k++) out.push(num(arr[k], 0));
    return out;
  }
  return defaultFor(spec);
}

/** Writes a dotted path immutably, clamping numbers to the spec's range. */
export function setField(node: RecipeNode, spec: FieldSpec, value: FieldValue): RecipeNode {
  const keys = segments(spec.path);
  const next = cloneNode(node);
  const root = fieldRoot(next);
  let current = root;
  for (let k = 0; k < keys.length - 1; k++) {
    const key = keys[k];
    const child = current[key];
    current[key] = isRecord(child) ? { ...child } : {};
    current = current[key] as Record<string, unknown>;
  }
  const last = keys[keys.length - 1];
  if (spec.type === "int" || spec.type === "float") {
    const lo = spec.min ?? -INCH;
    const hi = spec.max ?? INCH;
    current[last] = spec.type === "int" ? Math.round(clamp(num(value, 0), lo, hi)) : clamp(num(value, 0), lo, hi);
  } else if (spec.type === "bool") {
    current[last] = value === true || value === 1 || value === "1";
  } else if (spec.type === "vec2" || spec.type === "vec3") {
    current[last] = tripleOf(value, spec.type === "vec2" ? 2 : 3);
  } else if (spec.type === "enum") {
    current[last] = enumOr(value, spec.choices ?? [], String(defaultFor(spec)));
  } else {
    current[last] = typeof value === "string" ? value : String(value ?? "");
  }
  return next;
}

/** A fresh copy of a node, so an edit never mutates the caller's recipe. */
export function cloneNode(node: RecipeNode): RecipeNode {
  return JSON.parse(JSON.stringify(node)) as RecipeNode;
}

export interface FractalSpec {
  octaves: number;
  frequency: number;
  lacunarity: number;
  gain: number;
  offset: number;
  weight: number;
}

export interface ShapeSpec {
  fractal: FractalSpec;
  kind: FractalKind;
  anisotropy: number;
  rotation_deg: number;
  warp: number;
  warp_seed: number;
}

export interface GeneratorNode {
  kind: GeneratorKind;
  shape?: ShapeSpec;
  samples?: number[];
  [key: string]: FieldValue | ShapeSpec | undefined;
}

export interface FilterOpNode {
  kind: FilterOpKind;
  [key: string]: FieldValue | undefined;
}

export interface MaskNode {
  kind: MaskKind;
  [key: string]: FieldValue | undefined;
}

export interface SdfShapeNode {
  kind: SdfShapeKind;
  [key: string]: FieldValue | undefined;
}

export interface SdfOpNode {
  op: SdfOpKind;
  shape?: SdfShapeNode;
  [key: string]: FieldValue | SdfShapeNode | undefined;
}

export interface VolumeNode {
  chunks: [number, number, number];
  voxel_size: number;
  origin: [number, number, number];
  ops: SdfOpNode[];
}

export interface BlendTerm {
  source: string;
  weight: number;
  mask: MaskNode | null;
}

export interface RecipeNode {
  id: string;
  node: NodeKind;
  generator: GeneratorNode;
  source: string;
  op: FilterOpNode;
  terms: BlendTerm[];
  mask: MaskNode | null;
  mode: BlendMode;
  strength: number;
  seed: number;
  frequency: number;
  octaves: number;
  amplitude: number;
  terrain: VolumeNode;
}

export interface Recipe {
  /** Samples on each axis, which the core reads alongside the graph. */
  resolution: [number, number];
  cell_size: number;
  nodes: RecipeNode[];
  /** Node the heights come from. */
  output: string;
}

export function defaultShape(): ShapeSpec {
  return {
    fractal: { octaves: 6, frequency: 1.6, lacunarity: 2, gain: 0.5, offset: 1, weight: 1 },
    kind: "fbm",
    anisotropy: 0,
    rotation_deg: 0,
    warp: 0,
    warp_seed: 0,
  };
}

export function defaultMask(kind: MaskKind = "from_field"): MaskNode {
  switch (kind) {
    case "constant":
      return { kind, value: 1 };
    case "radial":
      return { kind, center: [0, 0], inner: 0, outer: 1024, invert: false };
    case "noise":
      return { kind, seed: 0, frequency: 0.01, octaves: 4, contrast: 1, invert: false };
    default:
      return { kind, source: "", field: "slope" };
  }
}

export function defaultSdfShape(kind: SdfShapeKind = "sphere"): SdfShapeNode {
  switch (kind) {
    case "capsule":
      return { kind, a: [-64, 0, 0], b: [64, 0, 0], radius: 32 };
    case "box":
      return { kind, center: [0, 0, 0], half: [64, 64, 64], rotation: [0, 0, 0] };
    case "cylinder":
      return { kind, base: [0, 0, 0], height: 128, radius: 64 };
    case "cone":
      return { kind, base: [0, 0, 0], height: 128, radius: 64 };
    case "torus":
      return { kind, center: [0, 0, 0], major: 96, minor: 24 };
    case "half_space":
      return { kind, point: [0, 0, 0], normal: [0, 0, 1] };
    default:
      return { kind, center: [0, 0, 0], radius: 64 };
  }
}

export function defaultSdfOp(op: SdfOpKind = "add"): SdfOpNode {
  switch (op) {
    case "union":
    case "subtract":
    case "intersect":
      return { op, shape: defaultSdfShape(), k: 0 };
    case "noise":
      return { op, amplitude: 32, frequency: 0.01, seed: 0, kind: "perlin" };
    case "shift":
      return { op, offset: [0, 0, 0] };
    case "scale":
      return { op, factor: 1 };
    default:
      return { op, shape: defaultSdfShape() };
  }
}

export function defaultVolume(): VolumeNode {
  return { chunks: [32, 32, 32], voxel_size: 16, origin: [0, 0, 0], ops: [defaultSdfOp("add")] };
}

export function defaultFilterOp(kind: FilterOpKind = "smooth"): FilterOpNode {
  const node: FilterOpNode = { kind };
  for (const spec of FILTER_OP_FIELDS[kind] ?? []) node[spec.path] = defaultFor(spec);
  return node;
}

export function defaultGenerator(kind: GeneratorKind = "rolling_hills"): GeneratorNode {
  const node: GeneratorNode = { kind };
  for (const spec of GENERATOR_FIELDS[kind] ?? []) {
    if (spec.path.startsWith("shape.")) continue;
    node[spec.path] = defaultFor(spec);
  }
  if (kind !== "heightmap") node.shape = defaultShape();
  return node;
}

let idCounter = 0;

export function nextNodeId(prefix = "n"): string {
  idCounter += 1;
  return `${prefix}${idCounter}`;
}

export function defaultNode(kind: NodeKind, subKind?: string): RecipeNode {
  const base: RecipeNode = {
    id: nextNodeId(),
    node: kind,
    generator: defaultGenerator((subKind as GeneratorKind) ?? "rolling_hills"),
    source: "",
    op: defaultFilterOp((subKind as FilterOpKind) ?? "smooth"),
    terms: [],
    mask: kind === "modulate" || kind === "blend" ? defaultMask("from_field") : null,
    mode: "mix",
    strength: 1,
    seed: 0,
    frequency: 0.01,
    octaves: 5,
    amplitude: 64,
    terrain: defaultVolume(),
  };
  let node = base;
  for (const spec of nodeFields(base)) node = setField(node, spec, defaultFor(spec));
  return node;
}

/** One rolling-hills generator at 512x512 samples, 64 inch cells. */
export function defaultRecipe(): Recipe {
  const node = defaultNode("generator", "rolling_hills");
  return { resolution: [512, 512], cell_size: 64, nodes: [node], output: node.id };
}

export class RecipeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "RecipeError";
  }
}

function parseShape(v: unknown): ShapeSpec {
  const out = defaultShape();
  if (!isRecord(v)) return out;
  if (isRecord(v.fractal)) {
    const fr = v.fractal;
    out.fractal = {
      octaves: Math.round(num(fr.octaves, out.fractal.octaves)),
      frequency: num(fr.frequency, out.fractal.frequency),
      lacunarity: num(fr.lacunarity, out.fractal.lacunarity),
      gain: num(fr.gain, out.fractal.gain),
      offset: num(fr.offset, out.fractal.offset),
      weight: num(fr.weight, out.fractal.weight),
    };
  }
  out.kind = enumOr(v.kind, FRACTAL_KINDS, out.kind) as FractalKind;
  out.anisotropy = num(v.anisotropy, out.anisotropy);
  out.rotation_deg = num(v.rotation_deg, out.rotation_deg);
  out.warp = num(v.warp, out.warp);
  out.warp_seed = Math.round(num(v.warp_seed, out.warp_seed));
  return out;
}

function parseTriple(v: unknown, fallback: [number, number, number]): [number, number, number] {
  const out = tripleOf(v, 3);
  return [num(out[0], fallback[0]), num(out[1], fallback[1]), num(out[2], fallback[2])];
}

/** Reads one scalar field out of a raw JSON object, coerced to its type. */
function readScalar(raw: unknown, spec: FieldSpec): FieldValue {
  if (spec.type === "bool") return raw === true || raw === 1;
  if (spec.type === "int") return Math.round(clamp(num(raw, num(defaultFor(spec), 0)), spec.min ?? -INCH, spec.max ?? INCH));
  if (spec.type === "float") return clamp(num(raw, num(defaultFor(spec), 0)), spec.min ?? -INCH, spec.max ?? INCH);
  if (spec.type === "enum") return enumOr(raw, spec.choices ?? [], String(defaultFor(spec)));
  return typeof raw === "string" ? raw : String(defaultFor(spec));
}

function parseGenerator(v: unknown): GeneratorNode {
  const kind = enumOr(isRecord(v) ? v.kind : null, GENERATOR_KINDS, "rolling_hills") as GeneratorKind;
  const node = defaultGenerator(kind);
  if (!isRecord(v)) return node;
  for (const spec of GENERATOR_FIELDS[kind]) {
    if (spec.path.startsWith("shape.")) continue;
    const raw = v[spec.path];
    if (raw !== undefined) node[spec.path] = readScalar(raw, spec);
  }
  if (kind === "heightmap") {
    // The samples are the bulk of the payload, so they are copied verbatim.
    const samples: number[] = [];
    if (Array.isArray(v.samples)) for (const n of v.samples) samples.push(clamp(Math.round(num(n, 0)), 0, 65535));
    node.samples = samples;
  } else {
    node.shape = parseShape(v.shape);
  }
  return node;
}

function parseFilterOp(v: unknown): FilterOpNode {
  const kind = enumOr(isRecord(v) ? v.op : null, FILTER_OP_KINDS, "smooth") as FilterOpKind;
  const node = defaultFilterOp(kind);
  if (!isRecord(v)) return node;
  for (const spec of FILTER_OP_FIELDS[kind] ?? []) {
    const raw = v[spec.path];
    if (raw !== undefined) node[spec.path] = readScalar(raw, spec);
  }
  return node;
}

function parseMask(v: unknown): MaskNode | null {
  if (!isRecord(v)) return null;
  const kind = enumOr(v.mask, MASK_KINDS, "constant") as MaskKind;
  const node = defaultMask(kind);
  for (const spec of MASK_FIELDS[kind]) {
    const key = spec.path === "field" ? "kind" : spec.path;
    const raw = v[key];
    if (raw === undefined) continue;
    if (spec.type === "vec2" || spec.type === "vec3") node[spec.path] = tripleOf(raw, spec.type === "vec2" ? 2 : 3);
    else node[spec.path] = readScalar(raw, spec);
  }
  return node;
}

function parseSdfShape(v: unknown): SdfShapeNode {
  const kind = enumOr(isRecord(v) ? v.shape : null, SDF_SHAPE_KINDS, "sphere") as SdfShapeKind;
  const node = defaultSdfShape(kind);
  if (!isRecord(v)) return node;
  for (const spec of SDF_SHAPE_FIELDS[kind]) {
    const raw = v[spec.path];
    if (raw === undefined) continue;
    if (spec.type === "vec3") node[spec.path] = parseTriple(raw, [0, 0, 0]);
    else node[spec.path] = readScalar(raw, spec);
  }
  return node;
}

/** A voxel terrain's stored op list, read into the form the editor edits. */
export function sdfOpsFromWire(v: unknown): SdfOpNode[] {
  return parseSdfOps(v);
}

/** The op list as the core reads it (`rterrain::sdf::SdfOp`, tagged by `op`). */
export function sdfOpsToWire(ops: readonly SdfOpNode[]): Record<string, unknown>[] {
  return ops.map(sdfOpJson);
}

function parseSdfOps(v: unknown): SdfOpNode[] {
  if (!Array.isArray(v)) return [];
  const out: SdfOpNode[] = [];
  for (const raw of v) {
    if (!isRecord(raw)) continue;
    const op = enumOr(raw.op, SDF_OP_KINDS, "add") as SdfOpKind;
    const node = defaultSdfOp(op);
    if (raw.shape !== undefined) node.shape = parseSdfShape(raw.shape);
    for (const spec of SDF_OP_FIELDS[op]) {
      const value = raw[spec.path];
      if (value === undefined) continue;
      if (spec.type === "vec3") node[spec.path] = parseTriple(value, [0, 0, 0]);
      else node[spec.path] = readScalar(value, spec);
    }
    out.push(node);
  }
  return out;
}

function parseVolume(v: unknown): VolumeNode {
  const node = defaultVolume();
  if (!isRecord(v)) return node;
  const chunks = Array.isArray(v.chunks) ? v.chunks : [];
  node.chunks = [
    Math.max(2, Math.round(num(chunks[0], node.chunks[0]))),
    Math.max(2, Math.round(num(chunks[1], node.chunks[1]))),
    Math.max(2, Math.round(num(chunks[2], node.chunks[2]))),
  ];
  node.voxel_size = num(v.voxel_size, node.voxel_size);
  node.origin = parseTriple(v.origin, node.origin);
  node.ops = parseSdfOps(v.ops);
  return node;
}

function parseNode(raw: unknown): RecipeNode | null {
  if (!isRecord(raw)) return null;
  const kind = enumOr(raw.node, NODE_KINDS, "");
  if (!kind) return null;
  const base = defaultNode(kind as NodeKind);
  base.id = typeof raw.id === "string" && raw.id.length > 0 ? raw.id : nextNodeId();
  switch (kind as NodeKind) {
    case "generator":
      base.generator = parseGenerator(raw.generator);
      return base;
    case "filter":
      base.source = typeof raw.source === "string" ? raw.source : "";
      base.op = parseFilterOp(raw.op);
      return base;
    case "blend": {
      base.mode = enumOr(raw.mode, BLEND_MODES, "mix") as BlendMode;
      base.terms = [];
      if (Array.isArray(raw.terms)) {
        for (const term of raw.terms) {
          if (!isRecord(term) || typeof term.source !== "string") continue;
          base.terms.push({ source: term.source, weight: num(term.weight, 1), mask: parseMask(term.mask) });
        }
      }
      base.mask = parseMask(raw.mask);
      return base;
    }
    case "modulate":
      base.source = typeof raw.source === "string" ? raw.source : "";
      base.mask = parseMask(raw.mask);
      base.strength = num(raw.strength, 1);
      return base;
    case "detail":
      base.seed = Math.round(num(raw.seed, base.seed));
      base.frequency = num(raw.frequency, base.frequency);
      base.octaves = Math.round(num(raw.octaves, base.octaves));
      base.amplitude = num(raw.amplitude, base.amplitude);
      return base;
    case "volume":
      base.terrain = parseVolume(raw.terrain);
      return base;
    default:
      return base;
  }
}

function shapeJson(node: RecipeNode): Record<string, unknown> {
  const s = node.generator.shape ?? defaultShape();
  return {
    fractal: { ...s.fractal },
    kind: s.kind,
    anisotropy: s.anisotropy,
    rotation_deg: s.rotation_deg,
    warp: s.warp,
    warp_seed: s.warp_seed,
  };
}

function maskJson(m: MaskNode | null): Record<string, unknown> | null {
  if (!m) return null;
  const out: Record<string, unknown> = { mask: m.kind };
  for (const spec of MASK_FIELDS[m.kind] ?? []) {
    // A from_field mask declares its field kind on `kind`, which is also where
    // the mirror keeps the mask's own tag, so the two are kept apart here.
    const key = spec.path === "field" ? "kind" : spec.path;
    out[key] = m[spec.path] ?? defaultFor(spec);
  }
  return out;
}

function sdfShapeJson(s: SdfShapeNode): Record<string, unknown> {
  const out: Record<string, unknown> = { shape: s.kind };
  for (const spec of SDF_SHAPE_FIELDS[s.kind]) out[spec.path] = s[spec.path] ?? defaultFor(spec);
  return out;
}

function sdfOpJson(op: SdfOpNode): Record<string, unknown> {
  const out: Record<string, unknown> = { op: op.op };
  if (op.shape && SDF_OP_FIELDS[op.op].length >= 0 && (op.op === "add" || op.op === "union" || op.op === "subtract" || op.op === "intersect")) {
    out.shape = sdfShapeJson(op.shape);
  }
  for (const spec of SDF_OP_FIELDS[op.op]) out[spec.path] = op[spec.path] ?? defaultFor(spec);
  return out;
}

function nodeJson(node: RecipeNode): Record<string, unknown> {
  switch (node.node) {
    case "generator": {
      const generator: Record<string, unknown> = { kind: node.generator.kind };
      for (const spec of GENERATOR_FIELDS[node.generator.kind]) {
        if (spec.path.startsWith("shape.")) continue;
        generator[spec.path] = node.generator[spec.path] ?? defaultFor(spec);
      }
      if (node.generator.kind === "heightmap") generator.samples = node.generator.samples ?? [];
      else generator.shape = shapeJson(node);
      return { node: "generator", generator };
    }
    case "filter": {
      const op: Record<string, unknown> = { op: node.op.kind };
      for (const spec of FILTER_OP_FIELDS[node.op.kind] ?? []) op[spec.path] = node.op[spec.path] ?? defaultFor(spec);
      return { node: "filter", source: node.source, op };
    }
    case "blend":
      return {
        node: "blend",
        mode: node.mode,
        terms: node.terms.map((t) => {
          const term: Record<string, unknown> = { source: t.source, weight: t.weight };
          const mask = maskJson(t.mask);
          if (mask) term.mask = mask;
          return term;
        }),
        mask: maskJson(node.mask) ?? undefined,
      };
    case "modulate":
      return { node: "modulate", source: node.source, strength: node.strength, mask: maskJson(node.mask) ?? undefined };
    case "detail":
      return { node: "detail", seed: node.seed, frequency: node.frequency, octaves: node.octaves, amplitude: node.amplitude };
    case "volume":
      return {
        node: "volume",
        terrain: {
          chunks: [...node.terrain.chunks],
          voxel_size: node.terrain.voxel_size,
          origin: [...node.terrain.origin],
          ops: node.terrain.ops.map(sdfOpJson),
        },
      };
    default:
      return { node: "generator", generator: { kind: "rolling_hills" } };
  }
}

/**
 * Parses recipe JSON, filling in defaults for anything missing. The grid and
 * output are read from the same object the core deserializes, which ignores the
 * fields it does not declare.
 */
export function parseRecipe(text: string): Recipe {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new RecipeError("recipe JSON does not parse");
  }
  if (!isRecord(parsed)) throw new RecipeError("recipe is not an object");
  const res = Array.isArray(parsed.resolution) ? parsed.resolution : [512, 512];
  const nodes: RecipeNode[] = [];
  if (Array.isArray(parsed.nodes)) {
    for (const raw of parsed.nodes) {
      const node = parseNode(raw);
      if (node) nodes.push(node);
    }
  }
  return {
    resolution: [
      Math.max(1, Math.round(num(res[0], 512))),
      Math.max(1, Math.round(num(res[1], 512))),
    ],
    cell_size: Math.max(0.01, num(parsed.cell_size, 64)),
    nodes,
    output: typeof parsed.output === "string" && parsed.output.length > 0 ? parsed.output : (nodes[0]?.id ?? ""),
  };
}

/** Serialises a recipe in the shape the core evaluates. */
export function recipeJson(recipe: Recipe): string {
  return JSON.stringify({
    resolution: recipe.resolution,
    cell_size: recipe.cell_size,
    output: recipe.output,
    nodes: recipe.nodes.map((n) => ({ id: n.id, ...nodeJson(n) })),
  });
}

/**
 * The request `Core.terrainGenerate` takes: the recipe itself under `recipe`,
 * with the grid it is evaluated on beside it (`r5ed-wasm` `terrain_generate`).
 */
export function generateRequestJson(recipe: Recipe): string {
  return JSON.stringify({
    recipe: { nodes: recipe.nodes.map((n) => ({ id: n.id, ...nodeJson(n) })), output: recipe.output },
    resolution: recipe.resolution,
    cell_size: recipe.cell_size,
  });
}

/** The recipe as the document stores it, without the grid the form owns. */
export function storedRecipeJson(recipe: Recipe): string {
  return JSON.stringify({ nodes: recipe.nodes.map((n) => ({ id: n.id, ...nodeJson(n) })), output: recipe.output });
}

/**
 * A heightmap generator's grid from a decoded image: the red channel of each
 * pixel, widened from 8 to 16 bits, row by row.
 */
export function heightmapFromImage(width: number, height: number, rgba: Uint8Array): { width: number; height: number; samples: number[] } {
  const samples = new Array<number>(width * height);
  for (let i = 0; i < width * height; i++) samples[i] = rgba[i * 4] * 257;
  return { width, height, samples };
}

export interface RecipeIssue {
  nodeId: string;
  message: string;
}

const MAX_NODES = 256;
const MAX_SAMPLES = 1 << 28;

/** Every node id a node reads from. */
export function inputsOf(node: RecipeNode): string[] {
  const fromMask = (m: MaskNode | null): string[] => (m && m.kind === "from_field" && m.source ? [String(m.source)] : []);
  switch (node.node) {
    case "filter":
    case "modulate":
      return node.source ? [node.source] : [];
    case "blend":
      return [...node.terms.map((t) => t.source), ...node.terms.flatMap((t) => fromMask(t.mask)), ...fromMask(node.mask)];
    default:
      return [];
  }
}

/** Structural checks the editor shows before generating. */
export function validateRecipe(recipe: Recipe): RecipeIssue[] {
  const issues: RecipeIssue[] = [];
  const seen = new Set<string>();
  for (const node of recipe.nodes) {
    if (seen.has(node.id)) issues.push({ nodeId: node.id, message: "duplicate node id" });
    seen.add(node.id);
  }
  const order = new Map<string, number>();
  recipe.nodes.forEach((n, i) => order.set(n.id, i));
  for (const node of recipe.nodes) {
    const self = order.get(node.id);
    for (const input of inputsOf(node)) {
      if (!seen.has(input)) issues.push({ nodeId: node.id, message: `input ${input} does not exist` });
      else if (input === node.id) issues.push({ nodeId: node.id, message: "node reads from itself" });
      else {
        const at = order.get(input);
        // Evaluation is a single pass in list order, so a node can only read
        // an earlier one.
        if (at !== undefined && self !== undefined && at > self) {
          issues.push({ nodeId: node.id, message: `input ${input} comes after it in the list` });
        }
      }
    }
    if ((node.node === "filter" || node.node === "modulate") && !node.source) {
      issues.push({ nodeId: node.id, message: `${node.node} has no source node` });
    }
    if (node.node === "generator" && node.generator.kind === "heightmap") {
      const want = Number(node.generator.width) * Number(node.generator.height);
      const have = node.generator.samples?.length ?? 0;
      if (have !== want) issues.push({ nodeId: node.id, message: have === 0 ? "the heightmap has no image; load one" : `the heightmap holds ${have} samples, not ${want}` });
      else if (node.generator.width !== recipe.resolution[0] || node.generator.height !== recipe.resolution[1]) {
        issues.push({ nodeId: node.id, message: `the heightmap is ${node.generator.width} x ${node.generator.height} but the grid is ${recipe.resolution[0]} x ${recipe.resolution[1]}` });
      }
    }
    if (node.node === "blend" && node.terms.length < 2) {
      issues.push({ nodeId: node.id, message: "a blend needs at least two terms" });
    }
    if (node.node === "modulate" && !node.mask) {
      issues.push({ nodeId: node.id, message: "a modulate needs a mask" });
    }
  }
  if (recipe.nodes.length === 0) issues.push({ nodeId: "", message: "recipe has no nodes" });
  if (recipe.nodes.length > MAX_NODES) issues.push({ nodeId: "", message: `recipe has more than ${MAX_NODES} nodes` });
  if (recipe.output === "" && recipe.nodes.length > 0) {
    issues.push({ nodeId: "", message: "the recipe has no output node" });
  } else if (recipe.output && !seen.has(recipe.output)) {
    issues.push({ nodeId: "", message: `output ${recipe.output} does not exist` });
  }
  if (recipe.resolution[0] * recipe.resolution[1] > MAX_SAMPLES) {
    issues.push({ nodeId: "", message: `resolution is larger than ${MAX_SAMPLES} samples` });
  }
  return issues;
}
