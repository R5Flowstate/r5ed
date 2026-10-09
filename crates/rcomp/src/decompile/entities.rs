//! Entity blocks read back as document objects: point entities, lights and the
//! brush models a volume is built from.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};

use glam::DVec3;
use indexmap::IndexMap;
use rbsp::lumps::collision::CHILD_HULL;
use rbsp::lumps::entities as ent_text;
use rbsp::{Bsp, Version};
use rdoc::model::{
    Brush, BrushFace, Entity, Id, KeyValues, Light, LightKind, Object, ObjectKind, Partition,
    Transform, UvMapping, Volume, VolumeKind, DEFAULT_MATERIAL,
};

use super::{decode, hidden_object, partition_bytes, DecompileError};
use crate::collision;

/// One entity block and the file it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub partition: Partition,
    pub text: ent_text::Entity,
}

impl Block {
    pub fn classname(&self) -> &str {
        self.text.classname().unwrap_or("")
    }
}

/// Every entity block of the map: the worldspawn lump first, then each
/// `.ent` partition in `Partition::FILES` order.
pub fn read(
    client: &Bsp,
    dedi: Option<&Bsp>,
    ents: &BTreeMap<String, Vec<u8>>,
    version: Version,
    stem: &str,
) -> Result<Vec<Block>, DecompileError> {
    let mut out: Vec<Block> = Vec::new();
    // A dedicated-server map holds the entity set a client map may leave empty.
    let own = client.lump(rbsp::ids::ENTITIES);
    let world: &[u8] = if own.is_empty() {
        dedi.map(|d| d.lump(rbsp::ids::ENTITIES)).unwrap_or_default()
    } else {
        own
    };
    if !world.is_empty() {
        let text = ent_text::parse(world, version).map_err(|e| decode(stem, e))?;
        out.extend(text.entities.into_iter().map(|text| Block { partition: Partition::Main, text }));
    }
    for p in Partition::FILES {
        let Some(data) = partition_bytes(ents, stem, p) else { continue };
        let text = ent_text::parse(data, version).map_err(|e| DecompileError::Partition {
            stem: stem.to_string(),
            name: p.name().to_string(),
            message: e.to_string(),
        })?;
        out.extend(text.entities.into_iter().map(|text| Block { partition: p, text }));
    }
    Ok(out)
}

/// One document object per block: a light where the class is one, a volume
/// where the block owns a brush model, an entity otherwise.
pub fn objects(blocks: &[Block], layer: Id, bsp: &Bsp) -> Result<Vec<Object>, DecompileError> {
    let collidable = collidable_models(bsp);
    let mut cache: HashMap<u32, Option<Vec<Brush>>> = HashMap::new();
    let mut out: Vec<Object> = Vec::with_capacity(blocks.len());
    let mut per_class: BTreeMap<String, usize> = BTreeMap::new();
    for b in blocks {
        let classname = b.classname().to_string();
        let seen = per_class.entry(classname.clone()).or_insert(0);
        let name = format!("{classname}_{}", *seen);
        *seen += 1;
        let (transform, keys) = split_transform(&b.text.pairs);
        let kind = match light_kind(&classname) {
            Some(k) => {
                let mut light = light_of(k, &keys);
                // `light_dynamic` and friends read as a point light; the class
                // they ship as rides along so they compile back to it.
                light.keyvalues.insert("classname".into(), classname.clone());
                // The shipped placement text rides along too: a light straight
                // up or down has several angle triples for one turn, and the
                // compiler writes the shipped one while the transform matches.
                for (k, v) in &b.text.pairs {
                    if k == "origin" || k == "angles" {
                        light.keyvalues.insert(k.clone(), v.clone());
                    }
                }
                ObjectKind::Light(light)
            }
            None => match brush_model(&b.text.pairs) {
                Some(model) => {
                    // Reading a model's tree means parsing the whole collision
                    // lump set, so a model several blocks name is read once.
                    let brushes = match cache.entry(model) {
                        Entry::Occupied(slot) => slot.into_mut(),
                        Entry::Vacant(slot) => slot.insert(brushes_of(bsp, model, &transform, &collidable)),
                    };
                    match brushes.clone() {
                        Some(brushes) => ObjectKind::Volume(Volume {
                            kind: volume_kind(&classname),
                            classname: classname.clone(),
                            brushes,
                            keyvalues: volume_keys(keys),
                        }),
                        // A model whose collision is not a set of convex volumes
                        // cannot be written as brushes, so the block stays an
                        // entity and keeps the model keys it came with.
                        None => ObjectKind::Entity(Entity {
                            classname: classname.clone(),
                            keyvalues: keys,
                            partition: Some(b.partition),
                            links: IndexMap::new(),
                        }),
                    }
                }
                None => ObjectKind::Entity(Entity {
                    classname: classname.clone(),
                    keyvalues: keys,
                    partition: Some(b.partition),
                    links: IndexMap::new(),
                }),
            },
        };
        out.push(hidden_object(name, layer, transform, kind));
    }
    Ok(out)
}

