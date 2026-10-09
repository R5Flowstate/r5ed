//! Direct and indirect light evaluation, and the lightmap page raster that
//! carries the result into the engine's page lumps.

use glam::Vec3;

use crate::bvh::{Bvh, Ray, Tri};
use crate::charts::{Chart, ChartLayout};
use crate::codec::RtlPage;
use crate::error::{Error, Result};

/// The albedo a surface is given when the scene carries no material colour.
pub const DEFAULT_ALBEDO: Vec3 = Vec3::splat(0.5);

/// Samples taken across an area light's rectangle.
const AREA_SAMPLES_MAX: usize = 256;
/// Rays across the sun's disc, so a penumbra softens the terminator.
const SUN_SAMPLES: usize = 8;
/// The sun's angular radius in radians.
const SUN_ANGULAR_RADIUS: f32 = 0.02;

/// Average surface colour per material path. A path the table does not name
/// takes [`DEFAULT_ALBEDO`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlbedoTable {
    pub entries: std::collections::BTreeMap<String, [f32; 3]>,
}

impl AlbedoTable {
    pub fn get(&self, material: &str) -> Vec3 {
        match self.entries.get(material) {
            Some(c) => Vec3::new(c[0], c[1], c[2]),
            None => DEFAULT_ALBEDO,
        }
    }

    pub fn set(&mut self, material: &str, colour: [f32; 3]) {
        self.entries.insert(material.to_string(), colour);
    }

    /// The albedo a surface takes, by its material path.
    pub fn for_surface(&self, s: &rir::RenderSurface) -> Vec3 {
        self.get(&s.material)
    }

    /// The albedo a static prop takes, by its model path.
    pub fn for_model(&self, model: &str) -> Vec3 {
        self.get(model)
    }
}

/// One lightmap page after shading.
///
/// A type-1 page is two RGBA8 images one after the other (the client binds
/// them as `lightmapTexture0` and `lightmapTexture1`):
/// - image 0 is the baked light without the direct sun, RGBM with the alpha
///   scaling rgb by `a * 3.984375 + 1/64`;
/// - image 1 is the dominant incoming direction, `rgb * 2.007874 - 1` in world
///   space with its length the directionality, and the sun visibility in alpha.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BakedPage {
    pub index: u32,
    pub width: u16,
    pub height: u16,
    /// Linear baked light per texel, direct sun excluded.
    pub light: Vec<Vec3>,
    /// Dominant incoming light direction per texel; length <= 1.
    pub dir: Vec<Vec3>,
    /// Sun visibility per texel, 0..1.
    pub sun_vis: Vec<f32>,
    pub rtl: RtlPage,
}

/// The largest scale image 0's alpha can carry.
pub const RGBM_MAX: f32 = 4.0;
const RGBM_MIN: f32 = 1.0 / 64.0;
const RGBM_STEP: f32 = 3.984375;
const DIR_SCALE: f32 = 2.007874;

impl BakedPage {
    /// A page of flat ambient light with the sun fully visible: the minimal
    /// page a map with no bake still needs.
    pub fn ambient(index: u32, width: u16, height: u16, ambient: [f32; 3]) -> Result<Self> {
        let mut p = Self::new(index, width, height)?;
        p.light.fill(Vec3::from_array(ambient));
        p.dir.fill(Vec3::new(0.0, 0.0, 0.5));
        p.sun_vis.fill(1.0);
        Ok(p)
    }

    pub fn new(index: u32, width: u16, height: u16) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::lighting("a lightmap page needs a non-zero size"));
        }
        let n = width as usize * height as usize;
        Ok(Self {
            index,
            width,
            height,
            light: vec![Vec3::ZERO; n],
            dir: vec![Vec3::ZERO; n],
            sun_vis: vec![0.0; n],
            rtl: RtlPage::new(width, height),
        })
    }

    fn at(&self, x: usize, y: usize) -> Option<usize> {
        let i = y * self.width as usize + x;
        (x < self.width as usize && i < self.light.len()).then_some(i)
    }

    pub fn set(&mut self, x: usize, y: usize, rgb: Vec3, sun: f32) {
        if let Some(i) = self.at(x, y) {
            self.light[i] = rgb;
            self.sun_vis[i] = sun;
        }
    }

    pub fn set_dir(&mut self, x: usize, y: usize, dir: Vec3) {
        if let Some(i) = self.at(x, y) {
            self.dir[i] = dir;
        }
    }

    pub fn texels(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// The baked light at a texel.
    pub fn colour(&self, x: usize, y: usize) -> Vec3 {
        self.at(x, y).map_or(Vec3::ZERO, |i| self.light[i])
    }

    /// The sun visibility at a texel.
    pub fn sun(&self, x: usize, y: usize) -> f32 {
        self.at(x, y).map_or(0.0, |i| self.sun_vis[i])
    }

    /// The page as the sky lump stores it: image 0 then image 1.
    pub fn encode(&self) -> Vec<u8> {
        let n = self.texels();
        let mut out = vec![0u8; n * 8];
        let (img0, img1) = out.split_at_mut(n * 4);
        for i in 0..n {
            let rgb = self.light[i].max(Vec3::ZERO);
            let rgb = if rgb.is_finite() { rgb } else { Vec3::ZERO };
            let m = rgb.max_element().clamp(RGBM_MIN, RGBM_MAX);
            let a = ((m - RGBM_MIN) / RGBM_STEP).clamp(0.0, 1.0);
            // Quantise the scale first and divide by what the shader will
            // rebuild, so the stored colour does not drift.
            let qa = quantise(a);
            let m_q = qa as f32 / 255.0 * RGBM_STEP + RGBM_MIN;
            let c = rgb / m_q;
            img0[i * 4..i * 4 + 4].copy_from_slice(&[quantise(c.x), quantise(c.y), quantise(c.z), qa]);
            let d = self.dir[i];
            let d = if d.is_finite() { d.clamp_length_max(1.0) } else { Vec3::ZERO };
            let e = (d + Vec3::ONE) / DIR_SCALE;
            img1[i * 4..i * 4 + 4].copy_from_slice(&[quantise(e.x), quantise(e.y), quantise(e.z), quantise(self.sun_vis[i])]);
        }
        out
    }
}

