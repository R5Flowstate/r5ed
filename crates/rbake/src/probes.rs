//! Light probes: placement, spherical-harmonic lighting, the tree over the
//! probe groups, and the prop assignments.
//!
//! The probe record's twelve ambient coefficients are four basis values per
//! colour channel, and the shipped maps keep the three channels exactly
//! proportional to one another, which is what fixes the reading: index 3, 7 and
//! 11 are the non-negative ambient term and the three before them are the
//! signed linear terms along x, y and z.

use glam::Vec3;
use rbsp::lumps::lighting as lmp;

use crate::bvh::Bvh;
use crate::error::{Error, Result};
use crate::light::{sky_light, sky_visibility, DirectContext};

/// The engine reads every ambient coefficient as the stored value over 8192,
/// in linear light, so 1.0 is stored as 8192 and the range tops out near 4.
pub const SH_SCALE: f32 = 8192.0;
/// The full-scale value of the packed sun direction and visibility words.
pub const SKY_WORD_SCALE: f32 = 8192.0;
/// Slots a probe record carries for static lights.
pub const PROBE_STATIC_SLOTS: usize = 4;
/// The value the shipped maps store in an unused static-light slot.
pub const PROBE_EMPTY_INDEX: i16 = -1;
/// A static-light index is stored with this bias where its weight is set.
pub const PROBE_INDEX_BIAS: i32 = 32;

/// One probe's world position and its lighting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    pub position: Vec3,
    /// Three colour channels of four basis values: three linear terms then the
    /// ambient.
    pub ambient: [[i16; 4]; 3],
    /// The packed sun direction and visibilities, at 1/8192.
    pub sky: [i16; 4],
    pub static_weights: [u8; 4],
    pub static_indexes: [i16; 4],
}

impl Probe {
    /// The record the engine reads.
    pub fn to_record(&self) -> lmp::LightProbe {
        let mut p = lmp::LightProbe::default();
        for (slot, src) in p.ambient_sh.chunks_exact_mut(4).zip(self.ambient.iter()) {
            slot.copy_from_slice(src);
        }
        p.sky_dir_sun_vis = self.sky;
        p.static_light_weights = self.static_weights;
        p.static_light_indexes = self.static_indexes;
        p
    }

    /// Reads a record back, ignoring any pad the older 48-byte form carries.
    pub fn from_record(p: &lmp::LightProbe) -> Self {
        let mut ambient = [[0i16; 4]; 3];
        for (slot, src) in ambient.iter_mut().zip(p.ambient_sh.chunks_exact(4)) {
            slot.copy_from_slice(src);
        }
        Self {
            position: Vec3::ZERO,
            ambient,
            sky: p.sky_dir_sun_vis,
            static_weights: p.static_light_weights,
            static_indexes: p.static_light_indexes,
        }
    }
}

/// The most references a leaf of the probe tree holds. The shipped trees hold
/// 1 to 8, and the client's walk has a fixed budget of records per query.
pub const LEAF_REFS: usize = 8;

/// The kind bits that mark a node as a leaf, and so end the engine's walk.
pub const LEAF: u32 = lmp::LightProbeTreeNode::LEAF;

/// Everything the probe pass produces.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probes {
    pub probes: Vec<Probe>,
    /// One reference per probe: where it is and which record it is. The probe
    /// record carries no position, so this is how the client finds a probe.
    pub refs: Vec<lmp::LightProbeRef>,
    /// The k-d tree over `refs`.
    pub tree: Vec<lmp::LightProbeTreeNode>,
    pub parent_infos: Vec<lmp::LightProbeParentInfo>,
    pub prop_indices: Vec<u32>,
}

impl Probes {
    /// The nearest probe to a world position, by index.
    pub fn nearest(&self, p: Vec3) -> Option<u32> {
        let mut best = None;
        let mut best_d = f32::INFINITY;
        for (i, probe) in self.probes.iter().enumerate() {
            let d = probe.position.distance_squared(p);
            if d < best_d {
                best_d = d;
                best = Some(i as u32);
            }
        }
        best
    }

