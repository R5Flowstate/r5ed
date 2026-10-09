//! World light records for 0x36.
//!
//! The record's own attenuation fields are the coefficients a runtime light
//! falls off by, and the shipped maps use two families: a plain `1 / range`
//! linear term, and a `1.952381 / range` linear term with a
//! `0.952381 / range^2` quadratic. The second is the one most shipped lights
//! carry, so that is what a bake emits.

use glam::Vec3;
use rbsp::lumps::lighting as lmp;

use crate::codec::WorldLightRecord;

/// The shipped `|linear|` factor.
pub const ATTEN_LINEAR: f32 = 1.952381;
/// The shipped quadratic factor.
pub const ATTEN_QUADRATIC: f32 = 0.952381;
/// The cone a point light is written with: an inner ten degrees and an outer
/// eighty-nine, which is how the shipped maps store a light with no cone.
pub const POINT_INNER_CONE_DEG: f32 = 10.0;
pub const POINT_OUTER_CONE_DEG: f32 = 89.0;
/// `dworldlight_t::type`, the engine's `emittype_t`.
pub const TYPE_POINT: u32 = 1;
pub const TYPE_SPOT: u32 = 2;
/// `emit_skylight`: the sun.
pub const TYPE_SKYLIGHT: u32 = 3;
/// `emit_skyambient`: the sky's ambient colour, first in the list.
pub const TYPE_SKYAMBIENT: u32 = 5;

/// The flags the shipped maps give each kind.
pub const FLAGS_SKYAMBIENT: u32 = 0x2000;
/// The sun record's `cluster` slot is the engine's sun bounce boost, which the
/// renderer feeds to the frame constants; shipped maps carry 3.
pub const SUN_BOUNCE_BOOST: f32 = 3.0;
pub const FLAGS_SKYLIGHT: u32 = 0x18;
pub const FLAGS_SPOT: u32 = 0x8c;
pub const FLAGS_POINT: u32 = 0x1c;

/// The sun a map gets when its document has none, since the engine expects a
/// sky light after the sky ambient.
const DEFAULT_SUN_DIRECTION: Vec3 = Vec3::new(-0.53, 0.34, -0.78);

fn finite3(v: Vec3) -> [f32; 3] {
    [v.x, v.y, v.z]
}

fn unit(v: Vec3) -> [f32; 3] {
    let n = v.normalize_or_zero();
    if n.length_squared() < 0.5 {
        [0.0, 0.0, 1.0]
    } else {
        finite3(n)
    }
}

/// Writes a spot or point scene light as a world light record.
pub fn to_record(light: &rir::BakeLight, index: u32) -> WorldLightRecord {
    let _ = index;
    let range = if light.range.is_finite() && light.range > 0.0 { light.range } else { 512.0 };
    let spot = light.kind == rir::LightType::Spot;
    let [constant, linear, quadratic] =
        light.attenuation.unwrap_or([1.0, -ATTEN_LINEAR / range, ATTEN_QUADRATIC / (range * range)]);
    WorldLightRecord {
        origin: finite3(light.position),
        intensity: finite3(light.color * light.intensity),
        normal: unit(light.direction),
        shadow_offset: [0.0; 3],
        cluster: 1.0,
        light_type: if spot { TYPE_SPOT } else { TYPE_POINT },
        style: 0,
        inner_cone: if spot { light.inner_angle.to_radians().cos() } else { 0.0 },
        outer_cone: if spot { light.outer_angle.to_radians().cos() } else { 0.0 },
        exponent: if spot { 1.0 } else { 0.0 },
        radius: range,
        constant_attenuation: constant,
        linear_attenuation: linear,
        quadratic_attenuation: quadratic,
        flags: light.flags.unwrap_or(if spot { FLAGS_SPOT } else { FLAGS_POINT }),
        texture_index: 0,
        parent: 0,
        scale: 0.0,
        extra: [1.0, 1.0],
    }
}

/// The sky ambient record: the sky colour, no direction.
pub fn sky_ambient_record(sky_colour: Vec3, sky_intensity: f32) -> WorldLightRecord {
    WorldLightRecord {
        origin: [0.0; 3],
        intensity: finite3(sky_colour * sky_intensity),
        normal: [0.0; 3],
        shadow_offset: [0.0; 3],
        cluster: 0.0,
        light_type: TYPE_SKYAMBIENT,
        style: 0,
        inner_cone: 0.0,
        outer_cone: 0.0,
        exponent: 0.0,
        radius: 0.0,
        constant_attenuation: 0.0,
        linear_attenuation: 0.0,
        quadratic_attenuation: 0.0,
        flags: FLAGS_SKYAMBIENT,
        texture_index: 0,
        parent: 0,
        scale: 0.0,
        extra: [0.0, 0.0],
    }
}