fn quantise(v: f32) -> u8 {
    if !v.is_finite() {
        return 0;
    }
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A surface texel resolved to a world position and normal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Texel {
    pub position: Vec3,
    pub normal: Vec3,
}

/// The front-face normal of a triangle. The engine winds front faces
/// clockwise, so this is `(c - a) x (b - a)`, the reverse of the
/// counter-clockwise convention.
pub fn front_normal(a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    (c - a).cross(b - a)
}

/// Rasterises a surface's triangles into its chart, one entry per covered inner
/// texel. The page offset is applied, so entry `(x, y)` sits at
/// `(origin + (x, y))` in the page. A texel's normal interpolates the vertex
/// normals when the surface carries one per vertex, and is the triangle's
/// front-face normal otherwise.
pub fn rasterize(chart: &Chart, surface: &rir::RenderSurface) -> Vec<Option<Texel>> {
    let (positions, normals, indices) = (&surface.positions, &surface.normals, &surface.indices);
    let smooth = normals.len() == positions.len();
    let iw = chart.inner[0] as usize;
    let ih = chart.inner[1] as usize;
    let mut out: Vec<Option<Texel>> = vec![None; iw.saturating_mul(ih)];
    if out.is_empty() {
        return out;
    }
    // The chart's own transform already lands the surface's lowest texel inside
    // the padding, so a texel here is the chart-local index directly.
    // An atlas chart places a vertex at its own uv1 texel instead.
    let to_chart = |i: u32, p: Vec3| -> (f32, f32) {
        if chart.atlas {
            let u = surface.uv1.get(i as usize).copied().unwrap_or_default();
            return (u.x * iw as f32, u.y * ih as f32);
        }
        let d = p - chart.anchor;
        (
            chart.basis[0][0] * d.x + chart.basis[1][0] * d.y + chart.basis[2][0] * d.z,
            chart.basis[0][1] * d.x + chart.basis[1][1] * d.y + chart.basis[2][1] * d.z,
        )
    };
    for c in indices.chunks_exact(3) {
        let (Some(a), Some(b), Some(d)) = (
            positions.get(c[0] as usize),
            positions.get(c[1] as usize),
            positions.get(c[2] as usize),
        ) else {
            continue;
        };
        let (ax, ay) = to_chart(c[0], *a);
        let (bx, by) = to_chart(c[1], *b);
        let (cx, cy) = to_chart(c[2], *d);
        let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
        if !area.is_finite() || area.abs() < 1.0e-9 {
            continue;
        }
        let flat = front_normal(*a, *b, *d).normalize_or_zero();
        if flat.length_squared() < 0.5 {
            continue;
        }
        let corner = |i: u32| normals.get(i as usize).copied().filter(|n| n.is_finite()).unwrap_or(flat);
        let lo_x = ax.min(bx).min(cx).floor().max(0.0) as i64;
        let hi_x = (ax.max(bx).max(cx).ceil() as i64).min(iw as i64 - 1);
        let lo_y = ay.min(by).min(cy).floor().max(0.0) as i64;
        let hi_y = (ay.max(by).max(cy).ceil() as i64).min(ih as i64 - 1);
        for ty in lo_y..=hi_y {
            for tx in lo_x..=hi_x {
                let (px, py) = (tx as f32 + 0.5, ty as f32 + 0.5);
                let (w0, w1, w2) = barycentric(ax, ay, bx, by, cx, cy, px, py, area);
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let at = ty as usize * iw + tx as usize;
                if out[at].is_some() {
                    continue;
                }
                let normal = if smooth {
                    let n = (w0 * corner(c[0]) + w1 * corner(c[1]) + w2 * corner(c[2])).normalize_or_zero();
                    if n.length_squared() < 0.5 { flat } else { n }
                } else {
                    flat
                };
                out[at] = Some(Texel {
                    position: w0 * *a + w1 * *b + w2 * *d,
                    normal,
                });
            }
        }
    }
    // A triangle thinner than a texel may hold no texel centre at all; it then
    // takes each empty texel it overlaps, lit at its own nearest point, so no
    // surface samples the chart's unlit padding.
    for c in indices.chunks_exact(3) {
        let (Some(a), Some(b), Some(d)) = (
            positions.get(c[0] as usize),
            positions.get(c[1] as usize),
            positions.get(c[2] as usize),
        ) else {
            continue;
        };
        let (ax, ay) = to_chart(c[0], *a);
        let (bx, by) = to_chart(c[1], *b);
        let (cx, cy) = to_chart(c[2], *d);
        let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
        let flat = front_normal(*a, *b, *d).normalize_or_zero();
        if !area.is_finite() || flat.length_squared() < 0.5 {
            continue;
        }
        let corner = |i: u32| normals.get(i as usize).copied().filter(|n| n.is_finite()).unwrap_or(flat);
        let lo_x = (ax.min(bx).min(cx) - 0.5).floor().max(0.0) as i64;
        let hi_x = ((ax.max(bx).max(cx) + 0.5).ceil() as i64).min(iw as i64 - 1);
        let lo_y = (ay.min(by).min(cy) - 0.5).floor().max(0.0) as i64;
        let hi_y = ((ay.max(by).max(cy) + 0.5).ceil() as i64).min(ih as i64 - 1);
        for ty in lo_y..=hi_y {
            for tx in lo_x..=hi_x {
                let at = ty as usize * iw + tx as usize;
                if out[at].is_some() {
                    continue;
                }
                let (px, py) = (tx as f32 + 0.5, ty as f32 + 0.5);
                let (w0, w1, w2, dist2) = closest_on_triangle(ax, ay, bx, by, cx, cy, px, py);
                if dist2 > 0.5 {
                    continue;
                }
                let normal = if smooth {
                    let n = (w0 * corner(c[0]) + w1 * corner(c[1]) + w2 * corner(c[2])).normalize_or_zero();
                    if n.length_squared() < 0.5 { flat } else { n }
                } else {
                    flat
                };
                out[at] = Some(Texel {
                    position: w0 * *a + w1 * *b + w2 * *d,
                    normal,
                });
            }
        }
    }
    out
}

/// Barycentrics of the point of a 2D triangle nearest (px, py), and the squared
/// distance to it. A degenerate triangle still answers along its longest edge.
#[allow(clippy::too_many_arguments)]
fn closest_on_triangle(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32, px: f32, py: f32) -> (f32, f32, f32, f32) {
    let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
    if area.abs() > 1.0e-9 {
        let (w0, w1, w2) = barycentric(ax, ay, bx, by, cx, cy, px, py, area);
        if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
            return (w0, w1, w2, 0.0);
        }
    }
    let seg = |x0: f32, y0: f32, x1: f32, y1: f32| -> (f32, f32) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 { (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let (qx, qy) = (x0 + dx * t - px, y0 + dy * t - py);
        (t, qx * qx + qy * qy)
    };
    let (t0, d0) = seg(ax, ay, bx, by);
    let (t1, d1) = seg(bx, by, cx, cy);
    let (t2, d2) = seg(cx, cy, ax, ay);
    if d0 <= d1 && d0 <= d2 {
        (1.0 - t0, t0, 0.0, d0)
    } else if d1 <= d2 {
        (0.0, 1.0 - t1, t1, d1)
    } else {
        (t2, 0.0, 1.0 - t2, d2)
    }
}

#[allow(clippy::too_many_arguments)]
fn barycentric(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32, px: f32, py: f32, area: f32) -> (f32, f32, f32) {
    let w0 = ((bx - px) * (cy - py) - (by - py) * (cx - px)) / area;
    let w1 = ((cx - px) * (ay - py) - (cy - py) * (ax - px)) / area;
    (w0, w1, 1.0 - w0 - w1)
}

/// Windowed inverse-square falloff: strictly decreasing, one at the light and
/// zero at twice the range. The window, Unreal's `(1 - x^4)^2` over the light's
/// reach, takes it to zero smoothly: the bare curve still has 1/9 of its value
/// at twice the range, and cutting it there drew a hard rim round every pool.
pub fn falloff(distance: f32, range: f32) -> f32 {
    if !distance.is_finite() || distance < 0.0 {
        return 0.0;
    }
    let r = if range.is_finite() && range > 0.0 { range } else { f32::INFINITY };
    if r.is_finite() && distance >= r * 2.0 {
        return 0.0;
    }
    let s = 1.0 + if r.is_finite() { distance / r } else { 0.0 };
    let window = if r.is_finite() {
        let x = distance / (2.0 * r);
        let x2 = x * x;
        (1.0 - x2 * x2) * (1.0 - x2 * x2)
    } else {
        1.0
    };
    window / (s * s)
}

/// A light's falloff at `distance`: its record's own polynomial when it
/// carries one, which reaches zero at its range, else the baker's curve.
pub fn light_falloff(light: &rir::BakeLight, distance: f32) -> f32 {
    let Some([c, l, q]) = light.attenuation else { return falloff(distance, light.range) };
    if !distance.is_finite() || distance < 0.0 || (light.range > 0.0 && distance >= light.range) {
        return 0.0;
    }
    (c + l * distance + q * distance * distance).max(0.0)
}

/// Smooth cone falloff between the inner and outer half angles, in radians.
pub fn cone_falloff(cos_angle: f32, inner: f32, outer: f32) -> f32 {
    let (inner, outer) = if inner <= outer { (inner, outer) } else { (outer, inner) };
    if cos_angle >= inner {
        return 1.0;
    }
    if cos_angle <= outer {
        return 0.0;
    }
    let t = ((cos_angle - outer) / (inner - outer)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The irradiance one light brings to a shaded point, before albedo.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LightSample {
    pub colour: Vec3,
    /// The fraction of the sun's disc the shadow ray reached.
    pub sun_visibility: f32,
}

/// Everything a direct or indirect gather needs.
pub struct DirectContext<'a> {
    pub bvh: &'a Bvh,
    pub tris: &'a [Tri],
    pub lights: &'a [rir::BakeLight],
    pub settings: &'a rir::BakeSettings,
    /// A fixed sequence seed, so two runs of the same bake agree.
    pub seed: u32,
    /// Which lights can reach a point; `None` walks every light.
    pub index: Option<&'a LightIndex>,
    /// Albedo per entry of `tris`, what a bounce off that triangle keeps of
    /// the light; empty gives every hit [`DEFAULT_ALBEDO`].
    pub albedo: &'a [Vec3],
}

impl DirectContext<'_> {
    /// The albedo of the triangle a ray hit.
    pub fn albedo_of(&self, tri: u32) -> Vec3 {
        self.albedo.get(tri as usize).copied().unwrap_or(DEFAULT_ALBEDO)
    }
}

/// How far short of a light its shadow ray stops, so the fixture face the
/// light sits on does not shadow it.
const SHADOW_STOP_SHORT: f32 = 2.0;

/// Point and spot lights by the grid cells their reach covers, so a gather
/// walks only the lights that can light it. A positional light gives exactly
/// zero at or past twice its range (`falloff`) and draws no random numbers, so
/// skipping one changes no sum: a bake with the index matches one without it
/// bit for bit. Suns, area lights (which draw random numbers) and lights
/// without a finite range are walked everywhere. CyberPark ports 1,387 local lights;
/// walked at every texel and every bounce hit, they took a final bake from half
/// an hour to many hours.
#[derive(Clone, Debug, Default)]
pub struct LightIndex {
    cell: f32,
    everywhere: Vec<u32>,
    cells: std::collections::HashMap<(i32, i32, i32), Vec<u32>>,
}

/// World units per grid cell.
const LIGHT_CELL: f32 = 1024.0;
/// A light covering more cells than this is walked everywhere instead.
const LIGHT_MAX_CELLS: i64 = 32768;

impl LightIndex {
    pub fn build(lights: &[rir::BakeLight]) -> Self {
        let mut out = LightIndex { cell: LIGHT_CELL, ..Default::default() };
        for (i, l) in lights.iter().enumerate() {
            let i = i as u32;
            let bounded = matches!(l.kind, rir::LightType::Point | rir::LightType::Spot)
                && l.range.is_finite()
                && l.range > 0.0
                && l.position.is_finite();
            if !bounded {
                out.everywhere.push(i);
                continue;
            }
            let reach = Vec3::splat(2.0 * l.range);
            let lo = out.key(l.position - reach);
            let hi = out.key(l.position + reach);
            let n = (hi.0 - lo.0 + 1) as i64 * (hi.1 - lo.1 + 1) as i64 * (hi.2 - lo.2 + 1) as i64;
            if n > LIGHT_MAX_CELLS {
                out.everywhere.push(i);
                continue;
            }
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        out.cells.entry((x, y, z)).or_default().push(i);
                    }
                }
            }
        }
        out
    }

    fn key(&self, p: Vec3) -> (i32, i32, i32) {
        let c = |v: f32| (v / self.cell).floor().clamp(i32::MIN as f32, i32::MAX as f32) as i32;
        (c(p.x), c(p.y), c(p.z))
    }

    /// The lights that can reach `p`, in their scene order.
    pub fn near(&self, p: Vec3) -> NearLights<'_> {
        let cell: &[u32] = if p.is_finite() { self.cells.get(&self.key(p)).map(Vec::as_slice).unwrap_or(&[]) } else { &[] };
        NearLights { a: &self.everywhere, b: cell }
    }
}

