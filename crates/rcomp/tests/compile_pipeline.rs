//! compile orchestration: both targets, determinism, nav soup, project errors.

mod common;

use common::*;

#[test]
fn compile_box_room_both_targets() {
    let doc = box_room();
    let opts = CompileOptions { skip_rooms: true, ..options() };
    let out = compile(&doc, &Fs::new(), &Assets::new(), &opts).expect("the room compiles");
    let client = out.client.as_ref().expect("a client map");
    let dedi = out.dedi.as_ref().expect("a dedicated-server map");
    assert_eq!(client.bsp.header.version, 51);
    assert_eq!(client.bsp.header.flags, 1);
    assert_eq!(dedi.bsp.header.version, 47);
    assert_eq!(dedi.bsp.header.flags, 0);
    for (label, map) in [("client", client), ("dedi", dedi)] {
        let issues = rbsp::validate(&map.bsp, &map.ent_bytes);
        let errors: Vec<String> = issues
            .iter()
            .filter(|i| i.is_error())
            .map(|i| format!("{:?} {:?}: {}", i.severity, i.lump, i.message))
            .collect();
        assert!(errors.is_empty(), "{label} map has {} errors: {}", errors.len(), errors.join(" | "));
        for id in dedi.bsp.lumps.keys() {
            if label == "dedi" {
                assert!(rbsp::ids::is_dedi_lump(*id), "{label} carries 0x{id:02x}, outside the server set");
            }
        }
    }
    // The worldspawn block, the spawn point, the light, the volume and the
    // three props are all present exactly once.
    let counts = out.report.stats.entities_per_class.clone();
    assert_eq!(counts.get("worldspawn"), Some(&1));
    assert_eq!(counts.get("info_player_start"), Some(&1));
    assert_eq!(counts.get("light_environment"), Some(&1));
    assert_eq!(counts.get("trigger_hurt"), Some(&1));
    assert_eq!(out.report.stats.props, 3);
    let client_blocks = client.entity_count();
    let dedi_blocks = dedi.entity_count();
    assert_eq!(client_blocks, dedi_blocks, "both targets carry the same entity count");
    let main = rbsp::lumps::entities::parse(
        client.bsp.lump(rbsp::ids::ENTITIES),
        rbsp::Version::ClientV51,
    )
    .expect("the worldspawn lump parses");
    assert_eq!(main.entities.len(), 1);
    assert_eq!(main.entities[0].classname(), Some("worldspawn"));
    assert!(main.entities[0].get("world_mins").is_some());
    let volume_file = client
        .ent_bytes
        .get("script")
        .or_else(|| client.ent_bytes.get("main"))
        .expect("the volume lives in a partition file");
    let text = String::from_utf8_lossy(volume_file);
    assert!(text.contains("\"classname\" \"trigger_hurt\""), "the volume is in a partition file");
    assert!(text.contains("\"*coll0\""), "the volume carries a collider blob");
}

/// Every lit mesh of a compiled client map names the lightmap page its chart
/// was baked on, and carries the world bit, the way every lit mesh of the
/// shipped S21 map does. A lit mesh whose sort said -1 sampled no lightmap
/// and drew black in game.
#[test]
fn every_lit_mesh_names_its_lightmap_page() {
    use rbsp::lumps::render as r;
    let out = compile(&box_room(), &Fs::new(), &Assets::new(), &options()).expect("the room compiles");
    let client = &out.client.as_ref().expect("a client map").bsp;
    let v = rbsp::Version::ClientV51;
    let meshes = r::parse_meshes(client.lump(rbsp::ids::MESHES), v).expect("meshes");
    let sorts = r::parse_material_sorts(client.lump(rbsp::ids::MATERIAL_SORT), v).expect("sorts");
    let pages = client.lump(rbsp::ids::LIGHTMAP_HEADERS).len() / 8;
    assert!(pages > 0, "the room has a lightmap page");
    let mut lit = 0;
    for (i, m) in meshes.iter().enumerate() {
        assert_ne!(m.flags & rcomp::meshes::MESH_FLAG_WORLD, 0, "mesh {i} lacks the world bit");
        if m.vertex_type & 3 != rbsp::lumps::render::VERTEX_TYPE_LIT_BUMP as u8 {
            continue;
        }
        let page = sorts[m.material_sort as usize].lightmap;
        assert!(page >= 0 && (page as usize) < pages, "lit mesh {i} names page {page} of {pages}");
        lit += 1;
    }
    assert!(lit > 0, "the room has lit meshes");
    let bump = client.lump(rbsp::ids::VERTEX_LIT_BUMP);
    for rec in bump.chunks_exact(32) {
        let u = f32::from_le_bytes(rec[20..24].try_into().unwrap());
        let v = f32::from_le_bytes(rec[24..28].try_into().unwrap());
        assert!((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v), "a lightmap coordinate off the page: {u} {v}");
    }
}

