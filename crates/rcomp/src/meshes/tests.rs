//! Render emission tests: a client map round trip and the mesh flag table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rbsp::lumps::render::Mesh;
use rbsp::{ids, Bsp, Version};
use rir::{AssetTables, VertexFormat};

use super::*;

/// The fixture root, or `None` when the gate did not set one.
fn fixtures() -> Option<PathBuf> {
    std::env::var_os("R5ED_FIXTURES").map(PathBuf::from)
}

/// Every map the flag table and the round trip read.
fn fixture_maps() -> Vec<(String, Bsp)> {
    let Some(root) = fixtures() else { return Vec::new() };
    let mut out: Vec<(String, Bsp)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("dedi")) {
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        for d in dirs {
            let maps = d.join("maps");
            let Ok(files) = std::fs::read_dir(&maps) else { continue };
            for f in files.flatten() {
                let p = f.path();
                if p.extension().is_none_or(|e| e != "bsp") {
                    continue;
                }
                if let Ok(b) = Bsp::read_path(&p) {
                    out.push((p.file_stem().unwrap_or_default().to_string_lossy().into_owned(), b));
                }
            }
        }
    }
    for sub in ["client/_test_s21_in", "client/_test_district_in"] {
        let d = root.join(sub);
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for f in rd.flatten() {
            let p = f.path();
            if p.extension().is_none_or(|e| e != "bsp") {
                continue;
            }
            if let Ok(b) = Bsp::read_path(&p) {
                out.push((p.file_stem().unwrap_or_default().to_string_lossy().into_owned(), b));
            }
        }
    }
    if let Some(refroot) = std::env::var_os("R5ED_TF2_REF") {
        for target in ["client", "dedi"] {
            let d = PathBuf::from(&refroot).join(target);
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for f in rd.flatten() {
                let p = f.path();
                if p.extension().is_none_or(|e| e != "bsp") {
                    continue;
                }
                if let Ok(b) = Bsp::read_path(&p) {
                    out.push((p.file_stem().unwrap_or_default().to_string_lossy().into_owned(), b));
                }
            }
        }
    }
    out
}

fn client_fixture() -> Option<Bsp> {
    let root = fixtures()?;
    let p = root.join("client/_test_s21_in/mp_rr_canyonlands_staging_mu1.bsp");
    Bsp::read_path(&p).ok()
}