/// Two ascending index lists merged, so lights come in scene order.
pub struct NearLights<'a> {
    a: &'a [u32],
    b: &'a [u32],
}

impl Iterator for NearLights<'_> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        match (self.a.first(), self.b.first()) {
            (Some(&x), Some(&y)) if x <= y => {
                self.a = &self.a[1..];
                if x == y {
                    self.b = &self.b[1..];
                }
                Some(x)
            }
            (_, Some(&y)) => {
                self.b = &self.b[1..];
                Some(y)
            }
            (Some(&x), None) => {
                self.a = &self.a[1..];
                Some(x)
            }
            (None, None) => None,
        }
    }
}

impl<'a> DirectContext<'a> {
    /// The lights that can reach `p`: every light without an index.
    /// A light that is not baked lights nothing here; the sun is always read,
    /// since its visibility is what the pages carry for it.
    pub fn lights_at(&self, p: Vec3) -> Box<dyn Iterator<Item = &'a rir::BakeLight> + '_> {
        let lights = self.lights;
        let baked = |l: &&rir::BakeLight| l.baked || l.kind == rir::LightType::Sun;
        match self.index {
            Some(ix) => Box::new(ix.near(p).filter_map(move |i| lights.get(i as usize)).filter(baked)),
            None => Box::new(lights.iter().filter(baked)),
        }
    }
}