/// Three whitespace-separated finite numbers, or nothing.
pub fn vec3(text: &str) -> Option<DVec3> {
    let mut it = text.split_whitespace();
    let mut next = || it.next()?.parse::<f64>().ok().filter(|v| v.is_finite());
    let out = DVec3::new(next()?, next()?, next()?);
    it.next().is_none().then_some(out)
}

/// Splits the block's class and placement keys off the rest. A value that is
/// not three numbers stays a key, so nothing is lost to a malformed line.
fn split_transform(pairs: &[(String, String)]) -> (Transform, KeyValues) {
    let mut origin = DVec3::ZERO;
    let mut angles = DVec3::ZERO;
    let mut keys = KeyValues::new();
    for (k, v) in pairs {
        match k.as_str() {
            "classname" => continue,
            "origin" => {
                if let Some(p) = vec3(v) {
                    origin = p;
                    continue;
                }
            }
            "angles" => {
                if let Some(a) = vec3(v) {
                    angles = a;
                    continue;
                }
            }
            _ => {}
        }
        keys.insert(k.clone(), v.clone());
    }
    (Transform::from_source_angles(origin, angles), keys)
}

/// The light classes a `Light` object compiles back to.
fn light_kind(classname: &str) -> Option<LightKind> {
    match classname {
        "light" | "light_dynamic" | "light_dynamic_lights" => Some(LightKind::Point),
        "light_spot" => Some(LightKind::Spot),
        "light_environment" => Some(LightKind::Sun),
        "sky_camera" => Some(LightKind::Sky),
        _ => None,
    }
}

fn number(keys: &KeyValues, key: &str) -> Option<f64> {
    keys.get(key)?.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn flag(keys: &KeyValues, key: &str) -> bool {
    number(keys, key).map(|v| v != 0.0).unwrap_or(false)
}

/// The `_light` triple and brightness: `r g b intensity` with the colour in
/// 0..255 and the intensity already scaled the way the writer stores it.
fn light_values(keys: &KeyValues) -> Option<([f32; 3], f32)> {
    let text = keys.get("_light")?;
    let mut it = text.split_whitespace();
    let mut next = || it.next()?.parse::<f64>().ok().filter(|v| v.is_finite());
    let channel = |v: f64| (v.clamp(0.0, 255.0) / 255.0) as f32;
    let colour = [channel(next()?), channel(next()?), channel(next()?)];
    let intensity = next()?;
    it.next().is_none().then_some((colour, intensity as f32))
}

/// The keys a light's own writer owns, per class: they are read back into the
/// fields.
#[allow(dead_code)]
fn light_keys(kind: LightKind) -> &'static [&'static str] {
    match kind {
        LightKind::Point => &["_light", "_distance", "realtime", "realtime_shadows"],
        LightKind::Spot => {
            &["_light", "_distance", "_inner_cone", "_cone", "realtime", "realtime_shadows", "shadow"]
        }
        LightKind::Sun => &["_light", "shadows"],
        LightKind::Sky => &["fogdensity"],
        LightKind::Area => &["_light"],
    }
}

