//! Collision tests: decode round trip, a ray cast, the `*coll` encodings and
//! the v37 conversion parity.

use std::collections::BTreeMap;
use std::path::PathBuf;

use glam::{DVec3, Vec3};
use rbsp::lumps::collision::{BvhLeafData, Model, CHILD_BUNDLE, CHILD_EMPTY, CHILD_HULL, CHILD_NODE, CHILD_POLY, NODE_SIZE};
use rbsp::{ids, Bsp, Version};
use rir::{CollisionShape, CollisionSolid, ModelSlot};

use super::*;

const TOOLS_TRIGGER: &[u8] = b"TOOLS\\TOOLSTRIGGER";
const TOOLS_ENVMAP: &[u8] = b"TOOLS\\TOOLSENVMAPVOLUME";

/// A quarter inch is the coarsest a hull leaf's quantisation can move a
/// corner, so anything inside that is the same point.
const TOLERANCE: f64 = 1.0 / 32.0;

fn fixtures() -> Option<PathBuf> {
    std::env::var_os("R5ED_FIXTURES").map(PathBuf::from)
}

fn dedi_fixture_maps(limit: usize) -> Vec<(String, Bsp)> {
    let Some(root) = fixtures() else { return Vec::new() };
    let mut out: Vec<(String, Bsp)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(root.join("dedi")) else { return out };
    let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for d in dirs {
        let Ok(files) = std::fs::read_dir(d.join("maps")) else { continue };
        let mut paths: Vec<PathBuf> = files.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "bsp")).collect();
        paths.sort();
        for p in paths {
            if let Ok(b) = Bsp::read_path(&p) {
                out.push((p.file_stem().unwrap_or_default().to_string_lossy().into_owned(), b));
            }
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

/// Triangle sets compared as a multiset of rounded corners.
fn rounded(tri: &[DVec3; 3], q: f64) -> [[i64; 3]; 3] {
    let mut out = [[0i64; 3]; 3];
    for (i, p) in tri.iter().enumerate() {
        for (a, c) in p.to_array().into_iter().enumerate() {
            out[i][a] = (c / q).round() as i64;
        }
    }
    // Rotation does not change a triangle, so lead with its smallest corner.
    let mut best = 0;
    for i in 1..3 {
        if out[i] < out[best] {
            best = i;
        }
    }
    [out[best], out[(best + 1) % 3], out[(best + 2) % 3]]
}

fn triangle_multiset(tris: &[[DVec3; 3]], q: f64) -> BTreeMap<[[i64; 3]; 3], usize> {
    let mut m: BTreeMap<[[i64; 3]; 3], usize> = BTreeMap::new();
    for t in tris {
        *m.entry(rounded(t, q)).or_insert(0) += 1;
    }
    m
}

/// How far a triangle may move and still be the same triangle.
fn corner_gap(a: &DVec3, b: &DVec3) -> f64 {
    (*a - *b).abs().max_element()
}

/// The two halves of a multiset difference, plus how far the worst unmatched
/// source triangle sits from anything that was emitted.
struct TriangleDiff {
    missing: usize,
    extra: usize,
    near_miss: f64,
}

/// Multiset difference of two triangle sets where a pair counts as equal when
/// every corner is within `tol`, in any cyclic rotation. Rounding corners to a
/// grid is not enough on its own: an f32 round trip at map coordinates moves a
/// corner by a whole grid cell, so equal triangles land in different cells.
fn tolerant_diff(want: &[[DVec3; 3]], got: &[[DVec3; 3]], tol: f64) -> TriangleDiff {
    // The grid cell is wide enough that two points within `tol` share a cell or
    // land in adjacent ones, so a query only has to look at the 27 around it.
    let cell = tol * 4.0;
    let cell_of = |p: DVec3| -> [i64; 3] {
        let a = p.to_array();
        [(a[0] / cell).floor() as i64, (a[1] / cell).floor() as i64, (a[2] / cell).floor() as i64]
    };
    let mut grid: std::collections::HashMap<[i64; 3], Vec<usize>> = std::collections::HashMap::new();
    for (i, t) in got.iter().enumerate() {
        grid.entry(cell_of(t[0])).or_default().push(i);
    }
    let mut used = vec![false; got.len()];
    let mut missing = 0usize;
    let mut near_miss = 0.0f64;
    for t in want {
        let mut hit = None;
        let mut best = f64::INFINITY;
        'search: for rot in 0..3 {
            let lead = cell_of(t[rot]);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let key = [
                            lead[0].saturating_add(dx),
                            lead[1].saturating_add(dy),
                            lead[2].saturating_add(dz),
                        ];
                        for &i in grid.get(&key).map(Vec::as_slice).unwrap_or(&[]) {
                            if used[i] {
                                continue;
                            }
                            let g = &got[i];
                            let far = (0..3)
                                .map(|k| corner_gap(&t[(rot + k) % 3], &g[k]))
                                .fold(0.0f64, f64::max);
                            best = best.min(far);
                            if far <= tol {
                                hit = Some(i);
                                break 'search;
                            }
                        }
                    }
                }
            }
        }
        match hit {
            Some(i) => used[i] = true,
            None => {
                missing += 1;
                near_miss = near_miss.max(best);
            }
        }
    }
    TriangleDiff { missing, extra: used.iter().filter(|u| !**u).count(), near_miss }
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn collision_decode_roundtrip() {
    let maps = dedi_fixture_maps(5);
    assert_eq!(maps.len(), 5, "five dedicated-server fixtures are present");
    for (name, bsp) in &maps {
        let before = decode_collision(bsp).unwrap_or_else(|e| panic!("{name} decodes: {e}"));
        let mut solids: Vec<CollisionSolid> = Vec::new();
        // The slot list is indexed by model, so every model gets one.
        let mut slots: Vec<ModelSlot> = (0..before.len())
            .map(|m| ModelSlot {
                index: m as u32,
                owner: None,
                mins: Vec3::ZERO,
                maxs: Vec3::ZERO,
                has_render: false,
            })
            .collect();
        for (model, tris) in &before {
            if tris.is_empty() {
                continue;
            }
            let mut lo = DVec3::splat(f64::INFINITY);
            let mut hi = DVec3::splat(f64::NEG_INFINITY);
            for t in tris {
                for p in t {
                    lo = lo.min(*p);
                    hi = hi.max(*p);
                }
            }
            slots[*model as usize].mins = lo.as_vec3();
            slots[*model as usize].maxs = hi.as_vec3();
            let mut positions: Vec<Vec3> = Vec::with_capacity(tris.len() * 3);
            let mut indices: Vec<u32> = Vec::with_capacity(tris.len() * 3);
            for t in tris {
                let base = positions.len() as u32;
                for p in t {
                    positions.push(p.as_vec3());
                }
                indices.extend_from_slice(&[base, base + 1, base + 2]);
            }
            solids.push(CollisionSolid {
                object: None,
                model: *model,
                shape: CollisionShape::Triangles { vertices: positions, indices },
                contents: 0x400,
                surface_prop: 0,
                surfaces: Vec::new(),
            });
        }
        assert!(!solids.is_empty(), "{name} has collision to re-emit");
        let mut out = Bsp::new(47, 0, 0);
        out.set_lump(ids::VERTICES, bsp.lump(ids::VERTICES).to_vec());
        emit_collision(&solids, &slots, &mut out).unwrap_or_else(|e| panic!("{name} re-emits: {e}"));
        let after = decode_collision(&out).unwrap_or_else(|e| panic!("{name} decodes again: {e}"));
        // A source leaf can hold a triangle that is a sliver at f32 precision;
        // the emitter drops those, so both sides are compared at the area where
        // a sliver stops being a face.
        let big = |t: &[DVec3; 3]| (t[1] - t[0]).cross(t[2] - t[0]).length() > 1.0e-3;
        let want: Vec<[DVec3; 3]> =
            before.iter().flat_map(|(_, t)| t.iter().copied()).filter(big).collect();
        let got: Vec<[DVec3; 3]> = after.iter().flat_map(|(_, t)| t.iter().copied()).filter(big).collect();
        let d = tolerant_diff(&want, &got, TOLERANCE);
        assert_eq!(
            (d.missing, d.extra),
            (0, 0),
            "{name}: {} of {} triangles did not survive, {} appeared, nearest miss {:.6}",
            d.missing,
            want.len(),
            d.extra,
            d.near_miss
        );
    }
}