/// The level info splits the world's meshes the way the shipped S21 map does:
/// the opaque pass draws `[0, first_decal_mesh)`, object references below
/// `first_sky_mesh` are mesh indices, and props start at that bound rounded up
/// to 64 (staging_mu1: sky 4640, first prop reference 4672). A level info of
/// zeros left the opaque range empty and the world drew nothing.
#[test]
fn level_info_mesh_ranges_match_the_shipped_layout() {
    use rbsp::lumps::{collision, level, render, vis, Lump};
    let out = compile(&box_room(), &Fs::new(), &Assets::new(), &options()).expect("the room compiles");
    let client = &out.client.as_ref().expect("a client map").bsp;
    let v = rbsp::Version::ClientV51;
    let meshes = render::parse_meshes(client.lump(rbsp::ids::MESHES), v).expect("meshes");
    let models = collision::parse_models(client.lump(rbsp::ids::MODELS), v).expect("models");
    let li = match rbsp::lumps::parse(rbsp::ids::LEVEL_INFO, v, client.lump(rbsp::ids::LEVEL_INFO)).expect("level info") {
        Lump::LevelInfo(li) => li,
        other => panic!("level info decoded as {}", other.kind()),
    };
    assert_eq!(models[0].first_mesh, 0);
    assert_eq!(li.first_sky_mesh, models[0].num_meshes, "the sky bound ends the world model");
    assert!(li.first_decal_mesh > 0, "the opaque pass has meshes to draw");
    assert!(li.first_decal_mesh <= li.first_transparent_mesh && li.first_transparent_mesh <= li.first_sky_mesh);
    assert_eq!(li.first_static_prop_index, level::first_static_prop_index(li.first_sky_mesh));
    assert_eq!(li.num_static_props, 3);
    let mut next = 0;
    for (k, m) in models.iter().enumerate().filter(|(_, m)| m.num_meshes > 0) {
        assert_eq!(m.first_mesh, next, "model {k}'s meshes follow the previous model's");
        next += m.num_meshes;
    }
    assert_eq!(next as usize, meshes.len());

    let refs = vis::parse_obj_refs(client.lump(rbsp::ids::OBJ_REFERENCES), v).expect("object references");
    let props = li.first_static_prop_index..li.first_static_prop_index + li.num_static_props;
    for r in &refs {
        assert!(r.get() < li.first_sky_mesh || props.contains(&r.get()), "reference {} is neither a world mesh nor a prop", r.get());
    }
    for i in (0..li.first_sky_mesh).chain(props) {
        assert!(refs.iter().any(|r| r.get() == i), "nothing references object {i}");
    }
}

#[test]
fn compile_deterministic() {
    let doc = box_room();
    let a = compile(&doc, &Fs::new(), &Assets::new(), &options()).expect("the room compiles");
    let b = compile(&doc, &Fs::new(), &Assets::new(), &options()).expect("the room compiles again");
    for (left, right) in [(a.client.as_ref(), b.client.as_ref()), (a.dedi.as_ref(), b.dedi.as_ref())] {
        let (left, right) = (left.unwrap(), right.unwrap());
        let mut ids: Vec<u16> = left.bsp.lumps.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            compare(left.bsp.lump(id), right.bsp.lump(id), &format!("lump 0x{id:02x}"));
        }
        assert_eq!(left.ent_files, right.ent_files);
        let keys: Vec<&String> = left.ent_bytes.keys().collect();
        let right_keys: Vec<&String> = right.ent_bytes.keys().collect();
        assert_eq!(keys, right_keys);
        for k in keys {
            compare(&left.ent_bytes[k], &right.ent_bytes[k], k);
        }
    }
}