impl DirectContext<'_> {
    /// Direct light at `position` on a surface facing `normal`, considering
    /// occluders no further than `reach`.
    pub fn direct(&self, light: &rir::BakeLight, position: Vec3, normal: Vec3, reach: f32) -> LightSample {
        if !light.color.is_finite() || !light.intensity.is_finite() || light.intensity <= 0.0 {
            return LightSample::default();
        }
        match light.kind {
            rir::LightType::Sun => self.direct_sun(light, position, normal, reach),
            rir::LightType::Area => self.direct_area(light, position, normal, reach),
            rir::LightType::Point => self.direct_positional(light, position, normal, reach, true),
            rir::LightType::Spot => self.direct_spot(light, position, normal, reach, true),
            rir::LightType::Sky => LightSample::default(),
        }
    }

    /// The sum of every light's direct contribution, and the sun visibility the
    /// sky page's alpha channel takes.
    pub fn direct_all(&self, position: Vec3, normal: Vec3, reach: f32) -> (Vec3, f32) {
        let mut acc = Vec3::ZERO;
        let mut sun = 0.0f32;
        for l in self.lights_at(position) {
            let s = self.direct(l, position, normal, reach);
            acc += s.colour;
            if l.kind == rir::LightType::Sun {
                sun = sun.max(s.sun_visibility);
            }
        }
        (acc, sun)
    }

    fn direct_sun(&self, light: &rir::BakeLight, position: Vec3, normal: Vec3, reach: f32) -> LightSample {
        let dir = light.direction.normalize_or_zero();
        if dir.length_squared() < 0.5 {
            return LightSample::default();
        }
        let ndotl = normal.dot(-dir);
        if ndotl <= 0.0 {
            return LightSample::default();
        }
        let colour = light.color * light.intensity * ndotl;
        if !light.cast_shadows {
            return LightSample { colour, sun_visibility: 1.0 };
        }
        let vis = self.sun_visibility(position, -dir, reach);
        LightSample { colour: colour * vis, sun_visibility: vis }
    }

    /// The fraction of the sun's disc a shadow ray from `position` reaches.
    pub fn sun_visibility(&self, position: Vec3, dir: Vec3, reach: f32) -> f32 {
        let limit = if reach.is_finite() && reach > 0.0 { reach } else { f32::MAX };
        if self.bvh.occluded(&Ray::new(position, dir, 0.01, limit)) {
            return 0.0;
        }
        let (t, b) = tangent_frame(dir);
        let mut blocked = 0usize;
        for i in 1..SUN_SAMPLES {
            let a = i as f32 / SUN_SAMPLES as f32 * std::f32::consts::TAU;
            let r = SUN_ANGULAR_RADIUS * (a * 0.5).sin();
            let d = (dir + (t * a.cos() + b * a.sin()) * r).normalize_or_zero();
            if d.length_squared() < 0.5 {
                continue;
            }
            if self.bvh.occluded(&Ray::new(position, d, 0.01, limit)) {
                blocked += 1;
            }
        }
        1.0 - blocked as f32 / SUN_SAMPLES as f32
    }

    fn direct_positional(&self, light: &rir::BakeLight, position: Vec3, normal: Vec3, reach: f32, soft: bool) -> LightSample {
        let to_light = light.position - position;
        let d = to_light.length();
        if d <= 1.0e-6 {
            return LightSample::default();
        }
        let dir = to_light / d;
        let ndotl = normal.dot(dir).max(0.0);
        if ndotl <= 0.0 {
            return LightSample::default();
        }
        // A positional light with a size is an emitter that wide, not a point:
        // the distance falloff sees at least half its size, so a surface next to
        // it gets a broad pool instead of a hot spot. CyberPark's 1,390 ported
        // panel lights (median 157 units wide) baked as points drew a bright
        // coloured dot on the nearest wall, about 8x the light around it.
        let half = 0.5 * light.size.max(glam::Vec2::ZERO);
        let soft_d = half.max_element();
        let atten = light_falloff(light, (d * d + soft_d * soft_d).sqrt());
        if atten <= 0.0 {
            return LightSample::default();
        }
        let vis = if !light.cast_shadows {
            1.0
        } else if soft && half.max_element() > 0.0 {
            self.panel_visibility(light, half, position, reach)
        } else if self.blocked(position, light.position, reach) {
            0.0
        } else {
            1.0
        };
        LightSample {
            colour: light.color * light.intensity * ndotl * atten * vis,
            sun_visibility: 0.0,
        }
    }

    /// Whether anything lies between `position` and the light point `at`. The
    /// ray stops short of the light: run past it, it found the ceiling a lamp
    /// hangs under and shadowed everything the lamp lit.
    fn blocked(&self, position: Vec3, at: Vec3, reach: f32) -> bool {
        let to = at - position;
        let d = to.length();
        if d <= SHADOW_STOP_SHORT {
            return false;
        }
        let limit = (d - SHADOW_STOP_SHORT).min(if reach.is_finite() && reach > 0.0 { reach } else { f32::MAX });
        self.bvh.occluded(&Ray::new(position, to / d, 0.01, limit))
    }

    /// The share of a panel light a point sees: 2x2 rays across the panel, and
    /// 4x4 where those disagree, so a shadow edge is a penumbra the panel's
    /// width instead of a texel stair. Fixed points, no random numbers.
    fn panel_visibility(&self, light: &rir::BakeLight, half: glam::Vec2, position: Vec3, reach: f32) -> f32 {
        let dir = light.direction.normalize_or_zero();
        let (t, b) = if dir.length_squared() < 0.5 { (Vec3::X, Vec3::Y) } else { tangent_frame(dir) };
        let at = |u: f32, v: f32| light.position + t * (half.x * u) + b * (half.y * v);
        let corners = [(-0.5, -0.5), (0.5, -0.5), (-0.5, 0.5), (0.5, 0.5)];
        let seen = corners.iter().filter(|(u, v)| !self.blocked(position, at(*u, *v), reach)).count();
        if seen == 0 || seen == corners.len() {
            return seen as f32 / corners.len() as f32;
        }
        let mut seen = 0usize;
        for j in 0..16 {
            let (u, v) = (((j % 4) as f32 + 0.5) / 2.0 - 1.0, ((j / 4) as f32 + 0.5) / 2.0 - 1.0);
            if !self.blocked(position, at(u, v), reach) {
                seen += 1;
            }
        }
        seen as f32 / 16.0
    }

    /// An area light is sampled across its rectangle, the same basis the sun
    /// frame uses: right along the light's own direction.
    fn direct_area(&self, light: &rir::BakeLight, position: Vec3, normal: Vec3, reach: f32) -> LightSample {
        let dir = light.direction.normalize_or_zero();
        let (t, b) = if dir.length_squared() < 0.5 {
            (Vec3::X, Vec3::Y)
        } else {
            tangent_frame(dir)
        };
        let n = self.area_samples(light);
        let side = (n as f32).sqrt().round().max(1.0) as usize;
        let mut acc = Vec3::ZERO;
        let mut vis_sum = 0.0f32;
        let mut taken = 0usize;
        for j in 0..n {
            let u = ((j % side) as f32 + 0.5) / side as f32 - 0.5;
            let v = ((j / side) as f32 + 0.5) / side as f32 - 0.5;
            let p = light.position + t * (light.size.x * u) + b * (light.size.y * v);
            let sample = self.direct_positional(
                &rir::BakeLight { kind: rir::LightType::Point, position: p, size: glam::Vec2::ZERO, ..light.clone() },
                position,
                normal,
                reach,
                false,
            );
            acc += sample.colour;
            vis_sum += sample.sun_visibility;
            taken += 1;
        }
        if taken == 0 {
            return LightSample::default();
        }
        LightSample {
            colour: acc / taken as f32,
            sun_visibility: vis_sum / taken as f32,
        }
    }

    fn direct_spot(&self, light: &rir::BakeLight, position: Vec3, normal: Vec3, reach: f32, soft: bool) -> LightSample {
        let base = self.direct_positional(light, position, normal, reach, soft);
        let dir = light.direction.normalize_or_zero();
        if dir.length_squared() < 0.5 {
            return base;
        }
        // The cone is measured along the beam: from the light out to the point.
        let from_light = (position - light.position).normalize_or_zero();
        let cone = cone_falloff(
            from_light.dot(dir),
            light.inner_angle.to_radians().cos(),
            light.outer_angle.to_radians().cos(),
        );
        LightSample {
            colour: base.colour * cone,
            sun_visibility: base.sun_visibility,
        }
    }

    /// One stochastic sample of a light's direct contribution. A bounce
    /// averages hundreds of these, so a penumbra or an area needs no more
    /// than one shadow ray each.
    pub fn direct_one(&self, light: &rir::BakeLight, position: Vec3, normal: Vec3, reach: f32, rng: &mut Rng) -> Vec3 {
        if !light.color.is_finite() || !light.intensity.is_finite() || light.intensity <= 0.0 {
            return Vec3::ZERO;
        }
        match light.kind {
            rir::LightType::Sun => {
                let dir = light.direction.normalize_or_zero();
                if dir.length_squared() < 0.5 {
                    return Vec3::ZERO;
                }
                let ndotl = normal.dot(-dir);
                if ndotl <= 0.0 {
                    return Vec3::ZERO;
                }
                let colour = light.color * light.intensity * ndotl;
                if !light.cast_shadows {
                    return colour;
                }
                let (t, b) = tangent_frame(-dir);
                let a = rng.unit() * std::f32::consts::TAU;
                let r = SUN_ANGULAR_RADIUS * rng.unit().sqrt();
                let d = (-dir + (t * a.cos() + b * a.sin()) * r).normalize_or_zero();
                let limit = if reach.is_finite() && reach > 0.0 { reach } else { f32::MAX };
                if self.bvh.occluded(&Ray::new(position, d, 0.01, limit)) {
                    Vec3::ZERO
                } else {
                    colour
                }
            }
            rir::LightType::Area => {
                let dir = light.direction.normalize_or_zero();
                let (t, b) = if dir.length_squared() < 0.5 { (Vec3::X, Vec3::Y) } else { tangent_frame(dir) };
                let p = light.position + t * (light.size.x * (rng.unit() - 0.5)) + b * (light.size.y * (rng.unit() - 0.5));
                self.direct_positional(&rir::BakeLight { kind: rir::LightType::Point, position: p, size: glam::Vec2::ZERO, ..light.clone() }, position, normal, reach, false)
                    .colour
            }
            // A bounce averages hundreds of these: one shadow ray to the centre.
            rir::LightType::Point => self.direct_positional(light, position, normal, reach, false).colour,
            rir::LightType::Spot => self.direct_spot(light, position, normal, reach, false).colour,
            _ => self.direct(light, position, normal, reach).colour,
        }
    }

    /// How many samples an area light takes.
    pub fn area_samples(&self, light: &rir::BakeLight) -> usize {
        if light.kind != rir::LightType::Area {
            return 1;
        }
        (self.settings.samples.max(1) as usize).min(AREA_SAMPLES_MAX)
    }
}