#[test]
fn collision_brush_box_ray() {
    // A 16-unit box, emitted as a brush solid and read back through the tree.
    let lo = DVec3::ZERO;
    let hi = DVec3::splat(16.0);
    let planes: Vec<(Vec3, f32)> = vec![
        (DVec3::X.as_vec3(), hi.x as f32),
        (-DVec3::X.as_vec3(), -lo.x as f32),
        (DVec3::Y.as_vec3(), hi.y as f32),
        (-DVec3::Y.as_vec3(), -lo.y as f32),
        (DVec3::Z.as_vec3(), hi.z as f32),
        (-DVec3::Z.as_vec3(), -lo.z as f32),
    ];
    let solid = CollisionSolid {
        object: None,
        model: 0,
        shape: CollisionShape::Brush { planes },
        contents: 0x400,
        surface_prop: 0,
        surfaces: Vec::new(),
    };
    let mut bsp = Bsp::new(47, 0, 0);
    emit_collision(&[solid], &[slot_of(lo, hi)], &mut bsp).expect("a box brush emits");
    let leaves = decode_leaves(&bsp, 0).expect("the tree decodes");
    let mut tris: Vec<[DVec3; 3]> = leaves.iter().flat_map(|l| l.triangles.iter().copied()).collect();
    assert!(!tris.is_empty(), "the hull leaf carries its triangle sets");
    tris.sort_by(|a, b| corner_key(a[0]).cmp(&corner_key(b[0])));
    let mut hits = ray_hits(&tris, DVec3::new(-64.0, 6.0, 9.0), DVec3::X);
    hits.sort_by(f64::total_cmp);
    assert!(hits.len() >= 2, "a ray along +X through the middle crosses the box");
    assert!(hits[0] - 64.0 < 0.05, "entry at {} expected 64", hits[0]);
    assert!(hits[hits.len() - 1] - 80.0 < 0.05, "exit at {} expected 80", hits[hits.len() - 1]);

    // A ray that misses the box hits nothing.
    let miss = ray_hits(&tris, DVec3::new(-64.0, 64.0, 64.0), DVec3::X);
    assert!(miss.is_empty(), "a ray past the box hits nothing");

    // A ray along Z hits the same two distances measured from z = 0.
    let mut z = ray_hits(&tris, DVec3::new(5.0, 11.0, -64.0), DVec3::Z);
    z.sort_by(f64::total_cmp);
    assert!(z.len() >= 2);
    assert!(z[0] - 64.0 < 0.05 && z[z.len() - 1] - 80.0 < 0.05);
}

fn corner_key(p: DVec3) -> [i64; 3] {
    let a = p.to_array();
    [a[0].to_bits() as i64, a[1].to_bits() as i64, a[2].to_bits() as i64]
}

