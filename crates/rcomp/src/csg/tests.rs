//! Brush CSG tests: the coplanar-face rules and the T-junction pass.

use glam::{DVec3, DMat4};
use rdoc::brush::box_brush;
use rdoc::doc::{new_object, Document};
use rdoc::model::{Id, MapDoc, Object, ObjectKind};
use glam::Vec3;
use rir::AssetTables;
use uuid::Uuid;

use super::*;

fn add_brush(doc: &mut Document, brush: rdoc::model::Brush) -> Object {
    let layer = doc.layer_ids()[0];
    let mut o = new_object("box", layer, ObjectKind::Brush(brush));
    o.id = Uuid::new_v4();
    doc.apply(rdoc::Command::AddObjects { objects: vec![o.clone()] }).unwrap();
    o
}

fn entity(classname: &str) -> rdoc::model::Entity {
    rdoc::model::Entity {
        classname: classname.into(),
        keyvalues: Default::default(),
        partition: None,
        links: Default::default(),
    }
}

fn world_of(objects: &[Object]) -> WorldMap {
    let mut m = WorldMap::new();
    for o in objects {
        m.insert(o.id, DMat4::IDENTITY);
    }
    m
}

#[test]
fn csg_two_boxes_shared_face_removed() {
    // Two 64-unit boxes stacked face to face: their shared plane carries two
    // opposing faces, and neither may survive.
    let lower = box_brush(DVec3::ZERO, DVec3::new(64.0, 64.0, 64.0), "world/wall").unwrap();
    let upper = box_brush(DVec3::new(0.0, 0.0, 64.0), DVec3::new(64.0, 64.0, 128.0), "world/wall").unwrap();
    let mut doc = Document::empty();
    let objects = vec![add_brush(&mut doc, lower), add_brush(&mut doc, upper)];
    let faces = build_faces(&doc.doc, &world_of(&objects)).unwrap();

    assert!(!faces.is_empty(), "the boxes keep their outer faces");
    for f in &faces {
        let shared = (f.plane.normal.dot(DVec3::Z).abs() > 0.999)
            && (f.plane.dist.abs() - 64.0).abs() < 1e-6;
        assert!(!shared, "a face survived on the shared plane: {:?} at {}", f.plane.normal, f.plane.dist);
    }
    // Four sides and two caps are left.
    assert_eq!(faces.len(), 10, "two boxes of 6 faces lose 2 shared faces");
    // Nothing of either box reaches into the other.
    for f in &faces {
        let on_shared = (f.plane.normal.dot(DVec3::Z).abs() > 0.999) && (f.plane.dist.abs() - 64.0).abs() < 1e-6;
        assert!(!on_shared);
    }
}

#[test]
fn csg_overlap_clips_the_inner_face() {
    // A small box entirely inside a big one: the big box's faces are all
    // outside it and survive, the small box loses every face.
    let big = box_brush(DVec3::ZERO, DVec3::splat(256.0), "world/wall").unwrap();
    let small = box_brush(DVec3::splat(64.0), DVec3::splat(192.0), "world/wall").unwrap();
    let mut doc = Document::empty();
    let objects = vec![add_brush(&mut doc, big), add_brush(&mut doc, small)];
    let faces = build_faces(&doc.doc, &world_of(&objects)).unwrap();
    assert_eq!(faces.len(), 6, "only the outer box has visible faces");
    for f in &faces {
        for p in &f.points {
            let on_shell = [0.0f64, 256.0].iter().any(|v| (p.x - v).abs() < 1e-6)
                || [0.0f64, 256.0].iter().any(|v| (p.y - v).abs() < 1e-6)
                || [0.0f64, 256.0].iter().any(|v| (p.z - v).abs() < 1e-6);
            assert!(on_shell, "point {p:?} is not on the outer shell");
        }
    }
}