#[test]
fn nav_soup_nonempty_bounded() {
    let doc = box_room();
    let mut doc = doc;
    doc.settings.navmesh.bounds =
        Some([DVec3::new(-1024.0, -1024.0, -256.0), DVec3::new(1024.0, 1024.0, 512.0)]);
    let out = compile(&doc, &Fs::new(), &Assets::new(), &options()).expect("the room compiles");
    assert!(out.nav_soup.triangles() > 0, "the collision produced a soup");
    let (lo, hi) = out.nav_soup.bounds();
    assert!(lo.cmpge(Vec3::splat(-1024.0)).all(), "the soup is inside the navmesh bounds");
    assert!(hi.cmple(Vec3::splat(1024.0)).all(), "the soup is inside the navmesh bounds");
    assert!(out.nav_soup.counts.prop_triangles >= 36, "each prop without collision became a box");
    let obj = rcomp::nav::to_obj(&out.nav_soup);
    assert!(obj.is_ascii() && obj.starts_with("# r5ed"));
}

#[test]
fn a_mesh_the_glb_reader_rejects_is_a_clean_project_error() {
    // The glb reader is another area's; this checks that whatever it says
    // about a file arrives at the caller as an error naming the object, and
    // that a missing source file reads the same way.
    let layer = Layer { id: Uuid::new_v4(), name: "default".into(), visible: true, locked: false };
    let mut fs = Fs::new();
    // A glb cut short: its binary chunk says it holds more than the file does.
    let mut cut = cube_glb();
    cut.truncate(cut.len() - 8);
    fs.0.insert("meshes/cube.glb".into(), cut);
    let mesh = |source: &str| {
        new_object(
            "cube",
            layer.id,
            ObjectKind::Mesh(rdoc::model::Mesh {
                source: source.into(),
                material_map: Default::default(),
                collision: CollisionMode::ConvexHulls,
                contents: 1,
                cast_shadows: true,
                lightmap_scale: 0.0,
                render: true,
                role: rdoc::model::MeshRole::Normal,
                modifiers: Vec::new(),
            }),
        )
    };
    let doc = |objects: Vec<Object>| MapDoc {
        format: rdoc::model::FORMAT_VERSION,
        meta: Default::default(),
        layers: vec![layer.clone()],
        objects,
        settings: Settings::default(),
    };
    let err = compile(&doc(vec![mesh("meshes/cube.glb")]), &fs, &Assets::new(), &options())
        .expect_err("a mesh the reader rejects is an error");
    let text = format!("{err}");
    assert!(text.contains("glb"), "the message names the reader: {text}");
    let err = compile(&doc(vec![mesh("meshes/absent.glb")]), &fs, &Assets::new(), &options())
        .expect_err("a missing mesh file is an error");
    assert!(format!("{err}").contains("meshes/absent.glb"), "the message names the file");
}


/// Mesh indices are relative to their material sort's vertex window. Read that
/// way, nearly every lit triangle of the shipped staging_mu1 client winds
/// clockwise about its vertex normal; read as absolute pool slots, meshes past
/// the first window pick up unrelated vertices and the split is near even.
#[test]
fn stock_meshes_decode_through_their_sort_window() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../agent-kit/bsp/staging_mu1_full/client/mp_rr_canyonlands_staging_mu1.bsp");
    if !path.exists() {
        eprintln!("skipping: {} is not present", path.display());
        return;
    }
    let bsp = rbsp::Bsp::read_path(&path).expect("the stock client reads");
    let runs = rcomp::meshes::decode_meshes(&bsp).expect("the stock meshes decode");
    let (mut with, mut against) = (0usize, 0usize);
    for run in runs.iter().filter(|r| r.vertex_type == rbsp::lumps::render::VERTEX_TYPE_LIT_BUMP as u8) {
        for t in run.indices.chunks_exact(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| run.positions[i as usize]);
            let n = rbake::light::front_normal(a, b, c);
            let d = n.dot(run.normals[t[0] as usize]);
            if d > 1.0e-6 {
                with += 1;
            } else if d < -1.0e-6 {
                against += 1;
            }
        }
    }
    assert!(with > 100_000, "only {with} lit triangles decoded");
    assert!(against * 100 < with, "{against} of {} decoded triangles face away from their normals", with + against);
}