#[test]
fn render_mesh_roundtrip_client() {
    let Some(map) = client_fixture() else { return };
    let decoded = decode_surfaces(&map).expect("a shipped client map decodes");
    assert!(decoded.len() > 1000, "the fixture carries a full map of meshes");

    // A mesh run starts at a u16, so the map is re-emitted one material at a
    // time; every material of the fixture has to survive its own round trip.
    let mut by_material: BTreeMap<String, Vec<RenderSurface>> = BTreeMap::new();
    for s in decoded.iter() {
        by_material.entry(s.material.clone()).or_default().push(s.clone());
    }
    assert!(by_material.len() > 50, "the fixture has a full material table");
    let total = |v: &[RenderSurface]| -> (usize, usize, Vec3, Vec3) {
        let mut verts = 0;
        let mut idx = 0;
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for s in v {
            verts += s.positions.len();
            idx += s.indices.len();
            for p in &s.positions {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        (verts, idx, lo, hi)
    };
    for (material, group) in &by_material {
        let want = total(group);
        let mut out = Bsp::new(map.header.version, map.header.flags, map.header.revision);
        out.set_lump(ids::VERTICES, bsp_lump(&map, ids::VERTICES));
        let index = emit_render(group, &AssetTables::default(), &mut out)
            .unwrap_or_else(|e| panic!("material {material} re-emits: {e}"));
        assert!(!index.meshes.is_empty());
        let again = decode_surfaces(&out).expect("the map this crate wrote decodes");
        let got = total(&again);
        assert_eq!(want.0, got.0, "material {material} vertex count");
        assert_eq!(want.1, got.1, "material {material} index count");
        assert!(want.2.distance(got.2) < 0.05, "material {material} mins");
        assert!(want.3.distance(got.3) < 0.05, "material {material} maxs");
        for r in &again {
            assert_eq!(r.material, *material, "the sort came back with its material");
            assert_eq!(r.indices.len() % 3, 0);
        }
    }
}

fn bsp_lump(bsp: &Bsp, id: u16) -> Vec<u8> {
    bsp.lump(id).to_vec()
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn mesh_flags_match_fixtures() {
    let maps = fixture_maps();
    assert!(maps.len() >= 20, "the fixture set is present ({} maps)", maps.len());
    // class -> the bits every map sets on every mesh of that class
    let mut table: BTreeMap<(u8, bool), u32> = BTreeMap::new();
    for (_, bsp) in &maps {
        let v = bsp.version_kind().unwrap_or(Version::ClientV51);
        let Ok(meshes) = rbsp::lumps::render::parse_meshes(bsp.lump(ids::MESHES), v) else { continue };
        let Ok(sorts) = rbsp::lumps::render::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), v) else { continue };
        let mut per_class: BTreeMap<(u8, bool), u32> = BTreeMap::new();
        for m in &meshes {
            let lit = sorts
                .get(m.material_sort as usize)
                .map(|s| s.lightmap >= 0)
                .unwrap_or(false);
            let e = per_class.entry((m.vertex_type & 3, lit)).or_insert(u32::MAX);
            *e &= m.flags;
        }
        for (class, bits) in per_class {
            let e = table.entry(class).or_insert(u32::MAX);
            *e &= bits;
        }
    }
    // The world bit is pinned by `shipped_s21_meshes_carry_the_world_bit_and_a_page`
    // against the S21 map in agent-kit; this set mixes in converted maps that do
    // not all carry it, so the intersection here cannot speak for it.
    let unlit = mesh_flags(VertexFormat::Unlit, false) & !MESH_FLAG_WORLD;
    let unlit_ts = mesh_flags(VertexFormat::UnlitTs, false) & !MESH_FLAG_WORLD;
    let lit_flat = mesh_flags(VertexFormat::LitFlat, true) & !MESH_FLAG_WORLD;
    let lit_bump = mesh_flags(VertexFormat::LitBump, true) & !MESH_FLAG_WORLD;
    for bits in table.values_mut() {
        *bits &= !MESH_FLAG_WORLD;
    }
    assert_eq!(table.get(&(0, false)), Some(&unlit), "unlit unlightmapped flags");
    assert_eq!(table.get(&(3, false)), Some(&unlit_ts), "tangent-space unlightmapped flags");
    assert_eq!(table.get(&(2, true)), Some(&lit_flat), "lit lightmapped flags");
    assert_eq!(lit_bump, lit_flat, "both lit formats share the flag");
    // A class no shipped map carries has no evidence behind it, so the emitter
    // is not allowed to claim one.
    for (class, bits) in &table {
        let claimed = match (class.0, class.1) {
            (0, false) => unlit,
            (3, false) => unlit_ts,
            (2, true) | (1, true) => lit_flat,
            _ => continue,
        };
        assert_eq!(*bits, claimed, "class {class:?}");
    }
}

/// The shipped S21 client map in agent-kit, when the checkout carries it.
fn kit_s21_client() -> Option<Bsp> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../agent-kit/bsp/staging_mu1_full/client/mp_rr_canyonlands_staging_mu1.bsp");
    if !p.is_file() {
        eprintln!("{} is not here, so the shipped S21 meshes are not checked", p.display());
        return None;
    }
    Some(Bsp::read_path(&p).expect("the shipped S21 map reads"))
}

