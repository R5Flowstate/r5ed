/**
 * WGSL sources. One module per pass family; entry points are selected by the
 * pipeline so the same vertex stage can target colour or the id buffer.
 */

export const FRAME_STRUCT = /* wgsl */ `
struct Frame {
  viewProj: mat4x4<f32>,
  invViewProj: mat4x4<f32>,
  cameraPos: vec4<f32>,
  sunDir: vec4<f32>,
  sunColor: vec4<f32>,
  skyColor: vec4<f32>,
  groundColor: vec4<f32>,
  fog: vec4<f32>,
  grid: vec4<f32>,
  time: vec4<f32>,
  // shade = (colorMode, xray, singleUnused, wireUnused); colorMode is
  // 0 material, 1 object hash, 2 single, 3 uv checker.
  shade: vec4<f32>,
  shadeColor: vec4<f32>,
};
@group(0) @binding(0) var<uniform> frame: Frame;
`;

export const MATERIAL_STRUCT = /* wgsl */ `
struct Material {
  baseColor: vec4<f32>,
  params: vec4<f32>,
};
@group(1) @binding(0) var albedoSampler: sampler;
@group(1) @binding(1) var albedoTex: texture_2d<f32>;
@group(1) @binding(2) var normalSampler: sampler;
@group(1) @binding(3) var normalTex: texture_2d<f32>;
@group(1) @binding(4) var lightmapSampler: sampler;
@group(1) @binding(5) var lightmapAtlas: texture_2d<f32>;
@group(1) @binding(6) var<uniform> material: Material;
`;

const LAYOUTS = /* wgsl */ `
struct VIn {
  @location(0) position: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) m0: vec4<f32>,
  @location(4) m1: vec4<f32>,
  @location(5) m2: vec4<f32>,
  @location(6) m3: vec4<f32>,
  @location(7) tint: vec4<f32>,
  @location(8) lmRect: vec4<f32>,
};
`;

