//! Scene and fixture helpers shared by the integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use glam::Vec3;
use rir::{BakeLight, BakeScene, BakeSettings, LightType, RenderSurface};

/// The read-only fixture root, taken from `R5ED_FIXTURES`.
pub fn fixtures() -> PathBuf {
    match std::env::var("R5ED_FIXTURES") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("fixtures-absent"),
    }
}

/// The shipped client map the encoder is checked against.
pub fn client_fixture() -> PathBuf {
    fixtures()
        .join("client")
        .join("_test_s21_in")
        .join("mp_rr_canyonlands_staging_mu1.bsp")
}

/// The fresh TF2 conversion the encoder is checked against.
pub fn tf2_ref() -> PathBuf {
    match std::env::var("R5ED_TF2_REF") {
        Ok(v) if !v.is_empty() => PathBuf::from(v).join("client").join("mp_rr_gauntlet.bsp"),
        _ => PathBuf::from("fixtures-absent").join("client").join("mp_rr_gauntlet.bsp"),
    }
}

pub fn fixture_exists(p: &Path) -> bool {
    p.is_file()
}

/// A quad as two triangles, wound so the normal points along `normal`.
pub fn quad(positions: [Vec3; 4], normal: Vec3, material: &str, cast: bool, texel: f32) -> RenderSurface {
    // Wound clockwise about the normal asked for, the way the engine and the
    // compiler wind a front face, so a caller never has to think about it.
    let winding = rbake::light::front_normal(positions[0], positions[1], positions[2]);
    let positions = if winding.dot(normal) < 0.0 {
        [positions[0], positions[3], positions[2], positions[1]]
    } else {
        positions
    };
    RenderSurface {
        object: Some(uuid::Uuid::from_u128(1)),
        model: 0,
        material: material.to_string(),
        positions: positions.to_vec(),
        normals: vec![normal; 4],
        tangents: vec![glam::Vec4::new(1.0, 0.0, 0.0, 1.0); 4],
        uv0: vec![glam::Vec2::new(0.0, 0.0), glam::Vec2::new(1.0, 0.0), glam::Vec2::new(1.0, 1.0), glam::Vec2::new(0.0, 1.0)],
        uv1: vec![glam::Vec2::ZERO; 4],
        colors: vec![[255; 4]; 4],
        indices: vec![0, 1, 2, 0, 2, 3],
        cast_shadows: cast,
        lightmap_texel: texel,
        lightmap_size: [0, 0],
    }
}

fn settings() -> BakeSettings {
    BakeSettings {
        texel_size: 16.0,
        bounces: 1,
        samples: 8,
        sky_color: Vec3::new(0.35, 0.42, 0.55),
        sky_intensity: 1.0,
        ambient_occlusion: true,
        probe_spacing: 192.0,
        cubemap_spacing: 1024.0,
    }
}

/// A closed box room: floor, ceiling and four walls, all facing inwards.
pub fn box_room(half: f32, height: f32) -> BakeScene {
    let s = half;
    let mut surfaces = vec![
        quad([Vec3::new(-s, -s, 0.0), Vec3::new(-s, s, 0.0), Vec3::new(s, s, 0.0), Vec3::new(s, -s, 0.0)], Vec3::Z, "dev/tex_floor", true, 16.0),
        quad([Vec3::new(s, -s, height), Vec3::new(s, s, height), Vec3::new(-s, s, height), Vec3::new(-s, -s, height)], Vec3::NEG_Z, "dev/tex_ceiling", true, 16.0),
        quad([Vec3::new(-s, s, 0.0), Vec3::new(-s, s, height), Vec3::new(s, s, height), Vec3::new(s, s, 0.0)], Vec3::NEG_Y, "dev/tex_wall", true, 16.0),
        quad([Vec3::new(s, -s, 0.0), Vec3::new(s, -s, height), Vec3::new(-s, -s, height), Vec3::new(-s, -s, 0.0)], Vec3::Y, "dev/tex_wall", true, 16.0),
        quad([Vec3::new(s, s, 0.0), Vec3::new(s, s, height), Vec3::new(s, -s, height), Vec3::new(s, -s, 0.0)], Vec3::NEG_X, "dev/tex_wall", true, 16.0),
        quad([Vec3::new(-s, -s, 0.0), Vec3::new(-s, -s, height), Vec3::new(s, -s, height), Vec3::new(s, -s, 0.0)], Vec3::X, "dev/tex_wall", true, 16.0),
    ];
    for (i, f) in surfaces.iter_mut().enumerate() {
        f.object = Some(uuid::Uuid::from_u128(0x100 + i as u128));
    }
    BakeScene {
        surfaces,
        props: Vec::new(),
        prop_occluders: Vec::new(),
        prop_lump_index: Vec::new(),
        shadow_only: Vec::new(),
        see_through: Vec::new(),
        shadow_transmission: Vec::new(),
        probes_ignore_props: false,
        lights: vec![BakeLight {
            object: None,
            kind: LightType::Sun,
            position: Vec3::ZERO,
            direction: Vec3::new(0.3, 0.2, -1.0).normalize(),
            color: Vec3::new(1.0, 0.96, 0.9),
            intensity: 2.0,
            range: 1024.0,
            inner_angle: 0.0,
            outer_angle: 0.0,
            size: Vec2::ZERO,
            cast_shadows: true,
            baked: true,
            realtime: false,
            tweak_index: None,
            attenuation: None,
            flags: None,
        }],
        settings: settings(),
        world_mins: Vec3::new(-half, -half, 0.0),
        world_maxs: Vec3::new(half, half, height),
        cubemap_points: Vec::new(),
        probe_points: Vec::new(),
    }
}