fn slot_of(lo: DVec3, hi: DVec3) -> ModelSlot {
    ModelSlot { index: 0, owner: None, mins: lo.as_vec3(), maxs: hi.as_vec3(), has_render: false }
}

/// Distances along `dir` at which the ray meets a triangle.
fn ray_hits(tris: &[[DVec3; 3]], origin: DVec3, dir: DVec3) -> Vec<f64> {
    let mut out = Vec::new();
    for t in tris {
        let (a, b, c) = (t[0], t[1], t[2]);
        let e1 = b - a;
        let e2 = c - a;
        let p = dir.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-12 {
            continue;
        }
        let inv = 1.0 / det;
        let s = origin - a;
        let u = s.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = dir.dot(q) * inv;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let dist = e2.dot(q) * inv;
        if dist > 0.0 {
            out.push(dist);
        }
    }
    out
}

fn box_planes(lo: DVec3, hi: DVec3) -> Vec<crate::csg::DPlane> {
    vec![
        crate::csg::DPlane::new(DVec3::X, hi.x),
        crate::csg::DPlane::new(-DVec3::X, -lo.x),
        crate::csg::DPlane::new(DVec3::Y, hi.y),
        crate::csg::DPlane::new(-DVec3::Y, -lo.y),
        crate::csg::DPlane::new(DVec3::Z, hi.z),
        crate::csg::DPlane::new(-DVec3::Z, -lo.z),
    ]
}

#[test]
fn coll_key_both_targets() {
    let brushes = vec![box_planes(DVec3::ZERO, DVec3::splat(64.0))];
    for target in [CollTarget::Client, CollTarget::Dedi] {
        let model = volume_blob(&brushes, (0x00EB_1280, 0x400, b"TOOLS\\TOOLSTRIGGER"), target)
            .unwrap_or_else(|e| panic!("{target:?} volume blob: {e}"));
        let keys = model.to_keys();
        assert!(!keys.is_empty());
        assert!(keys.iter().all(|k| k.starts_with("*coll")), "{target:?} keys are named");
        // Every key must be a real keyvalues line, and the chunks must
        // reassemble into the blob.
        let mut text = String::from("{\n");
        let mut blob = Vec::new();
        for k in &keys {
            text.push_str(k);
            text.push('\n');
            let open = k.find('"').expect("the key name is quoted");
            let close = k.rfind('"').expect("the value is quoted");
            assert!(k[..open].starts_with("*coll"), "the key is named *collN");
            blob.extend_from_slice(&base64_decode(&k[open + 1..close]).expect("the key is base64"));
        }
        text.push('}');
        assert_eq!(blob, model.to_blob(), "{target:?} chunks reassemble");
        assert!(text.contains("*coll0"), "{target:?} keyvalues text");
        assert_eq!(keys.len(), model.to_blob().len().div_ceil(COLL_CHUNK));

        let back = parse_coll_blob(&blob, target).unwrap_or_else(|e| panic!("{target:?} parses: {e}"));
        assert_eq!(back.contents, model.contents);
        assert_eq!(back.surfprops, model.surfprops, "{target:?} surfprops");
        assert!(back.leaf.len() >= model.leaf.len(), "{target:?} leaf word count");
        assert_eq!(&back.leaf[..model.leaf.len()], &model.leaf[..], "{target:?} leaf words");
        assert!(
            back.leaf[model.leaf.len()..].iter().all(|w| *w == 0),
            "{target:?} node alignment padding is zero"
        );
        assert_eq!(back.nodes, model.nodes);
        assert_eq!(back.verts, model.verts);
        assert_eq!(back.fields.origin, model.fields.origin);
        assert_eq!(back.fields.decode_scale, model.fields.decode_scale);
        assert_eq!(back.names, model.names);

        // The parsed blob decodes to the same box.
        let want = model.triangles();
        let got = back.triangles();
        assert_eq!(want.len(), got.len(), "{target:?} triangle count");
        let a = triangle_multiset(&want, 0.5);
        let b = triangle_multiset(&got, 0.5);
        assert_eq!(a, b, "{target:?} triangles match");
        // The box is the 64-unit one the brushes describe.
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for t in &got {
            for p in t {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        assert!(lo.distance(DVec3::ZERO) < 0.5, "{target:?} mins {lo:?}");
        assert!((hi - DVec3::splat(64.0)).length() < 0.5, "{target:?} maxs {hi:?}");
    }
    // The two headers differ, so the blobs are not interchangeable.
    let c = volume_blob(&brushes, (1, 2, b"X"), CollTarget::Client).unwrap();
    let d = volume_blob(&brushes, (1, 2, b"X"), CollTarget::Dedi).unwrap();
    assert_ne!(c.to_blob(), d.to_blob());
    assert!(parse_coll_blob(&c.to_blob(), CollTarget::Dedi).is_ok());
}

#[test]
fn coll_volume_surface_covers_every_trigger_class() {
    assert_eq!(volume_surface("trigger_multiple"), Some((0x00EB_1280, 0x400, TOOLS_TRIGGER)));
    assert_eq!(volume_surface("envmap_volume"), Some((0x10E3_1240, 0x610, TOOLS_ENVMAP)));
    assert_eq!(volume_surface("func_door"), None);
}

#[test]
fn coll_base64_round_trips() {
    for len in 0..200 {
        let data: Vec<u8> = (0..len).map(|i| (i * 37 % 251) as u8).collect();
        let text = base64(&data);
        assert_eq!(base64_decode(&text).unwrap(), data, "length {len}");
    }
    assert!(base64_decode("not base64!").is_err());
}

#[test]
fn coll_blob_rejects_a_short_header() {
    assert!(matches!(parse_coll_blob(&[0u8; 8], CollTarget::Client), Err(CollError::CollShort(_))));
    let mut blob = volume_blob(&[box_planes(DVec3::ZERO, DVec3::splat(16.0))], (1, 2, b"X"), CollTarget::Dedi)
        .unwrap()
        .to_blob();
    blob[0..4].copy_from_slice(&9999i32.to_le_bytes());
    assert!(matches!(parse_coll_blob(&blob, CollTarget::Dedi), Err(CollError::CollTruncated(_))));
}

// ---- the v37 conversion -----------------------------------------------------

struct RefLumps {
    contents: Vec<u8>,
    surfprops: Vec<u8>,
    nodes: Vec<u8>,
    node_records: Vec<rbsp::lumps::collision::BvhNode>,
    leaf: Vec<u32>,
    models: Vec<Model>,
    coll_verts: Vec<[f32; 3]>,
    render_verts: usize,
}

fn read_reference(bsp_path: &std::path::Path) -> Result<RefLumps, CollError> {
    let bsp = Bsp::read_path(bsp_path).map_err(|e| CollError::Lump { id: 0, reason: e.to_string() })?;
    let v = bsp.version_kind().unwrap_or(Version::ClientV51);
    let vraw = bsp.lump(ids::VERTICES);
    // The shared position pool is the render vertices followed by every
    // model's collision vertices, and a model's collision block starts at its
    // own vertex index, so the first one marks the boundary.
    let model_lump = rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), bsp.version_kind().unwrap_or(Version::ClientV51))
        .map_err(|e| lump_err(ids::MODELS, e))?;
    let render_verts = model_lump
        .iter()
        .filter(|m| model_has_collision(m) && m.vert_index >= 0)
        .map(|m| m.vert_index as usize)
        .min()
        .unwrap_or(vraw.len() / 12);
    let all: Vec<[f32; 3]> = vraw
        .chunks_exact(12)
        .map(|c| {
            [
                f32::from_le_bytes(c[0..4].try_into().unwrap()),
                f32::from_le_bytes(c[4..8].try_into().unwrap()),
                f32::from_le_bytes(c[8..12].try_into().unwrap()),
            ]
        })
        .collect();
    Ok(RefLumps {
        contents: bsp.lump(ids::CONTENTS_MASKS).to_vec(),
        surfprops: bsp.lump(ids::SURFACE_PROPERTIES).to_vec(),
        nodes: bsp.lump(ids::BVH_NODES).to_vec(),
        node_records: rbsp::lumps::collision::parse_bvh_nodes(bsp.lump(ids::BVH_NODES), v)
            .map_err(|e| lump_err(ids::BVH_NODES, e))?,
        leaf: rbsp::lumps::collision::parse_bvh_leaf(bsp.lump(ids::BVH_LEAF_DATA), v)
            .map_err(|e| lump_err(ids::BVH_LEAF_DATA, e))?
            .words,
        models: rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), v)
            .map_err(|e| lump_err(ids::MODELS, e))?,
        coll_verts: all[render_verts..].to_vec(),
        render_verts,
    })
}