    /// The probe the tree finds nearest to a position, walking it the way the
    /// client does: descend into the side of each split the position is on,
    /// then take the nearest reference of the leaf it lands in.
    pub fn tree_lookup(&self, p: Vec3) -> Option<u32> {
        let mut at = 0usize;
        for _ in 0..=self.tree.len() {
            let n = self.tree.get(at)?;
            if n.is_leaf() {
                let first = n.first() as usize;
                let refs = self.refs.get(first..first + n.count() as usize)?;
                return refs
                    .iter()
                    .min_by(|a, b| {
                        let da = Vec3::from_array(a.origin).distance_squared(p);
                        let db = Vec3::from_array(b.origin).distance_squared(p);
                        da.total_cmp(&db)
                    })
                    .map(|r| r.probe);
            }
            let child = n.first() as usize;
            at = if p[n.kind() as usize] <= n.split() { child } else { child + 1 };
        }
        None
    }
}

/// Open space a probe needs around it, in units.
const PROBE_CLEARANCE: f32 = 48.0;

/// The probe grid: points every `spacing` inside the world bounds, plus the
/// designer's own, culled to open space.
pub fn place_probes(
    world_mins: Vec3,
    world_maxs: Vec3,
    spacing: f32,
    extra: &[Vec3],
    bvh: &Bvh,
) -> Result<Vec<Vec3>> {
    let step = if spacing.is_finite() && spacing > 0.0 {
        spacing
    } else {
        192.0
    };
    let mut out: Vec<Vec3> = Vec::new();
    let size = world_maxs - world_mins;
    let per_axis = [
        (size.x / step).floor().clamp(0.0, 4096.0) as usize,
        (size.y / step).floor().clamp(0.0, 4096.0) as usize,
        (size.z / step).floor().clamp(0.0, 4096.0) as usize,
    ];
    let total: usize = per_axis.iter().map(|n| n.saturating_add(1)).product();
    if total > 4_000_000 {
        return Err(Error::lighting(format!(
            "a probe every {step} units over the world bounds needs {total} points; raise probe_spacing"
        )));
    }
    let inset = step * 0.25;
    for iz in 0..=per_axis[2] {
        for iy in 0..=per_axis[1] {
            for ix in 0..=per_axis[0] {
                let p = world_mins + Vec3::new(ix as f32 * step, iy as f32 * step, iz as f32 * step);
                out.push(p);
            }
        }
    }
    let _ = inset;
    let mut out: Vec<Vec3> = out.into_iter().filter(|p| in_open_space(bvh, *p, step)).collect();
    // Placed points are kept as they are, as cubemap points are: the open-space
    // test is a parity count that only holds on closed geometry, and a shipped
    // map's own probe positions are already where its probes belong.
    out.extend(extra.iter().copied().filter(|p| p.is_finite()));
    Ok(out)
}

/// A point is usable when it is outside every solid and can see open sky or at
/// least an open hemisphere. Parity decides "inside"; a shadow ray decides the
/// rest.
pub fn in_open_space(bvh: &Bvh, p: Vec3, probe_radius: f32) -> bool {
    if bvh.inside_solid(p) {
        return false;
    }
    // Room for a character, not half the grid step: at a coarse step the
    // larger test culled every probe in a corridor or under a low roof, and
    // whatever stood indoors took a probe from outside.
    let r = (probe_radius.max(1.0) * 0.5).min(PROBE_CLEARANCE);
    if bvh.occluded(&crate::bvh::Ray::new(p, Vec3::X, 0.01, r))
        || bvh.occluded(&crate::bvh::Ray::new(p, Vec3::NEG_X, 0.01, r))
        || bvh.occluded(&crate::bvh::Ray::new(p, Vec3::Y, 0.01, r))
        || bvh.occluded(&crate::bvh::Ray::new(p, Vec3::NEG_Y, 0.01, r))
        || bvh.occluded(&crate::bvh::Ray::new(p, Vec3::Z, 0.01, r))
    {
        return false;
    }
    true
}

/// The sun direction the sky and probe words use: the direction the sun's light
/// travels, or straight down when the scene has no sun.
pub fn sun_direction(lights: &[rir::BakeLight]) -> Vec3 {
    lights
        .iter()
        .find(|l| l.kind == rir::LightType::Sun)
        .map(|l| l.direction.normalize_or_zero())
        .filter(|d| d.length_squared() > 0.5)
        .unwrap_or(Vec3::NEG_Z)
}

