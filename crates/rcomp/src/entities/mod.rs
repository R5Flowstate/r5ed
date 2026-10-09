//! Entity emission: the worldspawn block, every document object that becomes an
//! entity, the per-class partition files and the `*coll` collision blobs.

use std::collections::{BTreeMap, HashMap};

use glam::{DVec3, Vec3};
use rdoc::model::{
    Brush, Entity as EntityDef, Id, Light, LightKind, MapDoc, Object, ObjectKind, Partition, PathKind,
    PathObject, Volume,
};
use rdoc::schema::EntitySchema;
use rir::{ModelSlot, RenderSurface};
use uuid::Uuid;

use crate::collision::{self, CollTarget};
use crate::csg::DPlane;

/// One entity block, as the engine text holds it.
pub type Block = rbsp::lumps::entities::Entity;

#[derive(Debug, thiserror::Error)]
pub enum EntityError {
    #[error("entity schema has no class named {0}")]
    UnknownClass(String),
    #[error("class {classname} of object {object}: {message}")]
    Invalid { classname: String, object: Uuid, message: String },
    #[error("lump 0x{id:02x}: {message}")]
    Lump { id: u16, message: String },
}

/// The map-level keys every shipped map's worldspawn block carries. The
/// values are the ones the 23 dedicated-server fixtures agree on.
const MUSIC_POSTFIX: &str = "Waterfront";
const MAP_VERSION: &str = "30";
const VORONOI_LEAFSIZE: &str = "1";
const SPAWNCLASS: &str = "idWorldspawn";

/// The engine revision the fixtures declare, and the value a fresh map writes.
pub const ENGINE_REVISION: u32 = 30;

/// Contents mask, surface flags and surface name a plane-defined volume's
/// collision blob carries. The masks are the ones the fixture `*coll` blobs
/// hold; the name is the class-specific tool surface the engine reports.
pub const TRIGGER_CONTENTS: u32 = 0x00EB_1280;
pub const TRIGGER_FLAGS: u16 = 0x400;
pub const ENVMAP_CONTENTS: u32 = 0x10E3_1240;
pub const ENVMAP_FLAGS: u16 = 0x610;

/// Volume classes whose tool surface is not the trigger default.
const ENVMAP_CLASSES: [&str; 4] = [
    "envmap_volume",
    "wind_volume",
    "wind_volume_brush",
    "out_of_bounds_volume",
];
const NAMED_VOLUME_SURFACES: [(&str, &str); 7] = [
    ("trigger_out_of_bounds", "TOOLS\\TOOLSOUT_OF_BOUNDS"),
    ("trigger_soundscape", "TOOLS\\TOOLSTRIGGER_SOUNDSCAPE"),
    ("trigger_slip", "TOOLS\\TOOLSTRIGGER_SLIP"),
    ("trigger_no_zipline", "TOOLS\\TOOLSTRIGGER_NO_ZIPLINE"),
    ("trigger_no_object_placement", "TOOLS\\TOOLSTRIGGER_NO_OBJECT_PLACEMENT"),
    ("trigger_no_object_placement_special", "TOOLS\\TOOLSTRIGGER_NO_OBJECT_PLACEMENT_SPECIAL"),
    ("light_environment_volume", "TOOLS\\TOOLSTRIGGER"),
];

/// The contents mask, surface flags and name of a volume class's collider.
pub fn volume_surface(classname: &str) -> (u32, u16, String) {
    if ENVMAP_CLASSES.contains(&classname) {
        return (ENVMAP_CONTENTS, ENVMAP_FLAGS, "TOOLS\\TOOLSENVMAPVOLUME".to_string());
    }
    for (cls, name) in NAMED_VOLUME_SURFACES {
        if classname == cls {
            return (TRIGGER_CONTENTS, TRIGGER_FLAGS, name.to_string());
        }
    }
    (TRIGGER_CONTENTS, TRIGGER_FLAGS, "TOOLS\\TOOLSTRIGGER".to_string())
}

/// Rounds a coordinate the way the entity text carries it.
pub fn num(v: f64) -> String {
    let v = if v == 0.0 { 0.0 } else { v };
    let s = format!("{v}");
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

pub fn vec3_text(v: DVec3) -> String {
    format!("{} {} {}", num(v.x), num(v.y), num(v.z))
}

pub fn angles_text(v: DVec3) -> String {
    format!("{} {} {}", num(v.x), num(v.y), num(v.z))
}

fn block(pairs: Vec<(String, String)>) -> Block {
    Block { pairs }
}

fn push(pairs: &mut Vec<(String, String)>, key: &str, value: impl Into<String>) {
    let value = value.into();
    if let Some(slot) = pairs.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value;
        return;
    }
    pairs.push((key.to_string(), value));
}

fn insert(pairs: &mut Vec<(String, String)>, key: &str, value: impl Into<String>) {
    pairs.push((key.to_string(), value.into()));
}

/// A value scaled to the 0..255 integer the `_light` keys carry.
fn channel(v: f32) -> i32 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    (v * 255.0).round() as i32
}

fn light_colour(l: &Light) -> (i32, i32, i32) {
    (channel(l.color[0]), channel(l.color[1]), channel(l.color[2]))
}

