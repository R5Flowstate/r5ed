//! The skybox: a shell of sky faces around the world, and the camera and vista
//! the engine draws through them.
//!
//! The layout follows the shipped maps: one sky-material shell encloses the
//! whole map, the vista stands far off the playable space and below it, turned
//! a quarter, and the `sky_camera` sits 152 units above the vista's origin.

use glam::{DQuat, DVec3, Vec2, Vec3, Vec4};
use rdoc::model::{KeyValues, Light, LightKind, MapDoc, ObjectKind, Transform};
use rir::{AssetTables, RenderSurface};

use crate::meshes::SKY_MATERIAL;
use crate::props::PropSource;

/// The sky camera's height above the vista's origin.
pub const CAMERA_ABOVE_VISTA: f64 = 152.0;
/// The sky camera's fog keys are staging_mu1's height fog, whose band lies
/// 5044 to 4044 units below that map's camera; the band keeps that distance
/// below ours, so the vista stays out of the thick fog wherever the sky sits.
pub const FOG_BAND_BELOW_CAMERA: (f64, f64) = (5044.0, 4044.0);
/// Where the vista stands relative to the middle of the world: habitat's
/// vista relative to the middle of its spawns, far off the playable space and
/// below it. The shipped maps turn the vista a quarter about z.
pub const VISTA_FROM_WORLD: DVec3 = DVec3::new(-38434.0, -29834.0, -3580.0);
pub const VISTA_YAW: f32 = 90.0;
/// Space between the world and the vista, and between both and the shell.
pub const MARGIN: f64 = 1024.0;
/// A fixed id for the camera object, so a compile stays deterministic.
const CAMERA_ID: u128 = 0x736b_795f_6361_6d65_7261_0000_0000_0001;

#[derive(Debug, thiserror::Error)]
pub enum SkyError {
    #[error("the sky vista {0:?} is not in the asset table, so it cannot be placed")]
    UnknownModel(String),
}

/// What the sky adds to a compile.
pub struct Sky {
    /// The shell, one world-model surface of sky faces.
    pub shell: RenderSurface,
    /// The vista prop, flagged to draw only in the skybox.
    pub vista: PropSource,
    /// Where the sky camera sits.
    pub camera: DVec3,
}

/// Places the sky around a world whose render surfaces and props are known.
pub fn build(
    model: &str,
    surfaces: &[RenderSurface],
    props: &[PropSource],
    extra: &[DVec3],
    tables: &AssetTables,
) -> Result<Sky, SkyError> {
    let info = tables.models.get(model).ok_or_else(|| SkyError::UnknownModel(model.to_string()))?;
    let (vmin, vmax) = (info.bounds_min.as_dvec3(), info.bounds_max.as_dvec3());

    let (lo, hi) = world_bounds(surfaces, props);
    let centre = (lo + hi) * 0.5;
    let reach = (vmax.x - vmin.x).max(vmax.y - vmin.y);
    let origin = vista_origin(centre, lo, reach);
    let camera = origin + DVec3::Z * CAMERA_ABOVE_VISTA;

    let vista = PropSource {
        model: model.to_string(),
        origin: origin.as_vec3(),
        angles: Vec3::new(0.0, VISTA_YAW, 0.0),
        scale: 1.0,
        skin: 0,
        solid: false,
        cast_shadows: false,
        fade_distance: 0.0,
        bounds_min: info.bounds_min,
        bounds_max: info.bounds_max,
        sky: true,
        keys: Default::default(),
    };
    let (vlo, vhi) = vista.world_bounds();
    Ok(Sky { shell: shell_for((lo, hi), extra, (vlo.as_dvec3(), vhi.as_dvec3()), camera), vista, camera })
}

/// The world box the vista origin is measured from: every render surface but
/// the sky shell, and every prop.
pub fn world_bounds(surfaces: &[RenderSurface], props: &[PropSource]) -> (DVec3, DVec3) {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for s in surfaces.iter().filter(|s| !crate::meshes::is_sky_material(&s.material)) {
        for p in &s.positions {
            lo = lo.min(p.as_dvec3());
            hi = hi.max(p.as_dvec3());
        }
    }
    for p in props {
        let (a, b) = p.world_bounds();
        lo = lo.min(a.as_dvec3());
        hi = hi.max(b.as_dvec3());
    }
    if lo.x > hi.x {
        lo = DVec3::ZERO;
        hi = DVec3::ZERO;
    }
    (lo, hi)
}

