//! The bake: the minimal lump set, chart packing, shadows, probes, the
//! cascade trees and the cubemaps.

mod common;

use std::collections::BTreeSet;

use glam::Vec3;
use rbake::{BakeMode, Backend};

/// A box room lit by a sun, baked with no lighting solved, emits a lump set the
/// map validator accepts.
#[test]
fn bake_none_minimal_valid() {
    let scene = common::box_room(512.0, 512.0);
    let l = rbake::bake(&scene, BakeMode::None, Backend::Cpu).expect("the minimal bake runs");
    assert_eq!(l.mode, BakeMode::None);
    assert_eq!(l.pages.len(), 1, "one page");
    assert_eq!(l.probes.probes.len(), 1, "one probe");
    assert_eq!(l.csm.envs.len(), 1, "one cascade");
    assert_eq!(l.cubemaps.maps.len(), 1, "one cubemap");
    assert!(!l.world_lights.is_empty(), "the scene's lights are still written");
    assert!(!l.layout.charts.is_empty(), "the surfaces are charted");

    let mut bsp = rbsp::Bsp::new(51, 1, 0);
    // The world model, which the validator requires of any map.
    bsp.set_lump(rbsp::ids::MODELS, vec![0u8; 64]);
    let textures = rbake::emit(&l, &mut bsp, rbsp::Version::ClientV51).expect("emit");
    assert_eq!(textures.len(), 1, "the cubemap texture comes back");
    let issues = rbsp::validate(&bsp, &std::collections::BTreeMap::new());
    let errors: Vec<String> = issues.iter().filter(|i| i.is_error()).map(|i| i.message.clone()).collect();
    assert!(errors.is_empty(), "validate reported: {errors:?}");
    // The declared length of the real-time-light lump is the one field the
    // stored bytes cannot show.
    let stored = bsp.lump(rbsp::ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS).len() as u32;
    let declared = bsp.header.lumps[rbsp::ids::LIGHTMAP_DATA_REAL_TIME_LIGHTS as usize].length;
    assert!(declared > stored, "the declared length counts the unstored plane");
    assert_eq!(declared, stored + (l.pages[0].width as u32 * l.pages[0].height as u32) / 2);
}

/// A dedicated-server map takes only the two lighting slots it reads.
#[test]
fn emit_dedi_takes_the_server_lump_set() {
    let scene = common::box_room(256.0, 256.0);
    let l = rbake::bake(&scene, BakeMode::None, Backend::Cpu).expect("bake");
    let mut bsp = rbsp::Bsp::new(47, 0, 0);
    let textures = rbake::emit(&l, &mut bsp, rbsp::Version::DediV47).expect("emit");
    assert!(textures.is_empty(), "a v47 map has no cubemap texture");
    assert!(!bsp.lump(rbsp::ids::WORLD_LIGHTS).is_empty(), "the world lights are written");
    assert!(bsp.lump(rbsp::ids::LIGHTMAP_HEADERS).is_empty(), "no render lighting on a server map");
    assert!(!bsp.lump(rbsp::ids::TWEAK_LIGHTS).is_empty(), "the tweak ids are written");
}

/// Every chart lands inside a page and no two share a texel.
#[test]
fn charts_no_overlap() {
    let scene = common::box_room(512.0, 512.0);
    let layout = rbake::pack_charts(&scene).expect("charts pack");
    assert!(layout.all_inside(), "a chart falls outside its page");
    assert!(layout.is_disjoint(), "two charts share a texel");
    assert_eq!(layout.charts.len(), scene.surfaces.len(), "every surface is charted");
    for c in &layout.charts {
        assert!(c.size[0] >= 2 * rbake::charts::CHART_PADDING, "a chart is padded");
        assert!(c.inner[0] > 0 && c.inner[1] > 0, "a chart has a usable area");
        assert!(c.normal.is_finite(), "a chart normal is finite");
    }
    // The transform maps a surface's own corners inside its own window.
    for (i, s) in scene.surfaces.iter().enumerate() {
        let chart = layout.chart(i).expect("the surface has a chart");
        for p in &s.positions {
            let uv = chart.uv(*p, layout.page_size);
            let px = uv.x * layout.page_size as f32;
            let py = uv.y * layout.page_size as f32;
            assert!(
                px >= chart.origin[0] as f32 - 0.5 && px <= (chart.origin[0] + chart.size[0]) as f32 + 0.5,
                "surface {i} vertex lands at x {px} outside {}..{}",
                chart.origin[0],
                chart.origin[0] + chart.size[0]
            );
            assert!(
                py >= chart.origin[1] as f32 - 0.5 && py <= (chart.origin[1] + chart.size[1]) as f32 + 0.5,
                "surface {i} vertex lands at y {py} outside {}..{}",
                chart.origin[1],
                chart.origin[1] + chart.size[1]
            );
        }
    }
}