/// Every face of a brush gets texels of its own: a triangle's lightmap
/// footprint has area in proportion to its world area, and no two faces
/// share page texels. Charting a whole surface onto one plane collapsed the
/// walls into lines on the floor's chart, so they sampled the floor's light.
#[test]
fn every_face_has_its_own_lightmap_texels() {
    use rbsp::lumps::render as r;
    let out = compile(&box_room(), &Fs::new(), &Assets::new(), &options()).expect("the room compiles");
    let client = &out.client.as_ref().expect("a client map").bsp;
    let v = rbsp::Version::ClientV51;
    let meshes = r::parse_meshes(client.lump(rbsp::ids::MESHES), v).expect("meshes");
    let sorts = r::parse_material_sorts(client.lump(rbsp::ids::MATERIAL_SORT), v).expect("sorts");
    let header = client.lump(rbsp::ids::LIGHTMAP_HEADERS);
    let page = u16::from_le_bytes([header[4], header[5]]) as f32;
    let indices = client.lump(rbsp::ids::MESH_INDICES);
    let bump = client.lump(rbsp::ids::VERTEX_LIT_BUMP);
    let verts = rbsp::lumps::prims::parse(client.lump(rbsp::ids::VERTICES), v).expect("positions");
    let f = |o: usize| f32::from_le_bytes(bump[o..o + 4].try_into().unwrap());
    let mut boxes: Vec<(glam::Vec3, [f32; 4])> = Vec::new();
    for m in meshes.iter().filter(|m| m.vertex_type & 3 == r::VERTEX_TYPE_LIT_BUMP as u8) {
        let base = sorts[m.material_sort as usize].first_vertex as usize;
        for t in 0..m.triangle_count as usize {
            let corner = |k: usize| {
                let at = (m.first_index as usize + t * 3 + k) * 2;
                let rec = (base + u16::from_le_bytes([indices[at], indices[at + 1]]) as usize) * 32;
                let p = verts[u32::from_le_bytes(bump[rec..rec + 4].try_into().unwrap()) as usize];
                (glam::Vec3::new(p.x, p.y, p.z), glam::Vec2::new(f(rec + 20), f(rec + 24)) * page)
            };
            let [(a, ta), (b, tb), (c, tc)] = [corner(0), corner(1), corner(2)];
            let world = (b - a).cross(c - a).length() * 0.5;
            if world < 1.0 {
                continue;
            }
            let texels = ((tb - ta).perp_dot(tc - ta) * 0.5).abs();
            assert!(texels > world / (64.0 * 64.0), "a {world} unit triangle covers {texels} texels");
            let n = (c - a).cross(b - a).normalize();
            let lo = ta.min(tb).min(tc);
            let hi = ta.max(tb).max(tc);
            boxes.push((n, [lo.x, lo.y, hi.x, hi.y]));
        }
    }
    assert!(boxes.len() > 4, "the room has lit faces");
    for (i, (na, a)) in boxes.iter().enumerate() {
        for (nb, b) in &boxes[i + 1..] {
            if na.dot(*nb) > 0.99 {
                continue;
            }
            let overlap = a[0] < b[2] - 0.5 && b[0] < a[2] - 0.5 && a[1] < b[3] - 0.5 && b[1] < a[3] - 0.5;
            assert!(!overlap, "faces facing {na} and {nb} share texels: {a:?} {b:?}");
        }
    }
}

/// Brushes left at the default contents collide with the player. A default of
/// 0 wrote the empty mask, so every BVH node was pruned and the player fell
/// through the room.
#[test]
fn default_brushes_block_player_movement() {
    // CONTENTS_SOLID | CONTENTS_WINDOW | CONTENTS_GRATE | CONTENTS_MOVEABLE |
    // CONTENTS_PLAYERCLIP | CONTENTS_MONSTER, the engine's player trace.
    const TRACE_MASK_PLAYERSOLID: u32 = 0x1 | 0x2 | 0x8 | 0x4000 | 0x10000 | 0x2000000;
    let out = compile(&box_room(), &Fs::new(), &Assets::new(), &options()).expect("the room compiles");
    for (label, map) in [("client", out.client.as_ref()), ("dedi", out.dedi.as_ref())] {
        let bsp = &map.expect("both targets").bsp;
        let leaves = rcomp::collision::decode_leaves(bsp, 0).expect("the world collision decodes");
        assert!(!leaves.is_empty(), "{label}: the world has collision");
        for l in &leaves {
            assert_eq!(l.contents, rcomp::collision::CONTENTS_WORLD, "{label}: a default brush writes the world mask");
            assert_ne!(l.contents & TRACE_MASK_PLAYERSOLID, 0, "{label}: the player trace hits the world");
        }
    }
}