fn tangent_frame(dir: Vec3) -> (Vec3, Vec3) {
    let up = if dir.z.abs() > 0.9 { Vec3::X } else { Vec3::Z };
    let t = up.cross(dir);
    if t.length_squared() < 1.0e-12 {
        return (Vec3::X, Vec3::Y);
    }
    let t = t.normalize();
    (t, dir.cross(t).normalize_or_zero())
}

/// A seed that differs between texels, so neighbours do not trace the same
/// directions and the noise stays uncorrelated for the denoiser.
pub(crate) fn position_hash(p: Vec3) -> u32 {
    let mut h = 2_166_136_261u32;
    for v in [p.x, p.y, p.z] {
        h = (h ^ v.to_bits()).wrapping_mul(16_777_619);
    }
    h ^ (h >> 15)
}

/// A small deterministic generator, so a bake is reproducible.
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed | 1)
    }

    pub fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / 16_777_216.0
    }

    /// A cosine-weighted direction around `n`.
    pub fn cosine_dir(&mut self, n: Vec3) -> Vec3 {
        let (t, b) = tangent_frame(n);
        let r = self.unit().sqrt();
        let phi = std::f32::consts::TAU * self.unit();
        (n + t * (r * phi.cos()) + b * (r * phi.sin())).normalize_or_zero()
    }
}

/// How much of a surface's upward hemisphere is open.
pub fn sky_visibility(bvh: &Bvh, position: Vec3, normal: Vec3, rays: usize) -> f32 {
    if rays == 0 {
        return 0.0;
    }
    let n = normal.normalize_or_zero();
    if n.length_squared() < 0.5 {
        return 0.0;
    }
    let mut rng = Rng::new(0x51ED_2701);
    let mut open = 0usize;
    for _ in 0..rays {
        let d = rng.cosine_dir(n);
        if d.length_squared() < 0.5 {
            continue;
        }
        if !bvh.occluded(&Ray::new(position + d * 0.05, d, 0.01, f32::MAX)) {
            open += 1;
        }
    }
    open as f32 / rays as f32
}