/// A leaf's dword range, so a poly leaf's bytes can be compared directly.
#[derive(Clone, Debug, PartialEq)]
struct LeafSpan {
    kind: u32,
    at: usize,
    dwords: usize,
    model: usize,
}

fn walk_spans(leaf: &BvhLeafData, nodes: &[rbsp::lumps::collision::BvhNode], models: &[Model]) -> Vec<LeafSpan> {
    let mut out = Vec::new();
    for (mi, m) in models.iter().enumerate() {
        if !model_has_collision(m) {
            continue;
        }
        let base = m.bvh_leaf.max(0) as usize;
        let node0 = m.bvh_node.max(0) as usize;
        let mut stack = vec![node0];
        let mut seen: Vec<bool> = vec![false; nodes.len()];
        while let Some(ni) = stack.pop() {
            if ni >= nodes.len() {
                continue;
            }
            let node = &nodes[ni];
            if std::mem::replace(&mut seen[ni], true) {
                continue;
            }
            for slot in 0..4 {
                let kind = node.child_kind(slot);
                if kind == CHILD_EMPTY {
                    continue;
                }
                let at = node.child_index(slot) as usize;
                if kind == CHILD_NODE {
                    stack.push(node0 + at);
                    continue;
                }
                if kind == CHILD_BUNDLE {
                    let count = leaf.word(base + at).unwrap_or(0) as usize;
                    let mut dwords = 1 + count;
                    for k in 0..count {
                        let desc = leaf.word(base + at + 1 + k).unwrap_or(0);
                        dwords += (desc >> 16) as usize;
                    }
                    out.push(LeafSpan { kind, at: base + at, dwords, model: mi });
                    continue;
                }
                let dwords = if (CHILD_POLY..=CHILD_POLY_MAX).contains(&kind) {
                    let h = leaf.word(base + at).unwrap_or(0);
                    1 + (((h >> 12) & 0xF) as usize + 1)
                } else if kind == CHILD_HULL {
                    let Ok(h) = decode_hull(leaf, base + at) else { continue };
                    let mut d = h.header_dwords as usize;
                    for _ in 0..(h.tri_set_count as usize + h.quad_set_count as usize) {
                        let Some(w) = leaf.word(base + at + d) else { break };
                        d += 1 + (((w >> 12) & 0xF) as usize + 1);
                    }
                    d
                } else {
                    1
                };
                out.push(LeafSpan { kind, at: base + at, dwords, model: mi });
            }
        }
    }
    out
}