/// A brush face's texture coordinates are in texture tiles, as the editor
/// draws them: at scale 1 a 512 texel texture repeats every 512 units.
/// Writing the raw texel coordinate tiled the dev texture 512 times too often.
#[test]
fn brush_texture_coordinates_are_in_tiles() {
    let out = compile(&box_room(), &Fs::new(), &Assets::new(), &options()).expect("the room compiles");
    let client = &out.client.as_ref().expect("a client map").bsp;
    let v = rbsp::Version::ClientV51;
    let texdata = rbsp::lumps::render::parse_texdata(client.lump(rbsp::ids::TEXTURE_DATA), v).expect("texdata");
    for t in &texdata {
        assert_eq!((t.width, t.height), (512, 512), "a texdata record declares the tile size");
    }
    let verts = rbsp::lumps::prims::parse(client.lump(rbsp::ids::VERTICES), v).expect("positions");
    let reach = verts.iter().map(|p| p.x.abs().max(p.y.abs()).max(p.z.abs())).fold(0.0f32, f32::max);
    let bump = client.lump(rbsp::ids::VERTEX_LIT_BUMP);
    let f = |o: usize| f32::from_le_bytes(bump[o..o + 4].try_into().unwrap());
    for rec in 0..bump.len() / 32 {
        let (u, w) = (f(rec * 32 + 8), f(rec * 32 + 12));
        assert!(u.abs().max(w.abs()) <= reach / 512.0 + 1.0e-3, "texture coordinate ({u}, {w}) past {reach} units of scale-1 faces");
    }
}