/// A large floor with a box standing on it: the sun-shadow fixture.
pub fn plane_with_box(extent: f32, box_min: Vec3, box_max: Vec3) -> BakeScene {
    let e = extent;
    let floor = quad(
        [
            Vec3::new(-e, -e, 0.0),
            Vec3::new(-e, e, 0.0),
            Vec3::new(e, e, 0.0),
            Vec3::new(e, -e, 0.0),
        ],
        Vec3::Z,
        "dev/tex_floor",
        true,
        8.0,
    );
    let cx = (box_min.x + box_max.x) * 0.5;
    let cy = (box_min.y + box_max.y) * 0.5;
    let h = box_max.z;
    let sides = [
        quad([Vec3::new(box_min.x, box_min.y, 0.0), Vec3::new(box_min.x, box_min.y, h), Vec3::new(box_max.x, box_min.y, h), Vec3::new(box_max.x, box_min.y, 0.0)], Vec3::Y, "dev/tex_crate", true, 8.0),
        quad([Vec3::new(box_max.x, box_max.y, 0.0), Vec3::new(box_max.x, box_max.y, h), Vec3::new(box_min.x, box_max.y, h), Vec3::new(box_min.x, box_max.y, 0.0)], Vec3::NEG_Y, "dev/tex_crate", true, 8.0),
        quad([Vec3::new(box_min.x, box_max.y, 0.0), Vec3::new(box_min.x, box_max.y, h), Vec3::new(box_min.x, box_min.y, h), Vec3::new(box_min.x, box_min.y, 0.0)], Vec3::X, "dev/tex_crate", true, 8.0),
        quad([Vec3::new(box_max.x, box_min.y, 0.0), Vec3::new(box_max.x, box_min.y, h), Vec3::new(box_max.x, box_max.y, h), Vec3::new(box_max.x, box_max.y, 0.0)], Vec3::NEG_X, "dev/tex_crate", true, 8.0),
        quad([Vec3::new(box_min.x, box_min.y, h), Vec3::new(box_min.x, box_max.y, h), Vec3::new(box_max.x, box_max.y, h), Vec3::new(box_max.x, box_min.y, h)], Vec3::Z, "dev/tex_crate", true, 8.0),
    ];
    let mut surfaces = vec![floor];
    surfaces.extend(sides);
    surfaces[0].object = Some(uuid::Uuid::from_u128(1));
    for (i, s) in surfaces.iter_mut().enumerate().skip(1) {
        s.object = Some(uuid::Uuid::from_u128(0x200 + i as u128));
    }
    let _ = (cx, cy);
    BakeScene {
        surfaces,
        props: Vec::new(),
        prop_occluders: Vec::new(),
        prop_lump_index: Vec::new(),
        shadow_only: Vec::new(),
        see_through: Vec::new(),
        shadow_transmission: Vec::new(),
        probes_ignore_props: false,
        lights: vec![BakeLight {
            object: None,
            kind: LightType::Sun,
            position: Vec3::new(cx, cy, 512.0),
            direction: Vec3::new(0.25, 0.15, -1.0).normalize(),
            color: Vec3::new(1.0, 0.95, 0.85),
            intensity: 3.0,
            range: 1024.0,
            inner_angle: 0.0,
            outer_angle: 0.0,
            size: Vec2::ZERO,
            cast_shadows: true,
            baked: true,
            realtime: false,
            tweak_index: None,
            attenuation: None,
            flags: None,
        }],
        settings: settings(),
        world_mins: Vec3::new(-e, -e, 0.0),
        world_maxs: Vec3::new(e, e, 256.0),
        cubemap_points: Vec::new(),
        probe_points: Vec::new(),
    }
}

use glam::Vec2;