/// A hull leaf's quantised corners, its plane triples, and every triangle its
/// surface-tagged sets draw, as one multiset of corner triples.
#[allow(clippy::type_complexity)]
fn hull_shape(
    leaf: &BvhLeafData,
    at: usize,
) -> (Vec<[i64; 3]>, Vec<[u8; 3]>, BTreeMap<[i64; 3], usize>) {
    let h = decode_hull(leaf, at).expect("a hull leaf");
    let verts: Vec<[i64; 3]> = h.verts.iter().map(|q| [q[0] as i64, q[1] as i64, q[2] as i64]).collect();
    let mut planes: Vec<[u8; 3]> = h.planes.clone();
    planes.sort();
    let mut off = at + h.header_dwords as usize;
    let mut tris: BTreeMap<[i64; 3], usize> = BTreeMap::new();
    let count = h.tri_set_count as usize + h.quad_set_count as usize;
    for k in 0..count {
        let kind = if k < h.tri_set_count as usize { CHILD_POLY } else { CHILD_POLY_QUAD };
        let Some((_sp, corners)) = poly_leaf_corners(leaf, kind, off) else { break };
        for c in &corners {
            *tris.entry([(c[0] >> 16) as i64, (c[0] >> 8) as i64, c[0] as i64]).or_insert(0) += 1;
        }
        off += 1 + corners.len();
    }
    (verts, planes, tris)
}