/// Builds the probe set: placement, lighting, groups, tree and prop
/// assignments.
pub fn bake_probes(
    scene: &rir::BakeScene,
    ctx: &DirectContext,
    bvh: &Bvh,
) -> Result<Probes> {
    let spacing = if scene.settings.probe_spacing > 0.0 {
        scene.settings.probe_spacing
    } else {
        192.0
    };
    let positions = place_probes(scene.world_mins, scene.world_maxs, spacing, &scene.probe_points, bvh)?;
    if positions.is_empty() {
        // A map with no open space still needs one probe: the engine reads the
        // ambient of the first record for the whole map.
        let centre = (scene.world_mins + scene.world_maxs) * 0.5;
        return Ok(Probes {
            probes: vec![Probe {
                position: centre,
                ambient: default_ambient(&scene.settings),
                sky: default_sky(sun_direction(&scene.lights)),
                ..Probe::default()
            }],
            prop_indices: scene.props.iter().map(|_| 0u32).collect(),
            ..single_probe_index(centre)
        });
    }
    let dir = sun_direction(&scene.lights);
    let rays = (scene.settings.samples.max(1) as usize).min(64);
    let meter = crate::progress::Meter::new("light probes", "probes", positions.len() as u64, 80, 86);
    // Each probe is lit on its own, so the set shades across every core.
    #[cfg(feature = "parallel")]
    let probes: Vec<Probe> = {
        use rayon::prelude::*;
        positions
            .par_iter()
            .map(|p| {
                let probe = shade_probe(ctx, bvh, *p, dir, rays);
                meter.tick(1);
                probe
            })
            .collect()
    };
    #[cfg(not(feature = "parallel"))]
    let probes: Vec<Probe> = positions
        .iter()
        .map(|p| {
            let probe = shade_probe(ctx, bvh, *p, dir, rays);
            meter.tick(1);
            probe
        })
        .collect();
    meter.finish();
    let (refs, tree) = build_tree(&positions);
    let prop_indices = scene
        .props
        .iter()
        .map(|prop| visible_nearest_index(&positions, &probes, prop, bvh))
        .collect();
    let parent_infos = vec![lmp::LightProbeParentInfo::over(refs.len(), tree.len())];
    Ok(Probes {
        probes,
        refs,
        tree,
        parent_infos,
        prop_indices,
    })
}

/// Probes a prop may take, nearest first, before falling back to the nearest.
const PROP_PROBE_CANDIDATES: usize = 32;

/// The nearest probe with a clear line to the prop. The nearest by distance
/// alone is often across a wall or under an awning, which lights neighbouring
/// props of one pile from opposite sides of a shadow. The line stops at the
/// prop's bounding sphere so the prop's own surfaces do not block it.
///
/// A single centre ray threads doorways: a prop deep inside takes an outdoor
/// probe through the slit and draws standing in full sun, 961 of them on one
/// map. The line has to see the prop's volume, not a point: the centre plus a
/// flank on each cross axis, offset at the prop end, with at least one flank
/// per axis clearing alongside the centre. Anything thinner is a slit and
/// falls through to the ordinary candidates. Of the seen candidates one that
/// sees the sun wins, nearest first on a tie; without a sunlit candidate the
/// prop keeps the nearest, which is what a prop genuinely under cover takes.
fn visible_nearest_index(positions: &[Vec3], probes: &[Probe], prop: &rir::StaticPropInstance, bvh: &Bvh) -> u32 {
    let centre = prop.world_centre();
    let scale = if prop.scale > 0.0 { prop.scale } else { 1.0 };
    let radius = if prop.bounds_min.cmple(prop.bounds_max).all() {
        (prop.bounds_max - prop.bounds_min).length() * 0.5 * scale
    } else {
        0.0
    };
    // Half-width of the volume a sight-line must see, in world units.
    const FLANK: f32 = 20.0;
    let mut order: Vec<(f32, u32)> = positions.iter().enumerate().map(|(i, q)| (q.distance_squared(centre), i as u32)).collect();
    let keep = PROP_PROBE_CANDIDATES.min(order.len());
    if keep == 0 {
        return 0;
    }
    if keep < order.len() {
        order.select_nth_unstable_by(keep - 1, |a, b| a.0.total_cmp(&b.0));
        order.truncate(keep);
    }
    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut multi_lit: Option<u32> = None;
    let mut multi_any: Option<u32> = None;
    let mut single_lit: Option<u32> = None;
    let mut single_any: Option<u32> = None;
    for &(d2, i) in &order {
        let probe = positions[i as usize];
        let dist = d2.sqrt();
        if dist < 1e-3 {
            let lit = probes.get(i as usize).map(|p| p.sky[3] > 0).unwrap_or(false);
            if lit && single_lit.is_none() {
                single_lit = Some(i);
            }
            if single_any.is_none() {
                single_any = Some(i);
            }
            continue;
        }
        let reach = dist - radius;
        let rdir = (centre - probe) / dist;
        let single = reach <= 0.0
            || !bvh.occluded(&crate::bvh::Ray::new(probe, rdir, 0.01, reach));
        if !single {
            continue;
        }
        let lit = probes.get(i as usize).map(|p| p.sky[3] > 0).unwrap_or(false);
        if lit && single_lit.is_none() {
            single_lit = Some(i);
        }
        if single_any.is_none() {
            single_any = Some(i);
        }
        let up = if rdir.z.abs() > 0.9 { Vec3::X } else { Vec3::Z };
        let t1 = rdir.cross(up).normalize_or_zero();
        let t2 = rdir.cross(t1).normalize_or_zero();
        let mut axis_clear = [false, false];
        for (a, t) in [(0, t1), (1, t2)] {
            for s in [1.0, -1.0] {
                let o = centre + t * (FLANK * s);
                let back = (probe - o).length();
                if back > 0.01
                    && !bvh.occluded(&crate::bvh::Ray::new(o, (probe - o) / back, 0.01, back))
                {
                    axis_clear[a] = true;
                    break;
                }
            }
        }
        if axis_clear[0] && axis_clear[1] {
            if lit && multi_lit.is_none() {
                multi_lit = Some(i);
            }
            if multi_any.is_none() {
                multi_any = Some(i);
            }
        }
    }
    multi_lit.or(multi_any).or(single_lit).or(single_any).unwrap_or_else(|| order[0].1)
}