#[test]
fn csg_solidity_orders_the_clip() {
    // A detail brush is clipped by ordinary geometry but never clips it, so
    // the solid box keeps all six faces while the detail box is cut back.
    let mut detail = box_brush(DVec3::splat(32.0), DVec3::splat(96.0), "world/wall").unwrap();
    detail.detail = true;
    let solid = box_brush(DVec3::ZERO, DVec3::splat(64.0), "world/wall").unwrap();
    let mut doc = Document::empty();
    let a = add_brush(&mut doc, detail);
    let b = add_brush(&mut doc, solid);
    let faces = build_faces(&doc.doc, &world_of(&[a.clone(), b.clone()])).unwrap();
    let detail_faces = faces.iter().filter(|f| f.object == Some(a.id)).count();
    let solid_faces = faces.iter().filter(|f| f.object == Some(b.id)).count();
    assert_eq!(solid_faces, 6, "a detail brush does not clip solid geometry");
    assert!(detail_faces > 0, "the part of the detail box outside the solid survives");
    for f in faces.iter().filter(|f| f.object == Some(a.id)) {
        for p in &f.points {
            assert!(
                p.x >= 64.0 || p.y >= 64.0 || p.z >= 64.0,
                "a detail face {p:?} still reaches into the solid box"
            );
        }
    }
}

#[test]
fn csg_tjunction_fixed() {
    // A slab with a pillar crossing it: the slab's clipped face has corners
    // strictly inside the pillar's taller side faces unless the edges split.
    let mut doc = Document::empty();
    let objects = vec![
        add_brush(&mut doc, box_brush(DVec3::ZERO, DVec3::new(128.0, 16.0, 128.0), "m").unwrap()),
        add_brush(&mut doc, box_brush(DVec3::new(32.0, 16.0, -64.0), DVec3::new(128.0, 64.0, 16.0), "m").unwrap()),
    ];
    let worlds = world_of(&objects);
    let raw = build_faces_raw(&doc.doc, &worlds).unwrap();
    assert!(!raw.is_empty());
    let lying = count_lying(&raw);
    assert!(lying > 0, "the two boxes really do leave {lying} lying vertices");
    let mut faces = raw.clone();
    fix_tjunctions(&mut faces);
    assert_eq!(count_lying(&faces), 0, "the pass split every edge that needed it");
    assert!(faces.iter().map(|f| f.points.len()).sum::<usize>() > raw.iter().map(|f| f.points.len()).sum::<usize>());
    // The pass only adds vertices; no face moves or disappears.
    assert_eq!(faces.len(), raw.len());
    for (a, b) in raw.iter().zip(faces.iter()) {
        assert_eq!(a.plane, b.plane);
        assert_eq!(a.material, b.material);
        for p in &a.points {
            assert!(b.points.iter().any(|q| (*q - *p).length() < 1e-9), "a corner moved");
        }
    }
    assert_eq!(count_lying(&build_faces(&doc.doc, &worlds).unwrap()), 0);
}

#[test]
fn csg_tjunction_diagonal_edge_is_split_quickly() {
    // A long diagonal wall touching a box corner mid-edge: the old fine-grid
    // search walked the edge's whole bounding box (~1e10 cells) and never ended.
    let mut doc = Document::empty();
    let wall = {
        let planes = [
            (DVec3::new(0.7071, -0.7071, 0.0), 0.0),
            (DVec3::new(-0.7071, 0.7071, 0.0), 16.0 * 0.7071),
            (DVec3::new(0.7071, 0.7071, 0.0), 2500.0 * 1.4142),
            (DVec3::new(-0.7071, -0.7071, 0.0), 0.0),
            (DVec3::Z, 64.0),
            (DVec3::NEG_Z, 0.0),
        ];
        let faces = planes
            .iter()
            .map(|(n, d)| rdoc::brush::BrushFace { normal: n.normalize(), dist: *d, material: "m".into(), ..Default::default() })
            .collect();
        rdoc::brush::Brush { faces, contents: 0, detail: false }
    };
    let objects = vec![
        add_brush(&mut doc, wall),
        add_brush(&mut doc, box_brush(DVec3::new(1200.0, 1200.0, 64.0), DVec3::new(1300.0, 1300.0, 128.0), "m").unwrap()),
    ];
    let worlds = world_of(&objects);
    let started = std::time::Instant::now();
    let faces = build_faces(&doc.doc, &worlds).unwrap();
    assert!(started.elapsed().as_secs() < 5, "fix_tjunctions took {:?}", started.elapsed());
    assert_eq!(count_lying(&faces), 0);
}

/// Vertices of the whole face list that sit strictly inside another face's edge.
fn count_lying(faces: &[Face]) -> usize {
    let all: Vec<DVec3> = faces.iter().flat_map(|f| f.points.iter().copied()).collect();
    let grid = VertexGrid::new(&all);
    let mut n = 0;
    for f in faces {
        for i in 0..f.points.len() {
            let a = f.points[i];
            let b = f.points[(i + 1) % f.points.len()];
            for p in grid.on_segment(a, b) {
                let t = (p - a).dot(b - a);
                let len2 = (b - a).length_squared();
                if t > 0.0 && t < len2 {
                    n += 1;
                }
            }
        }
    }
    n
}