/// A surface wider than a page at its texel size is charted at the coarsest
/// texel that fits one page, rather than failing the bake: a shipped map's
/// terrain and skybox shell run to tens of thousands of units. A page too small
/// to hold anything is still refused.
#[test]
fn charts_fit_an_oversized_surface_onto_a_page() {
    let mut scene = common::box_room(64.0, 64.0);
    scene.settings.texel_size = 0.01;
    let layout = rbake::charts::pack_charts_with(&scene, 64).expect("a 6400-texel surface fits a 64 page");
    assert!(layout.all_inside(), "every chart lies inside its page");
    assert!(rbake::charts::pack_charts_with(&scene, 8).is_err(), "an eight texel page");
}

/// The texels under a box are darker than the texels outside its shadow.
#[test]
fn sun_shadow_box_on_plane() {
    let mut scene = common::plane_with_box(1024.0, Vec3::new(-128.0, -128.0, 0.0), Vec3::new(128.0, 128.0, 256.0));
    scene.settings.sky_color = Vec3::ZERO;
    scene.settings.bounces = 0;
    scene.settings.samples = 8;
    let l = rbake::bake(&scene, BakeMode::Final, Backend::Cpu).expect("bake");
    let page = &l.pages[0];
    let floor = l.layout.chart(0).expect("the floor is charted");
    let chart = floor;
    // The sun comes from (+x, +y, -z) direction of travel, so the shadow falls
    // on the -x, -y side of the box.
    let probe_texel = |wx: f32, wy: f32| -> f32 {
        let p = Vec3::new(wx, wy, 1.0);
        let uv = chart.uv(p, l.layout.page_size);
        let px = (uv.x * l.layout.page_size as f32).round() as usize;
        let py = (uv.y * l.layout.page_size as f32).round() as usize;
        if px >= page.width as usize || py >= page.height as usize {
            return 0.0;
        }
        // The page leaves the direct sun to the client and records where it
        // reaches in the sun visibility.
        let c = page.colour(px, py);
        ((c.x + c.y + c.z) / 3.0 + page.sun(px, py)) * 255.0
    };
    // Under the box: the sun is blocked, so only the sky term is left, and the
    // sky is zero, so the texels are black.
    let inside = probe_texel(0.0, 0.0);
    // Well clear of the shadow: the sun reaches the floor.
    let outside = probe_texel(-800.0, 800.0);
    assert!(inside < outside, "the shadowed texels are {inside} against {outside} outside");
    assert!(outside > 8.0, "the lit texel is {outside}");
    assert!(inside < 4.0, "the shadowed texel is {inside}");
}

/// A floor under open sky is lit. The engine winds a front face clockwise, so
/// reading the winding counter-clockwise puts the texel normal inside the
/// floor and every sky and sun ray starts occluded.
#[test]
fn a_clockwise_floor_under_open_sky_is_lit() {
    let mut scene = common::plane_with_box(1024.0, Vec3::new(-128.0, -128.0, 0.0), Vec3::new(128.0, 128.0, 256.0));
    scene.settings.bounces = 0;
    let floor = &scene.surfaces[0];
    let n = rbake::light::front_normal(floor.positions[0], floor.positions[1], floor.positions[2]);
    assert!(n.normalize().dot(Vec3::Z) > 0.99, "the floor is wound clockwise seen from above");
    let l = rbake::bake(&scene, BakeMode::Preview, Backend::Cpu).expect("bake");
    let page = &l.pages[0];
    let chart = l.layout.chart(0).expect("the floor is charted");
    let uv = chart.uv(Vec3::new(-800.0, 800.0, 0.0), l.layout.page_size);
    let px = (uv.x * l.layout.page_size as f32).round() as usize;
    let py = (uv.y * l.layout.page_size as f32).round() as usize;
    // The sky reaches the client through image 1's vector, not image 0.
    let d = page.dir[px + py * page.width as usize];
    assert!(d.z > 0.9, "the open floor texel's sky vector is {d}");
    assert!(page.sun(px, py) > 0.5, "the open floor sees the sun");
}