/// Lights a probe: an ambient term and a linear gradient per channel, taken
/// from the direct light and the sky in eight cosine-weighted directions.
fn shade_probe(ctx: &DirectContext, bvh: &Bvh, position: Vec3, sun: Vec3, rays: usize) -> Probe {
    let up = Vec3::Z;
    let mut dirs: Vec<Vec3> = Vec::new();
    if let Some(l) = ctx.lights.iter().find(|l| l.kind == rir::LightType::Sun) {
        dirs.push(-l.direction.normalize_or_zero());
    }
    dirs.push(up);
    let base_dir = dirs.first().copied().unwrap_or(up);
    let (t, b) = {
        let a = if base_dir.z.abs() > 0.9 { Vec3::X } else { Vec3::Z };
        let t = a.cross(base_dir).normalize_or_zero();
        (t, base_dir.cross(t).normalize_or_zero())
    };
    dirs.push(t);
    dirs.push(-t);
    dirs.push(b);
    dirs.push(-b);
    let dir_rays = (rays / 2).max(8);
    let mut per_dir: Vec<Vec3> = Vec::with_capacity(dirs.len());
    for d in &dirs {
        // The client lights a probed object with the sun itself, gated by the
        // probe's sun visibility, so the stored ambient leaves the sun out as a
        // lightmap page does.
        let mut acc = Vec3::ZERO;
        for l in ctx.lights_at(position).filter(|l| l.kind != rir::LightType::Sun) {
            acc += ctx.direct(l, position, *d, f32::MAX).colour;
        }
        // Occluded per direction, so a roof takes the sky out of the gradient
        // as well as the ambient; the floor stands in for bounce light.
        let seen = sky_visibility(bvh, position, *d, dir_rays);
        acc += sky_light(ctx.settings, *d, 0.25 + 0.75 * seen);
        // Bounce light, as a lightmap texel takes it: without it a character
        // read darker than the lit walls around it.
        acc += crate::light::indirect(ctx, position, *d, Vec3::ONE, Vec3::ZERO, PROBE_BOUNCE_RAYS, ctx.settings.bounces);
        per_dir.push(acc);
    }
    // sky_light already carries the sky intensity; scaling the sum by it again
    // took a night map's probes (sky intensity 0.13) to an eighth of their light.
    let ambient = per_dir.iter().fold(Vec3::ZERO, |a, v| a + *v) / per_dir.len() as f32;
    let mut probe = Probe {
        position,
        ambient: pack_ambient(ambient, dirs_per_channel(&dirs, &per_dir)),
        sky: default_sky(sun),
        ..Probe::default()
    };
    let (sky_dir, _open) = sky_vector(ctx, bvh, position, rays);
    probe.sky[0] = quantise_i16(sky_dir.x, SKY_WORD_SCALE);
    probe.sky[1] = quantise_i16(sky_dir.y, SKY_WORD_SCALE);
    probe.sky[2] = quantise_i16(sky_dir.z, SKY_WORD_SCALE);
    probe.sky[3] = quantise_i16(sun_visibility_at(ctx, bvh, position, sun), SKY_WORD_SCALE);
    probe.static_weights = [0; 4];
    probe.static_indexes = [PROBE_EMPTY_INDEX; 4];
    probe
}