/// Read off staging_mu1: every mesh but the skybox's carries the world bit,
/// every lit mesh's sort names a lightmap page, and a lit mesh's flags are the
/// ones `mesh_flags` claims for it.
#[test]
fn shipped_s21_meshes_carry_the_world_bit_and_a_page() {
    use rbsp::lumps::render as r;
    let Some(bsp) = kit_s21_client() else { return };
    let v = Version::ClientV51;
    let meshes = r::parse_meshes(bsp.lump(ids::MESHES), v).expect("meshes");
    let sorts = r::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), v).expect("sorts");
    let texdata = r::parse_texdata(bsp.lump(ids::TEXTURE_DATA), v).expect("texdata");
    let (mut lit, mut world) = (0, 0);
    for (i, m) in meshes.iter().enumerate() {
        let sort = &sorts[m.material_sort as usize];
        let sky = texdata[sort.tex_data as usize].flags & 0x4 != 0;
        if !sky {
            assert_ne!(m.flags & MESH_FLAG_WORLD, 0, "mesh {i}");
            world += 1;
        }
        if m.vertex_type & 3 == VERTEX_TYPE_LIT_BUMP as u8 {
            assert!(sort.lightmap >= 0, "lit mesh {i} names no page");
            let claimed = mesh_flags(VertexFormat::LitBump, true);
            assert_eq!(m.flags & (MESH_FLAG_WORLD | 0x600), claimed, "lit mesh {i}");
            lit += 1;
        }
    }
    assert_eq!((lit, world), (4637, 4640), "the whole mesh table was read");
}

#[test]
fn a_sort_is_keyed_by_material_and_page() {
    let mut a = strip(4, 2);
    a.material = "world/dev/dev_gray_512".into();
    let mut b = a.clone();
    b.positions.iter_mut().for_each(|p| p.y += 100.0);
    let mut bsp = Bsp::new(51, 1, 30);
    let index = emit_render_paged(&[a.clone(), b, a], &[0, 1, 0], &AssetTables::default(), &mut bsp).expect("emits");
    let v = Version::ClientV51;
    let sorts = rbsp::lumps::render::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), v).expect("sorts");
    let texdata = rbsp::lumps::render::parse_texdata(bsp.lump(ids::TEXTURE_DATA), v).expect("texdata");
    assert_eq!(texdata.len(), 1, "one material, one texdata");
    assert_eq!(sorts.iter().map(|s| (s.tex_data, s.lightmap)).collect::<Vec<_>>(), vec![(0, 0), (0, 1)]);
    assert_eq!(index.sort_material.len(), 2);
    let meshes = rbsp::lumps::render::parse_meshes(bsp.lump(ids::MESHES), v).expect("meshes");
    assert!(meshes.iter().all(|m| m.flags & MESH_FLAG_WORLD != 0));
}

/// A surface of `tris` triangles over `n` distinct vertices.
fn strip(n: usize, tris: usize) -> RenderSurface {
    let mut s = RenderSurface {
        model: 0,
        material: "world/wall".into(),
        positions: (0..n).map(|i| Vec3::new(i as f32, 0.0, 0.0)).collect(),
        normals: vec![Vec3::Z; n],
        tangents: Vec::new(),
        uv0: vec![Vec2::ZERO; n],
        uv1: vec![Vec2::ZERO; n],
        colors: vec![[255; 4]; n],
        indices: Vec::new(),
        cast_shadows: true,
        lightmap_texel: 16.0,
        lightmap_size: [0, 0],
        object: None,
    };
    for t in 0..tris {
        let a = (t * 3) % n;
        s.indices.extend_from_slice(&[a as u32, ((a + 1) % n) as u32, ((a + 2) % n) as u32]);
    }
    s
}

#[test]
fn emit_render_splits_a_surface_the_index_cannot_address() {
    // A mesh's `triangle_count` is a u16, so a longer list splits into runs.
    let n = 20000;
    let tris = MAX_MESH_TRIANGLES + 1000;
    let s = strip(n, tris);
    let mut bsp = Bsp::new(51, 1, 0);
    let index = emit_render(&[s], &AssetTables::default(), &mut bsp).expect("a long surface splits");
    assert!(index.meshes.len() >= 2, "a mesh counts {} triangles at most", MAX_MESH_TRIANGLES);
    let mut covered = 0usize;
    for m in &index.meshes {
        assert!(m.triangle_count as usize <= MAX_MESH_TRIANGLES);
        assert!(m.first_vertex as usize + m.vertex_count as usize <= MAX_MESH_VERTICES);
        assert!(m.triangle_count.is_multiple_of(2), "the engine walks an even count");
        covered += m.triangle_count as usize;
    }
    assert!(covered >= tris && covered - tris <= index.meshes.len(), "every triangle is in exactly one run, plus at most one pad each");
}