/// Where the vista stands relative to the middle of the world. A world wider
/// than the offset still keeps the vista clear of its -x side.
pub fn vista_origin(centre: DVec3, lo: DVec3, reach: f64) -> DVec3 {
    let x = (centre.x + VISTA_FROM_WORLD.x).min(lo.x - MARGIN - reach);
    DVec3::new(x, centre.y + VISTA_FROM_WORLD.y, centre.z + VISTA_FROM_WORLD.z)
}

/// The shell around the world, the extra points and the sky bounds, with the
/// camera inside it.
pub fn shell_for(world: (DVec3, DVec3), extra: &[DVec3], sky: (DVec3, DVec3), camera: DVec3) -> RenderSurface {
    let (lo, hi) = world;
    let (elo, ehi) = extra.iter().fold((lo, hi), |(a, b), p| (a.min(*p), b.max(*p)));
    let shell_lo = elo.min(sky.0).min(camera) - DVec3::splat(MARGIN);
    let shell_hi = ehi.max(sky.1).max(camera) + DVec3::splat(MARGIN);
    shell(shell_lo, shell_hi)
}

/// The contents shipped maps give their sky faces: the world mask plus 0x100000.
pub const SKY_CONTENTS: u32 = 0x00FB_1280;

/// The shell as collision. The client shows the sun only while a trace toward
/// it ends on a sky-flagged face, so the shell has to be solid, not just drawn.
pub fn shell_solid(shell: &RenderSurface) -> rir::CollisionSolid {
    rir::CollisionSolid {
        object: None,
        model: 0,
        shape: rir::CollisionShape::Triangles { vertices: shell.positions.clone(), indices: shell.indices.clone() },
        contents: SKY_CONTENTS,
        surface_prop: 0,
        surfaces: vec![rir::CollisionSurface { material: SKY_MATERIAL.to_string(), surface_type: 0 }],
    }
}

