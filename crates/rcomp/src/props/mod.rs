//! Static props: the `sprp` game-lump block, its model dictionary and the
//! level-info prop counts.

use glam::Vec3;
use rbsp::lumps::game_lump::sprp::{self, StaticProp, StaticProps};
use rbsp::lumps::game_lump::{Block, GameLump, GameLumpEntry, PRPS_ID};
use rbsp::lumps::level::LevelInfo;
use rbsp::{Bsp, Version};
use rir::{AssetTables, StaticPropInstance};

use crate::report::Stats;

/// One prop as the document describes it, before the asset table resolves it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PropSource {
    pub model: String,
    pub origin: Vec3,
    /// Pitch, yaw, roll degrees.
    pub angles: Vec3,
    pub scale: f32,
    pub skin: i32,
    pub solid: bool,
    pub cast_shadows: bool,
    pub fade_distance: f32,
    /// Model bounds, filled in from the asset table.
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    /// The vista the sky camera looks at, drawn only in the skybox.
    pub sky: bool,
    /// The prop's own keys: the `_sprp_*` record fields and the `_coll_*`
    /// collision a decompiled prop carries.
    pub keys: rdoc::model::KeyValues,
}

#[derive(Debug, thiserror::Error)]
pub enum PropError {
    #[error("static prop {0} names a model path longer than the {1}-byte dictionary field")]
    PathTooLong(usize, usize),
    #[error("model {0:?} is not in the asset table, so its bounds are unknown")]
    UnknownModel(String),
    #[error("model {0:?} has a non-finite bound")]
    NotFinite(String),
}

fn raw_key(keys: &rdoc::model::KeyValues, key: &str) -> Option<u64> {
    keys.get(key)?.trim().parse().ok()
}

/// Writes the record fields a prop carries as `_sprp_*` keys over a record:
/// the fields the document has no field of its own for, which a decompiled
/// prop keeps so its record comes back whole.
pub fn apply_record_keys(record: &mut StaticProp, keys: &rdoc::model::KeyValues) {
    if let Some(v) = raw_key(keys, "_sprp_unknown_0") {
        record.unknown_0 = v as u8;
    }
    if let Some(v) = raw_key(keys, "_sprp_unknown_1") {
        record.unknown_1 = v as u8;
    }
    if let Some(v) = raw_key(keys, "_sprp_unknown_3") {
        record.unknown_3 = v as u16;
    }
    if let Some(v) = raw_key(keys, "_sprp_unknown_5") {
        record.unknown_5 = v as u32;
    }
    if let Some(v) = raw_key(keys, "_sprp_unknown_6") {
        record.unknown_6 = v as u32;
    }
    if let Some(v) = raw_key(keys, "_sprp_unknown_7") {
        record.unknown_7 = v as u32;
    }
    if let Some(v) = raw_key(keys, "_sprp_wind_data") {
        record.wind_data = v as u32;
    }
    if let Some(v) = raw_key(keys, "_sprp_setdress_level") {
        record.setdress_level = v as u8;
    }
    if let Some(v) = raw_key(keys, "_sprp_diffuse_modulation") {
        record.diffuse_modulation = v as u32;
    }
    if let Some(v) = raw_key(keys, "_sprp_unknown_8") {
        record.unknown_8 = [v as u8, (v >> 8) as u8, (v >> 16) as u8];
    }
    if let Some(v) = raw_key(keys, "_sprp_fade") {
        record.unknown_4 = f32::from_bits(v as u32);
    }
}

/// The angle triple a decompiled prop's record held, while the prop's
/// rotation is still the one that triple describes.
pub fn recorded_angles(keys: &rdoc::model::KeyValues, t: &rdoc::model::Transform) -> Option<Vec3> {
    let a = vec3_key(keys, "_sprp_angles")?;
    let r = rdoc::model::Transform::from_source_angles(glam::DVec3::ZERO, a.as_dvec3()).rotation;
    (r.dot(t.rotation).abs() > 1.0 - 1e-9).then_some(a)
}