#[test]
fn emit_render_reports_a_pool_the_index_cannot_address() {
    // `first_vertex` is a u16, so a format's whole pool is capped.
    let s = strip(MAX_MESH_VERTICES + 8, MAX_MESH_VERTICES + 8);
    let mut bsp = Bsp::new(51, 1, 0);
    match emit_render(&[s], &AssetTables::default(), &mut bsp) {
        Err(MeshError::PoolFull(records, limit)) => {
            assert!(records > MAX_MESH_VERTICES, "{records} records");
            assert_eq!(limit, MAX_MESH_VERTICES);
        }
        other => panic!("a pool past the u16 first_vertex must be reported, got {other:?}"),
    }
}

#[test]
fn emit_render_paged_splits_a_sort_past_the_window() {
    // A sort's meshes count from the sort's first vertex in u16, so one lit
    // material past 65536 vertices takes a second sort with the same key.
    let a = strip(40000, 40000);
    let mut b = strip(40000, 40000);
    b.positions.iter_mut().for_each(|p| p.x += 100000.0);
    let mut bsp = Bsp::new(51, 1, 0);
    let index = emit_render_paged(&[a, b], &[0, 0], &AssetTables::default(), &mut bsp).expect("a full window splits");
    assert_eq!(index.sort_material, vec!["world/wall".to_string(), "world/wall".to_string()]);
    for (si, last) in index.sort_last_vertex.iter().enumerate() {
        assert!(*last as usize + 1 <= MAX_MESH_VERTICES, "sort {si} spans more than a window");
    }
    let mut covered = 0usize;
    for m in &index.meshes {
        assert!((m.material_sort as usize) < index.sort_material.len(), "a mesh names a missing sort");
        assert!(m.first_vertex as usize + m.vertex_count as usize <= MAX_MESH_VERTICES, "a mesh runs past its sort window");
        covered += m.triangle_count as usize;
    }
    assert_eq!(covered, 80000, "every triangle is in exactly one run");
    let sorts = rbsp::lumps::render::parse_material_sorts(bsp.lump(ids::MATERIAL_SORT), Version::ClientV51).expect("sorts");
    assert_eq!(sorts.len(), 2, "the material repeats across two sorts");
    assert_eq!((sorts[0].tex_data, sorts[1].tex_data), (0, 0), "both sorts sample the one material");
}

#[test]
fn emit_render_rejects_a_broken_surface() {
    let mut bsp = Bsp::new(51, 1, 0);
    let bad = RenderSurface {
        positions: vec![Vec3::NAN, Vec3::ZERO, Vec3::X],
        normals: vec![Vec3::Z; 3],
        indices: vec![0, 1, 2],
        ..RenderSurface::default()
    };
    assert!(matches!(emit_render(&[bad], &AssetTables::default(), &mut bsp), Err(MeshError::NotFinite(0))));

    let short = RenderSurface {
        positions: vec![Vec3::ZERO, Vec3::X],
        normals: vec![Vec3::Z; 2],
        indices: vec![0, 1, 5],
        ..RenderSurface::default()
    };
    assert!(matches!(
        emit_render(&[short], &AssetTables::default(), &mut bsp),
        Err(MeshError::SurfaceIndex(0))
    ));

    let ragged = RenderSurface {
        positions: vec![Vec3::ZERO; 3],
        normals: vec![Vec3::Z; 3],
        indices: vec![0, 1],
        ..RenderSurface::default()
    };
    assert!(matches!(
        emit_render(&[ragged], &AssetTables::default(), &mut bsp),
        Err(MeshError::SurfaceTriangles(0, 2))
    ));
}