/// Rays per direction for a probe's bounce light.
const PROBE_BOUNCE_RAYS: usize = 16;

/// The direction a probe sees the sky in, scaled by how much of the upper
/// hemisphere is open: stock probes store that vector in their first three sky
/// words (length 1 under open sky, shorter and bent under cover).
fn sky_vector(ctx: &DirectContext, bvh: &Bvh, position: Vec3, rays: usize) -> (Vec3, f32) {
    let n = rays.max(8);
    let mut rng = crate::light::Rng::new(ctx.seed ^ crate::light::position_hash(position) ^ 0x5ca1_ab1e);
    let mut sum = Vec3::ZERO;
    let mut seen = 0usize;
    for _ in 0..n {
        let d = rng.cosine_dir(Vec3::Z);
        if d.length_squared() < 0.5 {
            continue;
        }
        if !bvh.occluded(&crate::bvh::Ray::new(position + d * 0.05, d, 0.01, f32::MAX)) {
            sum += d;
            seen += 1;
        }
    }
    let open = seen as f32 / n as f32;
    (sum.normalize_or_zero() * open, open)
}

fn sun_visibility_at(ctx: &DirectContext, bvh: &Bvh, position: Vec3, dir: Vec3) -> f32 {
    if ctx
        .lights
        .iter()
        .find(|l| l.kind == rir::LightType::Sun)
        .map(|l| !l.cast_shadows)
        .unwrap_or(true)
    {
        return 1.0;
    }
    // `dir` is the way the sun's light travels; the shadow ray goes back toward it.
    // The origin steps off the surface first: a probe placed flush against a wall
    // face starts the ray inside that wall and reads as fully occluded, which costs
    // the whole neighbourhood around it the sun. One ray decides the sun's on/off
    // state for every prop the probe lights, so it has to survive geometry it is
    // only touching.
    if bvh.occluded(&crate::bvh::Ray::new(position + -dir * 0.05, -dir, 0.01, f32::MAX)) {
        0.0
    } else {
        1.0
    }
}

/// The signed linear terms of each channel: every sample's light, per channel,
/// projected onto the direction it was taken in.
fn dirs_per_channel(dirs: &[Vec3], per_dir: &[Vec3]) -> [Vec3; 3] {
    let mut out = [Vec3::ZERO; 3];
    let n = per_dir.len().max(1) as f32;
    for (d, light) in dirs.iter().zip(per_dir) {
        for (c, slot) in out.iter_mut().enumerate() {
            *slot += *d * (light[c] / n);
        }
    }
    out
}

fn pack_ambient(ambient: Vec3, linear: [Vec3; 3]) -> [[i16; 4]; 3] {
    let mut out = [[0i16; 4]; 3];
    for c in 0..3 {
        let a = ambient[c];
        let l = linear[c];
        // The linear terms are stored negated: across staging_mu1's 52,457
        // probes, lit from above by the sky, the z word is negative on 85% and
        // falls as the ambient rises (correlation -0.6).
        out[c][3] = quantise_i16(a.max(0.0), SH_SCALE);
        out[c][0] = quantise_i16(-l.x, SH_SCALE);
        out[c][1] = quantise_i16(-l.y, SH_SCALE);
        out[c][2] = quantise_i16(-l.z, SH_SCALE);
    }
    out
}