/// The inverse of what the entity stage writes for a light: the values it reads
/// back out of the block, and every other key kept as it was.
fn light_of(kind: LightKind, keys: &KeyValues) -> Light {
    let (colour, intensity) = light_values(keys).unwrap_or(([1.0; 3], 1.0));
    let realtime = flag(keys, "realtime");
    let (range, inner, outer, cast, intensity) = match kind {
        LightKind::Point => (
            number(keys, "_distance").unwrap_or(300.0) as f32,
            0.0,
            0.0,
            flag(keys, "realtime_shadows"),
            intensity,
        ),
        LightKind::Spot => {
            let inner = number(keys, "_inner_cone").unwrap_or(45.0) as f32;
            let outer =
                number(keys, "_cone").filter(|v| *v > inner as f64).unwrap_or(inner as f64) as f32;
            (
                number(keys, "_distance").unwrap_or(300.0) as f32,
                inner,
                outer,
                flag(keys, "realtime_shadows") || flag(keys, "shadow"),
                intensity,
            )
        }
        LightKind::Sun => (0.0, 0.0, 0.0, flag(keys, "shadows"), intensity),
        // The sky camera's own intensity rides the fog density, at a fifth of
        // the value the light carries.
        LightKind::Sky => (0.0, 0.0, 0.0, false, number(keys, "fogdensity").map(|v| (v / 0.2) as f32).unwrap_or(intensity)),
        LightKind::Area => (0.0, 0.0, 0.0, false, intensity),
    };
    // The shipped keys stay, the ones the fields were read from included: a
    // light compiles back to its own text until one of its fields is edited
    // (rdoc drops the derived keys then), and fields such as a realtime
    // shadow mode of 2 have no field to live in.
    let keyvalues = keys.clone();
    Light {
        kind,
        color: colour,
        intensity,
        range,
        inner_angle: inner,
        outer_angle: outer,
        size: [0.0; 2],
        cast_shadows: cast,
        baked: !realtime,
        realtime,
        keyvalues,
    }
}

/// The volume class an entity class reads as.
fn volume_kind(classname: &str) -> VolumeKind {
    match classname {
        "trigger_hurt" | "trigger_multiple_hurt" | "trigger_laser_trip" => VolumeKind::TriggerHurt,
        "trigger_soundscape" => VolumeKind::TriggerSoundscape,
        "envmap_volume" => VolumeKind::EnvmapVolume,
        "light_environment_volume" | "light_probe_volume" => VolumeKind::LightEnvironmentVolume,
        "fog_volume" | "fog_volume_brush" | "trigger_fog" => VolumeKind::FogVolume,
        "playerclip" | "func_occluder" => VolumeKind::PlayerClip,
        "out_of_bounds_volume" | "trigger_out_of_bounds" => VolumeKind::OutOfBounds,
        c if c.starts_with("trigger_") => VolumeKind::Trigger,
        _ => VolumeKind::Custom,
    }
}

/// The model a block's brush geometry names, preferring the models-lump index
/// over the standalone `*coll` blob, which holds the same collision.
fn brush_model(pairs: &[(String, String)]) -> Option<u32> {
    let mut fallback = None;
    for (k, _) in pairs {
        let Some(model) = ent_text::model_index(k) else { continue };
        if k.starts_with("*coll") {
            fallback = fallback.or(Some(model));
        } else {
            return Some(model);
        }
    }
    fallback
}

/// Which model indices carry a collision tree. Reading the tree of a model that
/// has none means parsing the whole collision lump set, so the table is read
/// once and a model with no tree is never asked for.
fn collidable_models(bsp: &Bsp) -> Vec<bool> {
    let version = bsp.version_kind().unwrap_or(rbsp::Version::ClientV51);
    match rbsp::lumps::collision::parse_models(bsp.lump(rbsp::ids::MODELS), version) {
        Ok(list) => list.iter().map(collision::model_has_collision).collect(),
        Err(_) => Vec::new(),
    }
}

