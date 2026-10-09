//! The reference layer, against a hand-built map and against a shipped one.
//!
//! The hand-built map carries the render lumps byte for byte: one lit-mapped
//! quad in a run that starts part way into the vertex lump, one unlit-mapped
//! quad in a run of its own, an index stream that is relative to each mesh's
//! own first vertex, and a material name reached through the texdata table.

mod common;

use std::path::PathBuf;

use r5ed_host::reference::{self, Decoded};
use rbsp::Bsp;

const MESH: usize = 28;
const SORT: usize = 12;
const TEXDATA: usize = 16;
/// `LUMP_VERTS_LIT_BUMP`: position, normal, tangent, then the packed stream.
const LIT_BUMP: usize = 32;
/// `LUMP_VERTS_UNLIT`: position, normal, then the packed stream.
const UNLIT: usize = 20;

fn f32s(v: &[[f32; 3]]) -> Vec<u8> {
    v.iter().flat_map(|p| p.iter().flat_map(|x| x.to_le_bytes())).collect()
}

fn u16s(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn mesh(first_index: u32, tris: u16, first_vertex: u16, verts: u16, vtype: u8, sort: u16) -> Vec<u8> {
    let mut r = vec![0u8; MESH];
    r[0..4].copy_from_slice(&first_index.to_le_bytes());
    r[4..6].copy_from_slice(&tris.to_le_bytes());
    r[6..8].copy_from_slice(&first_vertex.to_le_bytes());
    r[8..10].copy_from_slice(&verts.to_le_bytes());
    r[10] = vtype;
    r[22..24].copy_from_slice(&sort.to_le_bytes());
    r
}

fn sort(tex: u16, lightmap: i16, first_vertex: u32) -> Vec<u8> {
    let mut r = vec![0u8; SORT];
    r[0..2].copy_from_slice(&tex.to_le_bytes());
    r[2..4].copy_from_slice(&lightmap.to_le_bytes());
    r[8..12].copy_from_slice(&first_vertex.to_le_bytes());
    r
}

fn texdata(name: u32) -> Vec<u8> {
    let mut r = vec![0u8; TEXDATA];
    r[0..4].copy_from_slice(&name.to_le_bytes());
    r[4..8].copy_from_slice(&128i32.to_le_bytes());
    r[8..12].copy_from_slice(&128i32.to_le_bytes());
    r
}

/// One lit-bump record per entry: position, normal, a tangent, then the packed
/// stream that fills the record out.
fn lit_bump(positions: &[(u32, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (p, n) in positions {
        out.extend_from_slice(&p.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.resize(out.len() + (LIT_BUMP - 8), 0);
    }
    out
}

/// One unlit record per entry: position, normal, then the packed stream.
fn unlit(positions: &[(u32, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (p, n) in positions {
        out.extend_from_slice(&p.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.resize(out.len() + (UNLIT - 8), 0);
    }
    out
}

/// A client map with two quads: a lit-mapped one whose vertices start at 2, and
/// an unlit-mapped one that owns the first two vertices of its own run.
fn two_quad_map() -> Bsp {
    let mut b = Bsp::new(51, 1, 0);
    // Four positions and two normals; the lit run uses 2..6 and the unlit 0..2.
    b.set_lump(0x03, f32s(&[[0.0, 0.0, 0.0], [8.0, 0.0, 4.0], [0.0, 16.0, 4.0], [8.0, 16.0, 8.0]]));
    b.set_lump(0x1E, f32s(&[[0.0, 0.0, 1.0], [1.0, 0.0, 0.0]]));
    // Seven lit-bump records: the sort's base of 2 means the quad's run of five
    // is the last five.
    b.set_lump(0x49, lit_bump(&[(0, 0), (0, 0), (2, 0), (3, 0), (0, 1), (1, 1), (0, 0)]));
    b.set_lump(0x47, unlit(&[(0, 0), (1, 1), (2, 0), (3, 0), (0, 0)]));
    // Indices are offsets from the sort's base, so each mesh's run starts at
    // its own first vertex.
    b.set_lump(0x4F, u16s(&[0, 1, 2, 2, 1, 3, 0, 1, 2, 2, 1, 3]));
    b.set_lump(0x50, {
        let mut m = mesh(0, 2, 0, 4, 2, 0);
        m.extend_from_slice(&mesh(6, 2, 0, 4, 0, 1));
        m
    });
    b.set_lump(0x52, {
        let mut s = sort(0, 0, 2);
        s.extend_from_slice(&sort(1, -1, 0));
        s
    });
    let mut names = Vec::new();
    names.extend_from_slice(b"world/wall_concrete\0");
    names.extend_from_slice(b"tools/toolblack\0");
    b.set_lump(0x02, {
        let mut t = texdata(0);
        t.extend_from_slice(&texdata(20));
        t
    });
    b.set_lump(0x0F, names);
    b.set_lump(
        0x00,
        rbsp::lumps::entities::write(
            &rbsp::lumps::entities::EntityText {
                entities: vec![rbsp::lumps::entities::Entity::from(vec![
                    ("classname".into(), "worldspawn".into()),
                    ("message".into(), "reference".into()),
                ])],
                ..Default::default()
            },
            rbsp::Version::ClientV51,
        ),
    );
    b
}

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("r5ed-reference-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    d
}

#[test]
fn a_lit_quad_decodes_with_its_own_vertex_base() {
    let d = reference::decode(&two_quad_map()).expect("decode");
    assert_eq!(d.meshes, 2, "both meshes are read");
    assert_eq!(d.decoded, 2);
    assert_eq!(d.triangles, 4);
    assert!(d.skipped.is_empty(), "{:?}", d.skipped);
    assert_eq!(d.chunks.len(), 1);
    let prims = &d.chunks[0].primitives;
    assert_eq!(prims.len(), 2);
    assert_eq!(prims[0].name, "world/wall_concrete");
    assert_eq!(prims[0].positions.len(), 5);
    // The sort's base of 2 puts the quad on the third and fourth positions.
    // A run spans its count inclusive, so a four-vertex quad carries five.
    assert_eq!(
        prims[0].positions,
        vec![[0.0, 16.0, 4.0], [8.0, 16.0, 8.0], [0.0, 0.0, 0.0], [8.0, 0.0, 4.0], [0.0, 0.0, 0.0]]
    );
    // The index stream is offset from the sort's base, so it is rebased onto
    // the mesh's own first vertex before it reaches the page.
    assert_eq!(prims[0].indices, vec![0, 1, 2, 2, 1, 3]);
    assert_eq!(prims[1].indices, vec![0, 1, 2, 2, 1, 3]);
    assert_eq!(
        prims[0].normals,
        vec![[0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]
    );
    // The unlit-mapped run starts at zero in its own lump.
    assert_eq!(prims[1].name, "tools/toolblack");
    assert_eq!(prims[1].positions[0], [0.0, 0.0, 0.0]);
    assert_eq!(prims[1].positions[3], [8.0, 16.0, 8.0]);
    assert_eq!(prims[1].positions[4], [0.0, 0.0, 0.0]);
    assert_eq!(d.bounds, ([0.0, 0.0, 0.0], [8.0, 16.0, 8.0]));
    assert_eq!(d.entities.len(), 1);
    assert_eq!(d.entities[0].classname, "worldspawn");
}

#[test]
fn a_mesh_whose_indices_are_out_of_range_is_counted_not_guessed() {
    let mut b = two_quad_map();
    // The second mesh claims a run that runs off the end of its lump.
    let mut m = b.lump(0x50).to_vec();
    m[MESH + 8..MESH + 10].copy_from_slice(&400u16.to_le_bytes());
    b.set_lump(0x50, m);
    let d = reference::decode(&b).expect("decode");
    assert_eq!(d.decoded, 1);
    assert_eq!(d.skipped.len(), 1, "{:?}", d.skipped);
    assert!(d.skipped[0].contains("vertex range"), "{:?}", d.skipped);
}

#[test]
fn a_mesh_pointing_past_its_index_lump_is_skipped() {
    let mut b = two_quad_map();
    let mut m = b.lump(0x50).to_vec();
    m[MESH..MESH + 4].copy_from_slice(&9_000u32.to_le_bytes());
    b.set_lump(0x50, m);
    let d = reference::decode(&b).expect("decode");
    assert_eq!(d.decoded, 1);
    assert!(d.skipped[0].contains("index range"), "{:?}", d.skipped);
}

#[test]
fn an_index_outside_its_own_mesh_is_not_trusted() {
    let mut b = two_quad_map();
    // The first mesh indexes vertex 9, outside its own run of four.
    let mut idx = b.lump(0x4F).to_vec();
    idx[0..2].copy_from_slice(&9u16.to_le_bytes());
    b.set_lump(0x4F, idx);
    let d = reference::decode(&b).expect("decode");
    assert_eq!(d.decoded, 1);
    assert!(d.skipped[0].contains("index"), "{:?}", d.skipped);
}

#[test]
fn a_version_this_build_cannot_show_is_refused() {
    let mut b = two_quad_map();
    b.header.version = 99;
    assert!(reference::decode(&b).is_err());
    let mut tf2 = two_quad_map();
    tf2.header.version = 37;
    let e = match reference::decode(&tf2) {
        Ok(_) => panic!("a v37 map must not decode"),
        Err(e) => e,
    };
    assert!(e.to_string().contains("v37"), "{e}");
}

#[test]
fn a_dedicated_map_with_no_render_lumps_decodes_to_nothing() {
    let mut b = two_quad_map();
    for id in [0x03u16, 0x1E, 0x47, 0x49, 0x4F] {
        b.set_lump(id, Vec::new());
    }
    b.set_lump(0x50, Vec::new());
    b.set_lump(0x52, Vec::new());
    b.header.version = 47;
    let d: Decoded = reference::decode(&b).expect("decode");
    assert_eq!(d.meshes, 0);
    assert_eq!(d.decoded, 0);
    assert!(d.chunks.is_empty());
    assert_eq!(d.bounds, ([0.0; 3], [0.0; 3]));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_reference_routes_serve_a_document_and_its_chunks() {
    use common::HarnessBuilder;
    let h = HarnessBuilder::new("ref-routes").start().await.expect("harness");
    // Nothing imported yet.
    assert_eq!(h.get("/api/maps/mp_x/reference").status, 404);
    assert_eq!(h.get("/api/maps/mp_x/chunk/0").status, 404);

    let stem = "mp_ref";
    let dir = reference::imported_bsp(&h.config.cache_root, stem);
    std::fs::create_dir_all(dir.parent().unwrap()).expect("import dir");
    two_quad_map().write_path(&dir).expect("write map");
    // The map has to survive its own round trip, so the decode is checked
    // before the route is asked for it.
    let back = Bsp::read_path(&dir).expect("read back");
    assert_eq!(reference::decode(&back).expect("decode").decoded, 2, "the written map decodes");
    reference::build(&h.config.cache_root, stem).expect("build");

    let r = h.get(&format!("/api/maps/{stem}/reference"));
    assert_eq!(r.status, 200, "{}", r.text());
    let doc: serde_json::Value = serde_json::from_str(&r.text()).expect("document");
    assert_eq!(doc["stem"], stem);
    assert_eq!(doc["chunks"].as_array().expect("chunks").len(), 1);
    assert_eq!(doc["chunks"][0], format!("/api/maps/{stem}/chunk/0"));
    assert_eq!(doc["meshes"], 2);
    assert_eq!(doc["decoded"], 2);
    assert_eq!(doc["triangles"], 4);
    assert_eq!(doc["bounds"]["max"][1], 16.0);
    assert!(doc["skipped"].as_array().expect("skipped").is_empty());

    let c = h.get(&format!("/api/maps/{stem}/chunk/0"));
    assert_eq!(c.status, 200);
    assert_eq!(c.header("content-type"), Some("model/gltf-binary"));
    // The bytes over the wire are the bytes on disk.
    let on_disk = reference::chunk(&h.config.cache_root, stem, 0).expect("chunk");
    assert_eq!(c.body.len(), on_disk.len());
    assert_eq!(c.body, on_disk);
    let glb = rpak::glb::read_glb(&on_disk).expect("a glb");
    assert_eq!(glb.json["meshes"][0]["primitives"].as_array().expect("prims").len(), 2);
    assert_eq!(glb.json["meshes"][0]["primitives"][0]["indices"], 2, "the first index accessor");
    assert_eq!(glb.json["accessors"][2]["count"], 6, "six indices in the first quad");
    assert_eq!(glb.json["accessors"][0]["count"], 5, "five positions in the first quad");
    assert_eq!(glb.json["accessors"][0]["max"][0], 8.0);
    assert_eq!(glb.json["materials"][1]["name"], "tools/toolblack");
    assert!(glb.bin_len > 0);

    // A stem that is not a stem, and a chunk that does not exist.
    assert_eq!(h.get("/api/maps/..%2Fetc/reference").status, 400);
    assert_eq!(h.get(&format!("/api/maps/{stem}/chunk/9")).status, 404);
    assert_eq!(h.get(&format!("/api/maps/{stem}/chunk/99999")).status, 404);
}

fn fixtures() -> PathBuf {
    PathBuf::from(std::env::var("R5ED_FIXTURES").unwrap_or_else(|_| "r5ed-fixtures".into()))
}

/// A shipped client map must decode without losing a single mesh. The dedicated
/// maps carry no render vertex lumps at all, so only the client one is checked.
#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn a_shipped_client_map_decodes_completely() {
    let dir = fixtures().join("client").join("_test_s21_in");
    let mut checked = 0;
    for e in std::fs::read_dir(&dir).expect("fixture dir").flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".bsp") {
            continue;
        }
        let stem = name.trim_end_matches(".bsp");
        let (bsp, _) = Bsp::read_dir_map(&dir, stem).expect("read map");
        let d = reference::decode(&bsp).expect("decode");
        assert!(d.meshes > 0, "{stem} has no meshes");
        assert_eq!(d.decoded, d.meshes, "{stem} skipped {:?}", d.skipped);
        assert!(d.triangles > 0, "{stem}");
        assert!(d.chunks.iter().all(|c| c.primitives.iter().all(|p| p.indices.len() % 3 == 0)));
        // Every decoded position is inside the world and finite.
        for c in &d.chunks {
            for p in &c.primitives {
                for v in &p.positions {
                    assert!(v.iter().all(|x| x.is_finite() && x.abs() < 1.0e6), "{stem} {v:?}");
                }
            }
        }
        let cache = temp(&format!("fixture-{stem}"));
        std::fs::create_dir_all(cache.join("import").join(stem)).expect("import");
        bsp.write_path(&cache.join("import").join(stem).join(format!("{stem}.bsp"))).expect("write");
        let (doc, files) = reference::build(&cache, stem).expect("build");
        assert_eq!(doc.chunks.len(), d.chunks.len());
        assert_eq!(files.len(), d.chunks.len());
        assert!(doc.bounds.max[0] >= doc.bounds.min[0]);
        for (i, f) in files.iter().enumerate() {
            let bytes = reference::chunk(&cache, stem, i).expect("chunk");
            assert!(bytes.starts_with(b"glTF"), "chunk {i} is not a glb");
            assert_eq!(&bytes, &std::fs::read(f).expect("read back"));
        }
        assert!(reference::chunk(&cache, stem, d.chunks.len()).is_err(), "one past the end");
        checked += 1;
    }
    assert!(checked > 0, "no client fixture map was read");
}