#[test]
fn csg_surfaces_group_by_model_and_material() {
    let mut doc = Document::empty();
    let objects = vec![
        add_brush(&mut doc, box_brush(DVec3::ZERO, DVec3::splat(64.0), "world/wall").unwrap()),
        add_brush(&mut doc, box_brush(DVec3::splat(128.0), DVec3::splat(192.0), "world/floor").unwrap()),
    ];
    let geo = build_geometry(&doc.doc, &world_of(&objects)).unwrap();
    assert_eq!(geo.models.len(), 1, "world brushes share model 0");
    assert_eq!(geo.surfaces.len(), 2, "one surface per material");
    for s in &geo.surfaces {
        assert_eq!(s.positions.len(), s.normals.len());
        assert_eq!(s.positions.len(), s.uv0.len());
        assert_eq!(s.positions.len(), s.uv1.len());
        assert_eq!(s.indices.len() % 3, 0);
        assert!(!s.indices.is_empty());
        // uv1 is a chart coordinate.
        for uv in &s.uv1 {
            assert!((-0.01..=1.01).contains(&uv.x) && (-0.01..=1.01).contains(&uv.y), "uv1 {uv:?} off chart");
        }
    }
    assert_eq!(geo.solids.len(), 2, "every brush keeps collision");
    let (lo, hi) = geo.bounds();
    assert!(lo.length() < 1e-6, "mins {lo:?}");
    assert!((hi - Vec3::splat(192.0)).length() < 1e-3, "maxs {hi:?}");
}

#[test]
fn csg_winding_opposes_the_vertex_normal() {
    let mut doc = Document::empty();
    let objects = vec![add_brush(&mut doc, box_brush(DVec3::ZERO, DVec3::splat(64.0), "world/wall").unwrap())];
    let geo = build_geometry(&doc.doc, &world_of(&objects)).unwrap();
    let s = &geo.surfaces[0];
    let mut checked = 0;
    for tri in s.indices.chunks_exact(3) {
        let p: Vec<DVec3> = tri.iter().map(|&i| s.positions[i as usize].as_dvec3()).collect();
        let n = (p[1] - p[0]).cross(p[2] - p[0]);
        if n.length() < 1e-9 {
            continue;
        }
        let vn = s.normals[tri[0] as usize].as_dvec3();
        assert!(n.dot(vn) < 0.0, "triangle {p:?} winds with the vertex normal");
        checked += 1;
    }
    assert_eq!(checked, 12, "a box is six quads, so twelve triangles");
}

#[test]
fn csg_brush_entities_and_volumes_get_their_own_model() {
    let mut doc = Document::empty();
    let layer = doc.layer_ids()[0];
    let mut world = new_object("wall", layer, ObjectKind::Brush(
        box_brush(DVec3::ZERO, DVec3::splat(64.0), "world/wall").unwrap(),
    ));
    world.id = Uuid::new_v4();
    let mut ent = new_object(
        "door",
        layer,
        ObjectKind::Entity(entity("func_door")),
    );
    ent.id = Uuid::new_v4();
    let mut child = new_object("door_brush", layer, ObjectKind::Brush(
        box_brush(DVec3::ZERO, DVec3::splat(32.0), "world/door").unwrap(),
    ));
    child.id = Uuid::new_v4();
    child.parent = Some(ent.id);
    let mut vol = new_object(
        "trig",
        layer,
        ObjectKind::Volume(rdoc::model::Volume {
            kind: rdoc::model::VolumeKind::Trigger,
            classname: "trigger_multiple".into(),
            brushes: vec![box_brush(DVec3::ZERO, DVec3::splat(16.0), "tools/trigger").unwrap()],
            keyvalues: Default::default(),
        }),
    );
    vol.id = Uuid::new_v4();
    let ent_id = ent.id;
    let child_id = child.id;
    let vol_id = vol.id;
    doc.apply(rdoc::Command::AddObjects { objects: vec![world, ent, child, vol] }).unwrap();
    let doc: MapDoc = doc.doc.clone();
    let (map, models) = model_assignment(&doc);
    assert_eq!(models.len(), 3, "world, one brush entity, one volume");
    assert_eq!(map.get(&ent_id), Some(&1));
    assert_eq!(map.get(&vol_id), Some(&2));
    let all = object_model(&doc);
    assert_eq!(all.get(&child_id), Some(&1), "a child brush follows its entity's model");
    let worlds = WorldMap::new();
    let geo = build_geometry(&doc, &worlds).unwrap();
    assert_eq!(geo.faces.iter().filter(|f| f.model == 1).count(), 6);
    assert_eq!(geo.faces.iter().filter(|f| f.model == 2).count(), 6);
    assert_eq!(geo.solids.iter().filter(|s| s.model == 2).count(), 1);
    // The volume is a mesh-less model.
    assert!(geo.models[2].has_render, "a volume brush still draws");
    assert_eq!(geo.surfaces.iter().filter(|s| s.model == 2).count(), 0, "tools/ is nodraw");
    assert_eq!(geo.surfaces.iter().filter(|s| s.model == 1).count(), 1);
}

