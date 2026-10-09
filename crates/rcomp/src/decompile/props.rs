//! The `sprp` game-lump block read back as static `Prop` objects.

use rbsp::lumps::game_lump::sprp::StaticProp;
use rbsp::{ids, Bsp, Version};
use rdoc::model::{Id, KeyValues, Object, ObjectKind, Prop, Transform};

use super::{decode, hidden_object, DecompileError};

/// Every static prop's collision leaf in the world model, by prop index.
fn prop_leaves(bsp: &Bsp) -> std::collections::HashMap<u32, crate::props::PropCollision> {
    let mut out: std::collections::HashMap<u32, crate::props::PropCollision> = std::collections::HashMap::new();
    let Ok(leaves) = crate::collision::decode_leaves(bsp, 0) else { return out };
    for l in leaves {
        if l.kind != rbsp::lumps::collision::CHILD_PROP || l.contents == 0 {
            continue;
        }
        let (lo, hi) = (l.bounds_min.as_vec3(), l.bounds_max.as_vec3());
        out.entry(l.prop_index & 0xFFFF)
            .and_modify(|c| {
                c.mins = c.mins.min(lo);
                c.maxs = c.maxs.max(hi);
            })
            .or_insert(crate::props::PropCollision { mins: lo, maxs: hi, contents: l.contents, flags: (l.prop_index >> 16) as u16 });
    }
    out
}

fn prop_block(bsp: &Bsp, version: Version) -> Result<Option<rbsp::lumps::game_lump::sprp::StaticProps>, DecompileError> {
    if bsp.lump(ids::GAME_LUMP).is_empty() {
        return Ok(None);
    }
    let parsed = rbsp::lumps::parse_at(ids::GAME_LUMP, version, bsp.lump(ids::GAME_LUMP), 0)
        .map_err(|e| decode("game lump", e))?;
    let rbsp::lumps::Lump::GameLump(gl) = parsed else { return Ok(None) };
    Ok(gl.static_props().cloned())
}

fn prop_object(
    block: &rbsp::lumps::game_lump::sprp::StaticProps,
    i: usize,
    collision: &std::collections::HashMap<u32, crate::props::PropCollision>,
    layer: Id,
) -> Object {
    let p = block.props[i];
    let mut transform = Transform::from_source_angles(p.origin.map(f64::from).into(), p.angles.map(f64::from).into());
    // The record's scale is the prop's, so it rides the transform.
    transform.scale = glam::DVec3::splat(if p.scale > 0.0 { p.scale as f64 } else { 1.0 });
    let mut keyvalues = extra_keyvalues(&p);
    // A prop collides when the world tree has a leaf naming it; the leaf's
    // box, mask and flags travel with the prop so it compiles back to the
    // same leaf.
    let leaf = collision.get(&(i as u32));
    if let Some(c) = leaf {
        crate::props::put_collision_keys(&mut keyvalues, c);
    }
    let kind = ObjectKind::Prop(Prop {
        model: block.model_of(i).unwrap_or("").to_string(),
        skin: p.unknown_2 as i32,
        static_prop: true,
        solid: leaf.is_some(),
        cast_shadows: true,
        // The writer stores "no fade" as a negative distance.
        fade_distance: if p.unknown_4.is_finite() && p.unknown_4 > 0.0 { p.unknown_4 } else { 0.0 },
        keyvalues,
    });
    hidden_object(format!("prop{i:04}"), layer, transform, kind)
}

/// Every static prop of the map, in the order the block lists them.
pub fn static_props(bsp: &Bsp, version: Version, layer: Id) -> Result<Vec<Object>, DecompileError> {
    let Some(block) = prop_block(bsp, version)? else { return Ok(Vec::new()) };
    let count = block.prop_count().min(block.props.len());
    let collision = prop_leaves(bsp);
    Ok((0..count).map(|i| prop_object(&block, i, &collision, layer)).collect())
}

/// Static prop record `index` as the object that stands for it.
pub fn static_prop(bsp: &Bsp, version: Version, layer: Id, index: u32) -> Result<Option<Object>, DecompileError> {
    let Some(block) = prop_block(bsp, version)? else { return Ok(None) };
    let i = index as usize;
    if i >= block.prop_count().min(block.props.len()) {
        return Ok(None);
    }
    Ok(Some(prop_object(&block, i, &prop_leaves(bsp), layer)))
}