/// A linear light value as the `_light` / `_ambient` key writes it: a colour
/// whose channels are gamma 2.2 and a brightness out of 255, so a channel's
/// value is `(c / 255)^2.2 * brightness / 255`. The shipped sun
/// `204 198 183 637` is the 1.508 1.411 1.183 its world light carries.
pub fn source_light(v: DVec3) -> String {
    let v = v.max(DVec3::ZERO);
    let peak = v.max_element();
    if peak <= 0.0 || !peak.is_finite() {
        return "255 255 255 0".to_string();
    }
    let c = |x: f64| ((x / peak).powf(1.0 / 2.2) * 255.0).round() as i32;
    format!("{} {} {} {}", c(v.x), c(v.y), c(v.z), num((peak * 255.0 * 100.0).round() / 100.0))
}

fn brightness(l: &Light) -> String {
    let b = if l.intensity.is_finite() && l.intensity > 0.0 { l.intensity } else { 1.0 };
    let b = if b >= 100.0 { b.round() } else { (b * 100.0).round() / 100.0 };
    format!("{b}")
}

/// The class a `Light` object compiles as. A directional light is a
/// `light_environment`, which is the class the shipped maps point the sun with.
pub fn light_class(kind: LightKind) -> &'static str {
    match kind {
        LightKind::Sun | LightKind::Area => "light_environment",
        LightKind::Point => "light",
        LightKind::Spot => "light_spot",
        LightKind::Sky => "sky_camera",
    }
}

/// The keys a light carries beyond the author's own, per class.
pub fn light_defaults(classname: &str, l: &Light) -> Vec<(String, String)> {
    let (r, g, b) = light_colour(l);
    let range = if l.range > 0.0 { l.range } else { 300.0 };
    let half = (range as f64 * 0.3).round() as i32;
    let realtime = i32::from(l.realtime || !l.baked);
    let mut out: Vec<(String, String)> = Vec::new();
    match classname {
        "light" => {
            push(&mut out, "classname", "light");
            push(&mut out, "_light", format!("{r} {g} {b} {}", brightness(l)));
            push(&mut out, "_distance", num(range as f64));
            push(&mut out, "_fifty_percent_distance", num(half as f64));
            push(&mut out, "_zero_percent_distance", num(range as f64));
            push(&mut out, "_emitter_radius", "0");
            push(&mut out, "_specular_intensity", "1");
            push(&mut out, "_pbr_falloff", "0");
            push(&mut out, "realtime", realtime.to_string());
            push(&mut out, "realtime_shadows", "0");
            push(&mut out, "style", "0");
            push(&mut out, "script_tweakable", "0");
            push(&mut out, "vfog_enabled", "0");
            push(&mut out, "vfog_boost", "1.0");
            push(&mut out, "bounce_boost", "1.0");
        }
        "light_spot" => {
            let inner = if l.inner_angle > 0.0 { l.inner_angle } else { 45.0 };
            let outer = if l.outer_angle > inner { l.outer_angle } else { inner };
            push(&mut out, "classname", "light_spot");
            push(&mut out, "_light", format!("{r} {g} {b} {}", brightness(l)));
            push(&mut out, "_distance", num(range as f64));
            push(&mut out, "_fifty_percent_distance", num(half as f64));
            push(&mut out, "_zero_percent_distance", num(range as f64));
            push(&mut out, "_inner_cone", num(inner as f64));
            push(&mut out, "_cone", num(outer as f64));
            push(&mut out, "_exponent", "1");
            push(&mut out, "_emitter_radius", "0");
            push(&mut out, "_specular_intensity", "1");
            push(&mut out, "_pbr_falloff", "0");
            push(&mut out, "realtime", realtime.to_string());
            push(&mut out, "realtime_shadows", i32::from(l.cast_shadows).to_string());
            push(&mut out, "shadow", i32::from(l.cast_shadows).to_string());
            push(&mut out, "shadow_bias", "0");
            push(&mut out, "shadow_filter_size", "3");
            push(&mut out, "shadow_upres", "0");
            push(&mut out, "style", "0");
            push(&mut out, "script_tweakable", "0");
            push(&mut out, "vfog_enabled", "0");
            push(&mut out, "vfog_boost", "1.0");
            push(&mut out, "bounce_boost", "1.0");
        }
        "light_environment" => {
            let sun = DVec3::new(l.color[0] as f64, l.color[1] as f64, l.color[2] as f64) * l.intensity as f64;
            push(&mut out, "classname", "light_environment");
            push(&mut out, "lightEnvironmentIndex", "*0");
            push(&mut out, "_light", source_light(sun));
            push(&mut out, "_ambient", source_light(DVec3::ZERO));
            push(&mut out, "cloudoffset", "0 0 0 0");
            push(&mut out, "shadows", i32::from(l.cast_shadows).to_string());
            push(&mut out, "sun_highlight_size", "2");
            push(&mut out, "SunSpreadAngle", "10");
            push(&mut out, "specular_bounce", "0");
            push(&mut out, "lighting_mode", "0");
            push(&mut out, "strict_realtime", "1");
            push(&mut out, "vfog_enabled", "0");
            push(&mut out, "bounce_boost", "3");
            let _ = (r, g, b);
        }
        "sky_camera" => {
            push(&mut out, "classname", "sky_camera");
            push(&mut out, "targetname", "skybox_cam_level");
            push(&mut out, "use_angles", "0");
            push(&mut out, "useworldfog", "0");
            push(&mut out, "fogenable", "1");
            push(&mut out, "fogcolor", "189 189 189");
            push(&mut out, "fogcolorstrength", "1");
            push(&mut out, "fogdensity", num(l.intensity as f64 * 0.2));
            push(&mut out, "fogdircolor", "255 233 220");
            push(&mut out, "fogdircolorstrength", "1");
            push(&mut out, "fogdirhalfangle", "60");
            push(&mut out, "fogdistoffset", "0");
            push(&mut out, "foghalfdistbottom", "500");
            push(&mut out, "foghalfdisttop", "5000");
            push(&mut out, "fogzbottom", "0");
            push(&mut out, "fogztop", "1000");
            push(&mut out, "fogangles", "0 90 0");
            // Both keys set the server's one skybox scale, so the later one
            // wins; shipped maps write `scale` first and the client divides
            // the view's offset from the camera by 1000.
            push(&mut out, "scale", "1");
            push(&mut out, "skyscale", "1000");
        }
        other => push(&mut out, "classname", other),
    }
    out
}

