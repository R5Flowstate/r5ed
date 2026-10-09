//! Titanfall 2 conversion byte-parity against the reference converter.
//!
//! Every collider the conversion writes is compared against the reference
//! field by field, and byte for byte wherever the two describe the same
//! surface: where the same surface yields the same triangles it must also
//! yield the same bytes, so a remaining difference is one of surface and never
//! one of padding or ordering. Two fields are compared on their own terms.
//! The quantisation origin, because the shared volume builder writes the box
//! centre unrounded where the reference rounds it. The hull leaves, which are
//! not compared at all, because the reference hands the writer one face loop
//! per bounding plane while this crate re-derives the faces with its own
//! convex hull and the two cut a different set of faces out of the same
//! points.

mod common;

use common::*;

#[test]
#[ignore = "needs R5ED_TF2_BSP and R5ED_TF2_REF"]
fn tf2_parity_client() {
    check_tf2_parity("client", rbsp::Version::ClientV51);
}

#[test]
#[ignore = "needs R5ED_TF2_BSP and R5ED_TF2_REF"]
fn tf2_parity_dedi() {
    check_tf2_parity("dedi", rbsp::Version::DediV47);
}

#[test]
#[ignore = "needs R5ED_TF2_BSP and R5ED_TF2_REF"]
fn tf2_collider_matches_the_reference() {
    use rcomp::collision::{parse_coll_blob, CollTarget};
    let (mut total, mut identical) = (0usize, 0usize);
    for (target, version) in [("client", rbsp::Version::ClientV51), ("dedi", rbsp::Version::DediV47)] {
        let ct = if version == rbsp::Version::ClientV51 { CollTarget::Client } else { CollTarget::Dedi };
        for (part, where_, class, ours, theirs) in colliders(target, version) {
            let what = format!("{target} {part} {where_} {class}");
            let a = parse_coll_blob(&ours, ct).unwrap_or_else(|e| panic!("{what}: ours parses: {e}"));
            let b = parse_coll_blob(&theirs, ct)
                .unwrap_or_else(|e| panic!("{what}: the reference parses: {e}"));
            assert_eq!(a.contents, b.contents, "{what}: contents mask table");
            assert_eq!(a.surfprops, b.surfprops, "{what}: surface property table");
            assert_eq!(a.names, b.names, "{what}: surface name table");
            assert_eq!(a.fields.flags, b.fields.flags, "{what}: model flags");
            assert_eq!(a.fields.bvh_node, b.fields.bvh_node, "{what}: first bvh node");
            assert_eq!(a.fields.bvh_leaf, b.fields.bvh_leaf, "{what}: first leaf");
            assert_eq!(
                a.fields.decode_scale.to_bits(),
                b.fields.decode_scale.to_bits(),
                "{what}: decode scale"
            );
            assert_eq!(a.verts.len(), b.verts.len(), "{what}: quantised vertex count");
            for (i, (p, q)) in a.verts.iter().zip(&b.verts).enumerate() {
                assert_eq!(corner(p), corner(q), "{what}: quantised vertex {i}");
            }
            for axis in 0..3 {
                let mine = a.fields.origin[axis];
                let want = b.fields.origin[axis];
                assert!(
                    mine.round() == want,
                    "{what}: origin axis {axis} is {mine}, the reference's centre is {want}"
                );
            }
            if same_surface(&a.triangles(), &b.triangles()) {
                compare(&ours, &theirs, &format!("{what} collider blob"));
                identical += 1;
            }
            total += 1;
        }
    }
    assert!(total > 0, "the source map carries colliders");
    assert!(identical > 0, "some colliders describe the same surface as the reference");
}

fn corner(v: &[f32; 3]) -> [u32; 3] {
    [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()]
}

/// Two triangle sets that cover the same surface, each triangle read as the
/// same three corners whatever order they are wound in.
fn same_surface(a: &[[DVec3; 3]], b: &[[DVec3; 3]]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let key = |t: &[DVec3; 3]| {
        let mut v: Vec<[i64; 3]> = t.iter().map(|p| corner64(*p)).collect();
        v.sort_unstable();
        v
    };
    let mut x: Vec<Vec<[i64; 3]>> = a.iter().map(key).collect();
    let mut y: Vec<Vec<[i64; 3]>> = b.iter().map(key).collect();
    x.sort();
    y.sort();
    x == y
}

fn corner64(p: DVec3) -> [i64; 3] {
    [(p.x * 64.0).round() as i64, (p.y * 64.0).round() as i64, (p.z * 64.0).round() as i64]
}

/// Every `*coll` payload the conversion wrote, paired with the reference's, as
/// (partition, block, classname, ours, reference).
fn colliders(
    target: &str,
    version: rbsp::Version,
) -> Vec<(String, String, String, Vec<u8>, Vec<u8>)> {
    let ref_root =
        PathBuf::from(std::env::var("R5ED_TF2_REF").expect("R5ED_TF2_REF is set")).join(target);
    let (tables, _) = tf2_tables();
    let (src, ents) = tf2_source();
    let out = rcomp::tf2::convert_tf2(&src, &ents, version, &tables, "mp_rr_gauntlet")
        .unwrap_or_else(|e| panic!("the {target} conversion failed: {e}"));
    let reference = rbsp::Bsp::read_path(&ref_root.join("mp_rr_gauntlet.bsp"))
        .expect("the reference map reads");

    let mut sources: Vec<(String, Vec<u8>, Vec<u8>)> = vec![(
        "worldspawn".into(),
        out.artifacts.bsp.lump(rbsp::ids::ENTITIES).to_vec(),
        reference.lump(rbsp::ids::ENTITIES).to_vec(),
    )];
    for (part, data) in &out.artifacts.ent_bytes {
        let path = ref_root.join(format!("mp_rr_gauntlet_{part}.ent"));
        if path.is_file() {
            let theirs = std::fs::read(&path).expect("a reference partition reads");
            sources.push((part.clone(), data.clone(), theirs));
        }
    }

    let mut found = Vec::new();
    for (name, ours, theirs) in &sources {
        let a = blocks(ours);
        let b = blocks(theirs);
        assert_eq!(a.len(), b.len(), "{target} {name}: entity block count");
        for (i, (mine, other)) in a.iter().zip(b.iter()).enumerate() {
            let (Some(x), Some(y)) = (blob_of(mine), blob_of(other)) else { continue };
            found.push((name.clone(), format!("block {i}"), classname(mine), x, y));
        }
    }
    found
}

fn blocks(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(open) = data[i..].iter().position(|&b| b == b'{') {
        let open = i + open;
        let Some(close) = data[open..].iter().position(|&b| b == b'}') else { break };
        out.push(&data[open..open + close + 1]);
        i = open + close + 1;
    }
    out
}

fn blob_of(block: &[u8]) -> Option<Vec<u8>> {
    let at = block.windows(8).position(|w| w == b"\"*coll0\"")?;
    let (blob, _) = coll_blob_at(block, at);
    if blob.is_empty() {
        None
    } else {
        Some(blob)
    }
}