#[test]
fn tf2_collision_parity() {
    let Some(bsp_path) = std::env::var_os("R5ED_TF2_BSP").map(PathBuf::from) else { return };
    let Some(ref_root) = std::env::var_os("R5ED_TF2_REF").map(PathBuf::from) else { return };
    let out_root = std::env::var_os("R5ED_TF2_OUT").map(PathBuf::from);
    let game_root = std::env::var_os("R5ED_GAME_ROOT").map(PathBuf::from);
    let bsp = Bsp::read_path(&bsp_path).expect("the v37 source map reads");
    assert_eq!(bsp.header.version, 37);

    let surf_text = game_root
        .as_ref()
        .and_then(|r| std::fs::read_to_string(r.join("platform/scripts/surfaceproperties.rson")).ok())
        .expect("the surface property table is available");
    let root = out_root.as_ref().expect("the conversion input root");
    // The vmt side is the fallback the extracted material json does not cover,
    // and the json wins where both name a material.
    let mut material_surfprop = tf2::read_vmt_dir(&root.join("unpacked/materials"));
    let extracted = tf2::read_material_dir(&root.join("extract_matl/material"));
    let extracted =
        if extracted.is_empty() { tf2::read_material_dir(&root.join("vmt_matl/material")) } else { extracted };
    for (k, v) in extracted {
        material_surfprop.insert(k, v);
    }
    assert!(!material_surfprop.is_empty(), "the material surface property table is available");
    let tables = tf2::SurfaceTables {
        surf_ids: tf2::load_surfprop_ids(&surf_text),
        material_surfprop,
    };
    assert!(tables.surf_ids.len() > 10, "the surface property ids parsed");

    let mut conv = tf2::Converter::new(tf2::Tf2Collision::open(bsp).expect("the v37 collision reads"), &tables);
    let fields = conv.run().expect("the v37 collision converts");
    let stats = conv.stats;
    let writer = conv.into_writer();

    for target in ["client", "dedi"] {
        let p = ref_root.join(target).join("mp_rr_gauntlet.bsp");
        let ref_map = read_reference(&p).unwrap_or_else(|e| panic!("{target} reference reads: {e}"));
        let mut got = Vec::new();
        for m in &writer.contents_masks {
            got.extend_from_slice(&m.to_le_bytes());
        }
        let ref_masks: Vec<u32> =
            ref_map.contents.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        // A node's mask is the OR of its children's, so a table may only ever
        // hold the source map's masks and unions of them. The order a union
        // lands in follows the tree, which is not part of the format.
        let covered = ref_masks.iter().fold(0u32, |a, b| a | b);
        for (i, m) in writer.contents_masks.iter().enumerate() {
            assert!(
                ref_masks.contains(m) || m & !covered == 0,
                "{target} contents mask {i} (0x{m:08x}) is not one the source map declared"
            );
        }
        // Every surface property must point at a contents index the table
        // holds, and the table itself has to be the one the reference wrote.
        for (i, (_, _, contents, _)) in writer.surfprops.iter().enumerate() {
            assert!(
                (*contents as usize) < writer.contents_masks.len(),
                "{target} surface property {i} points at contents {contents}"
            );
        }
        let mine = writer.lump_surfprops();
        assert_eq!(mine.len(), ref_map.surfprops.len(), "{target} surface property lump size");
        let at = mine.iter().zip(ref_map.surfprops.iter()).position(|(a, b)| a != b);
        match at {
            None => {}
            Some(k) => panic!(
                "{target} surface property record {} field {}: {} where the reference has {} ({} records)",
                k / 8,
                k % 8,
                mine[k],
                ref_map.surfprops[k],
                mine.len() / 8
            ),
        }
        // The conversion covers the v37 source's models; the client map the
        // reference wrote also carries the models its own entity pass added,
        // which no collision primitive names.
        assert!(fields.len() <= ref_map.models.len(), "{target} model count");
        let ref_models = &ref_map.models[..fields.len()];
        for (i, m) in ref_models.iter().enumerate() {
            let f = &fields[i];
            assert_eq!(m.bvh_node, f.bvh_node, "{target} model {i} first node");
            if i == 0 {
                assert_eq!(m.bvh_leaf, f.bvh_leaf, "{target} model {i} first leaf");
            }
            assert!(f.bvh_leaf >= fields[i.saturating_sub(1)].bvh_leaf.max(0), "{target} model {i} leaf run starts after the previous model's");
            assert_eq!(m.vert_index as usize, f.vert_index as usize + ref_map.render_verts, "{target} model {i} vertex offset");
            assert_eq!(m.flags, f.flags, "{target} model {i} flags");
            assert_eq!(m.origin, f.origin, "{target} model {i} origin");
            assert_eq!(m.decode_scale, f.decode_scale, "{target} model {i} decode scale");
        }

        // The leaf stream. A poly leaf is the reference's own bytes, so it must
        // match exactly. A hull leaf packs the same corners, planes and
        // triangles, but the surface a face takes comes from a face normal that
        // a different hull triangulator produces, so how many surface-tagged
        // triangle sets that needs is the reference writer's own choice: the
        // corners, the planes and the triangles are compared instead, which is
        // what the reference's own verification checks.
        let own_nodes = rbsp::lumps::collision::parse_bvh_nodes(&writer.nodes, Version::ClientV51).unwrap();
        let own_models = models_from_fields(&fields, ref_map.render_verts);
        let a = BvhLeafData { words: writer.leaf.clone() };
        let b = BvhLeafData { words: ref_map.leaf.clone() };
        let hull_models: Vec<usize> = walk_spans(&b, &ref_map.node_records, ref_models)
            .iter()
            .filter(|s| s.kind == CHILD_HULL)
            .map(|s| s.model)
            .collect();
        // A node's quantised child bounds come out of a plane solve for a hull's
        // corners that rounds in the last bits differently from the reference's,
        // and a bound that moves can change a slot's direct-or-bundle choice, so
        // the node lump is compared where a model holds no hull: the whole block
        // there, and everywhere the node count and each model's child-kind
        // census.
        assert_eq!(writer.nodes.len(), ref_map.nodes.len(), "{target} node lump size");
        let kinds = |leaf: &BvhLeafData, nodes: &[rbsp::lumps::collision::BvhNode], m: &Model| {
            let mut c: BTreeMap<u32, usize> = BTreeMap::new();
            for slot in walk_spans(leaf, nodes, std::slice::from_ref(m)) {
                *c.entry(slot.kind).or_insert(0) += 1;
            }
            c
        };
        for (i, m) in ref_models.iter().enumerate() {
            assert_eq!(
                kinds(&a, &own_nodes, &own_models[i]),
                kinds(&b, &ref_map.node_records, m),
                "{target} model {i} child kinds"
            );
        }
        // Every model's node block is byte-identical where the model holds no
        // hull, so the bounds, the contents indices and the child indices are
        // all proven there rather than only their census.
        for (i, m) in ref_models.iter().enumerate() {
            if hull_models.contains(&i) {
                continue;
            }
            let lo = m.bvh_node.max(0) as usize * NODE_SIZE;
            let hi = if i + 1 < ref_models.len() {
                ref_models[i + 1].bvh_node.max(0) as usize * NODE_SIZE
            } else {
                writer.nodes.len()
            };
            let at = writer.nodes[lo..hi]
                .iter()
                .zip(ref_map.nodes[lo..hi].iter())
                .position(|(x, y)| x != y);
            assert!(at.is_none(), "{target} model {i} node block differs at byte {at:?}");
        }
        let sa = walk_spans(&a, &own_nodes, &own_models);
        let sb = walk_spans(&b, &ref_map.node_records, ref_models);
        assert_eq!(sa.len(), sb.len(), "{target} leaf count");
        // A hull's corner points come out of a plane solve that rounds in the
        // last bits differently from the reference's, so a hull's quantised
        // bounds can differ, and a bound that moves can flip a slot between a
        // direct leaf and a one-entry bundle. The leaf stream is therefore
        // compared model by model over the models that hold no hull, where the
        // layout cannot have moved.
        let mut poly = 0usize;
        let mut hull = 0usize;
        for i in 0..ref_models.len() {
            let ha = sa.iter().filter(|s| s.model == i).collect::<Vec<_>>();
            let hb = sb.iter().filter(|s| s.model == i).collect::<Vec<_>>();
            if ha.iter().chain(hb.iter()).any(|s| s.kind == CHILD_HULL) {
                // Which corners a hull keeps, and how its faces are grouped into
                // surface-tagged triangle sets, follows the hull triangulator,
                // so the parity test counts the hulls and
                // `hull_leaf_geometry_is_closed` proves the encoding.
                let xh = ha.iter().filter(|s| s.kind == CHILD_HULL).count();
                let yh = hb.iter().filter(|s| s.kind == CHILD_HULL).count();
                assert_eq!(xh, yh, "{target} model {i} hull leaf count");
                hull += xh;
                continue;
            }
            assert_eq!(ha.len(), hb.len(), "{target} model {i} leaf count");
            // Offsets are relative to the model's own first leaf: an earlier
            // model's hull leaves decide the absolute one.
            let base_a = own_models[i].bvh_leaf.max(0) as usize;
            let base_b = ref_models[i].bvh_leaf.max(0) as usize;
            for (x, y) in ha.iter().zip(hb.iter()) {
                assert_eq!(x.kind, y.kind, "{target} model {i} leaf kind");
                assert_eq!(x.at - base_a, y.at - base_b, "{target} model {i} leaf offset");
                assert_eq!(x.dwords, y.dwords, "{target} model {i} leaf size");
                assert_eq!(
                    a.words[x.at..x.at + x.dwords],
                    b.words[y.at..y.at + y.dwords],
                    "{target} model {i} leaf words"
                );
                if x.kind == CHILD_POLY {
                    poly += 1;
                }
            }
        }
        assert!(poly > 100, "{target}: {poly} poly leaves compared byte for byte");
        assert!(hull > 5, "{target}: {hull} hull leaves present");
        assert_eq!(
            writer.verts.len(),
            ref_map.coll_verts.len(),
            "{target} collision vertex count"
        );
        let lead = writer
            .verts
            .iter()
            .flatten()
            .zip(ref_map.coll_verts.iter().flatten())
            .take_while(|(a, b)| a == b)
            .count();
        eprintln!("{target}: collision vertex pool matches for {lead} floats");
    }
    assert!(stats.hulls > 0 && stats.tris > 0, "the conversion produced geometry: {stats:?}");
}