/// A stable 64-bit id per endpoint. The fixtures carry a 16-hex-digit value
/// the engine only ever compares for equality, so any stable derivation of the
/// endpoint position serves.
pub fn path_guids(points: &[DVec3]) -> Vec<String> {
    let mut out = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |b: u8| {
            h ^= b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        };
        for b in (i as u64).to_le_bytes() {
            feed(b);
        }
        // 1/4096 of a unit, which is finer than the entity text carries.
        for c in [p.x, p.y, p.z] {
            for b in (c * 4096.0).round().to_le_bytes() {
                feed(b);
            }
        }
        out.push(format!("{:016x}", h));
    }
    out
}

/// The `zipline` and `zipline_end` chain a path object compiles to.
pub fn zipline_chain(kind: PathKind, path: &PathObject, world: &[DVec3]) -> Vec<Block> {
    let rail = kind == PathKind::Ziprail;
    let spline = kind == PathKind::Spline;
    let classname = if spline { "path_spline" } else { "zipline" };
    let width = if rail { "2" } else { "4" };
    let vertical = i32::from(world.len() < 2 || {
        let d = world[1] - world[0];
        (d.x * d.x + d.y * d.y).sqrt() <= (d.z * d.z).sqrt()
    });
    let length = world
        .first()
        .zip(world.last())
        .map(|(a, b)| (b - a).length())
        .unwrap_or(0.0);
    let auto = (length * 0.5).round().max(1.0);
    let guids = path_guids(world);
    let direction = if world.len() >= 2 { world[1] - world[0] } else { DVec3::X };
    let angles = direction_angles(direction);
    let mut out = Vec::new();
    if world.len() < 2 {
        return out;
    }
    let head_pairs = |end: Option<&String>| -> Vec<(String, String)> {
        let mut p: Vec<(String, String)> = vec![
            ("classname".into(), classname.into()),
            ("origin".into(), vec3_text(world[0])),
            ("angles".into(), angles_text(angles)),
            ("scale".into(), "1".into()),
            ("pitch".into(), num(angles.x)),
            ("Material".into(), "cable/zipline.vmt".into()),
            ("Width".into(), width.into()),
            ("ZiplineLengthScale".into(), "1".into()),
            ("ZiplineSpeedScale".into(), "1".into()),
            ("ZiplineFadeDistance".into(), "-1".into()),
            ("ZiplineDropToBottom".into(), "1".into()),
            ("ZiplinePreserveVelocity".into(), "0".into()),
            ("ZiplinePushOffInDirectionX".into(), "0".into()),
            ("ZiplineVertical".into(), vertical.to_string()),
            ("ZiplineAutoDetachDistance".into(), num(auto)),
            ("DetachEndOnUse".into(), "0".into()),
            ("DetachEndOnSpawn".into(), "0".into()),
            ("useAutoDetachSpeed".into(), "0".into()),
            ("ziprailMountReverseDistance".into(), "200".into()),
            ("link_guid".into(), guids[0].clone()),
        ];
        if rail {
            p.push(("isZiprailStart".into(), "1".into()));
            p.push(("tangent_type".into(), "1".into()));
            p.push(("ziplineMountReverseDistance".into(), "0".into()));
        } else {
            p.push(("isZiprailStart".into(), "0".into()));
        }
        for (i, point) in world.iter().enumerate() {
            p.push((format!("_zipline_rest_point_{i}"), vec3_text(*point)));
        }
        if let Some(g) = end {
            p.push(("link_to_guid_0".into(), g.clone()));
        }
        for (k, v) in &path.keyvalues {
            push(&mut p, k, v.clone());
        }
        p
    };
    out.push(block(head_pairs(Some(&guids[1]))));
    let mut tail: Vec<(String, String)> = vec![
        ("classname".into(), "zipline_end".into()),
        ("origin".into(), vec3_text(*world.last().unwrap())),
        ("link_guid".into(), guids[1].clone()),
        ("ZiplineAutoDetachDistance".into(), num((auto * 0.5).max(1.0))),
        ("ZiplinePushOffInDirectionX".into(), "0".into()),
    ];
    for (k, v) in &path.keyvalues {
        push(&mut tail, k, v.clone());
    }
    out.push(block(tail));
    out
}

/// Source angles for a path segment: a zipline hangs from its high end, so the
/// entity points along the segment with pitch taken from the elevation.
pub fn direction_angles(d: DVec3) -> DVec3 {
    let flat = (d.x * d.x + d.y * d.y).sqrt();
    let yaw = d.y.atan2(d.x).to_degrees();
    let pitch = (-d.z).atan2(flat).to_degrees();
    DVec3::new(pitch, yaw, 0.0)
}

/// The entities a document compiles to, grouped by the file they belong in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Built {
    pub main: Vec<Block>,
    /// Partition blocks, in `Partition::FILES` order.
    pub partitions: [Vec<Block>; 5],
    /// Partition names the map declares, in file order.
    pub names: Vec<String>,
}