/// The sun record: its colour and the direction its light travels.
pub fn sky_light_record(colour: Vec3, direction: Vec3) -> WorldLightRecord {
    WorldLightRecord {
        origin: [0.0; 3],
        intensity: finite3(colour),
        normal: unit(direction),
        shadow_offset: [0.0; 3],
        cluster: SUN_BOUNCE_BOOST,
        light_type: TYPE_SKYLIGHT,
        style: 0,
        inner_cone: 0.0,
        outer_cone: 0.0,
        exponent: 0.0,
        radius: 0.0,
        constant_attenuation: 0.0,
        linear_attenuation: 0.0,
        quadratic_attenuation: 0.0,
        flags: FLAGS_SKYLIGHT,
        texture_index: 0,
        parent: 0,
        scale: 0.0,
        extra: [0.01, 0.0],
    }
}

/// The scene lights that become spot and point records, in record order after
/// the sky ambient and the sun: spots first, then points.
fn local_lights(scene: &rir::BakeScene) -> Vec<&rir::BakeLight> {
    let mut out: Vec<&rir::BakeLight> = scene.lights.iter().filter(|l| l.kind == rir::LightType::Spot).collect();
    out.extend(scene.lights.iter().filter(|l| matches!(l.kind, rir::LightType::Point | rir::LightType::Area)));
    out
}

/// Every world light of a scene in the order the engine requires: the sky
/// ambient, the sun, then spots and points.
pub fn world_lights(scene: &rir::BakeScene) -> Vec<WorldLightRecord> {
    let mut out: Vec<WorldLightRecord> = Vec::new();
    out.push(sky_ambient_record(scene.settings.sky_color, scene.settings.sky_intensity));
    let sun = scene.lights.iter().find(|l| l.kind == rir::LightType::Sun);
    out.push(match sun {
        Some(l) => sky_light_record(l.color * l.intensity, l.direction),
        None => sky_light_record(Vec3::splat(1.5), DEFAULT_SUN_DIRECTION),
    });
    for l in local_lights(scene) {
        out.push(to_record(l, out.len() as u32));
    }
    out
}

/// The tweak-light table: entry `k` is the world light record of the light a
/// map's script reaches as tweak light `k` (its `tweak_light_index`). A map
/// with no tweakable light keeps one entry per record.
pub fn tweak_lights(scene: &rir::BakeScene, lights: &[WorldLightRecord]) -> Vec<u32> {
    let mut slots: Vec<(u32, u32)> = scene
        .lights
        .iter()
        .filter_map(|l| Some((l.tweak_index?, index_of(scene, l)?)))
        .collect();
    if slots.is_empty() {
        return (0..lights.len() as u32).collect();
    }
    slots.sort();
    let len = slots.last().map_or(0, |s| s.0 as usize + 1);
    let mut out = vec![u32::from(u16::MAX); len];
    for (k, record) in slots {
        out[k as usize] = record;
    }
    out
}

/// Which world light record a scene light became, or `None` when it was not
/// written.
pub fn index_of(scene: &rir::BakeScene, light: &rir::BakeLight) -> Option<u32> {
    if light.kind == rir::LightType::Sun {
        let first = scene.lights.iter().find(|l| l.kind == rir::LightType::Sun)?;
        return std::ptr::eq(first, light).then_some(1);
    }
    local_lights(scene).iter().position(|l| std::ptr::eq(*l, light)).map(|i| i as u32 + 2)
}

/// The 0x36 records, in the engine's own struct.
pub fn records(lights: &[WorldLightRecord]) -> Vec<[u8; 112]> {
    lights.iter().map(crate::codec::write_world_light).collect()
}

/// The record the engine reads for one world light.
pub fn as_lump(lights: &[WorldLightRecord]) -> Vec<lmp::WorldLight> {
    lights.iter().map(|l| lmp::WorldLight { origin: l.origin, direction: l.normal, unknown_0: [0; 6], intensity: l.cluster, light_type: l.light_type, unknown_1: 0, color: l.intensity, unknown_2: [0; 4], flags: l.flags, unknown_3: [0; 5] }).collect()
}