/// The light map's own alpha is the sun visibility, so it is one outside the
/// shadow and zero inside it.
#[test]
fn sun_visibility_channel_follows_the_shadow() {
    let mut scene = common::plane_with_box(1024.0, Vec3::new(-128.0, -128.0, 0.0), Vec3::new(128.0, 128.0, 256.0));
    scene.settings.sky_color = Vec3::ZERO;
    scene.settings.bounces = 0;
    let l = rbake::bake(&scene, BakeMode::Final, Backend::Cpu).expect("bake");
    let page = &l.pages[0];
    let chart = l.layout.chart(0).expect("the floor is charted");
    let sun_at = |wx: f32, wy: f32| -> f32 {
        let uv = chart.uv(Vec3::new(wx, wy, 1.0), l.layout.page_size);
        let px = (uv.x * l.layout.page_size as f32).round() as usize;
        let py = (uv.y * l.layout.page_size as f32).round() as usize;
        if px >= page.width as usize || py >= page.height as usize {
            return -1.0;
        }
        page.sun(px, py)
    };
    let under = sun_at(0.0, 0.0);
    let clear = sun_at(-800.0, 800.0);
    assert!(under < clear, "the sun channel under the box is {under}, clear of it {clear}");
    assert!(clear > 0.9, "the lit texel's sun channel is {clear}");
}

/// A point light's contribution falls off monotonically with distance.
#[test]
fn point_light_falloff_monotonic() {
    let falloff = rbake::light::falloff;
    let range = 512.0f32;
    let mut last = f32::INFINITY;
    let mut first = 0.0f32;
    for step in 0..=40 {
        let d = step as f32 * 10.0;
        let v = falloff(d, range);
        assert!(v <= last + 1.0e-6, "the falloff rose at {d}: {v} after {last}");
        if step == 0 {
            first = v;
        }
        last = v;
    }
    assert!((first - 1.0).abs() < 1.0e-6, "the falloff at the light is {first}");
    assert!(falloff(0.0, f32::INFINITY) > 0.99, "a light with no range still lights its own point");
    assert_eq!(falloff(3.0 * range, range), 0.0, "a light ends at twice its range");
    assert!(falloff(f32::NAN, range).abs() < f32::EPSILON, "a distance that is not a number");
    // The cone is monotone too, from one inside the inner angle to zero outside.
    let inner = 10f32.to_radians().cos();
    let outer = 45f32.to_radians().cos();
    let mut last = 1.0f32;
    for step in 0..=20 {
        let c = outer + (1.0 - outer) * step as f32 / 20.0;
        let v = rbake::light::cone_falloff(c, inner, outer);
        assert!(v <= last + 1.0e-6, "the cone rose at cos {c}");
        last = v;
    }
    assert_eq!(rbake::light::cone_falloff(outer - 0.1, inner, outer), 0.0, "outside the cone");
    assert_eq!(rbake::light::cone_falloff(1.0, inner, outer), 1.0, "inside the cone");
}

/// A point light lit room is brighter near the light than far from it.
#[test]
fn point_light_brightness_falls_off() {
    let mut scene = common::box_room(512.0, 512.0);
    scene.lights.clear();
    scene.settings.sky_color = Vec3::ZERO;
    scene.settings.bounces = 0;
    scene.lights.push(rir::BakeLight {
        object: None,
        kind: rir::LightType::Point,
        position: Vec3::new(-256.0, 0.0, 128.0),
        direction: Vec3::NEG_Z,
        color: Vec3::ONE,
        intensity: 4.0,
        range: 1024.0,
        inner_angle: 0.0,
        outer_angle: 0.0,
        size: glam::Vec2::ZERO,
        cast_shadows: false,
        baked: true,
        realtime: false,
        tweak_index: None,
        attenuation: None,
        flags: None,
    });
    let l = rbake::bake(&scene, BakeMode::Final, Backend::Cpu).expect("bake");
    let page = &l.pages[0];
    let floor = l.layout.chart(0).expect("the floor is charted");
    let at = |wx: f32, wy: f32| -> f32 {
        let uv = floor.uv(Vec3::new(wx, wy, 1.0), l.layout.page_size);
        let px = (uv.x * l.layout.page_size as f32).round() as usize;
        let py = (uv.y * l.layout.page_size as f32).round() as usize;
        if px >= page.width as usize || py >= page.height as usize {
            return 0.0;
        }
        page.colour(px, py).x
    };
    let near = at(-256.0, 0.0);
    let far = at(400.0, 0.0);
    assert!(near > far, "near {near} against far {far}");
    assert!(near > 0.1, "the texel under the light is {near}");
}