/// Where an imported map's prop table lives inside the project.
pub const PROP_TABLE_PATH: &str = "reference/props.bin";
pub const PROP_TABLE_MAGIC: [u8; 4] = *b"RPRP";
pub const PROP_TABLE_VERSION: u32 = 1;
/// `u32 model, u32 skin, u32 flags, f32 origin[3], f32 angles[3], f32 scale,
/// f32 fade, u32 pad`. The angles are the record's own, so a placement the page
/// rebuilds from them is the one the compiler reads back as unmoved.
pub const PROP_TABLE_RECORD: usize = 48;
pub const PROP_FLAG_SOLID: u32 = 1;

/// Every static prop of the map as one compact table the editor draws from:
/// a shipped map carries hundreds of thousands of props, far too many to hold
/// as document objects. A prop becomes an object only when it is edited.
///
/// `magic, u32 version, u32 model count, u32 prop count`, the model names as
/// `u16 length + bytes`, zero padding to 4, then one record per prop.
pub fn prop_table(bsp: &Bsp, version: Version) -> Result<Option<Vec<u8>>, DecompileError> {
    let Some(block) = prop_block(bsp, version)? else { return Ok(None) };
    let count = block.prop_count().min(block.props.len());
    let collision = prop_leaves(bsp);
    let mut out = Vec::with_capacity(16 + block.models.len() * 48 + count * PROP_TABLE_RECORD);
    out.extend_from_slice(&PROP_TABLE_MAGIC);
    out.extend_from_slice(&PROP_TABLE_VERSION.to_le_bytes());
    out.extend_from_slice(&(block.models.len() as u32).to_le_bytes());
    out.extend_from_slice(&(count as u32).to_le_bytes());
    for m in &block.models {
        let name = &m.as_bytes()[..m.len().min(u16::MAX as usize)];
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }
    for i in 0..count {
        let p = block.props[i];
        let flags = if collision.contains_key(&(i as u32)) { PROP_FLAG_SOLID } else { 0 };
        let fade = if p.unknown_4.is_finite() && p.unknown_4 > 0.0 { p.unknown_4 } else { 0.0 };
        let scale = if p.scale > 0.0 { p.scale } else { 1.0 };
        for v in [u32::from(p.model_index), u32::from(p.unknown_2), flags] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [p.origin[0], p.origin[1], p.origin[2], p.angles[0], p.angles[1], p.angles[2], scale, fade] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    Ok(Some(out))
}

/// The record fields a `Prop` has no field of its own for. A prop that leaves
/// them at their default gets no key, so the common prop stays clean.
fn extra_keyvalues(p: &StaticProp) -> KeyValues {
    let mut out: KeyValues = KeyValues::new();
    let mut put = |k: &str, v: u64, default: u64| {
        if v != default {
            out.insert(k.to_string(), v.to_string());
        }
    };
    put("_sprp_unknown_0", p.unknown_0 as u64, 0);
    put("_sprp_unknown_1", p.unknown_1 as u64, 0);
    put("_sprp_unknown_3", p.unknown_3 as u64, 0);
    put("_sprp_unknown_5", p.unknown_5 as u64, 0);
    put("_sprp_unknown_6", p.unknown_6 as u64, 0);
    put("_sprp_unknown_7", p.unknown_7 as u64, 0);
    put("_sprp_unknown_8", p.unknown_8[0] as u64 | ((p.unknown_8[1] as u64) << 8) | ((p.unknown_8[2] as u64) << 16), 0);
    put("_sprp_wind_data", p.wind_data as u64, 0);
    put("_sprp_setdress_level", p.setdress_level as u64, 0);
    // 0xFFFFFFFF is opaque white, the value a default record carries.
    put("_sprp_diffuse_modulation", p.diffuse_modulation as u64, 0xFFFF_FFFF);
    // The fade distance round-trips through the prop's own field when it is
    // positive or the writer's -1; anything else rides as its bits.
    if p.unknown_4 <= 0.0 && p.unknown_4 != -1.0 {
        out.insert("_sprp_fade".into(), p.unknown_4.to_bits().to_string());
    }
    // Angles go through a rotation in the document and can come back as
    // another triple for the same turn; the record's own triple rides along
    // and is used while the rotation stays the one it describes.
    if p.angles != [0.0; 3] {
        out.insert("_sprp_angles".into(), format!("{:?} {:?} {:?}", p.angles[0], p.angles[1], p.angles[2]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit;
    use crate::props::PropSource;
    use crate::report::Stats;
    use glam::Vec3;
    use rir::AssetTables;
    use uuid::Uuid;

    fn source(model: &str, origin: Vec3, skin: i32, fade: f32) -> PropSource {
        PropSource {
            model: model.to_string(),
            origin,
            angles: Vec3::new(0.0, 90.0, 0.0),
            scale: 2.0,
            skin,
            solid: true,
            cast_shadows: true,
            fade_distance: fade,
            bounds_min: Vec3::splat(-8.0),
            bounds_max: Vec3::splat(8.0),
            sky: false,
            keys: Default::default(),
        }
    }

    fn map_of(sources: &[PropSource]) -> Bsp {
        let mut bsp = emit::new_map(rbsp::Version::ClientV51);
        let mut stats = Stats::default();
        crate::props::emit(sources, &AssetTables::default(), &mut bsp, &mut stats).expect("the props write");
        bsp
    }

    #[test]
    fn every_prop_record_becomes_an_object() {
        let bsp = map_of(&[
            source("mdl/a/one.rmdl", Vec3::new(1.0, 2.0, 3.0), 0, 0.0),
            source("mdl/b/two.rmdl", Vec3::new(-4.0, 5.0, 6.0), 7, 250.0),
            source("mdl/a/one.rmdl", Vec3::ZERO, 0, 0.0),
        ]);
        let objects = static_props(&bsp, rbsp::Version::ClientV51, Uuid::new_v4()).expect("the props decompile");
        assert_eq!(objects.len(), 3, "one object per record");
        let prop = |i: usize| match &objects[i].kind {
            ObjectKind::Prop(p) => p,
            _ => panic!("object {i} is a prop"),
        };
        assert_eq!(prop(0).model, "mdl/a/one.rmdl");
        assert_eq!(prop(1).model, "mdl/b/two.rmdl");
        assert_eq!(objects[0].transform.position, glam::DVec3::new(1.0, 2.0, 3.0));
        assert!(objects[0].transform.to_source_angles().y > 89.0, "the yaw round trips");
        assert_eq!(objects[0].transform.scale, glam::DVec3::splat(2.0), "a static prop's scale rides its transform");
        assert_eq!(prop(1).skin, 7);
        assert_eq!(prop(1).fade_distance, 250.0);
        assert_eq!(prop(0).fade_distance, 0.0, "a negative distance is the writer's no-fade");
        assert!(prop(0).static_prop);
        for o in &objects {
            assert!(o.hidden, "a decompiled prop does not compile until it is unhidden");
        }
    }

    #[test]
    fn record_fields_the_prop_has_no_field_for_are_kept_as_keys() {
        let mut bsp = map_of(&[source("mdl/a/one.rmdl", Vec3::ZERO, 0, 0.0)]);
        // Rewrite the block with a record that is not the default one, which is
        // how a map carries a wind-animated or tinted prop.
        let mut gl = match rbsp::lumps::parse_at(ids::GAME_LUMP, rbsp::Version::ClientV51, bsp.lump(ids::GAME_LUMP), 0)
            .expect("the game lump parses")
        {
            rbsp::lumps::Lump::GameLump(gl) => gl,
            other => panic!("the game lump decoded as {}", other.kind()),
        };
        let Some(at) = gl.entries.iter().position(|e| e.is_sprp()) else { panic!("the block is a prop list") };
        let Some(rbsp::lumps::game_lump::Block::StaticProps(block)) = gl.blocks.get_mut(at) else {
            panic!("the block is a prop list")
        };
        block.props[0].wind_data = 12;
        block.props[0].setdress_level = 3;
        block.props[0].unknown_5 = 9;
        block.props[0].diffuse_modulation = 0x8040_8040;
        bsp.set_lump(ids::GAME_LUMP, rbsp::lumps::game_lump::write(&gl, rbsp::Version::ClientV51));
        let objects = static_props(&bsp, rbsp::Version::ClientV51, Uuid::new_v4()).expect("the props decompile");
        let ObjectKind::Prop(p) = &objects[0].kind else { panic!("an object is a prop") };
        assert_eq!(p.keyvalues.get("_sprp_wind_data").map(String::as_str), Some("12"));
        assert_eq!(p.keyvalues.get("_sprp_setdress_level").map(String::as_str), Some("3"));
        assert_eq!(p.keyvalues.get("_sprp_unknown_5").map(String::as_str), Some("9"));
        assert_eq!(
            p.keyvalues.get("_sprp_diffuse_modulation").map(String::as_str),
            Some(0x8040_8040u32.to_string().as_str()),
            "a tinted prop keeps its modulation"
        );
        assert!(!p.keyvalues.contains_key("_sprp_unknown_0"), "a default field is not a key");
    }

    #[test]
    fn the_prop_table_carries_every_record_the_page_draws() {
        let bsp = map_of(&[
            source("mdl/a/one.rmdl", Vec3::new(1.0, 2.0, 3.0), 0, 0.0),
            source("mdl/b/two.rmdl", Vec3::new(-4.0, 5.0, 6.0), 7, 250.0),
        ]);
        let t = prop_table(&bsp, rbsp::Version::ClientV51).expect("the table encodes").expect("the map has props");
        let u32at = |o: usize| u32::from_le_bytes(t[o..o + 4].try_into().unwrap());
        let f32at = |o: usize| f32::from_le_bytes(t[o..o + 4].try_into().unwrap());
        assert_eq!(&t[0..4], &PROP_TABLE_MAGIC);
        assert_eq!(u32at(4), PROP_TABLE_VERSION);
        assert_eq!((u32at(8), u32at(12)), (2, 2), "two models, two props");
        let mut o = 16;
        let mut names = Vec::new();
        for _ in 0..2 {
            let n = u16::from_le_bytes([t[o], t[o + 1]]) as usize;
            names.push(String::from_utf8(t[o + 2..o + 2 + n].to_vec()).unwrap());
            o += 2 + n;
        }
        assert_eq!(names, ["mdl/a/one.rmdl", "mdl/b/two.rmdl"]);
        let base = (o + 3) & !3;
        assert_eq!(t.len(), base + 2 * PROP_TABLE_RECORD);
        let r = base + PROP_TABLE_RECORD;
        assert_eq!(names[u32at(r) as usize], "mdl/b/two.rmdl");
        assert_eq!(u32at(r + 4), 7, "the skin");
        let object = static_prop(&bsp, rbsp::Version::ClientV51, Uuid::new_v4(), 1).unwrap().unwrap();
        let ObjectKind::Prop(p) = &object.kind else { panic!("the record is a prop") };
        assert_eq!(u32at(r + 8) & PROP_FLAG_SOLID == PROP_FLAG_SOLID, p.solid, "solid when the object is");
        assert_eq!([f32at(r + 12), f32at(r + 16), f32at(r + 20)], [-4.0, 5.0, 6.0]);
        let angles = [f32at(r + 24), f32at(r + 28), f32at(r + 32)];
        let t = Transform::from_source_angles(glam::DVec3::ZERO, glam::Vec3::from_array(angles).as_dvec3());
        assert!(t.rotation.dot(object.transform.rotation).abs() > 1.0 - 1e-12, "the angles rebuild the object's rotation");
        assert_eq!((f32at(r + 36), f32at(r + 40)), (2.0, 250.0), "scale and fade");
    }

    #[test]
    fn a_map_with_no_prop_block_has_no_props() {
        let bsp = emit::new_map(rbsp::Version::DediV47);
        let objects = static_props(&bsp, rbsp::Version::DediV47, Uuid::new_v4()).expect("a map with no props");
        assert!(objects.is_empty());
    }

    #[test]
    fn a_game_lump_that_does_not_parse_is_reported() {
        let mut bsp = emit::new_map(rbsp::Version::ClientV51);
        bsp.set_lump(ids::GAME_LUMP, b"rBSP".to_vec());
        let err = static_props(&bsp, rbsp::Version::ClientV51, Uuid::new_v4()).expect_err("a bad block is refused");
        assert!(matches!(err, DecompileError::Decode { .. }), "{err}");
    }
}
