//! the vis codec against a shipped map.

mod common;

use common::*;

#[test]
fn vis_single_cell_valid() {
    let doc = box_room();
    let out = compile(&doc, &Fs::new(), &Assets::new(), &options()).expect("the room compiles");
    let client = out.client.as_ref().expect("a client map");
    let lumps = vis::read(&client.bsp, rbsp::Version::ClientV51).expect("the vis lumps decode");
    assert_eq!(lumps.cells.len(), 1, "a map without hint brushes has one cell");
    assert_eq!(lumps.cell_bsp_nodes.len(), 1);
    assert!(lumps.cell_bsp_nodes[0].is_terminator());
    assert!(!lumps.obj_refs.is_empty(), "the cell references its objects");
    assert_eq!(lumps.obj_refs.len(), lumps.obj_ref_bounds.len());
    assert_eq!(lumps.totals.len(), lumps.aabb_nodes.len());
    assert_eq!(lumps.fade.len(), lumps.aabb_nodes.len());
    assert_eq!(lumps.totals[0] as usize, lumps.obj_refs.len());
    assert!(lumps.obj_refs.contains(&0), "the world model is referenced");
    let first_prop = client.bsp.lump(rbsp::ids::MODELS).len() / 64;
    assert!(
        lumps.obj_refs.iter().any(|r| *r as usize >= first_prop),
        "the static props are referenced past the model count"
    );
    // Re-encoding reproduces the bytes.
    for (id, data) in lumps.encode(rbsp::Version::ClientV51) {
        compare(data.as_slice(), client.bsp.lump(id), &format!("vis lump 0x{id:02x}"));
    }
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn vis_cells_roundtrip_client_fixture() {
    let dir = fixtures().join("client").join("_test_s21_in");
    let bsp_path = dir.join("mp_rr_canyonlands_staging_mu1.bsp");
    if !bsp_path.is_file() {
        panic!("the client fixture is missing");
    }
    let bsp = rbsp::Bsp::read_path(&bsp_path).expect("the client fixture reads");
    assert_eq!(bsp.header.version, 51);
    let lumps = vis::read(&bsp, rbsp::Version::ClientV51).expect("the cell lumps decode");
    assert!(lumps.cells.len() > 1, "a shipped map has many cells");
    assert!(!lumps.aabb_nodes.is_empty());
    assert_eq!(lumps.portal_vert_edges.len(), lumps.portal_vertices.len());
    for (i, n) in lumps.aabb_nodes.iter().enumerate() {
        assert!(
            n.first_child() as usize + n.child_count() as usize <= lumps.aabb_nodes.len(),
            "node {i} names children past the end"
        );
    }
    let round = lumps.encode(rbsp::Version::ClientV51);
    for (id, data) in round {
        compare(data.as_slice(), bsp.lump(id), &format!("lump 0x{id:02x}"));
    }
}

// ---- Titanfall 2 conversion -------------------------------------------------