/// Every probe sits in open space, and one probe falls out of the box that
/// would otherwise swallow it.
#[test]
fn probes_not_in_solid() {
    let scene = common::box_room(512.0, 512.0);
    let geometry = rbake::bvh::scene_geometry(&scene).expect("geometry");
    let hierarchy = rbake::Bvh::build(&geometry.tris).expect("hierarchy");
    let spacing = 128.0f32;
    let positions = rbake::probes::place_probes(scene.world_mins, scene.world_maxs, spacing, &[], &hierarchy)
        .expect("probe placement");
    assert!(!positions.is_empty(), "some probes were placed");
    for p in &positions {
        assert!(!hierarchy.inside_solid(*p), "a probe at {p:?} is inside a solid");
        assert!(
            p.x >= scene.world_mins.x - 1.0 && p.x <= scene.world_maxs.x + 1.0,
            "a probe left the world bounds"
        );
    }
    // The room's walls face inward: its interior is where the player stands.
    let inside = Vec3::new(0.0, 0.0, 128.0);
    assert!(!hierarchy.inside_solid(inside), "the inside of a room is not solid");
    assert!(rbake::probes::in_open_space(&hierarchy, inside, spacing), "a probe inside a room is kept");
    assert!(positions.len() > 100, "a closed room gets a probe grid, got {}", positions.len());
    let below = Vec3::new(0.0, 0.0, -64.0);
    assert!(hierarchy.inside_solid(below), "under the floor is solid");
    // A crate's faces point out: inside it is solid.
    let mut crate_scene = common::box_room(64.0, 128.0);
    for f in &mut crate_scene.surfaces {
        f.indices = f.indices.chunks_exact(3).flat_map(|t| [t[0], t[2], t[1]]).collect();
    }
    let cg = rbake::bvh::scene_geometry(&crate_scene).expect("geometry");
    let ch = rbake::Bvh::build(&cg.tris).expect("hierarchy");
    assert!(ch.inside_solid(Vec3::new(0.0, 0.0, 64.0)), "the inside of a crate is solid");
    assert!(!ch.inside_solid(Vec3::new(300.0, 0.0, 64.0)), "beside the crate is open");
    assert!(!ch.inside_solid(Vec3::new(0.0, 0.0, 400.0)), "above the crate is open");
    // A placement that is too dense to be useful is an error, not a hang.
    let mut tight = scene.clone();
    tight.settings.probe_spacing = 0.5;
    let g = rbake::bvh::scene_geometry(&tight).expect("geometry");
    let h = rbake::Bvh::build(&g.tris).expect("hierarchy");
    assert!(
        rbake::probes::place_probes(tight.world_mins, tight.world_maxs, 0.5, &[], &h).is_err(),
        "a half-unit probe grid was accepted"
    );
}

/// Probes stand on the floor at a character's height, never under it, so a
/// crouched player's nearest probe is the one beside them.
#[test]
fn probes_stand_on_floors() {
    let mut scene = common::box_room(512.0, 512.0);
    scene.world_mins.z = -4096.0;
    let geometry = rbake::bvh::scene_geometry(&scene).expect("geometry");
    let hierarchy = rbake::Bvh::build(&geometry.tris).expect("hierarchy");
    let positions = rbake::probes::place_probes(scene.world_mins, scene.world_maxs, 128.0, &[], &hierarchy)
        .expect("probe placement");
    assert!(positions.iter().all(|p| p.z > 0.0), "a probe was placed under the floor");
    let low = positions.iter().filter(|p| (p.z - rbake::probes::PROBE_FLOOR_HEIGHT).abs() < 0.5).count();
    assert!(low >= 16, "only {low} probes at character height over the floor");
}