/// The collision leaf a decompiled prop had: its world box, its contents mask
/// and the flags above the prop index in the leaf word.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PropCollision {
    pub mins: Vec3,
    pub maxs: Vec3,
    pub contents: u32,
    pub flags: u16,
}

fn vec3_key(keys: &rdoc::model::KeyValues, key: &str) -> Option<Vec3> {
    let mut it = keys.get(key)?.split_whitespace().map(|t| t.parse::<f32>().ok());
    let v = Vec3::new(it.next()??, it.next()??, it.next()??);
    v.is_finite().then_some(v)
}

/// The collision a prop's `_coll_*` keys describe.
pub fn collision_keys(keys: &rdoc::model::KeyValues) -> Option<PropCollision> {
    Some(PropCollision {
        mins: vec3_key(keys, "_coll_mins")?,
        maxs: vec3_key(keys, "_coll_maxs")?,
        contents: raw_key(keys, "_coll_contents").map(|v| v as u32).unwrap_or(crate::collision::CONTENTS_WORLD),
        flags: raw_key(keys, "_coll_flags").map(|v| v as u16).unwrap_or(0),
    })
}

/// Writes a prop's collision as `_coll_*` keys.
pub fn put_collision_keys(keys: &mut rdoc::model::KeyValues, c: &PropCollision) {
    let v = |p: Vec3| format!("{} {} {}", p.x, p.y, p.z);
    keys.insert("_coll_mins".into(), v(c.mins));
    keys.insert("_coll_maxs".into(), v(c.maxs));
    keys.insert("_coll_contents".into(), c.contents.to_string());
    if c.flags != 0 {
        keys.insert("_coll_flags".into(), c.flags.to_string());
    }
}

/// The collision leaf a solid prop compiles to, as prop `index` of the map.
/// A prop that knows its leaf (a decompiled one) keeps it; any other takes its
/// model's box from the asset table, and a prop with no box has none.
pub fn prop_solid(source: &PropSource, index: u32) -> Option<rir::CollisionSolid> {
    if !source.solid {
        return None;
    }
    let (mins, maxs, contents, flags) = match collision_keys(&source.keys) {
        Some(c) => (c.mins, c.maxs, c.contents, c.flags),
        None => {
            if source.bounds_min.x >= source.bounds_max.x {
                return None;
            }
            let (lo, hi) = source.world_bounds();
            (lo, hi, crate::collision::CONTENTS_WORLD, 0)
        }
    };
    Some(rir::CollisionSolid {
        object: None,
        model: 0,
        shape: rir::CollisionShape::Prop { word: (index & 0xFFFF) | ((flags as u32) << 16), mins, maxs },
        contents,
        surface_prop: 0,
        surfaces: Vec::new(),
    })
}