export const SURFACE_SHADER = FRAME_STRUCT + MATERIAL_STRUCT + LAYOUTS + /* wgsl */ `
fn instanceMatrix(v: VIn) -> mat4x4<f32> {
  return mat4x4<f32>(v.m0, v.m1, v.m2, v.m3);
}

fn tangentBasis(n: vec3<f32>) -> mat3x3<f32> {
  let up = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.z) > 0.999);
  let t = normalize(cross(up, n));
  let b = cross(n, t);
  return mat3x3<f32>(t, b, n);
}

struct VOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) world: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) lmRect: vec4<f32>,
  @location(4) @interpolate(flat) id: u32,
  @location(5) tint: vec4<f32>,
};

@vertex
fn vs(v: VIn) -> VOut {
  let m = instanceMatrix(v);
  let world = (m * vec4<f32>(v.position, 1.0)).xyz;
  var out: VOut;
  out.clip = frame.viewProj * vec4<f32>(world, 1.0);
  out.world = world;
  out.normal = normalize((m * vec4<f32>(v.normal, 0.0)).xyz);
  out.uv = v.uv;
  out.lmRect = v.lmRect;
  out.id = u32(v.tint.w + 0.5);
  out.tint = vec4<f32>(v.tint.rgb, 1.0);
  return out;
}

@vertex
fn vs_lines(v: VIn) -> VOut {
  let m = instanceMatrix(v);
  let world = (m * vec4<f32>(v.position, 1.0)).xyz;
  var out: VOut;
  out.clip = frame.viewProj * vec4<f32>(world, 1.0);
  out.world = world;
  out.normal = vec3<f32>(0.0, 0.0, 1.0);
  out.uv = v.uv;
  out.lmRect = vec4<f32>(0.0, 0.0, 0.0, 0.0);
  out.id = u32(v.tint.w + 0.5);
  out.tint = vec4<f32>(v.tint.rgb, 1.0);
  return out;
}

fn hemisphere(n: vec3<f32>) -> vec3<f32> {
  let t = n.z * 0.5 + 0.5;
  return mix(frame.groundColor.rgb, frame.skyColor.rgb, t);
}

// fog = (color.rgb, density per inch); grid = (major.rgb, fade distance);
// time = (minor.rgb, lightmap preview flag).
fn applyFog(color: vec3<f32>, world: vec3<f32>) -> vec3<f32> {
  let d = distance(world, frame.cameraPos.xyz);
  let f = 1.0 - exp(-d * max(frame.fog.w, 0.0));
  return mix(color, frame.fog.xyz, f);
}

@fragment
fn fs_surface(v: VOut) -> @location(0) vec4<f32> {
  let n0 = normalize(v.normal);
  let albedoTexel = textureSample(albedoTex, albedoSampler, v.uv);
  let albedoSample = albedoTexel.rgb;
  let normalSample = textureSample(normalTex, normalSampler, v.uv).xyz * 2.0 - 1.0;
  let atlasUv = v.lmRect.xy + fract(v.uv) * v.lmRect.zw;
  let lightmapSample = textureSample(lightmapAtlas, lightmapSampler, atlasUv).rgb;
  // Alpha-tested materials (foliage cards) cut out their transparent texels.
  if (material.params.y > 1.5 && albedoTexel.a < 0.5) {
    discard;
  }
  let albedo = material.baseColor.rgb * select(vec3<f32>(1.0), albedoSample, material.params.y > 0.5);
  let n = select(n0, normalize(tangentBasis(n0) * normalSample), material.params.z > 0.5);
  let view = normalize(frame.cameraPos.xyz - v.world);
  let sun = normalize(frame.sunDir.xyz);
  let ndotl = max(dot(n, sun), 0.0);
  let ambient = hemisphere(n0) * material.params.w;
  var color = albedo * (frame.sunColor.rgb * ndotl + ambient);
  let half = normalize(sun + view);
  let spec = pow(max(dot(n, half), 0.0), mix(8.0, 96.0, 1.0 - material.baseColor.a));
  color = color + frame.sunColor.rgb * spec * (1.0 - material.params.x) * 0.25;
  let useLightmap = v.lmRect.z > 0.0 && frame.time.w > 0.5;
  color = color * select(vec3<f32>(1.0), lightmapSample, useLightmap);
  return vec4<f32>(applyFog(color * v.tint.rgb, v.world), xrayAlpha(material.baseColor.a));
}

@fragment
fn fs_translucent(v: VOut) -> @location(0) vec4<f32> {
  let n0 = normalize(v.normal);
  let sun = normalize(frame.sunDir.xyz);
  let ndotl = 0.35 + 0.65 * max(dot(n0, sun), 0.0);
  let color = material.baseColor.rgb * v.tint.rgb * (frame.sunColor.rgb * ndotl + hemisphere(n0) * material.params.w);
  return vec4<f32>(color, xrayAlpha(material.baseColor.a));
}

@fragment
fn fs_pick(v: VOut) -> @location(0) u32 {
  return v.id;
}

fn hashColor(id: u32) -> vec3<f32> {
  let x = f32(id) * 0.61803398875 + 0.13;
  return fract(vec3<f32>(x * 3.7 + 0.11, x * 7.3 + 0.37, x * 5.1 + 0.73)) * 0.6 + 0.3;
}

fn checker(uv: vec2<f32>) -> vec3<f32> {
  let c = fract((floor(uv.x * 8.0) + floor(uv.y * 8.0)) * 0.5) * 2.0;
  return mix(vec3<f32>(0.78, 0.78, 0.8), vec3<f32>(0.22, 0.22, 0.24), c);
}

fn studio(n: vec3<f32>, albedo: vec3<f32>, world: vec3<f32>) -> vec3<f32> {
  let view = normalize(frame.cameraPos.xyz - world);
  let keyDir = normalize(view * 0.5 + vec3<f32>(0.4, 0.35, 0.7));
  let key = max(dot(n, keyDir), 0.0);
  let fill = max(dot(n, normalize(vec3<f32>(-0.45, -0.3, 0.25))), 0.0) * 0.4;
  let amb = mix(frame.groundColor.rgb, frame.skyColor.rgb, n.z * 0.5 + 0.5);
  return albedo * (frame.sunColor.rgb * (key * 1.1 + fill * 0.4) + amb * 0.7);
}

fn xrayAlpha(a: f32) -> f32 {
  return a * (1.0 - 0.85 * clamp(frame.shade.y, 0.0, 1.0));
}

@fragment
fn fs_solid(v: VOut) -> @location(0) vec4<f32> {
  let n = normalize(v.normal);
  var albedo = material.baseColor.rgb * v.tint.rgb;
  if (frame.shade.x > 0.5 && frame.shade.x < 1.5) {
    albedo = hashColor(v.id);
  } else if (frame.shade.x > 1.5 && frame.shade.x < 2.5) {
    albedo = frame.shadeColor.rgb;
  } else if (frame.shade.x > 2.5) {
    albedo = checker(v.uv);
  }
  var color = studio(n, albedo, v.world);
  let useLightmap = v.lmRect.z > 0.0 && frame.time.w > 0.5;
  let atlasUv = v.lmRect.xy + fract(v.uv) * v.lmRect.zw;
  let lightmapSample = textureSample(lightmapAtlas, lightmapSampler, atlasUv).rgb;
  color = color * select(vec3<f32>(1.0), lightmapSample, useLightmap);
  return vec4<f32>(applyFog(color, v.world), xrayAlpha(material.baseColor.a));
}

@fragment
fn fs_flat(v: VOut) -> @location(0) vec4<f32> {
  return vec4<f32>(v.tint.rgb, v.tint.a);
}
`;