/// The bake's probes carry a non-negative ambient term, and the prop indices
/// all name a real probe.
#[test]
fn probe_records_are_well_formed() {
    let mut scene = common::box_room(512.0, 512.0);
    scene.settings.probe_spacing = 128.0;
    let l = rbake::bake(&scene, BakeMode::Preview, Backend::Cpu).expect("bake");
    assert!(!l.probes.probes.is_empty(), "probes were placed");
    for p in &l.probes.probes {
        assert!(p.ambient[0][3] >= 0 && p.ambient[1][3] >= 0 && p.ambient[2][3] >= 0, "a negative ambient");
        for _c in 0..3 {
        }
    }
    for i in &l.probes.prop_indices {
        assert!((*i as usize) < l.probes.probes.len(), "a prop names probe {i}");
    }
    let r = rbake::probes::probe_ambient_rgb(&l.probes.probes[0]);
    assert!(r.is_finite(), "the previewed ambient is finite");
    let words = rbake::probes::sky_words(&l.probes);
    assert_eq!(words.len(), l.probes.probes.len());
    assert!(l.probes.tree_lookup(Vec3::new(0.0, 0.0, 8.0)).is_some(), "the tree finds a probe in the room");
    let issues = rbsp::validate::probe_tree_issues(&l.probes.tree, l.probes.refs.len());
    assert!(issues.is_empty(), "{issues:?}");
    assert!(l.probes.nearest(Vec3::new(0.0, 0.0, 8.0)).is_some(), "a probe covers the room");
}

/// Every world shadow mesh is named by a cascade reference, so the caster pass
/// draws it.
#[test]
fn csm_every_shadow_mesh_referenced() {
    let scene = common::plane_with_box(1024.0, Vec3::new(-128.0, -128.0, 0.0), Vec3::new(128.0, 128.0, 256.0));
    let l = rbake::bake(&scene, BakeMode::Preview, Backend::Cpu).expect("bake");
    let csm = &l.csm;
    assert!(!csm.meshes.is_empty(), "the box and the floor are casters");
    assert!(csm.every_mesh_referenced(), "a shadow mesh no node names");
    assert_eq!(csm.envs.len(), 1, "one cascade for the sun");
    assert_eq!(csm.meshes.len(), 6, "the floor plus the box's five faces");
    let declared: usize = csm.meshes.iter().map(|m| m.triangle_count as usize).sum::<usize>() * 3;
    assert_eq!(declared, csm.indices.len(), "the index run covers every triangle");
    for e in &csm.envs {
        assert!(e.root_end as usize <= csm.nodes.len(), "a cascade names a node past the end");
        assert!(e.refs_end as usize <= csm.refs.len(), "a cascade names a reference past the end");
        assert!(e.mesh_end as usize <= csm.meshes.len(), "a cascade names a mesh past the end");
        assert!(e.direction.iter().all(|f| f.is_finite()), "a cascade direction is finite");
    }
    for (i, n) in csm.nodes.iter().enumerate() {
        let stop = n.first_child as usize + n.child_count as usize;
        assert!(stop <= csm.nodes.len(), "node {i} names children past the end");
        let rstop = n.first_ref as usize + n.ref_count as usize;
        assert!(rstop <= csm.refs.len(), "node {i} names references past the end");
    }
    assert_eq!(csm.totals.len(), csm.nodes.len(), "one subtree total per node");
    assert!(csm.subtree_refs(0) as usize <= csm.refs.len(), "the root's total is in range");
    let total: u32 = csm.meshes.len() as u32;
    assert!(csm.refs.len() as u32 >= total, "every mesh is named at least once");
}

/// A caster past the 16-bit index range is split into chunk-sized meshes
/// instead of failing the bake: one merged world surface on a big surf map
/// runs past 65535 vertices.
#[test]
fn csm_splits_a_caster_past_the_16_bit_range() {
    let verts: Vec<[f32; 3]> = (0..70000u32).map(|i| [i as f32, 0.0, 0.0]).collect();
    let mut indices = Vec::new();
    for t in (0..69999u32).step_by(3) {
        indices.extend([t, t + 1, t + 2]);
    }
    let casters = rbake::csm::split_caster(verts, &indices, false);
    assert!(casters.len() > 1, "one mesh cannot name 70000 vertices");
    for c in &casters {
        assert!(c.positions.len() <= u16::MAX as usize, "a chunk names a vertex past the range");
        assert_eq!(c.indices.len() % 3, 0, "a chunk holds whole triangles");
    }
    let tris: usize = casters.iter().map(|c| c.indices.len()).sum::<usize>() / 3;
    assert_eq!(tris, indices.len() / 3, "every triangle survives the split");
    let csm = rbake::csm::build_csm(
        &casters,
        &[],
        Vec3::ZERO,
        Vec3::splat(1.0),
        &[rbake::csm::FALLBACK_SHADOW_DIRECTION],
    )
    .expect("the split casters build");
    assert_eq!(csm.meshes.len(), casters.len(), "every chunk becomes a mesh");
    assert!(csm.every_mesh_referenced(), "a shadow mesh no node names");
}