/// Resolves document props against the asset table and writes the game lump.
pub fn emit(
    sources: &[PropSource],
    assets: &AssetTables,
    bsp: &mut Bsp,
    stats: &mut Stats,
) -> Result<Vec<StaticPropInstance>, PropError> {
    let version = bsp.version_kind().unwrap_or(Version::ClientV51);
    // A model the table does not know still has to be packed, so the prop list
    // stays complete; its bounds come from the placement itself.
    let mut instances: Vec<StaticPropInstance> = Vec::with_capacity(sources.len());
    for s in sources {
        let info = assets.models.get(&s.model);
        let (bmin, bmax) = match info {
            Some(m) => {
                if !m.bounds_min.is_finite() || !m.bounds_max.is_finite() {
                    return Err(PropError::NotFinite(s.model.clone()));
                }
                (m.bounds_min, m.bounds_max)
            }
            None => {
                stats.props += 0;
                (Vec3::ZERO, Vec3::ZERO)
            }
        };
        instances.push(StaticPropInstance {
            model: s.model.clone(),
            origin: s.origin,
            angles: s.angles,
            scale: s.scale,
            skin: s.skin,
            solid: s.solid,
            cast_shadows: s.cast_shadows,
            fade_distance: s.fade_distance,
            bounds_min: bmin,
            bounds_max: bmax,
        });
    }

    let mut models: Vec<String> = Vec::new();
    let mut props: Vec<StaticProp> = Vec::with_capacity(sources.len());
    for (i, s) in sources.iter().enumerate() {
        if s.model.len() >= sprp::MODEL_NAME_SIZE {
            return Err(PropError::PathTooLong(i, sprp::MODEL_NAME_SIZE - 1));
        }
        let index = match models.iter().position(|m| *m == s.model) {
            Some(k) => k,
            None => {
                models.push(s.model.clone());
                models.len() - 1
            }
        };
        let mut p = sprp::default_prop();
        p.origin = s.origin.to_array();
        p.angles = s.angles.to_array();
        p.scale = if s.scale > 0.0 { s.scale } else { 1.0 };
        p.model_index = index as u16;
        // The fixture convention: the skin rides the second halfword, the fade
        // distance the float after it, and the lighting origin is the zero
        // vector the map leaves unless it wants a probe elsewhere.
        p.unknown_2 = (s.skin.clamp(0, u16::MAX as i32)) as u16;
        p.unknown_4 = if s.fade_distance > 0.0 { s.fade_distance } else { -1.0 };
        if s.sky {
            // The flag bytes of the shipped skybox vistas; 0x40 is set on no
            // other prop. Its cubemap stays 0 (unknown_3 is m_EnvCubemap), as
            // skulltown's does: every map with a cubemap has that one.
            p.unknown_1 = 0x54;
        }
        apply_record_keys(&mut p, &s.keys);
        props.push(p);
    }

    // The counts after the total are the opaque prop count and the first prop
    // the translucent pass draws. Sources arrive sorted by draw class with the
    // vista last, and the vista is drawn by both passes, as in the shipped maps.
    let n = props.len() as u32;
    let first_translucent = sources
        .iter()
        .position(|s| s.sky || DrawClass::of(s) >= DrawClass::Mixed)
        .unwrap_or(sources.len()) as u32;
    let block = StaticProps {
        models,
        counts: [n, n, first_translucent],
        props,
        tail_count: 0,
        groups: Vec::new(),
    };
    let encoded = block.encode();
    let gl = GameLump {
        entries: vec![GameLumpEntry {
            id: PRPS_ID,
            flags: 0,
            version: version.number(),
            // Blocks follow the directory back to back, and a packed map
            // rebases this pointer when the lump moves.
            file_offset: (rbsp::lumps::game_lump::HEADER_SIZE
                + rbsp::lumps::game_lump::DIRECTORY_ENTRY_SIZE) as i32,
            length: encoded.len() as i32,
        }],
        blocks: vec![Block::StaticProps(block)],
        tail: Vec::new(),
    };
    bsp.set_lump(rbsp::ids::GAME_LUMP, rbsp::lumps::game_lump::write(&gl, version));
    stats.props = sources.len();
    Ok(instances)
}

/// Where the world's mesh ranges end, as the level info records them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeshRanges {
    /// World meshes before this are opaque; rcomp writes no decal meshes, so
    /// this is also the first decal mesh.
    pub first_transparent: u32,
    /// The end of the world model's meshes.
    pub first_sky: u32,
    pub sun_normal: [f32; 3],
}