impl Built {
    pub fn partition(&self, p: Partition) -> &[Block] {
        match p {
            Partition::Main => &self.main,
            Partition::Env => &self.partitions[0],
            Partition::Fx => &self.partitions[1],
            Partition::Script => &self.partitions[2],
            Partition::Snd => &self.partitions[3],
            Partition::Spawn => &self.partitions[4],
        }
    }

    /// Partition files that hold at least one block.
    pub fn present(&self) -> Vec<Partition> {
        Partition::FILES.into_iter().filter(|p| !self.partition(*p).is_empty()).collect()
    }
}

fn slot(p: Partition) -> usize {
    match p {
        Partition::Main => 0,
        Partition::Env => 0,
        Partition::Fx => 1,
        Partition::Script => 2,
        Partition::Snd => 3,
        Partition::Spawn => 4,
    }
}

/// Everything the entity stage needs from the rest of the compiler.
pub struct Context<'a> {
    pub doc: &'a MapDoc,
    pub schema: &'a EntitySchema,
    pub model_of: &'a HashMap<Id, u32>,
    pub worlds: &'a crate::csg::WorldMap,
    pub models: &'a [ModelSlot],
    pub surfaces: &'a [RenderSurface],
    /// Which `*coll` encoding the target reads.
    pub target: CollTarget,
    /// Prop instances, for the static prop lightmap bounds the report needs.
    pub world_bounds: (Vec3, Vec3),
}

/// The worldspawn block every map opens with.
pub fn worldspawn(bounds: (Vec3, Vec3), settings: &rdoc::model::Settings) -> Block {
    let mut p: Vec<(String, String)> = Vec::new();
    p.push(("classname".into(), "worldspawn".into()));
    p.push(("name".into(), "world".into()));
    p.push(("spawnclass".into(), SPAWNCLASS.into()));
    p.push((
        "world_mins".into(),
        format!("{} {} {}", num(bounds.0.x as f64), num(bounds.0.y as f64), num(bounds.0.z as f64)),
    ));
    p.push((
        "world_maxs".into(),
        format!("{} {} {}", num(bounds.1.x as f64), num(bounds.1.y as f64), num(bounds.1.z as f64)),
    ));
    p.push(("lightprobe_voronoi_leafsize".into(), VORONOI_LEAFSIZE.into()));
    p.push(("musicpostfix".into(), MUSIC_POSTFIX.into()));
    p.push(("mapversion".into(), MAP_VERSION.into()));
    p.push((
        "cullLightmappedStaticProps".into(),
        "0".to_string(),
    ));
    p.push((
        "luxelscale".into(),
        num(settings.bake.texel_size.max(1.0)),
    ));
    block(p)
}

fn partition_of(def: Option<Partition>, classname: &str, schema: &EntitySchema) -> Partition {
    def.or_else(|| schema.home_partition(classname)).unwrap_or(Partition::Main)
}

/// The `*collN` keys a plane-defined solid compiles to, or an empty vector when
/// the solid has fewer than four planes and no hull can be built. Each brush is
/// paired with the object whose transform places it in the world.
pub fn coll_keys(
    brushes: &[(Id, &Brush)],
    worlds: &crate::csg::WorldMap,
    classname: &str,
    target: CollTarget,
) -> Result<Vec<String>, EntityError> {
    let mut all: Vec<Vec<DPlane>> = Vec::with_capacity(brushes.len());
    for (owner, b) in brushes {
        let world = worlds.get(owner).copied().unwrap_or(glam::DMat4::IDENTITY);
        let (scale, rot, offset) = world.to_scale_rotation_translation();
        let lin = glam::DMat3::from_diagonal(scale) * glam::DMat3::from_quat(rot);
        all.push(
            b.faces
                .iter()
                .map(|f| {
                    // n.p = d becomes n'.p' = d' with n' = normalize(M^-T n) and
                    // d' = (d + (M^-T n).t) / |M^-T n|.
                    let raw: DVec3 = lin.transpose().inverse() * f.normal;
                    let len = raw.length();
                    if len <= f64::EPSILON {
                        return DPlane::new(f.normal, f.dist);
                    }
                    let n = raw / len;
                    DPlane::new(n, (f.dist + raw.dot(offset)) / len)
                })
                .collect(),
        );
    }
    if all.is_empty() {
        return Ok(Vec::new());
    }
    let (contents, flags, name) = volume_surface(classname);
    let surface = (contents, flags, name.as_bytes());
    let model = collision::volume_blob(&all, surface, target).map_err(|e| EntityError::Invalid {
        classname: classname.into(),
        object: brushes[0].0,
        message: e.to_string(),
    })?;
    Ok(model.to_keys())
}

/// The `*collN` blob chunks a solid compiles to, as entity key/value pairs.
pub fn coll_pairs(keys: &[String]) -> Vec<(String, String)> {
    keys.iter()
        .filter_map(|k| {
            let (name, rest) = k.split_once(' ')?;
            Some((name.to_string(), rest.trim().trim_matches('"').to_string()))
        })
        .collect()
}

/// The class an authored `Entity` object compiles as.
pub fn entity_classname(def: &EntityDef) -> &str {
    def.classname.as_str()
}