#[test]
fn emit_render_keeps_the_collision_fields_of_a_model() {
    let mut bsp = Bsp::new(51, 1, 0);
    let mut models = vec![empty_model_for_test(), empty_model_for_test()];
    models[0].bvh_node = 0;
    models[0].bvh_leaf = 4;
    models[0].vert_index = 12;
    models[0].origin = [1.0, 2.0, 3.0];
    models[0].decode_scale = 0.5;
    bsp.set_lump(ids::MODELS, rbsp::lumps::collision::write_models(&models, Version::ClientV51));
    let s = RenderSurface {
        material: "world/wall".into(),
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
        normals: vec![Vec3::Z; 3],
        indices: vec![0, 1, 2],
        ..RenderSurface::default()
    };
    emit_render(&[s], &AssetTables::default(), &mut bsp).expect("emission works");
    let after = rbsp::lumps::collision::parse_models(bsp.lump(ids::MODELS), Version::ClientV51).unwrap();
    assert_eq!(after[0].bvh_node, 0);
    assert_eq!(after[0].vert_index, 12);
    assert_eq!(after[0].origin, [1.0, 2.0, 3.0]);
    assert_eq!(after[0].decode_scale, 0.5);
    assert!(after[0].first_mesh == 0 && after[0].num_meshes == 1);
    assert_eq!(after[1].bvh_node, -1, "a model with no collision stays empty");
}

fn empty_model_for_test() -> rbsp::lumps::collision::Model {
    rbsp::lumps::collision::Model {
        mins: [0.0; 3],
        maxs: [0.0; 3],
        first_mesh: 0,
        num_meshes: 0,
        bvh_node: -1,
        bvh_leaf: -1,
        vert_index: 0,
        flags: 0,
        origin: [0.0; 3],
        decode_scale: 0.0,
    }
}

#[test]
fn mesh_vertex_kind_matches_the_record_sizes() {
    for (f, size) in [
        (VertexFormat::Unlit, 20usize),
        (VertexFormat::LitFlat, 28),
        (VertexFormat::LitBump, 32),
        (VertexFormat::UnlitTs, 24),
    ] {
        let kind = vertex_kind(f);
        let got = rbsp::lumps::render::vertex_lump_size(kind, Version::ClientV51).unwrap();
        assert_eq!(got, size, "{f:?} record size");
    }
    assert!(rbsp::lumps::render::vertex_lump_size(4, Version::ClientV51).is_err());
}

#[test]
fn decode_rejects_a_mismatched_bounds_lump() {
    let mut bsp = Bsp::new(51, 1, 0);
    let mesh = Mesh {
        first_index: 0,
        triangle_count: 1,
        first_vertex: 0,
        lightmap_vertex_count: 3,
        vertex_type: 2,
        cube_index: 0,
        unknown_0: [0; 4],
        lightmap_scale: [0; 2],
        fade_distance: 0,
        material_sort: 0,
        flags: 0,
    };
    bsp.set_lump(ids::MESHES, rbsp::lumps::render::write_meshes(&[mesh], Version::ClientV51));
    bsp.set_lump(ids::MESH_BOUNDS, Vec::new());
    assert!(matches!(decode_meshes(&bsp), Err(MeshError::BoundsCount(1, 0))));
}

#[test]
fn texture_data_names_round_trip() {
    let mut bsp = Bsp::new(51, 1, 0);
    let surfaces: Vec<RenderSurface> = ["world/a", "world/b", "tools/c"]
        .iter()
        .map(|m| RenderSurface {
            material: (*m).to_string(),
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            normals: vec![Vec3::Z; 3],
            indices: vec![0, 1, 2],
            ..RenderSurface::default()
        })
        .collect();
    let index = emit_render(&surfaces, &AssetTables::default(), &mut bsp).unwrap();
    assert_eq!(index.sort_material, vec!["world/a", "world/b", "tools/c"]);
    let back = decode_surfaces(&bsp).unwrap();
    let mut got: Vec<String> = back.iter().map(|s| s.material.clone()).collect();
    got.sort();
    got.dedup();
    assert_eq!(got, vec!["tools/c", "world/a", "world/b"]);
}