/// The 0x7B record: the world mesh ranges, where prop references start and the
/// prop count.
pub fn write_level_info(bsp: &mut Bsp, ranges: MeshRanges, stats: &mut Stats) {
    let count = match rbsp::lumps::parse_at(
        rbsp::ids::GAME_LUMP,
        bsp.version_kind().unwrap_or(Version::ClientV51),
        bsp.lump(rbsp::ids::GAME_LUMP),
        0,
    ) {
        Ok(rbsp::lumps::Lump::GameLump(g)) => g.static_props().map(|s| s.prop_count() as u32).unwrap_or(0),
        _ => 0,
    };
    let info = LevelInfo {
        first_decal_mesh: ranges.first_transparent,
        first_transparent_mesh: ranges.first_transparent,
        first_sky_mesh: ranges.first_sky,
        first_static_prop_index: rbsp::lumps::level::first_static_prop_index(ranges.first_sky),
        num_static_props: count,
        sun_normal: ranges.sun_normal,
        // 1 on every shipped map.
        unknown_0: 1,
    };
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    bsp.set_lump(rbsp::ids::LEVEL_INFO, rbsp::lumps::level::write_level_info(&info, v));
    stats.prop_models = count as usize;
}

/// Which static prop range a prop belongs to. The client draws props
/// `[0, first translucent)` in the opaque pass, stops the depth prepass at the
/// first prop with a decal material, and draws `[first translucent, ..)` in
/// the translucent pass, so the list must be sorted by these classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DrawClass {
    Opaque = 0,
    Decal = 1,
    /// Opaque parts and translucent parts.
    Mixed = 2,
    Translucent = 3,
}

/// The `_sprp_draw_class` key: 0 opaque, 1 decal, 2 mixed, 3 translucent.
pub const DRAW_CLASS_KEY: &str = "_sprp_draw_class";

impl DrawClass {
    pub fn of(prop: &PropSource) -> DrawClass {
        match raw_key(&prop.keys, DRAW_CLASS_KEY) {
            Some(1) => DrawClass::Decal,
            Some(2) => DrawClass::Mixed,
            Some(3) => DrawClass::Translucent,
            _ => DrawClass::Opaque,
        }
    }
}

/// Sorts props into the ranges the client expects and, inside each range, by
/// model and skin. The client draws visible props in list order and batches
/// each run of one model and skin, with a fixed cap on batches per view, so
/// an interleaved list overflows the cap and drops props depending on view.
pub fn sort_by_draw_class(props: &mut [PropSource]) {
    props.sort_by(|a, b| {
        DrawClass::of(a).cmp(&DrawClass::of(b)).then_with(|| a.model.cmp(&b.model)).then_with(|| a.skin.cmp(&b.skin))
    });
}