/// Emits every entity of a document, in document order, into the partition its
/// class belongs to.
pub fn build(ctx: &Context<'_>) -> Result<Built, EntityError> {
    let mut world = worldspawn(ctx.world_bounds, &ctx.doc.settings);
    // A worldspawn object (a decompiled map has one) is the map's own block:
    // its keys are written as they are, and only a world box it lacks comes
    // from the world's bounds.
    let own = ctx.doc.objects.iter().find_map(|o| match &o.kind {
        ObjectKind::Entity(e) if e.classname == "worldspawn" => Some(e),
        _ => None,
    });
    if let Some(e) = own {
        let mut pairs: Vec<(String, String)> = vec![("classname".into(), "worldspawn".into())];
        for (k, v) in &e.keyvalues {
            if k == "classname" || k == "origin" || k == "angles" {
                continue;
            }
            pairs.push((k.clone(), v.clone()));
        }
        for key in ["world_mins", "world_maxs"] {
            if !pairs.iter().any(|(k, _)| k == key) {
                if let Some(v) = world.get(key) {
                    pairs.push((key.into(), v.to_string()));
                }
            }
        }
        world = block(pairs);
    }
    let mut out = Built { main: vec![world], ..Built::default() };
    let children: HashMap<Id, Vec<&Object>> = {
        let mut m: HashMap<Id, Vec<&Object>> = HashMap::new();
        for o in &ctx.doc.objects {
            if let Some(p) = o.parent {
                m.entry(p).or_default().push(o);
            }
        }
        m
    };
    // Target names are resolved after every object has been seen, so a link may
    // point forward.
    let mut targetnames: HashMap<Id, String> = HashMap::new();
    for o in &ctx.doc.objects {
        if let ObjectKind::Entity(e) = &o.kind {
            let own = e.keyvalues.get("targetname").or_else(|| e.keyvalues.get("script_name"));
            let name = own.cloned().unwrap_or_else(|| default_targetname(o));
            targetnames.insert(o.id, name);
        }
    }

    let mut deferred: Vec<(Object, Vec<(String, String)>)> = Vec::new();
    for o in &ctx.doc.objects {
        match &o.kind {
            ObjectKind::Entity(e) => {
                if e.classname == "worldspawn" {
                    continue;
                }
                let mut pairs: Vec<(String, String)> = Vec::new();
                pairs.push(("classname".into(), e.classname.clone()));
                pairs.push(("origin".into(), vec3_text(o.transform.position)));
                let angles = o.transform.to_source_angles();
                if angles.length() > 0.0 {
                    pairs.push(("angles".into(), angles_text(angles)));
                }
                // A model the compiler builds for this entity replaces the one
                // it names; an entity with no model of its own here (a
                // decompiled brush entity) keeps its `model` and `*coll` keys,
                // which name a slot and carry the collision themselves.
                let own_model = ctx.model_of.contains_key(&o.id);
                for (k, v) in &e.keyvalues {
                    if k == "origin" || k == "angles" || (own_model && (k == "model" || k.starts_with("*coll"))) {
                        continue;
                    }
                    insert(&mut pairs, k, v.clone());
                }
                for (key, target) in &e.links {
                    if let Some(name) = targetnames.get(target) {
                        insert(&mut pairs, key, name.clone());
                    }
                }
                if let Some(&model) = ctx.model_of.get(&o.id) {
                    pairs.push(("model".into(), format!("*{model}")));
                    if let Some(kids) = children.get(&o.id) {
                        let brushes: Vec<(Id, &Brush)> = kids
                            .iter()
                            .filter_map(|c| match &c.kind {
                                ObjectKind::Brush(b) => Some((c.id, b)),
                                _ => None,
                            })
                            .collect();
                        if !brushes.is_empty() {
                            for (name, value) in
                                coll_pairs(&coll_keys(&brushes, ctx.worlds, &e.classname, ctx.target)?)
                            {
                                pairs.push((name, value));
                            }
                        }
                    }
                }
                // A block that says it lives in the main lump (a decompiled
                // map's 550 do) goes there; one that leaves it to its class
                // goes to the class's file.
                if e.partition == Some(Partition::Main) {
                    out.main.push(block(pairs));
                } else {
                    let part = partition_of(e.partition, &e.classname, ctx.schema);
                    push_block(&mut out, part, block(pairs))?;
                }
            }
            ObjectKind::Volume(v) => {
                let pairs = volume_pairs(o, v, ctx)?;
                let part = partition_of(None, &v.classname, ctx.schema);
                push_block(&mut out, part, block(pairs))?;
            }
            ObjectKind::Light(l) if l.keyvalues.contains_key("_light") => {
                // A light carrying its shipped text (a decompiled one no one
                // has edited) is written as it shipped: class and keys as they
                // are, placement from its transform.
                let classname = l.keyvalues.get("classname").map(String::as_str).unwrap_or(light_class(l.kind)).to_string();
                let mut pairs: Vec<(String, String)> = vec![("classname".into(), classname.clone())];
                // The shipped triples stay while the transform is the one they
                // describe: angles near straight up or down have more than one
                // triple for the same turn.
                let shipped = |k: &str| l.keyvalues.get(k).and_then(|t| crate::decompile::entity_vec3(t).map(|v| (t, v)));
                let origin = match shipped("origin") {
                    Some((t, v)) if v.distance(o.transform.position) < 1e-3 => t.clone(),
                    _ => vec3_text(o.transform.position),
                };
                pairs.push(("origin".into(), origin));
                match shipped("angles") {
                    Some((t, v))
                        if rdoc::model::Transform::from_source_angles(DVec3::ZERO, v).rotation.dot(o.transform.rotation).abs()
                            > 1.0 - 1e-9 =>
                    {
                        pairs.push(("angles".into(), t.clone()))
                    }
                    _ => {
                        let angles = o.transform.to_source_angles();
                        if angles.length() > 0.0 || l.kind == LightKind::Spot || l.kind == LightKind::Sun {
                            pairs.push(("angles".into(), angles_text(angles)));
                        }
                    }
                }
                for (k, v) in &l.keyvalues {
                    if k == "classname" || k == "origin" || k == "angles" {
                        continue;
                    }
                    pairs.push((k.clone(), v.clone()));
                }
                let part = partition_of(None, &classname, ctx.schema);
                push_block(&mut out, part, block(pairs))?;
            }
            ObjectKind::Light(l) => {
                let classname = light_class(l.kind);
                let mut pairs = light_defaults(classname, l);
                pairs.push(("origin".into(), vec3_text(o.transform.position)));
                let angles = o.transform.to_source_angles();
                if l.kind == LightKind::Sun || l.kind == LightKind::Area || l.kind == LightKind::Spot {
                    // A light shines along its local -Z; the entity's angles
                    // point that way and `pitch` repeats it negated, as the
                    // shipped light_environment does.
                    let forward = o.transform.rotation * DVec3::NEG_Z;
                    let aim = direction_angles(forward);
                    pairs.push(("angles".into(), angles_text(aim)));
                    pairs.push(("pitch".into(), num(-aim.x)));
                    if classname == "light_environment" {
                        let b = &ctx.doc.settings.bake;
                        let sky = DVec3::new(b.sky_color[0] as f64, b.sky_color[1] as f64, b.sky_color[2] as f64) * b.sky_intensity as f64;
                        push(&mut pairs, "_ambient", source_light(sky));
                    }
                } else if l.kind == LightKind::Sky {
                    pairs.push(("angles".into(), angles_text(angles)));
                }
                for (k, v) in &l.keyvalues {
                    // Placement comes from the transform.
                    if k != "origin" && k != "angles" {
                        push(&mut pairs, k, v.clone());
                    }
                }
                let part = partition_of(None, classname, ctx.schema);
                push_block(&mut out, part, block(pairs))?;
            }
            ObjectKind::Path(p) => {
                let world: Vec<DVec3> = p
                    .points
                    .iter()
                    .map(|pt| {
                        let m = ctx.worlds.get(&o.id).copied().unwrap_or(glam::DMat4::IDENTITY);
                        m.transform_point3(*pt)
                    })
                    .collect();
                if world.len() < 2 {
                    continue;
                }
                for b in zipline_chain(p.kind, p, &world) {
                    let part = partition_of(None, b.classname().unwrap_or("zipline"), ctx.schema);
                    push_block(&mut out, part, b)?;
                }
            }
            ObjectKind::Prop(p) if !p.static_prop => {
                let classname =
                    p.keyvalues.get("classname").cloned().unwrap_or_else(|| "prop_dynamic".to_string());
                let mut pairs: Vec<(String, String)> = vec![
                    ("classname".into(), classname.clone()),
                    ("model".into(), p.model.clone()),
                    ("origin".into(), vec3_text(o.transform.position)),
                    ("angles".into(), angles_text(o.transform.to_source_angles())),
                    ("scale".into(), num(o.transform.scale.x)),
                    ("skin".into(), p.skin.to_string()),
                    ("solid".into(), solid_flags(p.solid).to_string()),
                ];
                for (k, v) in &p.keyvalues {
                    if k == "classname" || k == "model" || k == "origin" || k == "angles" {
                        continue;
                    }
                    insert(&mut pairs, k, v.clone());
                }
                let part = partition_of(None, &classname, ctx.schema);
                push_block(&mut out, part, block(pairs))?;
            }
            _ => {}
        }
    }
    for (o, pairs) in deferred.drain(..) {
        let part = partition_of(None, pairs_class(&pairs), ctx.schema);
        push_block(&mut out, part, block(pairs))?;
        let _ = o;
    }
    out.names = out.present().iter().map(|p| p.name().to_string()).collect();
    Ok(out)
}