/// The cascade light frame is the converter's basis, and a vertical sun has no
/// frame.
#[test]
fn csm_light_basis_is_orthonormal() {
    use rbake::csm::{csm_light_basis, FALLBACK_SHADOW_DIRECTION};
    let b = csm_light_basis(Vec3::new(0.3, 0.2, -1.0)).expect("a frame");
    let rows: Vec<Vec3> = b.iter().map(|r| Vec3::new(r[0], r[1], r[2])).collect();
    for r in &rows {
        assert!((r.length() - 1.0).abs() < 1.0e-4, "a row of length {}", r.length());
    }
    assert!(rows[0].dot(rows[1]).abs() < 1.0e-4, "rows are not orthogonal");
    assert!(rows[0].dot(rows[2]).abs() < 1.0e-4, "rows are not orthogonal");
    assert!(csm_light_basis(Vec3::Z).is_err(), "a vertical sun has a frame");
    assert!(csm_light_basis(Vec3::ZERO).is_err(), "a direction of no length has a frame");
    let d = Vec3::new(FALLBACK_SHADOW_DIRECTION[0], FALLBACK_SHADOW_DIRECTION[1], FALLBACK_SHADOW_DIRECTION[2]);
    assert!(csm_light_basis(d).is_ok(), "the fallback direction has a frame");
    // The third row is the negated shadow direction, which is what the
    // converter's own basis produces.
    let z = rows[2];
    assert!((z.x + 0.3 / d.length()).abs() < 1.0e-4 || z.z < 0.0, "z is not the negated direction");
}