#[test]
fn csg_rejects_a_broken_brush() {
    let bad = rdoc::model::Brush { faces: Vec::new(), contents: 0, detail: false };
    let mut doc = Document::empty();
    add_brush(&mut doc, bad);
    let err = build_faces(&doc.doc, &WorldMap::new()).unwrap_err();
    assert!(matches!(err, CsgError::Brush { .. }), "{err}");
}

#[test]
fn csg_rejects_a_non_finite_transform() {
    let mut doc = Document::empty();
    let objects = vec![add_brush(&mut doc, box_brush(DVec3::ZERO, DVec3::splat(64.0), "m").unwrap())];
    let mut worlds = world_of(&objects);
    let id = objects[0].id;
    worlds.insert(id, DMat4::from_scale_rotation_translation(
        DVec3::new(f64::NAN, 1.0, 1.0),
        glam::DQuat::IDENTITY,
        DVec3::ZERO,
    ));
    assert!(matches!(build_faces(&doc.doc, &worlds), Err(CsgError::Transform(_))));
}

#[test]
fn csg_object_model_matches_the_slot_table() {
    let mut doc = Document::empty();
    let objects = vec![add_brush(&mut doc, box_brush(DVec3::ZERO, DVec3::splat(64.0), "m").unwrap())];
    let map = object_model(&doc.doc);
    let id: Id = objects[0].id;
    assert_eq!(map.get(&id), Some(&0));
}

#[test]
fn csg_nodraw_material_leaves_collision() {
    let mut doc = Document::empty();
    let objects = vec![add_brush(&mut doc, box_brush(DVec3::ZERO, DVec3::splat(64.0), "tools/clip").unwrap())];
    let faces = build_faces(&doc.doc, &world_of(&objects)).unwrap();
    assert_eq!(faces.len(), 6, "the faces exist");
    let surfaces = build_surfaces(&faces, &AssetTables::default(), DEFAULT_LIGHTMAP_SCALE).unwrap();
    assert!(surfaces.is_empty(), "a tools/ material draws nothing");
    let geo = build_geometry(&doc.doc, &world_of(&objects)).unwrap();
    assert_eq!(geo.solids.len(), 1, "but it still collides");
}

#[test]
fn csg_surfaces_reject_a_broken_lightmap_scale() {
    let mut doc = Document::empty();
    let mut b = box_brush(DVec3::ZERO, DVec3::splat(64.0), "world/wall").unwrap();
    for f in b.faces.iter_mut() {
        f.lightmap_scale = f64::NAN;
    }
    let objects = vec![add_brush(&mut doc, b)];
    let faces = build_faces(&doc.doc, &world_of(&objects)).unwrap();
    let group: Vec<&Face> = faces.iter().collect();
    let mut s = triangulate_group(&group, 0, "world/wall", VertexFormat::LitBump, None);
    let scales: Vec<f64> = group.iter().map(|_| 1.0e-9).collect();
    assert!(matches!(chart_surface(&mut s, &scales), Err(CsgError::ChartTooBig { .. })));
    // A sane scale charts the same surface without complaint.
    let scales: Vec<f64> = group.iter().map(|_| DEFAULT_LIGHTMAP_SCALE).collect();
    assert!(chart_surface(&mut s, &scales).is_ok());
}