#[test]
fn asset_table_is_read_for_the_vertex_format() {
    let mut assets = AssetTables::default();
    assets.materials.insert(
        "world/sky".into(),
        rir::MaterialInfo {
            path: "world/sky".into(),
            guid: 0,
            shader_set: String::new(),
            vertex_format: VertexFormat::UnlitTs,
            surface_prop: String::new(),
            translucent: true,
            nodraw: false,
            textures: Default::default(),
        },
    );
    let mut bsp = Bsp::new(51, 1, 0);
    let s = RenderSurface {
        material: "world/sky".into(),
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
        normals: vec![Vec3::Z; 3],
        tangents: vec![Vec4::W; 3],
        indices: vec![0, 1, 2],
        ..RenderSurface::default()
    };
    let index = emit_render(&[s], &assets, &mut bsp).unwrap();
    assert_eq!(index.meshes[0].vertex_type, 3, "an UnlitTs material draws from 0x4A");
    assert_eq!(index.meshes[0].flags, MESH_FLAG_WORLD | MESH_FLAG_UNLIT_TS);
    let td = rbsp::lumps::render::parse_texdata(bsp.lump(ids::TEXTURE_DATA), Version::ClientV51).unwrap();
    assert_eq!(td[0].flags & TEXDATA_TRANSLUCENT, TEXDATA_TRANSLUCENT);
    assert_eq!(td[0].flags & TEXDATA_DRAW, TEXDATA_DRAW);
}

/// The uv, lightmap, colour and tangent fields of a shipped map's vertex
/// records, read straight out of the lump. A record whose texture coordinate
/// is a two-f32 pair and whose lightmap coordinate is a page fraction decodes
/// to sane values; one that packs them as fixed point does not, which is what
/// makes this a check on the layout rather than on the values.
fn shipped_records(bsp: &Bsp, id: u16, size: usize) -> Vec<(Vec2, Vec2, [u8; 4], Option<u32>)> {
    bsp.lump(id)
        .chunks_exact(size)
        .take(4096)
        .map(|r| {
            let uv = uv_of(r, 8);
            let lm = uv_of(r, 20);
            let colour = [r[16], r[17], r[18], r[19]];
            let tang = if size == 32 || size == 24 {
                Some(u32::from_le_bytes([r[size - 4], r[size - 3], r[size - 2], r[size - 1]]))
            } else {
                None
            };
            (Vec2::new(uv.0, uv.1), Vec2::new(lm.0, lm.1), colour, tang)
        })
        .collect()
}

#[test]
fn vertex_record_layout_matches_the_fixture_maps() {
    let maps = fixture_maps();
    if maps.is_empty() {
        return;
    }
    let mut checked = 0usize;
    for (_, bsp) in &maps {
        let v = bsp.version_kind().unwrap_or(Version::ClientV51);
        for (id, size, lit) in [
            (ids::VERTEX_UNLIT, 20usize, false),
            (ids::VERTEX_LIT_FLAT, 28, true),
            (ids::VERTEX_LIT_BUMP, 32, true),
            (ids::VERTEX_UNLIT_TS, 24, false),
        ] {
            if bsp.lump(id).is_empty() || bsp.lump(id).len() % size != 0 {
                continue;
            }
            checked += 1;
            let recs = shipped_records(bsp, id, size);
            assert!(!recs.is_empty(), "lump 0x{id:02x} has records");
            for (uv, lm, _colour, tang) in &recs {
                for c in [uv.x, uv.y] {
                    assert!(c.is_finite(), "lump 0x{id:02x} texture coordinate {c} is not a float");
                    assert!(c.abs() < 1.0e7, "lump 0x{id:02x} texture coordinate {c} is out of range");
                }
                if lit {
                    for c in [lm.x, lm.y] {
                        assert!(
                            c.is_finite() && (-0.01..=1.01).contains(&c),
                            "lump 0x{id:02x} lightmap coordinate {c} is not a page fraction"
                        );
                    }
                }
                if let Some(t) = tang {
                    let normals = bsp.lump(ids::VERTEX_NORMALS).len() / 12;
                    assert!(
                        (t & 0x7FFF_FFFF) as usize <= normals.max(1) || v == Version::Tf2V37,
                        "lump 0x{id:02x} tangent {t:#x} is past the {normals} normals it indexes"
                    );
                }
            }
        }
    }
    assert!(checked >= 3, "the fixture set carries {checked} vertex lumps to pin the layout against");
}

/// A vector's component bit patterns, so a round trip is compared exactly
/// rather than through a tolerance that would hide a mis-placed field.
fn bits<T: Copy>(v: T, f: impl Fn(&T) -> [f32; 4]) -> [u32; 4] {
    f(&v).map(f32::to_bits)
}