impl PropSource {
    /// The prop's world box, with its rotation and scale applied to the model
    /// bounds the asset table reports.
    pub fn world_bounds(&self) -> (Vec3, Vec3) {
        let (bmin, bmax) = if self.bounds_min.x > self.bounds_max.x {
            (Vec3::splat(-8.0), Vec3::splat(8.0))
        } else {
            (self.bounds_min, self.bounds_max)
        };
        let (pitch, yaw, roll) =
            (self.angles.x.to_radians(), self.angles.y.to_radians(), self.angles.z.to_radians());
        // ZYX order, the one the document's transform uses.
        let rot = glam::Mat3::from_quat(glam::Quat::from_euler(glam::EulerRot::ZYX, yaw, pitch, roll));
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for x in [bmin.x, bmax.x] {
            for y in [bmin.y, bmax.y] {
                for z in [bmin.z, bmax.z] {
                    let p = rot * Vec3::new(x, y, z) * scale + self.origin;
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
            }
        }
        if lo.x > hi.x {
            return (self.origin, self.origin);
        }
        (lo, hi)
    }
}

/// The prop count a level-info record declares, for the report.
pub fn level_info_props(bsp: &Bsp) -> u32 {
    match rbsp::lumps::parse_at(
        rbsp::ids::LEVEL_INFO,
        bsp.version_kind().unwrap_or(Version::ClientV51),
        bsp.lump(rbsp::ids::LEVEL_INFO),
        0,
    ) {
        Ok(rbsp::lumps::Lump::LevelInfo(i)) => i.num_static_props,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(model: &str) -> PropSource {
        PropSource {
            model: model.into(),
            origin: Vec3::new(1.0, 2.0, 3.0),
            angles: Vec3::new(0.0, 90.0, 0.0),
            scale: 1.0,
            skin: 2,
            solid: true,
            cast_shadows: true,
            fade_distance: 0.0,
            bounds_min: Vec3::splat(-8.0),
            bounds_max: Vec3::splat(8.0),
            sky: false,
            keys: Default::default(),
        }
    }

    fn classed(model: &str, class: u8) -> PropSource {
        let mut s = source(model);
        s.keys.insert(DRAW_CLASS_KEY.into(), class.to_string());
        s
    }

    #[test]
    fn translucent_props_sort_after_opaque_and_start_the_translucent_range() {
        let mut props = vec![
            classed("mdl/glass.rmdl", 3),
            classed("mdl/crate.rmdl", 0),
            classed("mdl/lamp.rmdl", 2),
            classed("mdl/sign.rmdl", 1),
            classed("mdl/chair.rmdl", 0),
        ];
        sort_by_draw_class(&mut props);
        let order: Vec<&str> = props.iter().map(|p| p.model.as_str()).collect();
        assert_eq!(order, ["mdl/chair.rmdl", "mdl/crate.rmdl", "mdl/sign.rmdl", "mdl/lamp.rmdl", "mdl/glass.rmdl"]);
        let mut bsp = Bsp::new(51, 1, 30);
        emit(&props, &AssetTables::default(), &mut bsp, &mut Stats::default()).expect("the prop list writes");
        let parsed = rbsp::lumps::parse(rbsp::ids::GAME_LUMP, Version::ClientV51, bsp.lump(rbsp::ids::GAME_LUMP))
            .expect("the game lump parses");
        assert_eq!(parsed.as_static_props().expect("sprp").counts, [5, 5, 3]);
    }

    #[test]
    fn props_of_one_model_and_skin_are_contiguous() {
        let mut props = vec![source("mdl/b.rmdl"), source("mdl/a.rmdl"), source("mdl/b.rmdl"), source("mdl/a.rmdl")];
        props[3].skin = 0;
        sort_by_draw_class(&mut props);
        let order: Vec<(&str, i32)> = props.iter().map(|p| (p.model.as_str(), p.skin)).collect();
        assert_eq!(order, [("mdl/a.rmdl", 0), ("mdl/a.rmdl", 2), ("mdl/b.rmdl", 2), ("mdl/b.rmdl", 2)]);
    }

    #[test]
    fn props_emit_a_dictionary_and_a_count() {
        let mut bsp = Bsp::new(51, 1, 30);
        let mut stats = Stats::default();
        let out = emit(
            &[source("mdl/a/one.rmdl"), source("mdl/b/two.rmdl"), source("mdl/a/one.rmdl")],
            &AssetTables::default(),
            &mut bsp,
            &mut stats,
        )
        .expect("the prop list writes");
        write_level_info(&mut bsp, MeshRanges { first_transparent: 1, first_sky: 1, sun_normal: [0.0; 3] }, &mut stats);
        assert_eq!(out.len(), 3);
        assert_eq!(stats.props, 3);
        assert_eq!(level_info_props(&bsp), 3);
        let parsed = rbsp::lumps::parse(rbsp::ids::GAME_LUMP, Version::ClientV51, bsp.lump(rbsp::ids::GAME_LUMP))
            .expect("the game lump parses");
        let block = parsed.as_static_props().expect("it carries the sprp block");
        assert_eq!(block.models, vec!["mdl/a/one.rmdl", "mdl/b/two.rmdl"]);
        assert_eq!(block.props[0].model_index, 0);
        assert_eq!(block.props[1].model_index, 1);
        assert_eq!(block.props[2].model_index, 0);
        assert_eq!(block.props[0].diffuse_modulation, 0xFFFF_FFFF);
        assert_eq!(block.props[0].unknown_4, -1.0);
        assert_eq!(block.props[0].unknown_2, 2);
        assert_eq!(block.tail_count, 0);
        assert_eq!(block.counts, [3, 3, 3], "every prop is in the opaque range");
    }

    #[test]
    fn the_vista_is_drawn_by_both_passes() {
        let mut bsp = Bsp::new(51, 1, 30);
        let mut stats = Stats::default();
        let mut vista = source("mdl/vistas/v.rmdl");
        vista.sky = true;
        emit(&[source("mdl/a/one.rmdl"), vista], &AssetTables::default(), &mut bsp, &mut stats)
            .expect("the prop list writes");
        let parsed = rbsp::lumps::parse(rbsp::ids::GAME_LUMP, Version::ClientV51, bsp.lump(rbsp::ids::GAME_LUMP))
            .expect("the game lump parses");
        let block = parsed.as_static_props().expect("it carries the sprp block");
        assert_eq!(block.counts, [2, 2, 1], "the vista sits between the first translucent prop and the opaque count");
    }

    #[test]
    fn level_info_rounds_the_first_index_to_sixty_four() {
        let mut bsp = Bsp::new(47, 0, 30);
        let mut stats = Stats::default();
        emit(&[source("mdl/a/one.rmdl")], &AssetTables::default(), &mut bsp, &mut stats)
            .expect("the prop list writes");
        let ranges = MeshRanges { first_transparent: 3, first_sky: 5, sun_normal: [0.0, 0.0, -1.0] };
        write_level_info(&mut bsp, ranges, &mut stats);
        let parsed = rbsp::lumps::parse(rbsp::ids::LEVEL_INFO, Version::DediV47, bsp.lump(rbsp::ids::LEVEL_INFO))
            .expect("level info parses");
        match parsed {
            rbsp::lumps::Lump::LevelInfo(i) => {
                assert_eq!(i.first_decal_mesh, 3);
                assert_eq!(i.first_transparent_mesh, 3);
                assert_eq!(i.first_sky_mesh, 5);
                assert_eq!(i.first_static_prop_index, 64);
                assert_eq!(i.num_static_props, 1);
                assert_eq!(i.sun_normal, [0.0, 0.0, -1.0]);
            }
            other => panic!("level info decoded as {}", other.kind()),
        }
    }

    #[test]
    fn a_prop_box_covers_its_model_bounds() {
        let mut s = source("mdl/a/one.rmdl");
        s.angles = Vec3::new(0.0, 45.0, 0.0);
        let (lo, hi) = s.world_bounds();
        // A 45 degree yaw is a turn about Z, so X and Y reach 8 * sqrt(2) and Z
        // is untouched.
        assert!(lo.x.abs() > 8.5 && hi.x > 8.5, "x reaches {}", hi.x);
        assert!(lo.y.abs() > 8.5, "y reaches {}", lo.y);
        assert_eq!(lo.z, s.origin.z - 8.0, "z is untouched by a yaw");
        assert_eq!(hi.z, s.origin.z + 8.0);
        s.angles = Vec3::ZERO;
        s.bounds_min = Vec3::ZERO;
        s.bounds_max = Vec3::ZERO;
        let (lo, hi) = s.world_bounds();
        assert_eq!(lo, s.origin, "a model with no bounds collapses onto the origin");
        assert_eq!(hi, s.origin);
    }

    #[test]
    fn a_long_model_path_is_an_error() {
        let mut bsp = Bsp::new(51, 1, 30);
        let mut stats = Stats::default();
        let long = format!("mdl/{}.rmdl", "x".repeat(200));
        let err = emit(&[source(&long)], &AssetTables::default(), &mut bsp, &mut stats);
        assert!(matches!(err, Err(PropError::PathTooLong(0, _))), "a long path is rejected");
    }
}