fn quantise_i16(v: f32, scale: f32) -> i16 {
    if !v.is_finite() {
        return 0;
    }
    (v * scale).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

/// A probe with no light at all: the ambient term alone.
pub fn default_ambient(settings: &rir::BakeSettings) -> [[i16; 4]; 3] {
    let a = settings.sky_color * settings.sky_intensity * 0.5;
    pack_ambient(a, [Vec3::ZERO; 3])
}

/// The sky words of an unlit probe: open sky straight up, sun visible. Words 0-2
/// are the open-sky direction scaled by openness, word 3 the sun visibility (as
/// staging_mu1's probes hold them: word 3 is 0 or 8192 on 90% of them).
pub fn default_sky(_sun: Vec3) -> [i16; 4] {
    [0, 0, SH_WORD_MAX, SH_WORD_MAX]
}

const SH_WORD_MAX: i16 = 8192;

/// The references and tree for a map with one probe at `position`: one
/// reference and one leaf that covers it. The parent info names that probe.
pub fn single_probe_index(position: Vec3) -> Probes {
    let (refs, tree) = build_tree(&[position]);
    Probes {
        refs,
        tree,
        parent_infos: vec![lmp::LightProbeParentInfo::over(1, 1)],
        ..Probes::default()
    }
}

/// One reference per probe and a binary k-d tree over them, in the layout the
/// shipped maps use.
///
/// Each interior node splits its references at the median of their widest
/// axis; its two children are written side by side after it, and the tree is
/// laid out depth first, so the leaves' reference ranges tile the reference
/// lump in order. A leaf holds at most [`LEAF_REFS`] references and stores
/// their count as an integer.
pub fn build_tree(positions: &[Vec3]) -> (Vec<lmp::LightProbeRef>, Vec<lmp::LightProbeTreeNode>) {
    let mut items: Vec<(Vec3, u32)> = positions.iter().enumerate().map(|(i, p)| (*p, i as u32)).collect();
    let mut refs = Vec::with_capacity(items.len());
    let mut tree = vec![lmp::LightProbeTreeNode::default()];
    build_node(0, &mut items, &mut refs, &mut tree);
    (refs, tree)
}

fn build_node(
    at: usize,
    items: &mut [(Vec3, u32)],
    refs: &mut Vec<lmp::LightProbeRef>,
    tree: &mut Vec<lmp::LightProbeTreeNode>,
) {
    if items.len() <= LEAF_REFS {
        tree[at] = lmp::LightProbeTreeNode::leaf(refs.len() as u32, items.len() as u32);
        refs.extend(items.iter().map(|(p, i)| lmp::LightProbeRef {
            origin: p.to_array(),
            probe: *i,
            cube_index: 0,
            unknown: 0,
        }));
        return;
    }
    let (lo, hi) = items.iter().fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), (p, _)| (lo.min(*p), hi.max(*p)));
    let extent = hi - lo;
    let axis = if extent.x >= extent.y && extent.x >= extent.z {
        0
    } else if extent.y >= extent.z {
        1
    } else {
        2
    };
    items.sort_by(|a, b| a.0[axis].total_cmp(&b.0[axis]).then(a.1.cmp(&b.1)));
    // The boundary between two distinct coordinates nearest the median, so a
    // probe on the split plane is never on both sides of it. Coincident probes
    // have no such boundary and split at the median.
    let half = items.len() / 2;
    let distinct = |k: usize| items[k - 1].0[axis] < items[k].0[axis];
    let mid = (0..items.len())
        .flat_map(|d| [half.checked_sub(d), half.checked_add(d)])
        .flatten()
        .find(|&k| k >= 1 && k < items.len() && distinct(k))
        .unwrap_or(half);
    let split = (items[mid - 1].0[axis] + items[mid].0[axis]) * 0.5;
    let child = tree.len();
    tree.push(lmp::LightProbeTreeNode::default());
    tree.push(lmp::LightProbeTreeNode::default());
    tree[at] = lmp::LightProbeTreeNode::interior(axis as u32, child as u32, split);
    let (low, high) = items.split_at_mut(mid);
    build_node(child, low, refs, tree);
    build_node(child + 1, high, refs, tree);
}

/// A small deterministic sequence for the probe pass, so two runs agree.
pub fn probe_seed(scene: &rir::BakeScene) -> u32 {
    let mut seed = 0x9E37_79B9u32;
    for p in &scene.probe_points {
        for c in [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()] {
            seed = seed.rotate_left(5) ^ c;
        }
    }
    seed
}

/// Convenience: the sun visibility words of a probe set, for a report.
pub fn sky_words(probes: &Probes) -> Vec<[i16; 4]> {
    probes.probes.iter().map(|p| p.sky).collect()
}