/// The convex volumes a brush model is made of, in the block's own space, or
/// `None` when its collision is not convex pieces.
fn brushes_of(bsp: &Bsp, model: u32, transform: &Transform, collidable: &[bool]) -> Option<Vec<Brush>> {
    if !collidable.get(model as usize).copied().unwrap_or(false) {
        return None;
    }
    let leaves = collision::decode_leaves(bsp, model as usize).ok()?;
    let inverse = inverse_matrix(transform);
    let mut out: Vec<Brush> = Vec::new();
    for leaf in &leaves {
        if leaf.triangles.is_empty() {
            continue;
        }
        if leaf.kind != CHILD_HULL {
            return None;
        }
        let piece: Vec<[DVec3; 3]> = leaf
            .triangles
            .iter()
            .map(|t| [inverse.transform_point3(t[0]), inverse.transform_point3(t[1]), inverse.transform_point3(t[2])])
            .collect();
        out.push(convex_brush(&piece)?);
    }
    (!out.is_empty()).then_some(out)
}

/// The matrix that takes a world point into the object's own space.
fn inverse_matrix(t: &Transform) -> glam::DMat4 {
    glam::DMat4::from_scale_rotation_translation(t.scale, t.rotation, t.position).inverse()
}

/// One convex solid of a piece of collision: the piece's triangles lie in a
/// handful of planes, and one plane is one face of the solid.
///
/// A hull leaf stores each side as one triangle or as a parallelogram pair, so
/// the faces come out of the winding alone; a general hull search would be
/// superlinear in the corner count and a real piece has a lot of them.
fn convex_brush(triangles: &[[DVec3; 3]]) -> Option<Brush> {
    // Two triangles of one side are the same plane to within the corner grid
    // they were quantised on.
    const PLANE_EPS: f64 = 1e-3;
    const DIST_EPS: f64 = 1e-2;
    let mut planes: Vec<(DVec3, f64)> = Vec::new();
    for t in triangles {
        let n = (t[1] - t[0]).cross(t[2] - t[0]);
        if n.length_squared() <= 0.0 {
            continue;
        }
        let n = n.normalize();
        let d = n.dot(t[0]);
        if !n.is_finite() || !d.is_finite() {
            continue;
        }
        if planes.iter().any(|(p, q)| (*p - n).abs().max_element() <= PLANE_EPS && (*q - d).abs() <= DIST_EPS) {
            continue;
        }
        planes.push((n, d));
    }
    let faces: Vec<BrushFace> = planes
        .iter()
        .map(|(normal, dist)| BrushFace {
            normal: *normal,
            dist: *dist,
            material: DEFAULT_MATERIAL.to_string(),
            uv: UvMapping::default(),
            lightmap_scale: 0.0,
        })
        .collect();
    (!faces.is_empty()).then_some(Brush { faces, contents: 1, detail: false })
}

/// A volume's own keys: the model, its collider chunks and the bounds the
/// writer derives from the brushes are all rebuilt from the geometry.
fn volume_keys(keys: KeyValues) -> KeyValues {
    keys.into_iter()
        .filter(|(k, _)| k != "model" && k != "mins" && k != "maxs" && !ent_text::is_model_key(k))
        .collect()
}