fn pairs_class(pairs: &[(String, String)]) -> &str {
    pairs
        .iter()
        .find(|(k, _)| k == "classname")
        .map(|(_, v)| v.as_str())
        .unwrap_or("worldspawn")
}

fn solid_flags(solid: bool) -> u32 {
    if solid {
        2
    } else {
        0
    }
}

fn default_targetname(o: &Object) -> String {
    format!("{}_{}", slug(&o.name), short_id(o.id))
}

fn short_id(id: Uuid) -> String {
    let b = id.as_bytes();
    format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3], b[4], b[5])
}

/// A filesystem- and key-safe fragment of an object name.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "entity".to_string()
    } else {
        trimmed
    }
}

fn push_block(out: &mut Built, part: Partition, b: Block) -> Result<(), EntityError> {
    let i = slot(part);
    out.partitions[i].push(b);
    Ok(())
}

fn volume_pairs(o: &Object, v: &Volume, ctx: &Context<'_>) -> Result<Vec<(String, String)>, EntityError> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    pairs.push(("classname".into(), v.classname.clone()));
    pairs.push(("origin".into(), vec3_text(o.transform.position)));
    for (k, value) in &v.keyvalues {
        if k == "classname" || k == "origin" {
            continue;
        }
        insert(&mut pairs, k, value.clone());
    }
    if let Some(&model) = ctx.model_of.get(&o.id) {
        let brushes: Vec<(Id, &Brush)> = v.brushes.iter().map(|b| (o.id, b)).collect();
        let keys = coll_keys(&brushes, ctx.worlds, &v.classname, ctx.target)?;
        if let Some(model) = ctx.models.get(model as usize) {
            push(&mut pairs, "mins", vec3_text(model.mins.as_dvec3()));
            push(&mut pairs, "maxs", vec3_text(model.maxs.as_dvec3()));
        }
        pairs.push(("model".into(), format!("*{model}")));
        pairs.extend(coll_pairs(&keys));
    }
    Ok(pairs)
}