/// Six inward faces of a box, wound as the engine reads a front face: the
/// triangle's cross product opposes the normal.
fn shell(lo: DVec3, hi: DVec3) -> RenderSurface {
    let c = |x: bool, y: bool, z: bool| {
        Vec3::new(if x { hi.x } else { lo.x } as f32, if y { hi.y } else { lo.y } as f32, if z { hi.z } else { lo.z } as f32)
    };
    let faces: [(Vec3, [Vec3; 4]); 6] = [
        (Vec3::X, [c(false, false, false), c(false, true, false), c(false, true, true), c(false, false, true)]),
        (Vec3::NEG_X, [c(true, false, false), c(true, true, false), c(true, true, true), c(true, false, true)]),
        (Vec3::Y, [c(false, false, false), c(true, false, false), c(true, false, true), c(false, false, true)]),
        (Vec3::NEG_Y, [c(false, true, false), c(true, true, false), c(true, true, true), c(false, true, true)]),
        (Vec3::Z, [c(false, false, false), c(true, false, false), c(true, true, false), c(false, true, false)]),
        (Vec3::NEG_Z, [c(false, false, true), c(true, false, true), c(true, true, true), c(false, true, true)]),
    ];
    let mut s = RenderSurface {
        model: 0,
        material: SKY_MATERIAL.to_string(),
        cast_shadows: false,
        lightmap_texel: crate::csg::DEFAULT_LIGHTMAP_SCALE as f32,
        ..RenderSurface::default()
    };
    for (n, mut q) in faces {
        if rbake::light::front_normal(q[0], q[1], q[2]).dot(n) < 0.0 {
            q.reverse();
        }
        let base = s.positions.len() as u32;
        for p in q {
            s.positions.push(p);
            s.normals.push(n);
            s.tangents.push(Vec4::W);
            s.uv0.push(Vec2::ZERO);
            s.uv1.push(Vec2::ZERO);
            s.colors.push([255; 4]);
        }
        s.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    s
}

/// A Source 2D skybox (`worldspawn skyname`) as the S21 client draws it: six
/// `world/source/skybox/<name><face>` materials on an inward-facing cube
/// around the vista, which the sky camera sees and the world never collides
/// with. The face names are what the player sees looking that way from inside:
/// front is +X, right is -Y, and every side face stands texture-up on +Z.
pub const SOURCE_SKY_PREFIX: &str = "world/source/skybox/";
/// Half the edge of the cube around the vista origin. It holds the sky camera
/// 152 units above the origin with room to spare, and stays clear of the world
/// the vista offset keeps it away from.
pub const CUBE_HALF: f64 = 4096.0;
/// Suffix, inward normal, texture-right and texture-up, in world axes.
pub const SOURCE_SKY_FACES: [(&str, Vec3, Vec3, Vec3); 6] = [
    ("bk", Vec3::NEG_X, Vec3::Y, Vec3::Z),
    ("dn", Vec3::NEG_Z, Vec3::NEG_Y, Vec3::X),
    ("ft", Vec3::X, Vec3::NEG_Y, Vec3::Z),
    ("lf", Vec3::Y, Vec3::X, Vec3::Z),
    ("rt", Vec3::NEG_Y, Vec3::NEG_X, Vec3::Z),
    ("up", Vec3::Z, Vec3::NEG_Y, Vec3::NEG_X),
];

/// The material a skybox face draws with, the name `scripts/source/skybox.py`
/// builds it under.
pub fn face_material(sky: &str, face: &str) -> String {
    format!("{SOURCE_SKY_PREFIX}{}{face}", sky.trim().to_ascii_lowercase())
}

/// The six face materials of a skybox, in [`SOURCE_SKY_FACES`] order.
pub fn face_materials(sky: &str) -> [String; 6] {
    SOURCE_SKY_FACES.map(|(face, _, _, _)| face_material(sky, face))
}

/// Whether a material is one face of a Source skybox cube.
pub fn is_source_sky_material(material: &str) -> bool {
    material.starts_with(SOURCE_SKY_PREFIX)
}

/// What the sky adds to a compile on top of the shell, camera and vista.
pub struct SourceSky {
    /// One inward-facing surface per face, in [`SOURCE_SKY_FACES`] order.
    pub surfaces: Vec<RenderSurface>,
    /// Where the sky camera sits: above the cube's centre, inside it.
    pub camera: DVec3,
    /// The cube's corners, for the shell and the skybox cell.
    pub bounds: (Vec3, Vec3),
}

/// The skybox cube at an origin, or None when a face material is missing from
/// the asset table. The caller reports that and compiles on without a sky.
pub fn build_source(sky: &str, origin: DVec3, tables: &AssetTables) -> Option<SourceSky> {
    let materials = face_materials(sky);
    if materials.iter().any(|m| !tables.materials.contains_key(m)) {
        return None;
    }
    let camera = origin + DVec3::Z * CAMERA_ABOVE_VISTA;
    let half = CUBE_HALF as f32;
    let lo = (origin.as_vec3() - Vec3::splat(half)).min(camera.as_vec3());
    let hi = (origin.as_vec3() + Vec3::splat(half)).max(camera.as_vec3());
    Some(SourceSky { surfaces: cube(origin, CUBE_HALF, &materials), camera, bounds: (lo, hi) })
}

/// Six inward quads of a box, one material per face, wound as the engine reads
/// a front face. Texture (0, 0) is the corner at -right -up of the face.
fn cube(center: DVec3, half: f64, materials: &[String; 6]) -> Vec<RenderSurface> {
    let h = half as f32;
    let c = center.as_vec3();
    let mut out = Vec::with_capacity(6);
    for (i, (face, n, u, v)) in SOURCE_SKY_FACES.iter().enumerate() {
        let _ = face;
        let mut quad = [
            (c + n * h - u * h - v * h, Vec2::new(0.0, 0.0)),
            (c + n * h + u * h - v * h, Vec2::new(1.0, 0.0)),
            (c + n * h + u * h + v * h, Vec2::new(1.0, 1.0)),
            (c + n * h - u * h + v * h, Vec2::new(0.0, 1.0)),
        ];
        if rbake::light::front_normal(quad[0].0, quad[1].0, quad[2].0).dot(*n) < 0.0 {
            quad.reverse();
        }
        let mut s = RenderSurface {
            model: 0,
            material: materials[i].clone(),
            cast_shadows: false,
            lightmap_texel: crate::csg::DEFAULT_LIGHTMAP_SCALE as f32,
            ..RenderSurface::default()
        };
        for (p, uv) in quad {
            s.positions.push(p);
            s.normals.push(*n);
            s.tangents.push(Vec4::W);
            s.uv0.push(uv);
            s.uv1.push(Vec2::ZERO);
            s.colors.push([255; 4]);
        }
        s.indices.extend([0, 1, 2, 0, 2, 3]);
        out.push(s);
    }
    out
}

/// The fog colour keys the sky camera takes from a map's own world fog.
const FOG_COLOR_KEYS: [&str; 4] = ["fogcolor", "fogcolorstrength", "fogdircolor", "fogdircolorstrength"];

/// The sky fog for a camera at height `z`: its height band, and the colours of
/// the map's own `env_fog_controller` when it authors one. Without them the
/// camera's defaults are a clear day's haze, which reads as daylight over a
/// night vista.
fn sky_fog(doc: &MapDoc, z: f64) -> KeyValues {
    let mut keys = KeyValues::default();
    let world_fog = doc.objects.iter().find_map(|o| match &o.kind {
        ObjectKind::Entity(e) if e.classname.eq_ignore_ascii_case("env_fog_controller") => Some(&e.keyvalues),
        _ => None,
    });
    for k in FOG_COLOR_KEYS {
        if let Some(v) = world_fog.and_then(|f| f.get(k)) {
            keys.insert(k.into(), v.clone());
        }
    }
    keys.insert("fogzbottom".into(), format!("{}", (z - FOG_BAND_BELOW_CAMERA.0).round()));
    keys.insert("fogztop".into(), format!("{}", (z - FOG_BAND_BELOW_CAMERA.1).round()));
    keys
}

/// A copy of the document with the sky camera added, for the entity pass.
pub fn with_camera(doc: &MapDoc, camera: DVec3) -> MapDoc {
    let mut out = doc.clone();
    let layer = out.layers.first().map(|l| l.id).unwrap_or_default();
    let mut object = rdoc::doc::new_object(
        "sky_camera",
        layer,
        ObjectKind::Light(Light {
            kind: LightKind::Sky,
            color: [1.0; 3],
            intensity: 1.0,
            range: 0.0,
            inner_angle: 0.0,
            outer_angle: 0.0,
            size: [0.0; 2],
            cast_shadows: false,
            baked: false,
            realtime: false,
            keyvalues: sky_fog(doc, camera.z),
        }),
    );
    object.id = uuid::Uuid::from_u128(CAMERA_ID);
    object.transform = Transform { position: camera, rotation: DQuat::IDENTITY, scale: DVec3::ONE };
    out.objects.push(object);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn tables(sky: &str) -> AssetTables {
        let mut tables = AssetTables::default();
        for m in face_materials(sky) {
            tables.materials.insert(
                m.clone(),
                rir::MaterialInfo {
                    path: m,
                    guid: 1,
                    shader_set: "unlit".into(),
                    vertex_format: rir::VertexFormat::Unlit,
                    surface_prop: "concrete".into(),
                    translucent: false,
                    decal: false,
                    nodraw: false,
                    textures: BTreeMap::new(),
                },
            );
        }
        tables
    }

    #[test]
    fn face_materials_match_the_python_tool() {
        assert_eq!(
            face_materials("Tides"),
            [
                "world/source/skybox/tidesbk",
                "world/source/skybox/tidesdn",
                "world/source/skybox/tidesft",
                "world/source/skybox/tideslf",
                "world/source/skybox/tidesrt",
                "world/source/skybox/tidesup",
            ]
            .map(String::from)
        );
        for m in face_materials("tides") {
            assert!(is_source_sky_material(&m));
        }
        assert!(!is_source_sky_material("tools/toolsskybox"));
    }

    /// A synthetic six-material skybox lands each material on the right cube
    /// face: the face whose inward normal is the face's axis.
    #[test]
    fn each_face_material_lands_on_its_own_axis() {
        let origin = DVec3::new(-38434.0, -29834.0, -3580.0);
        let src = build_source("tides", origin, &tables("tides")).expect("six materials build a sky");
        assert_eq!(src.surfaces.len(), 6);
        let materials = face_materials("tides");
        for (i, (s, (face, n, u, v))) in src.surfaces.iter().zip(SOURCE_SKY_FACES).enumerate() {
            assert_eq!(s.material, materials[i]);
            assert_eq!(s.material, face_material("tides", face));
            assert_eq!(s.indices, vec![0, 1, 2, 0, 2, 3]);
            assert!(s.normals.iter().all(|x| *x == n), "{face} normals point inward along {n}");
            let front = rbake::light::front_normal(s.positions[0], s.positions[1], s.positions[2]);
            assert!(front.dot(n) > 0.0, "{face} winds inward");
            let h = CUBE_HALF as f32;
            let c = origin.as_vec3();
            let corner = c + n * h - u * h - v * h;
            let at: Vec<Vec3> = s.positions.clone();
            assert!(at.iter().any(|p| p.distance(corner) < 0.01), "{face} has the -right -up corner");
            let zero = s.uv0.iter().position(|t| *t == Vec2::new(0.0, 0.0)).expect("a (0, 0) corner");
            assert!(s.positions[zero].distance(corner) < 0.01, "{face} maps (0, 0) to its -right -up corner");
            let mut uvs = s.uv0.clone();
            uvs.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
            assert_eq!(uvs, vec![Vec2::new(0.0, 0.0), Vec2::new(0.0, 1.0), Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0)]);
            assert!(!s.cast_shadows);
            let _ = face;
        }
    }

    #[test]
    fn the_sky_fog_takes_the_maps_own_fog_colours() {
        let camera_fog = |doc: &MapDoc| {
            let out = with_camera(doc, DVec3::new(0.0, 0.0, 6000.0));
            match &out.objects.last().unwrap().kind {
                ObjectKind::Light(l) => l.keyvalues.clone(),
                _ => panic!("the camera is a light"),
            }
        };
        let mut doc = MapDoc::default();
        let plain = camera_fog(&doc);
        assert!(!plain.contains_key("fogcolor"), "no world fog: the camera keeps its own defaults");
        assert_eq!(plain.get("fogzbottom").map(String::as_str), Some("956"));
        let mut kv = KeyValues::default();
        kv.insert("fogcolor".into(), "11 35 53".into());
        kv.insert("fogdircolor".into(), "129 103 214".into());
        kv.insert("fogdensity".into(), "0.3".into());
        doc.objects.push(rdoc::doc::new_object(
            "fog",
            Default::default(),
            ObjectKind::Entity(rdoc::model::Entity {
                classname: "env_fog_controller".into(),
                keyvalues: kv,
                partition: None,
                links: Default::default(),
            }),
        ));
        let night = camera_fog(&doc);
        assert_eq!(night.get("fogcolor").map(String::as_str), Some("11 35 53"));
        assert_eq!(night.get("fogdircolor").map(String::as_str), Some("129 103 214"));
        assert!(!night.contains_key("fogdensity"), "only colours follow; the vista keeps its own density");
    }

    #[test]
    fn the_cube_holds_the_sky_camera() {
        let origin = DVec3::new(-38434.0, -29834.0, -3580.0);
        let src = build_source("tides", origin, &tables("tides")).expect("six materials build a sky");
        assert!((src.camera - (origin + DVec3::Z * CAMERA_ABOVE_VISTA)).length() < 1e-9);
        for (s, (_, n, _, _)) in src.surfaces.iter().zip(SOURCE_SKY_FACES) {
            for p in &s.positions {
                assert!((*p - src.camera.as_vec3()).dot(n) > 0.0, "the camera stands inside every face");
            }
        }
    }

    #[test]
    fn a_sky_without_materials_is_left_out() {
        let origin = DVec3::ZERO;
        assert!(build_source("tides", origin, &AssetTables::default()).is_none());
        let mut partial = tables("tides");
        partial.materials.remove("world/source/skybox/tidesup");
        assert!(build_source("tides", origin, &partial).is_none());
    }

    #[test]
    fn a_source_sky_takes_the_default_vistas_place() {
        let mut doc = MapDoc::default();
        assert_eq!(crate::sky_vista(&doc), Some(crate::DEFAULT_SKY.to_string()));
        doc.meta.source_sky = Some("tides".into());
        assert_eq!(crate::sky_vista(&doc), None, "the skybox is the sky on its own");
        doc.meta.sky = Some("mdl/vistas/eden_se.rmdl".into());
        assert_eq!(crate::sky_vista(&doc), Some("mdl/vistas/eden_se.rmdl".to_string()));
        doc.meta.sky = Some("none".into());
        assert_eq!(crate::sky_vista(&doc), None);
    }
}