/// A map that names a sky vista gets the skybox the shipped maps build: a
/// shell of sky faces around the world, drawn unlit after the world's meshes
/// (flags 0x404, no lightmap, texdata 128 x 128 with flags 0x404), a
/// `sky_camera` 152 units above the vista, and the vista flagged 0x40 so it
/// draws only in the skybox. The shell must not darken the world's light.
#[test]
fn a_named_vista_builds_the_skybox() {
    use rbsp::lumps::{collision, render as r, Lump};
    let vista = "mdl/props/one.rmdl";
    let mut plain = box_room();
    plain.settings.bake.texel_size = 64.0;
    plain.settings.bake.samples = 4;
    plain.settings.bake.bounces = 0;
    let mut doc = plain.clone();
    doc.meta.sky = Some(vista.to_string());
    let opts = CompileOptions { bake: rbake::BakeMode::Preview, ..options() };
    let without = compile(&plain, &Fs::new(), &Assets::new(), &opts).expect("the room compiles");
    let out = compile(&doc, &Fs::new(), &Assets::new(), &opts).expect("the room compiles with a sky");
    let client = &out.client.as_ref().expect("a client map").bsp;
    let v = rbsp::Version::ClientV51;

    let meshes = r::parse_meshes(client.lump(rbsp::ids::MESHES), v).expect("meshes");
    let sorts = r::parse_material_sorts(client.lump(rbsp::ids::MATERIAL_SORT), v).expect("sorts");
    let texdata = r::parse_texdata(client.lump(rbsp::ids::TEXTURE_DATA), v).expect("texdata");
    let models = collision::parse_models(client.lump(rbsp::ids::MODELS), v).expect("models");
    let li = match rbsp::lumps::parse(rbsp::ids::LEVEL_INFO, v, client.lump(rbsp::ids::LEVEL_INFO)).expect("level info") {
        Lump::LevelInfo(li) => li,
        other => panic!("level info decoded as {}", other.kind()),
    };
    let world = models[0].num_meshes as usize;
    assert!((li.first_sky_mesh as usize) < world, "the sky meshes close the world model");
    for (i, m) in meshes.iter().enumerate().take(world) {
        let sky = i >= li.first_sky_mesh as usize;
        let sort = sorts[m.material_sort as usize];
        let td = texdata[sort.tex_data as usize];
        if sky {
            assert_eq!(m.flags, 0x404, "sky mesh {i}");
            assert_eq!(m.vertex_type & 3, 0, "sky mesh {i} is unlit");
            assert_eq!(sort.lightmap, -1, "sky mesh {i} samples no lightmap");
            assert_eq!((td.width, td.height, td.flags), (128, 128, 0x404), "sky texdata");
        } else {
            assert_ne!(m.flags & rcomp::meshes::MESH_FLAG_WORLD, 0, "world mesh {i}");
        }
    }
    let refs = rbsp::lumps::vis::parse_obj_refs(client.lump(rbsp::ids::OBJ_REFERENCES), v).expect("refs");
    for r in &refs {
        assert!(r.get() < li.first_sky_mesh || r.get() >= li.first_static_prop_index, "a cell references sky mesh {}", r.get());
    }

    let gl = match rbsp::lumps::parse(rbsp::ids::GAME_LUMP, v, client.lump(rbsp::ids::GAME_LUMP)).expect("game lump") {
        Lump::GameLump(g) => g,
        other => panic!("game lump decoded as {}", other.kind()),
    };
    let sprp = gl.static_props().expect("props");
    let vistas: Vec<_> = sprp.props.iter().filter(|p| sprp.models[p.model_index as usize] == vista && p.unknown_1 & 0x40 != 0).collect();
    assert_eq!(vistas.len(), 1, "one vista, flagged for the skybox");
    assert_eq!(sprp.props.iter().filter(|p| p.unknown_1 & 0x40 != 0).count(), 1, "no other prop is flagged");
    let camera = format!(
        "\"origin\" \"{} {} {}\"",
        vistas[0].origin[0],
        vistas[0].origin[1],
        vistas[0].origin[2] + rcomp::sky::CAMERA_ABOVE_VISTA as f32
    );
    let text: String = out.client.as_ref().unwrap().ent_bytes.values().map(|b| String::from_utf8_lossy(b).into_owned()).collect();
    let block = text.split('{').find(|b| b.contains("\"classname\" \"sky_camera\"")).expect("a sky_camera is placed");
    assert!(block.contains(&camera), "the camera sits above the vista: {block}");
    // The server reads both keys into one scale; the later one must be 1000.
    let at = |k: &str| block.find(k).unwrap_or_else(|| panic!("{k} missing: {block}"));
    assert!(at("\"scale\" \"1\"") < at("\"skyscale\" \"1000\""), "skyscale comes after scale: {block}");

    // The server lists the client's props, the vista included.
    let dedi = &out.dedi.as_ref().expect("a server map").bsp;
    let dv = rbsp::Version::DediV47;
    let dgl = match rbsp::lumps::parse(rbsp::ids::GAME_LUMP, dv, dedi.lump(rbsp::ids::GAME_LUMP)).expect("game lump") {
        Lump::GameLump(g) => g,
        other => panic!("game lump decoded as {}", other.kind()),
    };
    let dsprp = dgl.static_props().expect("props");
    assert_eq!(dsprp.props, sprp.props, "the server lists the client's props");
    assert_eq!(dsprp.counts, sprp.counts, "and the same opaque and translucent ranges");
    // Both halves end with the shipped maps' skybox cell: no portals, skyFlags
    // 3, an unbounded root holding only the vista, and a cell BSP that sends
    // the sky camera into it and the world into cell 0.
    let cam = [vistas[0].origin[0], vistas[0].origin[1], vistas[0].origin[2] + rcomp::sky::CAMERA_ABOVE_VISTA as f32];
    let first_prop = li.first_static_prop_index;
    for (bsp, ver) in [(client, v), (dedi, dv)] {
        let cells = rbsp::lumps::vis::parse_cells(bsp.lump(rbsp::ids::CELLS), ver).expect("cells");
        assert_eq!(cells.len(), 2, "the world cell and the skybox cell");
        assert!(cells.iter().all(|c| c.sky_flags & rbsp::lumps::vis::CELL_SKY_VISIBLE != 0), "the sky is visible from every cell");
        let sky = cells[1];
        assert_eq!((sky.portal_count, sky.sky_flags, sky.unused), (0, 3, 0xFFFF), "the skybox cell");
        let nodes = rbsp::lumps::vis::parse_aabb_nodes(bsp.lump(0x77), ver).expect("cell nodes");
        let root = nodes[1];
        assert!(root.mins.iter().all(|m| *m == f32::NEG_INFINITY) && root.maxs.iter().all(|m| *m == f32::INFINITY), "an unbounded root");
        assert_eq!(root.child_count(), 0);
        let refs = rbsp::lumps::vis::parse_obj_refs(bsp.lump(rbsp::ids::OBJ_REFERENCES), ver).expect("refs");
        let vista_ref = first_prop + sprp.props.iter().position(|p| p.unknown_1 & 0x40 != 0).unwrap() as u32;
        let own: Vec<u32> = refs[root.first_obj_ref() as usize..(root.first_obj_ref() + root.obj_ref_count()) as usize].iter().map(|r| r.get()).collect();
        assert_eq!(own, vec![vista_ref], "the skybox cell holds the vista, last");
        assert_eq!(refs.len() as u32, root.first_obj_ref() + 1, "its reference is the last one");
        assert_eq!(refs.iter().filter(|r| r.get() == vista_ref).count(), 1, "the world cell does not reference the vista");
        let planes = match rbsp::lumps::parse(rbsp::ids::PLANES, ver, bsp.lump(rbsp::ids::PLANES)).expect("planes") {
            Lump::Plane(p) => p,
            other => panic!("planes decoded as {}", other.kind()),
        };
        let bsp_nodes = rbsp::lumps::vis::parse_cell_bsp_nodes(bsp.lump(rbsp::ids::CELL_BSP_NODES), ver).expect("cell bsp");
        let cell_of = |p: [f32; 3]| {
            let mut n = 0usize;
            while !bsp_nodes[n].is_terminator() {
                let pl = &planes[bsp_nodes[n].contents as usize];
                let d = pl.normal[0] * p[0] + pl.normal[1] * p[1] + pl.normal[2] * p[2] - pl.dist;
                n = bsp_nodes[n].child as usize + usize::from(d < 0.0);
            }
            bsp_nodes[n].child
        };
        assert_eq!(cell_of(cam), 1, "the sky camera stands in the skybox cell");
        assert_eq!(cell_of([0.0, 0.0, 16.0]), 0, "the room is the world cell");
    }

    let page = |bsp: &rbsp::Bsp| bsp.lump(rbsp::ids::LIGHTMAP_DATA_SKY).to_vec();
    assert_eq!(page(client), page(&without.client.as_ref().unwrap().bsp), "the sky leaves the world's light as it was");
}