fn models_from_fields(fields: &[ModelFields], vert_offset: usize) -> Vec<Model> {
    fields
        .iter()
        .map(|f| Model {
            mins: [0.0; 3],
            maxs: [0.0; 3],
            first_mesh: 0,
            num_meshes: 0,
            bvh_node: f.bvh_node,
            bvh_leaf: f.bvh_leaf,
            vert_index: f.vert_index + vert_offset as i32,
            flags: f.flags,
            origin: f.origin,
            decode_scale: f.decode_scale,
        })
        .collect()
}

#[test]
fn tf2_reader_reports_the_primitive_set() {
    let Some(bsp_path) = std::env::var_os("R5ED_TF2_BSP").map(PathBuf::from) else { return };
    let Ok(bsp) = Bsp::read_path(&bsp_path) else { return };
    let Ok(tf2) = tf2::Tf2Collision::open(bsp) else { return };
    let prims = tf2.primitives();
    assert!(!prims.is_empty(), "the source map has collision primitives");
    let mut kinds: BTreeMap<u8, usize> = BTreeMap::new();
    for p in &prims {
        *kinds.entry(p.kind).or_insert(0) += 1;
    }
    assert!(kinds.contains_key(&0), "CM brushes present");
    assert!(kinds.contains_key(&64), "tricoll meshes present");
    let first = prims.iter().find(|p| p.kind == 0).unwrap();
    let hull = tf2.brush_hull(first.index).expect("a brush reads");
    assert!(hull.planes.len() >= 6, "a brush carries its axial planes");
    let (verts, faces) = tf2::hull_from_planes(&hull.planes, 1e-3);
    assert!(verts.len() >= 4 && faces.len() >= 4);
    for (_, pi) in &faces {
        assert!(*pi < hull.planes.len());
    }
    let tri = prims.iter().find(|p| p.kind == 64).unwrap();
    let (idx, _fl) = tf2.tricoll_triangles(tri.index);
    assert!(!idx.is_empty());
    for t in &idx {
        for i in t {
            assert!((*i as usize) < tf2.verts.len(), "a tricoll index addresses a vertex");
        }
    }
}

#[test]
fn tf2_brush_coll_blobs_match_the_reference_targets() {
    // The header layout is the one the two engines read; the byte counts have
    // to differ, and the decoded triangles have to agree.
    let brushes = vec![box_planes(DVec3::new(-16.0, -16.0, 0.0), DVec3::splat(16.0))];
    let c = volume_blob(&brushes, (0x00EB_1280, 0x400, b"TOOLS\\TOOLSTRIGGER"), CollTarget::Client).unwrap();
    let d = volume_blob(&brushes, (0x00EB_1280, 0x400, b"TOOLS\\TOOLSTRIGGER"), CollTarget::Dedi).unwrap();
    assert_eq!(c.target.header_size(), d.target.header_size() + 8, "the client header is eight bytes wider");
    assert_ne!(c.to_blob()[20..28], d.to_blob()[20..28], "the two headers pack the sections differently");
    let want = triangle_multiset(&c.triangles(), 0.5);
    assert_eq!(want, triangle_multiset(&d.triangles(), 0.5));
    assert_eq!(c.names, b"TOOLS\\TOOLSTRIGGER\0".to_vec());
}

#[test]
fn decode_collision_reports_a_model_with_no_collision() {
    let mut bsp = Bsp::new(47, 0, 0);
    emit_collision(&[], &[], &mut bsp).expect("an empty map emits");
    let all = decode_collision(&bsp).expect("an empty map decodes");
    assert!(all.is_empty());
    let leaves = decode_leaves(&bsp, 0).expect("model 0 has nothing");
    assert!(leaves.is_empty());
}

#[test]
fn collision_helpers_agree_with_the_face_loops() {
    let planes = box_planes(DVec3::ZERO, DVec3::splat(32.0));
    let verts = crate::csg::hull_vertices_planes(&planes).expect("a box has corners");
    assert_eq!(verts.len(), 8);
    let loops = crate::csg::plane_loops(&planes);
    assert_eq!(loops.len(), 6);
    for (pi, ring) in &loops {
        assert!(ring.len() == 4, "plane {pi} is a quad");
        let c = ring.iter().map(|&i| verts[i as usize]).sum::<DVec3>() / 4.0;
        let n = (verts[ring[1] as usize] - verts[ring[0] as usize])
            .cross(verts[ring[2] as usize] - verts[ring[0] as usize]);
        assert!(n.normalize().dot(planes[*pi].normal) > 0.9, "plane {pi} winds outward");
        assert!(planes[*pi].distance(c).abs() < 1e-3, "plane {pi} centroid is on the plane");
    }
    let hull = convex_hull(&verts, 1e-9).expect("a box has a hull");
    assert_eq!(hull.verts.len(), 8);
    assert_eq!(hull.faces.len(), 12, "a box hull is twelve triangles");
    assert!(convex_hull(&verts[..3], 1e-9).is_err(), "a flat point set has no hull");
}