/// The class count a built entity set reports, in classname order.
pub fn class_counts(built: &Built) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for b in built.main.iter().chain(built.partitions.iter().flatten()) {
        if let Some(cn) = b.classname() {
            *out.entry(cn.to_string()).or_insert(0) += 1;
        }
    }
    out
}

/// The highest model slot a `model "*N"` key of any emitted block names.
pub fn max_model_key(built: &Built) -> Option<u32> {
    built
        .main
        .iter()
        .chain(built.partitions.iter().flatten())
        .filter_map(|b| b.get("model")?.strip_prefix('*')?.parse::<u32>().ok())
        .max()
}

/// The highest `*collN` chunk number any emitted block uses.
pub fn max_coll_chunk(built: &Built) -> usize {
    let mut out = 0usize;
    let mut note = |block: &Block| {
        for (k, _) in &block.pairs {
            if let Some(rest) = k.strip_prefix("*coll") {
                if let Ok(n) = rest.parse::<usize>() {
                    out = out.max(n);
                }
            }
        }
    };
    for b in &built.main {
        note(b);
    }
    for p in &built.partitions {
        for b in p {
            note(b);
        }
    }
    out
}

/// The main lump's text, the way the map stores it. The trailing `\n\0` is
/// inside the lump: the engine reads the lump's own last byte as the end of the
/// entity text and walks one step past it without it.
pub fn main_lump_text(built: &Built) -> String {
    rbsp::lumps::entities::EntityText {
        header: None,
        entities: built.main.clone(),
        tail: rbsp::lumps::entities::AFTER_LAST_BLOCK.to_vec(),
    }
    .to_text()
}

/// A partition file's bytes: the `ENTITIES02 num_models=N` header the engine
/// requires, one block per line, NUL terminated.
pub fn partition_bytes(built: &Built, part: Partition, num_models: u32) -> Vec<u8> {
    let blocks = built.partition(part);
    let text = rbsp::lumps::entities::EntityText {
        header: Some(rbsp::lumps::entities::TextHeader { ident: "ENTITIES02".into(), num_models }),
        entities: blocks.to_vec(),
        tail: rbsp::lumps::entities::AFTER_LAST_BLOCK.to_vec(),
    };
    text.to_text().into_bytes()
}