/// A map naming a Source skybox draws its six faces on a cube around the far
/// vista position: six unlit meshes with no lightmap, held by the skybox cell
/// with the sky camera inside them, and no collision.
#[test]
fn a_source_sky_draws_its_six_faces_around_the_vista() {
    use rbsp::lumps::{render as r, vis as vl, Lump};
    let mut doc = box_room();
    doc.meta.source_sky = Some("tides".into());
    let mut assets = Assets::new();
    assets.add_source_sky("tides");
    let mut vista_doc = box_room();
    vista_doc.meta.sky = Some("mdl/props/one.rmdl".into());
    let opts = CompileOptions { bake: rbake::BakeMode::Preview, ..options() };
    let plain = compile(&box_room(), &Fs::new(), &Assets::new(), &opts).expect("the room compiles");
    let vista = compile(&vista_doc, &Fs::new(), &Assets::new(), &opts).expect("the room compiles with a vista");
    let out = compile(&doc, &Fs::new(), &assets, &opts).expect("the room compiles with a sky");
    let client = &out.client.as_ref().expect("a client map").bsp;
    let v = rbsp::Version::ClientV51;

    let runs = rcomp::meshes::decode_meshes(client).expect("meshes decode");
    let faces: Vec<(usize, &rcomp::meshes::DecodedRun)> =
        runs.iter().enumerate().filter(|(_, m)| rcomp::sky::is_source_sky_material(&m.material)).collect();
    assert_eq!(faces.len(), 6, "one mesh per skybox face");
    let want: Vec<String> = rcomp::sky::face_materials("tides").into_iter().collect();
    assert_eq!(faces.iter().map(|(_, m)| m.material.clone()).collect::<Vec<_>>(), want);
    let meshes = r::parse_meshes(client.lump(rbsp::ids::MESHES), v).expect("meshes");
    let sorts = r::parse_material_sorts(client.lump(rbsp::ids::MATERIAL_SORT), v).expect("sorts");
    for (i, run) in &faces {
        let m = &meshes[*i];
        assert_eq!(m.vertex_type & 3, 0, "face mesh {i} is unlit");
        assert_eq!(
            m.flags,
            rcomp::meshes::MESH_FLAG_WORLD | rcomp::meshes::MESH_FLAG_PLAIN,
            "face mesh {i} is a world mesh"
        );
        assert_eq!(sorts[m.material_sort as usize].lightmap, -1, "face mesh {i} samples no lightmap");
        let n = run.normals[0];
        assert!(run.normals.iter().all(|x| *x == n), "face mesh {i} has one inward normal");
        let (_, want_n, _, _) =
            rcomp::sky::SOURCE_SKY_FACES.iter().find(|(f, _, _, _)| run.material.ends_with(f)).expect("a known face");
        assert_eq!(n, *want_n, "the material lands on its own axis");
    }

    let text: String = out.client.as_ref().unwrap().ent_bytes.values().map(|b| String::from_utf8_lossy(b).into_owned()).collect();
    let block = text.split('{').find(|b| b.contains("\"classname\" \"sky_camera\"")).expect("a sky_camera is placed");
    let quoted: Vec<&str> = block.split('"').skip(1).step_by(2).collect();
    let mut origin = None;
    for pair in quoted.chunks_exact(2) {
        if pair[0] == "origin" {
            origin = Some(pair[1].split_whitespace().filter_map(|x| x.parse::<f32>().ok()).collect::<Vec<_>>());
        }
    }
    let [cx, cy, cz] = origin.expect("the camera has an origin")[..] else { panic!("the camera has an origin") };
    assert!((cx + 38434.0).abs() < 1.0 && (cy + 29834.0).abs() < 1.0, "the camera stands at the far vista ({cx} {cy} {cz})");
    for (_, run) in &faces {
        let n = run.normals[0];
        for p in &run.positions {
            assert!((*p - Vec3::new(cx, cy, cz)).dot(n) > 0.0, "the camera stands inside every face");
        }
    }

    assert_eq!(
        out.report.stats.collision_solids, vista.report.stats.collision_solids,
        "the cube adds no collision beyond the shell a vista build has"
    );

    let refs = vl::parse_obj_refs(client.lump(rbsp::ids::OBJ_REFERENCES), v).expect("refs");
    let at: Vec<u32> = faces.iter().map(|(i, _)| *i as u32).collect();
    for r in &at {
        assert_eq!(refs.iter().filter(|x| x.get() == *r).count(), 1, "face mesh {r} is referenced once");
    }
    let cam = [cx, cy, cz];
    for (bsp, ver) in [(client, v), (&out.dedi.as_ref().expect("a server map").bsp, rbsp::Version::DediV47)] {
        let cells = vl::parse_cells(bsp.lump(rbsp::ids::CELLS), ver).expect("cells");
        assert_eq!(cells.len(), 2, "the world cell and the skybox cell");
        let refs = vl::parse_obj_refs(bsp.lump(rbsp::ids::OBJ_REFERENCES), ver).expect("refs");
        let nodes = vl::parse_aabb_nodes(bsp.lump(0x77), ver).expect("cell nodes");
        let root = nodes[1];
        let own: Vec<u32> =
            refs[root.first_obj_ref() as usize..(root.first_obj_ref() + root.obj_ref_count()) as usize].iter().map(|r| r.get()).collect();
        assert_eq!(own, at, "the skybox cell holds the six face meshes");
        let planes = match rbsp::lumps::parse(rbsp::ids::PLANES, ver, bsp.lump(rbsp::ids::PLANES)).expect("planes") {
            Lump::Plane(p) => p,
            other => panic!("planes decoded as {}", other.kind()),
        };
        let bsp_nodes = vl::parse_cell_bsp_nodes(bsp.lump(rbsp::ids::CELL_BSP_NODES), ver).expect("cell bsp");
        let cell_of = |p: [f32; 3]| {
            let mut n = 0usize;
            while !bsp_nodes[n].is_terminator() {
                let pl = &planes[bsp_nodes[n].contents as usize];
                let d = pl.normal[0] * p[0] + pl.normal[1] * p[1] + pl.normal[2] * p[2] - pl.dist;
                n = bsp_nodes[n].child as usize + usize::from(d < 0.0);
            }
            bsp_nodes[n].child
        };
        assert_eq!(cell_of(cam), 1, "the sky camera stands in the skybox cell");
        assert_eq!(cell_of([0.0, 0.0, 16.0]), 0, "the room is the world cell");
    }

    let page = |bsp: &rbsp::Bsp| bsp.lump(rbsp::ids::LIGHTMAP_DATA_SKY).to_vec();
    assert_eq!(page(client), page(&plain.client.as_ref().unwrap().bsp), "the sky leaves the world's light as it was");
}