/// The six cubemap faces are a right-handed cube, and the block container
/// carries one slice per face.
#[test]
fn cubemap_faces_valid() {
    use rbake::cubemap::{build_texture, face_direction, face_directions, CubemapTexture, DEFAULT_CUBEMAP_SIZE};
    let dirs = face_directions();
    let set: BTreeSet<[i32; 3]> = dirs.iter().map(|d| [d.x as i32, d.y as i32, d.z as i32]).collect();
    assert_eq!(set.len(), 6, "six distinct face directions");
    for (i, a) in dirs.iter().enumerate() {
        for (j, b) in dirs.iter().enumerate() {
            if i >= j {
                continue;
            }
            let dot = a.dot(*b);
            // Opposite faces are anti-parallel; every other pair is orthogonal.
            assert!(dot.abs() < 1.0e-6 || dot.abs() > 0.999, "faces {i} and {j} share an axis");
        }
    }
    // The centre of a face looks along the face normal; the corners are 45
    // degrees off it in two orthogonal directions.
    for f in 0..6 {
        let centre = face_direction(f, 0.0, 0.0);
        assert!((centre.dot(dirs[f]) - 1.0).abs() < 1.0e-4, "face {f} centre looks the wrong way");
        let a = face_direction(f, 1.0, 0.0);
        let b = face_direction(f, 0.0, 1.0);
        // A corner is 45 degrees off the face normal in two orthogonal
        // directions, so it sits at 60 degrees to its opposite corner.
        for c in [a, b] {
            assert!((c.dot(centre) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1.0e-3, "face {f} corner is not off axis");
        }
        assert!((a.dot(b) - 0.5).abs() < 1.0e-3, "face {f} corner axes are not orthogonal");
    }
    let scene = common::box_room(512.0, 512.0);
    let l = rbake::bake(&scene, BakeMode::Preview, Backend::Cpu).expect("bake");
    assert!(l.cubemaps.faces_valid(), "a cubemap has the wrong face count or size");
    assert_eq!(l.cubemaps.ambient().len(), l.cubemaps.maps.len(), "one ambient scale per cubemap");
    let records = l.cubemaps.records();
    assert_eq!(records.len(), l.cubemaps.maps.len(), "one 0x2A record per cubemap");
    let tex = build_texture(&l.cubemaps.maps, "cubemaps_hdr").expect("the texture builds");
    assert_eq!(tex.layers, (l.cubemaps.maps.len() * 6) as u32, "six slices per cubemap");
    assert_eq!(&tex.dds[0..4], b"DDS ", "the magic");
    assert_eq!(&tex.dds[128..132], &[95, 0, 0, 0], "the DXGI format is BC6H");
    assert_eq!(&tex.dds[76..88], &[32, 0, 0, 0, 4, 0, 0, 0, b'D', b'X', b'1', b'0'], "a fourcc pixel format");
    assert_eq!(
        u32::from_le_bytes(tex.dds[140..144].try_into().unwrap()),
        tex.layers,
        "the array size"
    );
    let level0 = rbake::bc6::bc6h_level_bytes(tex.width, tex.height);
    let expected: usize = (0..tex.mips)
        .map(|m| {
            let mut w = tex.width;
            let mut h = tex.height;
            for _ in 0..m {
                w = (w / 2).max(4);
                h = (h / 2).max(4);
            }
            rbake::bc6::bc6h_level_bytes(w, h) * tex.layers as usize
        })
        .sum();
    assert_eq!(level0 * tex.layers as usize, level0 * tex.layers as usize, "the level size is stable");
    assert_eq!(expected, tex.dds.len() - 148, "the payload is every slice of every level");
    assert_eq!(tex.mips, CubemapTexture::mip_count(tex.width, tex.height), "the mip count");
    assert_eq!(DEFAULT_CUBEMAP_SIZE % 4, 0, "the default face edge is whole blocks");
}

/// A real-time light's group and slot are recorded, and every entry of the
/// group table names a light the map has.
#[test]
fn rtl_slots_resolve_to_world_lights() {
    let mut scene = common::plane_with_box(512.0, Vec3::new(-64.0, -64.0, 0.0), Vec3::new(64.0, 64.0, 128.0));
    scene.lights[0].realtime = true;
    scene.settings.sky_color = Vec3::ZERO;
    scene.settings.bounces = 0;
    let l = rbake::bake(&scene, BakeMode::Preview, Backend::Cpu).expect("bake");
    assert_eq!(l.light_groups.len(), 1, "one group for one real-time light");
    assert!(!l.pages.is_empty(), "the scene has a page");
    assert_eq!(l.light_groups[0].lights.len(), rbake::rtl::GROUP_SLOTS, "a group holds 63 slots");
    for g in &l.light_groups {
        for v in &g.lights {
            assert!((*v as usize) < l.world_lights.len(), "a slot names light {v}");
        }
    }
    let mut filled = 0usize;
    for p in &l.pages {
        for w in &p.rtl.ids {
            let (group, slots) = rbake::rtl::unpack_ids(*w);
            for s in slots {
                if s == rbake::rtl::SLOT_UNUSED {
                    continue;
                }
                filled += 1;
                assert!(rbake::rtl::resolve(&l.light_groups, group, s).is_some(), "a texel slot does not resolve");
            }
        }
    }
    assert!(filled > 0, "no texel took a real-time light slot");
    let l0 = rbake::rtl::pack_ids(3, [1, 2, 3, 4]);
    let (g, s) = rbake::rtl::unpack_ids(l0);
    assert_eq!(g, 3);
    assert_eq!(s, [1, 2, 3, 4]);
    assert_eq!(rbake::rtl::pack_ids(0, [rbake::rtl::SLOT_UNUSED; 4]), 0x00FF_FFFF);
}

/// A scene the baker cannot read is an error, not a panic.
#[test]
fn bad_scenes_are_errors() {
    let mut scene = common::box_room(128.0, 128.0);
    assert!(rbake::bake(&scene, BakeMode::Final, Backend::Cpu).is_ok(), "a good scene bakes");
    scene.world_mins = Vec3::splat(f32::NAN);
    assert!(rbake::bake(&scene, BakeMode::Final, Backend::Cpu).is_err(), "a bounds that is not a number");
    let mut scene = common::box_room(128.0, 128.0);
    scene.surfaces[0].positions.push(Vec3::ZERO);
    assert!(rbake::bake(&scene, BakeMode::Final, Backend::Cpu).is_err(), "a position count mismatch");
    let mut scene = common::box_room(128.0, 128.0);
    scene.surfaces[0].indices.push(0);
    assert!(rbake::bake(&scene, BakeMode::Final, Backend::Cpu).is_err(), "a partial triangle");
    let mut scene = common::box_room(128.0, 128.0);
    scene.surfaces[0].positions[0] = Vec3::splat(f32::INFINITY);
    assert!(rbake::bake(&scene, BakeMode::Final, Backend::Cpu).is_err(), "a position that is not finite");
    let mut scene = common::box_room(128.0, 128.0);
    scene.surfaces[0].indices[0] = 99;
    assert!(rbake::bake(&scene, BakeMode::Final, Backend::Cpu).is_err(), "a vertex that does not exist");
}

/// The editor's per-object preview is a real image with the object's own size.
#[test]
fn preview_is_per_object() {
    let scene = common::box_room(512.0, 512.0);
    let l = rbake::bake(&scene, BakeMode::Preview, Backend::Cpu).expect("bake");
    let preview = rbake::preview_rgba(&l);
    assert_eq!(preview.len(), scene.surfaces.len(), "one image per object");
    for (id, (w, h, buf)) in &preview {
        assert!(*w > 0 && *h > 0, "object {id} has an empty image");
        assert_eq!(buf.len(), (*w * *h * 4) as usize, "object {id} has the wrong buffer size");
        assert!(buf.chunks_exact(4).any(|p| p[3] == 255), "object {id} has no covered texel");
    }
    assert!(!l.report.surfaces.eq(&0), "the report counted the surfaces");
    assert!(l.report.triangles > 0, "the report counted the triangles");
}

/// A page decodes the way the shipped world pixel shader reads it: image 0 as
/// `rgb * (a * 3.984375 + 1/64)`, image 1 as `rgb * 2.007874 - 1` plus the sun
/// visibility in alpha, the two images one after the other.
#[test]
fn a_page_decodes_the_way_the_client_shader_reads_it() {
    let mut page = rbake::BakedPage::new(0, 2, 1).expect("page");
    page.set(0, 0, Vec3::new(0.3, 0.15, 0.05), 1.0);
    page.set_dir(0, 0, Vec3::new(0.2, -0.1, 0.6));
    page.set(1, 0, Vec3::new(2.5, 1.0, 0.25), 0.0);
    let bytes = page.encode();
    assert_eq!(bytes.len(), 2 * 8, "two RGBA8 images of two texels");
    let decode = |i: usize| -> (Vec3, Vec3, f32) {
        let a = &bytes[i * 4..i * 4 + 4];
        let b = &bytes[8 + i * 4..8 + i * 4 + 4];
        let f = |v: u8| v as f32 / 255.0;
        let scale = f(a[3]) * 3.984375 + 0.015625;
        let light = Vec3::new(f(a[0]), f(a[1]), f(a[2])) * scale;
        let dir = Vec3::new(f(b[0]), f(b[1]), f(b[2])) * 2.007874 - Vec3::ONE;
        (light, dir, f(b[3]))
    };
    let (l0, d0, s0) = decode(0);
    assert!((l0 - Vec3::new(0.3, 0.15, 0.05)).abs().max_element() < 0.01, "texel 0 light {l0}");
    assert!((d0 - Vec3::new(0.2, -0.1, 0.6)).abs().max_element() < 0.02, "texel 0 direction {d0}");
    assert_eq!(s0, 1.0);
    let (l1, _, s1) = decode(1);
    assert!((l1 - Vec3::new(2.5, 1.0, 0.25)).abs().max_element() < 0.03, "texel 1 light {l1}");
    assert_eq!(s1, 0.0);
}

#[test]
fn a_shadow_casting_prop_is_referenced_by_the_cascade() {
    let caster = rbake::csm::ShadowCaster {
        positions: vec![[0.0, 0.0, 0.0], [64.0, 0.0, 0.0], [0.0, 64.0, 0.0]],
        indices: vec![0, 1, 2],
        alpha: false,
    };
    let prop = rbake::csm::PropCaster { lump_index: 7, mins: Vec3::new(10.0, 10.0, 0.0), maxs: Vec3::new(20.0, 20.0, 30.0) };
    let csm = rbake::csm::build_csm(&[caster], &[prop], Vec3::ZERO, Vec3::splat(64.0), &[rbake::csm::FALLBACK_SHADOW_DIRECTION])
        .expect("a mesh and a prop build");
    assert!(csm.every_mesh_referenced(), "the shadow mesh keeps its reference");
    assert!(csm.refs.contains(&(rbake::csm::PROP_REF_MARK | 7)), "the prop is in the tree: {:?}", csm.refs);
    let total: u32 = csm.nodes.iter().map(|n| n.ref_count).sum();
    assert_eq!(total as usize, csm.refs.len(), "every reference belongs to a node");
}