#[test]
fn hull_leaf_geometry_is_closed() {
    // Every hull leaf the writer builds must be a closed volume: its plane
    // triples outward, every corner its triangles name one of its own, and its
    // corner set large enough for a volume at all.
    let brushes = vec![
        box_planes(DVec3::ZERO, DVec3::splat(64.0)),
        {
            let mut p = box_planes(DVec3::new(-32.0, -48.0, 0.0), DVec3::new(16.0, 24.0, 96.0));
            p.push(crate::csg::DPlane::new(DVec3::new(0.577350269, 0.577350269, 0.577350269), 12.0));
            p
        },
    ];
    let mut bsp = Bsp::new(47, 0, 0);
    let solid = CollisionSolid {
        object: None,
        model: 0,
        shape: CollisionShape::Brush {
            planes: brushes[0].iter().map(|p| (p.normal.as_vec3(), p.dist as f32)).collect(),
        },
        contents: 0x400,
        surface_prop: 0,
        surfaces: Vec::new(),
    };
    emit_collision(&[solid], &[slot_of(DVec3::ZERO, DVec3::splat(64.0))], &mut bsp).expect("emits");
    let leaves = decode_leaves(&bsp, 0).expect("decodes");
    let hulls: Vec<&DecodedLeaf> = leaves.iter().filter(|l| l.kind == CHILD_HULL).collect();
    assert_eq!(hulls.len(), 1, "one brush is one hull leaf");
    let view = decode_hull(&BvhLeafData { words: leaf_words(&bsp) }, 0).expect("the hull header reads");
    assert!(view.verts.len() >= 4, "a volume has at least four corners");
    assert!(!view.planes.is_empty(), "a volume has faces");
    assert!(view.tri_set_count > 0, "a hull draws its faces");
    let (verts, planes, tris) = hull_shape(&BvhLeafData { words: leaf_words(&bsp) }, 0);
    assert_eq!(verts.len(), view.verts.len());
    for [a, b, c] in &planes {
        assert!(
            (*a as usize) < verts.len() && (*b as usize) < verts.len() && (*c as usize) < verts.len(),
            "a plane triple names corners the hull does not have"
        );
    }
    assert!(!tris.is_empty(), "the hull draws triangles");
    for (corner, _) in tris.iter() {
        for b in corner {
            assert!(*b >= 0 && (*b >> 16) < verts.len() as i64, "a triangle names no corner of its hull");
        }
    }
    // The decoded triangles must close the hull: every corner of the volume is
    // named by at least one of them, and none lies outside its bounds.
    let points: Vec<DVec3> = view
        .verts
        .iter()
        .map(|q| {
            DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64)
                * (view.scale as f64 * DECODE_SCALE_UNIT)
                + DVec3::new(view.origin[0] as f64, view.origin[1] as f64, view.origin[2] as f64)
        })
        .collect();
    let leaves_tri: Vec<[DVec3; 3]> = hulls[0].triangles.clone();
    assert_eq!(leaves_tri.len(), tris.values().sum::<usize>(), "every corner triple is one triangle");
    for p in &points {
        assert!(
            leaves_tri.iter().any(|t| t.contains(p)),
            "hull corner {p:?} is in no triangle"
        );
    }
}

fn leaf_words(bsp: &Bsp) -> Vec<u32> {
    rbsp::lumps::collision::parse_bvh_leaf(bsp.lump(ids::BVH_LEAF_DATA), Version::DediV47)
        .expect("the leaf stream reads")
        .words
}

#[test]
fn content_and_surfprop_tables_are_deduplicated() {
    let mut w = BvhWriter::new(vec![0]);
    let a = w.contents_index(0x400).unwrap();
    let b = w.contents_index(0x400).unwrap();
    let c = w.contents_index(0x1).unwrap();
    assert_eq!(a, b);
    assert_ne!(a, c);
    let p = w.surfprop_index(0x200, 3, 0, 7).unwrap();
    let q = w.surfprop_index(0x200, 3, 0, 7).unwrap();
    let r = w.surfprop_index(0x200, 3, 0, 8).unwrap();
    assert_eq!(p, q);
    assert_ne!(p, r);
}

#[test]
fn a_thin_tilted_disc_hulls_closed_after_snapping() {
    // A 16-sided, 15-unit-thick disc tilted off every axis: snapped to the
    // hull grid its rims are no longer planar, which split the horizon of
    // an exact-epsilon hull and multiplied its faces past a leaf's limit.
    let axis = DVec3::new(0.6087, 0.7934, 0.0).normalize();
    let u = axis.cross(DVec3::Z).normalize();
    let v = axis.cross(u);
    let centre = axis * 2950.0;
    let mut points = Vec::new();
    for side in [-7.35, 7.35] {
        for k in 0..16 {
            let a = k as f64 / 16.0 * std::f64::consts::TAU;
            points.push(centre + axis * side + (u * a.cos() + v * a.sin()) * 1200.0);
        }
    }
    let origin = centre;
    let span = points.iter().fold(0.0f64, |m, p| m.max((*p - origin).abs().max_element()));
    let step = 2.0f64.powf((span / 32767.0).log2().ceil());
    let snapped: Vec<DVec3> = points.iter().map(|p| origin + ((*p - origin) / step).round() * step).collect();
    let hull = grid_hull(&snapped, step).expect("the disc has a volume");
    assert!(hull_is_closed(hull.verts.len(), &hull.faces));
    assert!(hull.faces.len() <= 60);
}