/// The open sky seen from a surface as image 1 stores it: the integral of the
/// unoccluded directions over the hemisphere divided by pi, so a fully open
/// surface gets its own normal at length 1. The client lights with it as
/// `c_skyColor * 0.5|d|(N.d + max(N.d, |d|^2))`.
pub fn sky_vector(bvh: &Bvh, position: Vec3, normal: Vec3, rays: usize, seed: u32) -> Vec3 {
    let n = normal.normalize_or_zero();
    if rays == 0 || n.length_squared() < 0.5 {
        return Vec3::ZERO;
    }
    // A jittered grid over (cos theta, phi) keeps the estimate smooth at the
    // low ray counts a preview bake uses.
    let side = (rays as f32).sqrt().ceil().max(1.0) as usize;
    let (t, b) = tangent_frame(n);
    let mut rng = Rng::new(seed ^ position_hash(position));
    let mut acc = Vec3::ZERO;
    for i in 0..side * side {
        let z = ((i / side) as f32 + rng.unit()) / side as f32;
        let phi = std::f32::consts::TAU * ((i % side) as f32 + rng.unit()) / side as f32;
        let r = (1.0 - z * z).max(0.0).sqrt();
        let d = (n * z + t * (r * phi.cos()) + b * (r * phi.sin())).normalize_or_zero();
        if d.length_squared() < 0.5 {
            continue;
        }
        if !bvh.occluded(&Ray::new(position + d * 0.05, d, 0.01, f32::MAX)) {
            acc += d;
        }
    }
    (acc * (2.0 / (side * side) as f32)).clamp_length_max(1.0)
}

/// The sky's contribution at a surface, weighted by how much of it is open.
pub fn sky_light(settings: &rir::BakeSettings, normal: Vec3, visibility: f32) -> Vec3 {
    let weight = 0.5 + 0.5 * normal.normalize_or_zero().z;
    settings.sky_color * settings.sky_intensity * weight * visibility.clamp(0.0, 1.0)
}

/// Diffuse indirect light at a lightmap texel: `rays` cosine-weighted paths of
/// up to `bounces` hits, each continuing along one ray, with the lights taken
/// as the next event at every hit.
pub fn indirect(
    ctx: &DirectContext,
    position: Vec3,
    normal: Vec3,
    albedo: Vec3,
    direct: Vec3,
    rays: usize,
    bounces: u32,
) -> Vec3 {
    if rays == 0 || bounces == 0 {
        return Vec3::ZERO;
    }
    let n = normal.normalize_or_zero();
    if n.length_squared() < 0.5 {
        return Vec3::ZERO;
    }
    let mut rng = Rng::new(ctx.seed ^ position_hash(position));
    let mut acc = Vec3::ZERO;
    for _ in 0..rays {
        acc += path_radiance(ctx, position, n, bounces, false, &mut rng);
    }
    direct.mul_add(albedo, albedo * acc / rays as f32)
}

/// The light one cosine-weighted ray from `position` brings back. A ray that
/// escapes sees the sky, counted only when `sky_on_escape` (the first ray's sky
/// is image 1's job); one that hits carries the lights at the hit and, past the
/// last bounce, the sky the hit can see along one more ray.
fn path_radiance(ctx: &DirectContext, position: Vec3, normal: Vec3, bounces: u32, sky_on_escape: bool, rng: &mut Rng) -> Vec3 {
    let d = rng.cosine_dir(normal);
    if d.length_squared() < 0.5 {
        return Vec3::ZERO;
    }
    let ray = Ray::new(position + d * 0.05, d, 0.01, f32::MAX);
    let Some(hit) = ctx.bvh.closest_hit(&ray) else {
        return if sky_on_escape { sky_light(ctx.settings, d, 1.0) } else { Vec3::ZERO };
    };
    let p = hit.point(&ray);
    let hn = hit.normal(&ray, ctx.tris);
    let incoming = bounce_lights(ctx, p, hn, hit.t, rng);
    let mut incoming = incoming;
    incoming += if bounces > 1 {
        path_radiance(ctx, p, hn, bounces - 1, true, rng)
    } else {
        sky_light(ctx.settings, hn, sky_visibility_along(ctx.bvh, p, hn, rng))
    };
    ctx.albedo_of(hit.tri) * incoming
}

/// The direct light at a bounce hit: every light that can reach it, summed.
/// Picking one at random weighted by their count (tried for speed) left
/// coloured blotches: neon lights hundreds of times brighter than their
/// neighbours make that estimate swing, and the denoiser smears the swings
/// into dots. With ranges that end where the source's lights did, a hit sees
/// a few dozen lights, and an unshadowed one costs no ray.
fn bounce_lights(ctx: &DirectContext, p: Vec3, n: Vec3, reach: f32, rng: &mut Rng) -> Vec3 {
    let mut acc = Vec3::ZERO;
    for l in ctx.lights_at(p) {
        acc += ctx.direct_one(l, p, n, reach, rng);
    }
    acc
}

/// One cosine-weighted visibility ray: 1 when it reaches the sky.
fn sky_visibility_along(bvh: &Bvh, position: Vec3, normal: Vec3, rng: &mut Rng) -> f32 {
    let d = rng.cosine_dir(normal);
    if d.length_squared() < 0.5 || bvh.occluded(&Ray::new(position + d * 0.05, d, 0.01, f32::MAX)) {
        0.0
    } else {
        1.0
    }
}