fn mesh_surface(positions: Vec<Vec3>, normals: Vec<Vec3>, indices: Vec<u32>) -> RenderSurface {
    let n = positions.len();
    RenderSurface {
        material: "world/dev/dev_gray_512".into(),
        positions,
        normals,
        tangents: vec![glam::Vec4::W; n],
        uv0: vec![Vec2::ZERO; n],
        uv1: vec![Vec2::ZERO; n],
        colors: vec![[255; 4]; n],
        indices,
        lightmap_texel: 16.0,
        ..RenderSurface::default()
    }
}

#[test]
fn a_mesh_grid_charts_as_one_sheet() {
    let side = 20u32;
    let mut positions = Vec::new();
    for y in 0..=side {
        for x in 0..=side {
            positions.push(Vec3::new(x as f32 * 64.0, y as f32 * 64.0, ((x * 7 + y * 3) % 5) as f32 * 4.0));
        }
    }
    let mut indices = Vec::new();
    for y in 0..side {
        for x in 0..side {
            let i = y * (side + 1) + x;
            indices.extend_from_slice(&[i, i + side + 1, i + 1, i + 1, i + side + 1, i + side + 2]);
        }
    }
    let normals = vec![Vec3::Z; positions.len()];
    let mut s = mesh_surface(positions, normals, indices);
    chart_mesh_surface(&mut s, 16.0).unwrap();
    // One sheet: no vertex is split, and the chart is the grid's own extent
    // plus padding rather than one padded rect per triangle.
    assert_eq!(s.positions.len(), 441);
    let [w, h] = s.lightmap_size;
    assert!(w as u64 * h as u64 <= 128 * 128, "{w}x{h}");
    assert!(s.uv1.iter().all(|u| (0.0..=1.0).contains(&u.x) && (0.0..=1.0).contains(&u.y)));
}

#[test]
fn a_mesh_fold_splits_its_shared_edge() {
    // A floor and a wall meeting along x = 0..64 at y = 64.
    let positions = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(64.0, 0.0, 0.0),
        Vec3::new(0.0, 64.0, 0.0),
        Vec3::new(64.0, 64.0, 0.0),
        Vec3::new(0.0, 64.0, 64.0),
        Vec3::new(64.0, 64.0, 64.0),
    ];
    let normals = vec![Vec3::Z, Vec3::Z, Vec3::Z, Vec3::Z, Vec3::NEG_Y, Vec3::NEG_Y];
    let indices = vec![0, 2, 1, 1, 2, 3, 2, 4, 3, 3, 4, 5];
    let mut s = mesh_surface(positions, normals, indices);
    chart_mesh_surface(&mut s, 16.0).unwrap();
    assert_eq!(s.positions.len(), 8, "the two shared vertices belong to both charts");
    assert_eq!(s.indices.len(), 12);
    // No two triangles of different charts share a lightmap texel.
    let floor: Vec<Vec2> = s.indices[..6].iter().map(|&i| s.uv1[i as usize]).collect();
    let wall: Vec<Vec2> = s.indices[6..].iter().map(|&i| s.uv1[i as usize]).collect();
    let bounds = |v: &[Vec2]| v.iter().fold((Vec2::splat(9.0), Vec2::splat(-9.0)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    let (fl, fh) = bounds(&floor);
    let (wl, wh) = bounds(&wall);
    assert!(fh.x <= wl.x || wh.x <= fl.x || fh.y <= wl.y || wh.y <= fl.y);
}

#[test]
fn a_sky_brushs_nodraw_backs_cast_no_shadow() {
    let face = |object: Uuid, material: &str| Face {
        object: Some(object),
        model: 0,
        material: material.into(),
        plane: DPlane::new(DVec3::Z, 0.0),
        points: vec![DVec3::ZERO, DVec3::X, DVec3::Y],
        uv: UvMapping::default(),
        lightmap_scale: 16.0,
        contents: 0,
        surface_prop: String::new(),
    };
    let (sky, wall) = (Uuid::new_v4(), Uuid::new_v4());
    let faces = [
        face(sky, "tools/toolsskybox"),
        face(sky, "tools/toolsnodraw"),
        face(wall, "tools/toolsnodraw"),
    ];
    let batches = shadow_only_faces(&faces);
    let tris: usize = batches.iter().map(|(_, i)| i.len() / 3).sum();
    assert_eq!(tris, 1, "only the plain wall's nodraw face casts");
}