/// Parses an emitted partition back, for the round-trip tests.
pub fn parse_partition(bytes: &[u8]) -> Result<rbsp::lumps::entities::EntityText, EntityError> {
    rbsp::lumps::entities::parse(bytes, rbsp::Version::ClientV51)
        .map_err(|e| EntityError::Lump { id: rbsp::ids::ENTITIES, message: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdoc::model::{BrushFace, Transform, UvMapping, VolumeKind};

    fn box_brush(mins: DVec3, maxs: DVec3) -> Brush {
        let faces = [
            (DVec3::X, maxs.x),
            (-DVec3::X, -mins.x),
            (DVec3::Y, maxs.y),
            (-DVec3::Y, -mins.y),
            (DVec3::Z, maxs.z),
            (-DVec3::Z, -mins.z),
        ];
        Brush {
            faces: faces
                .into_iter()
                .map(|(normal, dist)| BrushFace {
                    normal,
                    dist,
                    material: "world/dev/dev_gray_512".into(),
                    uv: UvMapping::default(),
                    lightmap_scale: 0.0,
                })
                .collect(),
            contents: 1,
            detail: false,
        }
    }

    fn volume(classname: &str) -> Volume {
        Volume {
            kind: VolumeKind::Trigger,
            classname: classname.into(),
            brushes: vec![box_brush(DVec3::new(-64.0, -64.0, 0.0), DVec3::new(64.0, 64.0, 128.0))],
            keyvalues: Default::default(),
        }
    }

    fn doc_with(mut objects: Vec<Object>) -> MapDoc {
        let mut doc = MapDoc::default();
        let layer = doc.layers[0].id;
        for o in &mut objects {
            o.layer = layer;
            o.transform = Transform::default();
        }
        doc.objects = objects;
        doc
    }

    fn object(kind: ObjectKind) -> Object {
        Object {
            id: Uuid::new_v4(),
            name: "test".into(),
            layer: Uuid::nil(),
            parent: None,
            transform: Transform::default(),
            hidden: false,
            locked: false,
            kind,
        }
    }

    fn schema() -> EntitySchema {
        crate::default_schema().clone()
    }

    #[test]
    fn volume_surface_masks_match_the_fixtures() {
        assert_eq!(volume_surface("trigger_hurt").0, TRIGGER_CONTENTS);
        assert_eq!(volume_surface("trigger_hurt").1, TRIGGER_FLAGS);
        assert_eq!(volume_surface("trigger_hurt").2, "TOOLS\\TOOLSTRIGGER");
        assert_eq!(volume_surface("envmap_volume").0, ENVMAP_CONTENTS);
        assert_eq!(volume_surface("envmap_volume").1, ENVMAP_FLAGS);
        assert_eq!(volume_surface("envmap_volume").2, "TOOLS\\TOOLSENVMAPVOLUME");
        assert_eq!(volume_surface("trigger_slip").2, "TOOLS\\TOOLSTRIGGER_SLIP");
        assert_eq!(volume_surface("fog_volume").2, "TOOLS\\TOOLSTRIGGER");
    }

    #[test]
    fn coll_keys_round_trip_the_box() {
        let b = box_brush(DVec3::new(-64.0, -64.0, 0.0), DVec3::new(64.0, 64.0, 128.0));
        let worlds = HashMap::new();
        for target in [CollTarget::Client, CollTarget::Dedi] {
            let keys = coll_keys(&[(Uuid::nil(), &b)], &worlds, "trigger_hurt", target)
                .expect("the box compiles to a collider");
            assert!(!keys.is_empty(), "a box produces at least one chunk");
            let mut blob = Vec::new();
            let re: Vec<String> = keys
                .iter()
                .map(|k| {
                    let v = k.split_whitespace().nth(1).unwrap_or("").trim_matches('"').to_string();
                    blob.extend_from_slice(&collision::base64_decode(&v).expect("the chunk decodes"));
                    v
                })
                .collect();
            let _ = re;
            let model = collision::parse_coll_blob(&blob, target).expect("the blob parses");
            assert_eq!(model.target, target);
            assert_eq!(model.contents, vec![TRIGGER_CONTENTS]);
            assert!(!model.nodes.is_empty());
        }
    }

    #[test]
    fn worldspawn_carries_the_fixture_key_set() {
        let block = worldspawn((Vec3::splat(-1024.0), Vec3::splat(1024.0)), &rdoc::model::Settings::default());
        for key in [
            "classname",
            "name",
            "spawnclass",
            "world_mins",
            "world_maxs",
            "lightprobe_voronoi_leafsize",
            "musicpostfix",
            "mapversion",
            "cullLightmappedStaticProps",
        ] {
            assert!(block.get(key).is_some(), "worldspawn carries {key}");
        }
        assert_eq!(block.get("world_mins"), Some("-1024 -1024 -1024"));
        assert_eq!(block.get("world_maxs"), Some("1024 1024 1024"));
    }

    #[test]
    fn lights_emit_the_classes_the_shipped_maps_use() {
        for (kind, class) in [
            (LightKind::Point, "light"),
            (LightKind::Spot, "light_spot"),
            (LightKind::Sun, "light_environment"),
            (LightKind::Area, "light_environment"),
            (LightKind::Sky, "sky_camera"),
        ] {
            let l = Light {
                kind,
                color: [1.0, 0.5, 0.25],
                intensity: 100.0,
                range: 300.0,
                inner_angle: 30.0,
                outer_angle: 60.0,
                size: [0.0; 2],
                cast_shadows: true,
                baked: true,
                realtime: false,
                keyvalues: Default::default(),
            };
            let mut pairs = light_defaults(light_class(kind), &l);
            pairs.push(("origin".into(), "0 0 0".into()));
            let b = block(pairs);
            assert_eq!(b.classname(), Some(class));
        }
    }

    #[test]
    fn zipline_chain_links_every_endpoint() {
        let path = PathObject { kind: PathKind::Zipline, points: vec![], keyvalues: Default::default() };
        let world = vec![DVec3::new(0.0, 0.0, 128.0), DVec3::new(0.0, 512.0, 64.0)];
        let chain = zipline_chain(PathKind::Zipline, &path, &world);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].classname(), Some("zipline"));
        assert_eq!(chain[1].classname(), Some("zipline_end"));
        let link = chain[0].get("link_to_guid_0").expect("the head links onward").to_string();
        assert_eq!(Some(link.as_str()), chain[1].get("link_guid"));
        assert_eq!(chain[0].get("_zipline_rest_point_0"), Some("0 0 128"));
        assert_eq!(chain[0].get("_zipline_rest_point_1"), Some("0 512 64"));
    }

    #[test]
    fn volume_object_emits_model_and_coll() {
        let doc = doc_with(vec![object(ObjectKind::Volume(volume("trigger_hurt")))]);
        let schema = schema();
        let models = crate::csg::model_assignment(&doc).1;
        let model_of = crate::csg::model_assignment(&doc).0;
        let worlds = HashMap::new();
        let ctx = Context {
            doc: &doc,
            schema: &schema,
            model_of: &model_of,
            worlds: &worlds,
            models: &models,
            surfaces: &[],
            target: CollTarget::Client,
            world_bounds: (Vec3::ZERO, Vec3::ZERO),
        };
        let built = build(&ctx).expect("the document compiles");
        assert_eq!(built.main[0].classname(), Some("worldspawn"));
        assert_eq!(built.main.len(), 1, "the volume does not stay in the worldspawn lump");
        assert_eq!(built.partition(Partition::Script).len(), 1);
        let text = partition_bytes(&built, Partition::Script, 2);
        let parsed = parse_partition(&text).expect("the partition file parses");
        assert_eq!(parsed.entities[0].classname(), Some("trigger_hurt"));
        assert_eq!(parsed.entities[0].get("model"), Some("*1"));
        assert!(parsed.entities[0].get("*coll0").is_some(), "the volume carries a collider");
        assert_eq!(parsed.header.expect("a partition header").num_models, 2);
        assert!(
            rbsp::lumps::entities::terminator_is_intact(&text),
            "a partition one byte short of the terminator walks the engine parser off the buffer"
        );
        let main = main_lump_text(&built).into_bytes();
        assert!(
            rbsp::lumps::entities::terminator_is_intact(&main),
            "the main lump needs the same terminator: {:?}",
            &main[main.len().saturating_sub(4)..]
        );
    }
}