/// Shades one chart into its page: the baked colour, the sun visibility the
/// sky page's alpha carries, and the real-time-light slots the texels take.
pub fn shade_chart(
    scene: &rir::BakeScene,
    ctx: &DirectContext,
    chart: &Chart,
    page: &mut BakedPage,
    albedo: &AlbedoTable,
    mode: crate::BakeMode,
    rtl: &crate::rtl::RtlSlots,
) -> Result<()> {
    let Some(surface) = scene.surfaces.get(chart.surface) else {
        return Err(Error::Chart {
            surface: chart.surface,
            chart: 0,
            msg: "the layout names a surface the scene does not have".to_string(),
        });
    };
    let _ = albedo;
    let texels = rasterize(chart, surface);
    let rays = match mode {
        crate::BakeMode::None => 0,
        crate::BakeMode::Preview => (scene.settings.samples.max(1) as usize / 4).max(1),
        crate::BakeMode::Final => scene.settings.samples.max(1) as usize,
    };
    let ao_rays = (scene.settings.samples.max(1) as usize / 4).max(1);
    let iw = chart.inner[0] as usize;
    let pw = page.width as usize;
    let ph = page.height as usize;
    // Every texel reads the scene and writes only its own place, so the chart
    // shades across every core and the results land on the page afterwards.
    let shade = |i: usize, slot: &Option<Texel>| {
        let lx = i % iw;
        let ly = i / iw;
        let px = chart.origin[0] as usize + lx;
        let py = chart.origin[1] as usize + ly;
        if px >= pw || py >= ph {
            return None;
        }
        // An uncovered texel keeps the page's padding value.
        let t: &Texel = slot.as_ref()?;
        let n = t.normal.normalize_or_zero();
        // Image 0 is bounce light and the baked local lights only. The client
        // adds the direct sun from image 1's alpha and the direct sky from
        // image 1's vector times the sky ambient light, so neither is baked here.
        let mut light = Vec3::ZERO;
        let mut sun = 0.0f32;
        for l in ctx.lights_at(t.position) {
            let s = ctx.direct(l, t.position, t.normal, f32::MAX);
            if l.kind == rir::LightType::Sun {
                sun = sun.max(s.sun_visibility);
            } else {
                light += s.colour;
            }
        }
        if rays > 0 && scene.settings.bounces > 0 {
            light += indirect(ctx, t.position, t.normal, Vec3::ONE, Vec3::ZERO, rays, scene.settings.bounces);
        }
        let light = light.clamp(Vec3::ZERO, Vec3::splat(RGBM_MAX));
        let sun_term = if mode == crate::BakeMode::None { 1.0 } else { sun };
        let dir = if mode == crate::BakeMode::None { n } else { sky_vector(ctx.bvh, t.position, n, rays.max(ao_rays), ctx.seed) };
        Some((px, py, px + py * pw, light, sun_term, dir, t.position, t.normal))
    };
    #[cfg(feature = "parallel")]
    let shaded: Vec<_> = {
        use rayon::prelude::*;
        texels.par_iter().enumerate().map(|(i, slot)| shade(i, slot)).collect()
    };
    #[cfg(not(feature = "parallel"))]
    let shaded: Vec<_> = texels.iter().enumerate().map(|(i, slot)| shade(i, slot)).collect();
    for (px, py, texel, light, sun_term, dir, position, normal) in shaded.into_iter().flatten() {
        page.set(px, py, light, sun_term);
        page.set_dir(px, py, dir);
        if mode != crate::BakeMode::None {
            rtl.fill(&mut page.rtl, texel, ctx, &scene.lights, position, normal);
        }
    }
    Ok(())
}

/// Shades every chart of one page.
pub fn shade_page(
    scene: &rir::BakeScene,
    ctx: &DirectContext,
    layout: &ChartLayout,
    page: &mut BakedPage,
    albedo: &AlbedoTable,
    mode: crate::BakeMode,
    rtl: &crate::rtl::RtlSlots,
    meter: Option<&crate::progress::Meter>,
) -> Result<()> {
    let page_index = page.index;
    for chart in layout.charts.iter().filter(|c| c.page == page_index) {
        shade_chart(scene, ctx, chart, page, albedo, mode, rtl)?;
        if let Some(m) = meter {
            m.tick(chart.inner[0] as u64 * chart.inner[1] as u64);
        }
    }
    Ok(())
}

/// Checks a scene before shading, so a bad input is reported once.
pub fn check_scene(scene: &rir::BakeScene) -> Result<()> {
    if !scene.world_mins.is_finite() || !scene.world_maxs.is_finite() {
        return Err(Error::scene("the world bounds are not finite"));
    }
    if scene.world_mins.x > scene.world_maxs.x || scene.world_mins.y > scene.world_maxs.y {
        return Err(Error::scene("the world bounds are inverted"));
    }
    if !scene.settings.sky_color.is_finite() || !scene.settings.sky_intensity.is_finite() {
        return Err(Error::scene("the sky term is not finite"));
    }
    for (i, s) in scene.surfaces.iter().enumerate() {
        if s.positions.len() != s.normals.len() {
            return Err(Error::scene(format!(
                "surface {i} has {} positions and {} normals",
                s.positions.len(),
                s.normals.len()
            )));
        }
        if s.indices.len() % 3 != 0 {
            return Err(Error::scene(format!(
                "surface {i} has {} indices, not a multiple of 3",
                s.indices.len()
            )));
        }
        if s.positions.iter().any(|p| !p.is_finite()) {
            return Err(Error::scene(format!("surface {i} has a non-finite position")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod raster_tests {
    use super::*;
    use crate::charts::Chart;

    #[test]
    fn a_sliver_thinner_than_a_texel_still_gets_one() {
        // A 0.2-texel-wide sliver between texel centres (x = 0.6..0.8 of an 8x8 atlas).
        let uv = |x: f32, y: f32| glam::Vec2::new(x / 8.0, y / 8.0);
        let surface = rir::RenderSurface {
            positions: vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 64.0, 0.0)],
            normals: vec![Vec3::Z; 3],
            uv1: vec![uv(0.6, 0.2), uv(0.8, 0.2), uv(0.6, 7.8)],
            indices: vec![0, 2, 1],
            lightmap_size: [8, 8],
            ..Default::default()
        };
        let chart = Chart {
            surface: 0,
            object: None,
            page: 0,
            origin: [2, 2],
            size: [12, 12],
            basis: [[0.0; 2]; 3],
            anchor: Vec3::ZERO,
            inner: [8, 8],
            normal: Vec3::Z,
            atlas: true,
        };
        let texels = rasterize(&chart, &surface);
        let covered = texels.iter().filter(|t| t.is_some()).count();
        assert!(covered >= 7, "the sliver covered {covered} texels");
    }
}

#[cfg(test)]
mod light_index_tests {
    use super::*;

    fn light(kind: rir::LightType, position: Vec3, range: f32) -> rir::BakeLight {
        rir::BakeLight {
            object: None,
            kind,
            position,
            direction: Vec3::NEG_Z,
            color: Vec3::ONE,
            intensity: 1.0,
            range,
            inner_angle: 30.0,
            outer_angle: 60.0,
            size: glam::Vec2::splat(64.0),
            cast_shadows: false,
            baked: true,
            realtime: false,
            tweak_index: None,
            attenuation: None,
            flags: None,
        }
    }

    /// A small deterministic generator, so the test needs no crate.
    fn lcg(state: &mut u64) -> f32 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*state >> 33) as f32) / (1u64 << 31) as f32
    }

    #[test]
    fn every_light_that_reaches_a_point_is_near_it_in_scene_order() {
        let mut s = 7u64;
        let mut lights = vec![light(rir::LightType::Sun, Vec3::ZERO, 0.0)];
        for _ in 0..400 {
            let p = Vec3::new(lcg(&mut s), lcg(&mut s), lcg(&mut s)) * 20000.0 - 10000.0;
            let kind = if lcg(&mut s) < 0.5 { rir::LightType::Point } else { rir::LightType::Spot };
            lights.push(light(kind, p, 50.0 + lcg(&mut s) * 3000.0));
        }
        lights.push(light(rir::LightType::Area, Vec3::ZERO, 100.0));
        lights.push(light(rir::LightType::Point, Vec3::ZERO, f32::INFINITY));
        let index = LightIndex::build(&lights);
        for _ in 0..4000 {
            let p = Vec3::new(lcg(&mut s), lcg(&mut s), lcg(&mut s)) * 24000.0 - 12000.0;
            let near: Vec<u32> = index.near(p).collect();
            assert!(near.windows(2).all(|w| w[0] < w[1]), "scene order, no repeats");
            for (i, l) in lights.iter().enumerate() {
                let reaches = match l.kind {
                    rir::LightType::Point | rir::LightType::Spot => falloff(p.distance(l.position), l.range) > 0.0,
                    _ => true,
                };
                if reaches {
                    assert!(near.contains(&(i as u32)), "light {i} reaches {p} but is not near it");
                }
            }
        }
    }

    #[test]
    fn a_far_light_is_skipped() {
        let lights = vec![light(rir::LightType::Point, Vec3::ZERO, 100.0)];
        let index = LightIndex::build(&lights);
        assert_eq!(index.near(Vec3::splat(50000.0)).count(), 0);
        assert_eq!(index.near(Vec3::splat(10.0)).count(), 1);
    }
}