export const LINE_SHADER = FRAME_STRUCT + /* wgsl */ `
struct LIn {
  @location(0) position: vec3<f32>,
  @location(1) weight: f32,
  @location(2) color: vec4<f32>,
};
struct LOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) world: vec3<f32>,
  @location(1) weight: f32,
  @location(2) color: vec4<f32>,
};

@vertex
fn vs(v: LIn) -> LOut {
  var out: LOut;
  out.clip = frame.viewProj * vec4<f32>(v.position, 1.0);
  out.world = v.position;
  out.weight = v.weight;
  out.color = v.color;
  return out;
}

struct GIn {
  @location(0) position: vec3<f32>,
  @location(1) weight: f32,
};

@vertex
fn vs_grid(v: GIn) -> LOut {
  var out: LOut;
  out.clip = frame.viewProj * vec4<f32>(v.position, 1.0);
  out.world = v.position;
  out.weight = v.weight;
  out.color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
  return out;
}

@fragment
fn fs_color(v: LOut) -> @location(0) vec4<f32> {
  return v.color;
}

@fragment
fn fs_grid(v: LOut) -> @location(0) vec4<f32> {
  let d = distance(v.world, frame.cameraPos.xyz);
  let t = clamp(d / max(1.0, frame.grid.w), 0.0, 1.0);
  let fade = 1.0 - t * t;
  let minor = vec4<f32>(frame.time.rgb, 0.6);
  let major = vec4<f32>(frame.grid.rgb, 1.0);
  let line = mix(minor, major, clamp(v.weight, 0.0, 1.0));
  return vec4<f32>(line.rgb, line.a * fade);
}
`;

export const BILLBOARD_SHADER = FRAME_STRUCT + /* wgsl */ `
@group(1) @binding(0) var atlasSampler: sampler;
@group(1) @binding(1) var atlasTex: texture_2d<f32>;

struct BIn {
  @location(0) center: vec4<f32>,
  @location(1) color: vec4<f32>,
  @location(2) uvRect: vec4<f32>,
};
struct BOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) color: vec4<f32>,
};

@vertex
fn vs(v: BIn, @builtin(vertex_index) vi: u32) -> BOut {
  let corners = array<vec2<f32>, 6>(
    vec2<f32>(-0.5, -0.5), vec2<f32>(0.5, -0.5), vec2<f32>(-0.5, 0.5),
    vec2<f32>(-0.5, 0.5), vec2<f32>(0.5, -0.5), vec2<f32>(0.5, 0.5),
  );
  let c = corners[vi];
  let camRight = normalize(vec3<f32>(frame.invViewProj[0][0], frame.invViewProj[1][0], frame.invViewProj[2][0]));
  let camUp = normalize(vec3<f32>(frame.invViewProj[0][1], frame.invViewProj[1][1], frame.invViewProj[2][1]));
  let world = v.center.xyz + (camRight * c.x + camUp * c.y) * v.center.w;
  var out: BOut;
  out.clip = frame.viewProj * vec4<f32>(world, 1.0);
  out.uv = vec2<f32>(mix(v.uvRect.x, v.uvRect.z, c.x + 0.5), mix(v.uvRect.y, v.uvRect.w, 1.0 - (c.y + 0.5)));
  out.color = v.color;
  return out;
}

@fragment
fn fs_billboard(v: BOut) -> @location(0) vec4<f32> {
  let s = textureSample(atlasTex, atlasSampler, v.uv);
  let edge = smoothstep(0.0, 0.03, min(min(v.uv.y, 1.0 - v.uv.y), 0.5));
  return vec4<f32>(v.color.rgb * s.rgb, v.color.a * s.a * edge);
}
`;

export const BOX_SHADER = FRAME_STRUCT + /* wgsl */ `
struct BIn {
  @location(0) center: vec4<f32>,
  @location(1) color: vec4<f32>,
  @location(2) halfSize: vec4<f32>,
};
struct BOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) world: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) color: vec4<f32>,
};

const FACES = array<vec3<f32>, 6>(
  vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, -1.0),
  vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0),
  vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
);

@vertex
fn vs(v: BIn, @builtin(vertex_index) vi: u32) -> BOut {
  let quad = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
    vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
  );
  let face = vi / 6u;
  let corner = quad[vi % 6u];
  let n = FACES[face];
  let helper = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), abs(n.z) > 0.5);
  let t = normalize(cross(helper, n));
  let b = cross(n, t);
  let half = v.halfSize.xyz;
  let world = v.center.xyz + (t * corner.x + b * corner.y) * half;
  var out: BOut;
  out.clip = frame.viewProj * vec4<f32>(world, 1.0);
  out.world = world;
  out.normal = n;
  out.color = v.color;
  return out;
}

@fragment
fn fs_box(v: BOut) -> @location(0) vec4<f32> {
  let n = normalize(v.normal);
  let sun = normalize(frame.sunDir.xyz);
  let lit = 0.45 + 0.55 * max(dot(n, sun), 0.0);
  return vec4<f32>(v.color.rgb * lit, v.color.a);
}
`;

export const FULLSCREEN_SHADER = /* wgsl */ `
struct FSOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> FSOut {
  let p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
  var out: FSOut;
  out.clip = vec4<f32>(p[vi], 0.0, 1.0);
  out.uv = vec2<f32>(p[vi].x * 0.5 + 0.5, 0.5 - p[vi].y * 0.5);
  return out;
}
`;