/// A tiny helper the editor uses to preview one probe's ambient.
pub fn probe_ambient_rgb(p: &Probe) -> Vec3 {
    Vec3::new(p.ambient[0][3] as f32 / SH_SCALE, p.ambient[1][3] as f32 / SH_SCALE, p.ambient[2][3] as f32 / SH_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_stores_light_at_the_scale_the_engine_reads() {
        // The engine multiplies every coefficient by 1/8192.
        let packed = pack_ambient(Vec3::new(0.5, 0.25, 1.0), [Vec3::new(0.125, 0.0, -0.25); 3]);
        assert_eq!([packed[0][3], packed[1][3], packed[2][3]], [4096, 2048, 8192]);
        assert_eq!(&packed[0][..3], &[-1024, 0, 2048], "linear terms are stored negated");
        let back = probe_ambient_rgb(&Probe { ambient: packed, ..Probe::default() });
        assert!(back.abs_diff_eq(Vec3::new(0.5, 0.25, 1.0), 1e-4));
    }

    #[test]
    fn linear_terms_point_where_the_light_comes_from() {
        let dirs = [Vec3::Z, -Vec3::Z, Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y];
        let light = [Vec3::splat(6.0), Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO];
        let l = dirs_per_channel(&dirs, &light);
        for (c, chan) in l.iter().enumerate() {
            assert!(chan.abs_diff_eq(Vec3::new(0.0, 0.0, 1.0), 1e-6), "channel {c}: {chan:?}");
        }
    }
    use rbsp::validate::{probe_tree_issues, Severity};

    fn errors(nodes: &[lmp::LightProbeTreeNode], refs: usize) -> Vec<String> {
        probe_tree_issues(nodes, refs)
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.message)
            .collect()
    }

    /// The invariants every shipped tree holds, measured on the client map in
    /// `agent-kit/`: interior children follow their parent side by side, every
    /// reference sits inside the box its path of splits describes, leaves hold
    /// 1 to 8 references counted as integers, and in depth-first order the
    /// leaves tile the reference lump.
    fn shipped_shape(nodes: &[lmp::LightProbeTreeNode], refs: &[lmp::LightProbeRef]) -> std::result::Result<usize, String> {
        let mut next_ref = 0u32;
        let mut leaves = 0usize;
        let mut stack = vec![(0usize, [f32::NEG_INFINITY; 3], [f32::INFINITY; 3])];
        while let Some((at, lo, hi)) = stack.pop() {
            let n = nodes.get(at).ok_or(format!("node {at} is out of range"))?;
            if n.is_leaf() {
                if !(1..=LEAF_REFS as u32).contains(&n.count()) {
                    return Err(format!("leaf {at} counts {} refs", n.count()));
                }
                if n.first() != next_ref {
                    return Err(format!("leaf {at} starts at ref {} not {next_ref}", n.first()));
                }
                for r in &refs[n.first() as usize..(n.first() + n.count()) as usize] {
                    for ax in 0..3 {
                        if r.origin[ax] < lo[ax] || r.origin[ax] > hi[ax] {
                            return Err(format!("a ref of leaf {at} is outside its box on axis {ax}"));
                        }
                    }
                }
                next_ref += n.count();
                leaves += 1;
                continue;
            }
            let c = n.first() as usize;
            if c <= at {
                return Err(format!("node {at} names child {c}, which does not follow it"));
            }
            let ax = n.kind() as usize;
            let (mut lo_hi, mut hi_lo) = (hi, lo);
            lo_hi[ax] = n.split();
            hi_lo[ax] = n.split();
            // Pushed high first so the low child is walked first: depth-first order.
            stack.push((c + 1, hi_lo, hi));
            stack.push((c, lo, lo_hi));
        }
        if next_ref as usize != refs.len() {
            return Err(format!("the leaves cover {next_ref} of {} refs", refs.len()));
        }
        Ok(leaves)
    }

    fn grid(n: usize, step: f32) -> Vec<Vec3> {
        let mut out = Vec::new();
        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    out.push(Vec3::new(x as f32 * step, y as f32 * step, z as f32 * step - 100.0));
                }
            }
        }
        out
    }

    #[test]
    fn the_tree_has_the_shipped_shape_and_passes_the_validator() {
        for positions in [grid(1, 64.0), grid(2, 64.0), grid(3, 64.0), grid(7, 128.0), grid(12, 96.0)] {
            let (refs, tree) = build_tree(&positions);
            assert!(errors(&tree, refs.len()).is_empty(), "{:?}", errors(&tree, refs.len()));
            let leaves = shipped_shape(&tree, &refs).expect("the shipped shape");
            assert!(leaves >= positions.len().div_ceil(LEAF_REFS));
            let mut seen = vec![false; positions.len()];
            for r in &refs {
                let i = r.probe as usize;
                assert!(!seen[i], "probe {i} is referenced twice");
                seen[i] = true;
                assert_eq!(r.origin, positions[i].to_array(), "a ref carries its probe's position");
                assert_eq!(r.unknown, 0);
            }
            assert!(seen.iter().all(|s| *s), "every probe is referenced");
        }
    }

    /// The 10:48 client crash. A one-probe map wrote its leaf count as the
    /// float 1.0, which the client reads as 0x3F800000 references and scans
    /// off the end of the lump. The crash registers held 0x3F800000.
    #[test]
    fn a_leaf_count_is_an_integer_not_a_float() {
        let (refs, tree) = build_tree(&[Vec3::new(0.0, 0.0, 64.0)]);
        assert_eq!(refs.len(), 1);
        assert_eq!(tree.len(), 1);
        assert!(tree[0].is_leaf());
        assert_eq!(tree[0].data, 1, "the count is the integer 1");
        assert_ne!(tree[0].data, 1.0f32.to_bits());
        let bytes: &[u8] = bytemuck::cast_slice(&tree);
        assert_eq!(bytes, &[3, 0, 0, 0, 1, 0, 0, 0], "the node the engine reads");

        let parent = single_probe_index(Vec3::ZERO).parent_infos;
        assert_eq!(parent[0].words, [0, 0, 0, 0, 1, 0, 1], "the parent info counts the references and the tree nodes");

        let as_float = lmp::LightProbeTreeNode { flags: LEAF, data: 1.0f32.to_bits() };
        let e = errors(&[as_float], 1);
        assert!(e.iter().any(|m| m.contains("float")), "the float count is refused: {e:?}");
    }

    /// What the compiler wrote before: an interior node that names itself as
    /// its own first child, which walked the client off its stack.
    #[test]
    fn a_self_referential_node_is_refused() {
        let self_referential = lmp::LightProbeTreeNode { flags: 1, data: 0 };
        assert!(!errors(&[self_referential], 1).is_empty());
        let cycle = [
            lmp::LightProbeTreeNode::interior(0, 1, 0.0),
            lmp::LightProbeTreeNode::interior(0, 0, 0.0),
            lmp::LightProbeTreeNode::leaf(0, 1),
        ];
        assert!(!errors(&cycle, 1).is_empty());
    }

    #[test]
    fn the_tree_walk_lands_on_a_nearby_probe() {
        let positions = grid(6, 100.0);
        let (refs, tree) = build_tree(&positions);
        let probes = Probes { refs, tree, ..Probes::default() };
        for (i, p) in positions.iter().enumerate() {
            assert_eq!(probes.tree_lookup(*p), Some(i as u32), "a probe's own position finds it");
        }
        let q = Vec3::new(260.0, 140.0, 20.0);
        let found = probes.tree_lookup(q).expect("a probe") as usize;
        assert!(positions[found].distance(q) < 200.0);
    }

    /// The shipped client map in `agent-kit/`: the validator's predicate has to
    /// accept the tree the engine ships, and that tree has the shape the
    /// emitter reproduces. This anchors both to shipped bytes rather than to
    /// the emitter's own idea of the format.
    #[test]
    fn the_shipped_tree_passes_the_same_checks() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../agent-kit/bsp/apex_v51_client_reduced.bsp");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("{} is not here, so the shipped tree is not checked", path.display());
            return;
        };
        let bsp = rbsp::Bsp::from_monolithic(&bytes).expect("the shipped map parses");
        let tree: Vec<lmp::LightProbeTreeNode> = bsp
            .lump(rbsp::ids::LIGHTPROBE_TREE)
            .chunks_exact(8)
            .map(bytemuck::pod_read_unaligned)
            .collect();
        let refs: Vec<lmp::LightProbeRef> = bsp
            .lump(rbsp::ids::LIGHTPROBE_REFERENCES)
            .chunks_exact(20)
            .map(bytemuck::pod_read_unaligned)
            .collect();
        let probes = bsp.lump(rbsp::ids::LIGHTPROBES).len() / 44;
        assert!(tree.len() > 1000 && refs.len() > 1000, "the shipped tree was read");
        assert!(errors(&tree, refs.len()).is_empty(), "{:?}", errors(&tree, refs.len()));
        assert!(refs.iter().all(|r| (r.probe as usize) < probes), "every ref names a probe");
        let leaves = shipped_shape(&tree, &refs).expect("the shipped tree has the shipped shape");
        assert_eq!(leaves, tree.iter().filter(|n| n.is_leaf()).count());
    }
}