#[test]
fn emit_render_records_the_uvs_the_decoder_reads_back() {
    // The emitter and the decoder must agree on where a record keeps its uv
    // pair, its colour and its tangent, so a surface that goes out and comes
    // back is byte-identical in those fields.
    for (format, size) in [
        (VertexFormat::Unlit, 20usize),
        (VertexFormat::LitFlat, 28),
        (VertexFormat::LitBump, 32),
        (VertexFormat::UnlitTs, 24),
    ] {
        let mut assets = AssetTables::default();
        assets.materials.insert(
            "world/uv".into(),
            rir::MaterialInfo {
                path: "world/uv".into(),
                guid: 0,
                shader_set: String::new(),
                vertex_format: format,
                surface_prop: String::new(),
                translucent: false,
                nodraw: false,
                textures: Default::default(),
            },
        );
        let lit = is_lightmapped(format);
        let want = RenderSurface {
            material: "world/uv".into(),
            positions: vec![Vec3::ZERO, Vec3::new(1.0, 2.0, 3.0), Vec3::new(-4.0, 5.0, -6.0)],
            normals: vec![Vec3::Z, Vec3::X, Vec3::Y],
            tangents: vec![Vec4::new(1.0, 0.0, 0.0, -1.0), Vec4::W, Vec4::new(0.0, 0.0, 1.0, 1.0)],
            uv0: vec![Vec2::new(0.25, -3.5), Vec2::new(512.0, 0.0), Vec2::new(1.0, 1.0)],
            uv1: vec![Vec2::new(0.0, 0.0), Vec2::new(0.5, 0.25), Vec2::new(1.0, 0.75)],
            colors: vec![[255, 255, 255, 255], [10, 20, 30, 40], [0, 0, 0, 0]],
            indices: vec![0, 1, 2],
            ..RenderSurface::default()
        };
        let mut bsp = Bsp::new(51, 1, 0);
        emit_render(std::slice::from_ref(&want), &assets, &mut bsp).expect("emission works");
        let lump = match format {
            VertexFormat::Unlit => ids::VERTEX_UNLIT,
            VertexFormat::LitFlat => ids::VERTEX_LIT_FLAT,
            VertexFormat::LitBump => ids::VERTEX_LIT_BUMP,
            VertexFormat::UnlitTs => ids::VERTEX_UNLIT_TS,
        };
        assert_eq!(bsp.lump(lump).len(), 3 * size, "{format:?} wrote the wrong record size");
        let back = decode_surfaces(&bsp).expect("the map this crate wrote decodes");
        assert_eq!(back.len(), 1, "{format:?}");
        let got = &back[0];
        for i in 0..3 {
            assert_eq!(got.positions[i].to_array(), want.positions[i].to_array(), "{format:?} position {i}");
            assert_eq!(bits(got.normals[i], |v| [v.x, v.y, v.z, 0.0]), bits(want.normals[i], |v| [v.x, v.y, v.z, 0.0]), "{format:?} normal {i}");
            assert_eq!(bits(got.uv0[i], |v| [v.x, v.y, 0.0, 0.0]), bits(want.uv0[i], |v| [v.x, v.y, 0.0, 0.0]), "{format:?} uv0 {i}");
            assert_eq!(got.colors[i], want.colors[i], "{format:?} colour {i}");
            if lit {
                assert_eq!(bits(got.uv1[i], |v| [v.x, v.y, 0.0, 0.0]), bits(want.uv1[i], |v| [v.x, v.y, 0.0, 0.0]), "{format:?} uv1 {i}");
            }
            if format == VertexFormat::LitBump || format == VertexFormat::UnlitTs {
                assert_eq!(bits(got.tangents[i], |v| [v.x, v.y, v.z, v.w]), bits(want.tangents[i], |v| [v.x, v.y, v.z, v.w]), "{format:?} tangent {i}");
            }
        }
    }
}

#[test]
fn fixture_root_has_the_maps_the_gate_promises() {
    let Some(root) = fixtures() else { return };
    assert!(root.join("dedi").is_dir(), "the dedi fixture tree is present");
    assert!(Path::new(&root.join("client/_test_s21_in")).is_dir());
}