/// A light's colour and intensity as the entity text carries them.
#[cfg(test)]
pub fn light_text(light: &Light) -> String {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as i32;
    format!(
        "{} {} {} {}",
        channel(light.color[0]),
        channel(light.color[1]),
        channel(light.color[2]),
        light.intensity
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit;
    use crate::entities::light_class;
    use rir::{CollisionShape, CollisionSolid, ModelSlot};
    use uuid::Uuid;

    fn sample(kind: LightKind) -> Light {
        Light {
            kind,
            color: [1.0, 0.5, 0.0],
            intensity: 2.5,
            range: 640.0,
            inner_angle: 20.0,
            outer_angle: 55.0,
            size: [0.0; 2],
            cast_shadows: true,
            baked: true,
            realtime: false,
            keyvalues: KeyValues::from([("style".to_string(), "3".to_string())]),
        }
    }

    /// The block the compiler writes for a light, with its placement keys: the
    /// class's own keys, then the author's, each replacing the one before it.
    fn written_block(light: &Light) -> Vec<(String, String)> {
        let mut pairs = crate::entities::light_defaults(light_class(light.kind), light);
        pairs.push(("origin".into(), "64 -32 16".into()));
        if light.kind != LightKind::Point {
            pairs.push(("angles".into(), "-15 90 0".into()));
        }
        for (k, v) in &light.keyvalues {
            match pairs.iter_mut().find(|(name, _)| name == k) {
                Some(slot) => slot.1 = v.clone(),
                None => pairs.push((k.clone(), v.clone())),
            }
        }
        pairs
    }

    #[test]
    fn a_written_light_block_reads_back_as_the_same_light() {
        for kind in [LightKind::Point, LightKind::Spot, LightKind::Sun, LightKind::Sky] {
            let light = sample(kind);
            let pairs = written_block(&light);
            let (transform, keys) = split_transform(&pairs);
            let back = light_of(kind, &keys);
            assert_eq!(transform.position, DVec3::new(64.0, -32.0, 16.0), "{kind:?} keeps its origin");
            let again = written_block(&back);
            assert_eq!(again, pairs, "{kind:?} writes the block it was read from");
        }
    }

    #[test]
    fn a_light_colour_and_intensity_survive_the_text() {
        let light = sample(LightKind::Point);
        let text = light_text(&light);
        let keys = KeyValues::from([("_light".to_string(), text.clone())]);
        let back = light_of(LightKind::Point, &keys);
        assert_eq!(light_text(&back), text, "the text a light writes is the text it reads");
        assert_eq!(back.intensity, light.intensity);
        // The colour rides eight bits per channel, so it comes back to within
        // one step of the quantisation grid.
        for (a, b) in light.color.iter().zip(back.color.iter()) {
            assert!((a - b).abs() <= 1.0 / 255.0, "{a} came back as {b}");
        }
    }

    #[test]
    fn a_light_without_its_own_keys_falls_back_to_the_writers_defaults() {
        let back = light_of(LightKind::Spot, &KeyValues::new());
        assert_eq!(back.range, 300.0, "the writer's default range");
        assert_eq!(back.inner_angle, 45.0);
        assert_eq!(back.outer_angle, 45.0, "a cone no wider than the inner one");
        assert_eq!(back.color, [1.0; 3]);
        assert!(back.baked && !back.realtime, "a light with no realtime key is baked");
    }

    #[test]
    fn a_placement_value_that_is_not_three_numbers_stays_a_key() {
        let pairs = vec![
            ("classname".to_string(), "trigger_hurt".to_string()),
            ("origin".to_string(), "somewhere".to_string()),
            ("angles".to_string(), "0 0".to_string()),
        ];
        let (transform, keys) = split_transform(&pairs);
        assert_eq!(transform.position, DVec3::ZERO);
        assert_eq!(keys.get("origin").map(String::as_str), Some("somewhere"));
        assert_eq!(keys.get("angles").map(String::as_str), Some("0 0"));
        assert!(!keys.contains_key("classname"), "the class is a field of its own");
    }

    #[test]
    fn three_numbers_must_be_three_numbers() {
        assert_eq!(vec3("1 2 3"), Some(DVec3::new(1.0, 2.0, 3.0)));
        assert_eq!(vec3("  -1.5\t2 3 "), Some(DVec3::new(-1.5, 2.0, 3.0)));
        assert_eq!(vec3("1 2"), None);
        assert_eq!(vec3("1 2 3 4"), None);
        assert_eq!(vec3("1 2 x"), None);
        assert_eq!(vec3("1 2 inf"), None, "a non-finite coordinate is not a coordinate");
        assert_eq!(vec3(""), None);
    }

    #[test]
    fn the_models_lump_index_wins_over_the_collider_chunk() {
        let pairs = vec![
            ("*coll0".to_string(), String::new()),
            ("*7".to_string(), String::new()),
            ("*coll1".to_string(), String::new()),
        ];
        assert_eq!(brush_model(&pairs), Some(7));
        let only_coll = vec![("*coll0".to_string(), String::new())];
        assert_eq!(brush_model(&only_coll), Some(0), "a volume may only carry the blob");
        assert_eq!(brush_model(&[("model".to_string(), "x".to_string())]), None);
    }

    #[test]
    fn a_volume_drops_the_keys_its_geometry_rebuilds() {
        let keys = KeyValues::from([
            ("model".to_string(), "*3".to_string()),
            ("mins".to_string(), "0 0 0".to_string()),
            ("maxs".to_string(), "64 64 64".to_string()),
            ("*coll0".to_string(), "AAAA".to_string()),
            ("targetname".to_string(), "door".to_string()),
        ]);
        let out = volume_keys(keys);
        assert_eq!(out.len(), 1);
        assert_eq!(out.get("targetname").map(String::as_str), Some("door"));
    }

    #[test]
    fn the_shipped_volume_classes_map_to_their_own_kinds() {
        assert_eq!(volume_kind("trigger_hurt"), VolumeKind::TriggerHurt);
        assert_eq!(volume_kind("trigger_multiple"), VolumeKind::Trigger);
        assert_eq!(volume_kind("trigger_soundscape"), VolumeKind::TriggerSoundscape);
        assert_eq!(volume_kind("envmap_volume"), VolumeKind::EnvmapVolume);
        assert_eq!(volume_kind("light_environment_volume"), VolumeKind::LightEnvironmentVolume);
        assert_eq!(volume_kind("fog_volume"), VolumeKind::FogVolume);
        assert_eq!(volume_kind("playerclip"), VolumeKind::PlayerClip);
        assert_eq!(volume_kind("out_of_bounds_volume"), VolumeKind::OutOfBounds);
        assert_eq!(volume_kind("func_door"), VolumeKind::Custom);
    }

    /// A map whose only model is a box written as a hull leaf, the shape a
    /// brush entity's collision takes.
    fn box_model() -> Bsp {
        let (lo, hi) = (DVec3::new(-64.0, -64.0, 0.0), DVec3::new(64.0, 64.0, 128.0));
        let planes: Vec<(glam::Vec3, f32)> = vec![
            (glam::Vec3::X, hi.x as f32),
            (-glam::Vec3::X, -lo.x as f32),
            (glam::Vec3::Y, hi.y as f32),
            (-glam::Vec3::Y, -lo.y as f32),
            (glam::Vec3::Z, hi.z as f32),
            (-glam::Vec3::Z, -lo.z as f32),
        ];
        let slots = vec![ModelSlot {
            index: 0,
            owner: None,
            mins: lo.as_vec3(),
            maxs: hi.as_vec3(),
            has_render: false,
        }];
        let solids = vec![CollisionSolid {
            object: None,
            model: 0,
            shape: CollisionShape::Brush { planes },
            contents: 1,
            surface_prop: 0,
            surfaces: Vec::new(),
        }];
        let mut bsp = emit::new_map(rbsp::Version::ClientV51);
        collision::emit_collision(&solids, &slots, &mut bsp).expect("the box collision writes");
        bsp
    }

    #[test]
    fn a_hull_model_becomes_one_brush_of_six_planes() {
        let bsp = box_model();
        let brushes = brushes_of(&bsp, 0, &Transform::default(), &collidable_models(&bsp)).expect("the box is a set of convex volumes");
        assert_eq!(brushes.len(), 1, "one hull leaf, one brush");
        // A hull leaf stores each side as a pair of triangles, and the two of a
        // side share a plane, so the brush comes out as the box's six sides.
        assert_eq!(brushes[0].faces.len(), 6, "one face per side");
        for f in &brushes[0].faces {
            assert!(f.normal.is_finite() && f.dist.is_finite());
            assert!((f.normal.length() - 1.0).abs() < 1e-9, "the plane normal is a unit vector");
        }
        let mut normals: Vec<[i64; 3]> = brushes[0]
            .faces
            .iter()
            .map(|f| {
                let n = f.normal.as_vec3() * 1000.0;
                [n.x as i64, n.y as i64, n.z as i64]
            })
            .collect();
        normals.sort_unstable();
        normals.dedup();
        assert_eq!(normals.len(), 6, "one plane per side, each wound outwards");
        let solid = rdoc::brush::Solid::from_faces(&brushes[0].faces).expect("the planes bound a solid");
        assert_eq!(solid.vertices.len(), 8, "the hull has eight corners");
    }

    #[test]
    fn a_brush_is_rebuilt_in_the_objects_own_space() {
        let bsp = box_model();
        let transform = Transform::from_source_angles(DVec3::new(1024.0, 0.0, 0.0), DVec3::new(0.0, 90.0, 0.0));
        let brushes = brushes_of(&bsp, 0, &transform, &collidable_models(&bsp)).expect("the box is a set of convex volumes");
        let solid = rdoc::brush::Solid::from_faces(&brushes[0].faces).expect("the planes bound a solid");
        let world = glam::DMat4::from_scale_rotation_translation(
            transform.scale,
            transform.rotation,
            transform.position,
        );
        let lo = solid.vertices.iter().fold(DVec3::splat(f64::INFINITY), |a, v| a.min(world.transform_point3(*v)));
        let hi = solid.vertices.iter().fold(DVec3::splat(f64::NEG_INFINITY), |a, v| a.max(world.transform_point3(*v)));
        // The brushes are stored around the object's own origin, so putting the
        // transform back has to land on the box the collision was read from.
        assert!((lo - DVec3::new(-64.0, -64.0, 0.0)).length() < 1e-6, "mins {lo}");
        assert!((hi - DVec3::new(64.0, 64.0, 128.0)).length() < 1e-6, "maxs {hi}");
        let local = solid.vertices.iter().fold(DVec3::splat(f64::INFINITY), |a, v| a.min(*v));
        // A quarter turn about Z moves the box a thousand units along Y in its
        // own space, which is what proves the corners are not world space.
        assert!(local.y > 900.0, "the corners are far from the object origin, y starts at {}", local.y);
    }

    #[test]
    fn a_model_with_no_collision_is_not_a_volume() {
        let bsp = emit::new_map(rbsp::Version::ClientV51);
        assert_eq!(brushes_of(&bsp, 0, &Transform::default(), &collidable_models(&bsp)), None);
        assert_eq!(brushes_of(&bsp, 9, &Transform::default(), &collidable_models(&bsp)), None, "a model index past the table");
    }

    #[test]
    fn a_block_with_a_brush_model_becomes_a_volume() {
        let mut bsp = box_model();
        let text = rbsp::lumps::entities::EntityText {
            header: None,
            entities: vec![rbsp::lumps::entities::Entity {
                pairs: vec![
                    ("classname".into(), "trigger_hurt".into()),
                    ("model".into(), "*0".into()),
                    ("*coll0".into(), "AAAA".into()),
                    ("mins".into(), "0 0 0".into()),
                    ("damage".into(), "10".into()),
                ],
            }],
            tail: vec![0],
        };
        bsp.set_lump(rbsp::ids::ENTITIES, text.to_text().into_bytes());
        let blocks = read(&bsp, None, &BTreeMap::new(), rbsp::Version::ClientV51, "mp_rr_x")
            .expect("the map's entity text parses");
        let objects = objects(&blocks, Uuid::new_v4(), &bsp).expect("the blocks become objects");
        let ObjectKind::Volume(v) = &objects[0].kind else { panic!("a brush model became a volume") };
        assert_eq!(v.classname, "trigger_hurt");
        assert_eq!(v.kind, VolumeKind::TriggerHurt);
        assert_eq!(v.brushes.len(), 1);
        assert_eq!(v.keyvalues.len(), 1, "only the author's own key is left");
        assert_eq!(v.keyvalues.get("damage").map(String::as_str), Some("10"));
    }
}