#[cfg(test)]
mod soft_light_tests {
    use super::*;

    fn point(size: f32) -> rir::BakeLight {
        rir::BakeLight {
            object: None,
            kind: rir::LightType::Point,
            position: Vec3::new(0.0, 0.0, 10.0),
            direction: Vec3::NEG_Z,
            color: Vec3::ONE,
            intensity: 1.0,
            range: 200.0,
            inner_angle: 30.0,
            outer_angle: 60.0,
            size: glam::Vec2::splat(size),
            cast_shadows: false,
            baked: true,
            realtime: false,
            tweak_index: None,
            attenuation: None,
            flags: None,
        }
    }

    #[test]
    fn a_sized_light_has_no_hot_spot_and_matches_a_point_far_away() {
        let tris = vec![];
        let bvh = Bvh::build(&tris).unwrap();
        let settings = rir::BakeSettings::default();
        let lights = [point(0.0), point(160.0)];
        let ctx = DirectContext { bvh: &bvh, tris: &bvh.triangles, lights: &lights, settings: &settings, seed: 1, index: None, albedo: &[] };
        let at = |l: &rir::BakeLight, p: Vec3| ctx.direct(l, p, Vec3::Z, f32::MAX).colour.x;
        let near = Vec3::ZERO;
        let far = Vec3::new(250.0, 0.0, 0.0);
        assert!(at(&lights[1], near) < 0.75 * at(&lights[0], near), "the hot spot under the light is spread");
        let (a, b) = (at(&lights[0], far), at(&lights[1], far));
        assert!(b > 0.0 && (a - b).abs() / a < 0.5, "far away the two agree within a half: {a} vs {b}");
    }

    #[test]
    fn the_falloff_fades_to_zero_without_a_step() {
        let r = 100.0;
        assert!(falloff(199.0, r) < 1.0e-3 * falloff(0.0, r), "next to the end of its reach the light is all but gone");
        let mut last = falloff(0.0, r);
        for i in 1..200 {
            let v = falloff(i as f32, r);
            assert!(v < last && last - v < 0.03, "smooth and decreasing at {i}: {last} -> {v}");
            last = v;
        }
    }

    /// A floor quad at z 0 and a ceiling quad at `ceiling`, both `half` wide.
    fn slabs(ceiling: f32, half: f32) -> Vec<Tri> {
        let mut tris = vec![];
        for z in [0.0, ceiling] {
            let (a, b, c, d) = (Vec3::new(-half, -half, z), Vec3::new(half, -half, z), Vec3::new(half, half, z), Vec3::new(-half, half, z));
            tris.push(Tri::new(a, b, c));
            tris.push(Tri::new(a, c, d));
        }
        tris
    }

    #[test]
    fn a_lamp_under_a_ceiling_lights_the_floor() {
        let bvh = Bvh::build(&slabs(12.0, 1000.0)).unwrap();
        let settings = rir::BakeSettings::default();
        let lights = [rir::BakeLight { cast_shadows: true, ..point(0.0) }, rir::BakeLight { cast_shadows: true, ..point(40.0) }];
        let ctx = DirectContext { bvh: &bvh, tris: &bvh.triangles, lights: &lights, settings: &settings, seed: 1, index: None, albedo: &[] };
        for l in &lights {
            let lit = ctx.direct(l, Vec3::new(30.0, 0.0, 0.01), Vec3::Z, f32::MAX).colour.x;
            assert!(lit > 0.0, "the ceiling above the lamp does not shadow the floor below it");
        }
    }

    #[test]
    fn a_panel_casts_a_penumbra() {
        // A wall edge at x = 0 between the panel (centred over it) and the floor:
        // a floor point under the edge sees part of the panel.
        let wall = vec![Tri::new(Vec3::new(0.0, -500.0, 2.0), Vec3::new(500.0, -500.0, 2.0), Vec3::new(500.0, 500.0, 2.0)),
                        Tri::new(Vec3::new(0.0, -500.0, 2.0), Vec3::new(500.0, 500.0, 2.0), Vec3::new(0.0, 500.0, 2.0))];
        let bvh = Bvh::build(&wall).unwrap();
        let settings = rir::BakeSettings::default();
        let panel = rir::BakeLight { cast_shadows: true, ..point(16.0) };
        let lights = [panel];
        let ctx = DirectContext { bvh: &bvh, tris: &bvh.triangles, lights: &lights, settings: &settings, seed: 1, index: None, albedo: &[] };
        let full = ctx.direct(&lights[0], Vec3::new(-50.0, 0.0, 0.0), Vec3::Z, f32::MAX).colour.x;
        let edge = ctx.direct(&lights[0], Vec3::new(0.5, 0.0, 0.0), Vec3::Z, f32::MAX).colour.x;
        let open = ctx.direct(&rir::BakeLight { cast_shadows: false, ..lights[0].clone() }, Vec3::new(0.5, 0.0, 0.0), Vec3::Z, f32::MAX).colour.x;
        let dark = ctx.direct(&lights[0], Vec3::new(50.0, 0.0, 0.0), Vec3::Z, f32::MAX).colour.x;
        assert!(full > 0.0 && dark == 0.0, "{full} {dark}");
        assert!(edge > 0.1 * open && edge < 0.9 * open, "under the edge the panel is partly hidden: {edge} of {open}");
    }
}
