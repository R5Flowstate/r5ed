//! The model reader, the glb converter and the thumbnail raster, against the
//! synthetic fixture in `tests/data`.

use std::path::PathBuf;

use rpak::glb::{model_to_glb, read_glb};
use rpak::model_info::ModelInfo;
use rpak::rmdl::{parse_model, studiohdr_to_vertex_space, unpack_normal32, unpack_position64};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join(name)
}

fn fixture() -> (Vec<u8>, Vec<u8>) {
    (
        std::fs::read(data("tri.rmdl")).unwrap(),
        std::fs::read(data("tri.vg")).unwrap(),
    )
}

fn find_bin(glb: &[u8]) -> &[u8] {
    let jlen = u32::from_le_bytes([glb[12], glb[13], glb[14], glb[15]]) as usize;
    let bo = (20 + jlen).div_ceil(4) * 4;
    &glb[bo + 8..]
}

#[test]
fn reads_the_synthetic_model() {
    let (rmdl, vg) = fixture();
    let m = parse_model(&rmdl, &vg).unwrap();
    assert_eq!(m.name, "r5ed/synthetic/tri");
    assert_eq!(m.version, 17);
    assert_eq!(m.bone_count, 1);
    assert_eq!(m.contents, 1);
    assert_eq!(m.textures, vec![0x984D_F667_4916_9863]);
    assert_eq!(m.meshes.len(), 1);
    let mesh = &m.meshes[0];
    assert_eq!(mesh.positions.len(), 4);
    assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3]);
    assert_eq!(mesh.material, 0);
}

#[test]
fn rsx_model_to_glb() {
    let (rmdl, vg) = fixture();
    let model = parse_model(&rmdl, &vg).unwrap();
    let glb = model_to_glb(&model).unwrap();
    assert_eq!(&glb[0..4], b"glTF");
    assert_eq!(u32::from_le_bytes([glb[4], glb[5], glb[6], glb[7]]), 2);
    let total = u32::from_le_bytes([glb[8], glb[9], glb[10], glb[11]]) as usize;
    assert_eq!(total, glb.len(), "the declared length must match the file");

    let summary = read_glb(&glb).unwrap();
    let j = &summary.json;
    assert_eq!(j["asset"]["version"], "2.0");
    assert_eq!(j["meshes"].as_array().unwrap().len(), 1);
    let prim = &j["meshes"][0]["primitives"][0];
    assert_eq!(prim["mode"], 4, "TRIANGLES");
    let acc = j["accessors"].as_array().unwrap();
    assert_eq!(acc.len(), 5, "position, normal, uv, colour, indices");
    assert_eq!(acc[0]["type"], "VEC3");
    assert_eq!(acc[0]["count"], 4);
    assert_eq!(acc[1]["type"], "VEC3");
    assert_eq!(acc[2]["type"], "VEC2");
    assert_eq!(acc[3]["type"], "VEC4");
    assert_eq!(acc[4]["type"], "SCALAR");
    assert_eq!(acc[4]["count"], 6);
    assert_eq!(j["materials"].as_array().unwrap().len(), 1);
    assert_eq!(j["materials"][0]["extras"]["textureGuid"], "0x984DF66749169863");
    assert_eq!(summary.bin_len % 4, 0);
    assert!(summary.bin_len >= 4 * 36 + 12);
}

#[test]
fn the_glb_carries_the_decoded_positions() {
    let (rmdl, vg) = fixture();
    let model = parse_model(&rmdl, &vg).unwrap();
    let glb = model_to_glb(&model).unwrap();
    let s = read_glb(&glb).unwrap();
    let off = s.json["bufferViews"][0]["byteOffset"].as_u64().unwrap() as usize;
    let bin = find_bin(&glb);
    let mut got = [0f32; 3];
    for (k, g) in got.iter_mut().enumerate() {
        let o = off + k * 4;
        *g = f32::from_le_bytes([bin[o], bin[o + 1], bin[o + 2], bin[o + 3]]);
    }
    // The fixture's first vertex is the origin in stored space, which the
    // static-prop flip-flop leaves at the origin.
    assert!(got[0].abs() < 0.01, "x {}", got[0]);
    assert!(got[1].abs() < 0.01, "y {}", got[1]);
    assert!(got[2].abs() < 0.01, "z {}", got[2]);
}

#[test]
fn the_unpackers_match_their_bit_layouts() {
    // The bias makes the zero code map to -1024 on x and y and -2048 on z.
    assert_eq!(unpack_position64(0), [-1024.0, -1024.0, -2048.0]);
    let max = 0x1F_FFFFu64 | (0x1F_FFFFu64 << 21) | (0x3F_FFFFu64 << 42);
    let hi = unpack_position64(max);
    assert!((hi[0] - 1024.0).abs() < 0.01, "{hi:?}");
    assert!((hi[1] - 1024.0).abs() < 0.01, "{hi:?}");
    assert!((hi[2] - 2048.0).abs() < 0.01, "{hi:?}");
    // Every decoded normal is unit length, and a Z-dominant one lands on z.
    let n = unpack_normal32((2 << 29) | (0x100 << 19) | (0x100 << 10)).unwrap();
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    assert!((l - 1.0).abs() < 1e-5, "{n:?}");
    assert!((n[2] - 1.0).abs() < 1e-5, "{n:?}");
    let flipped = unpack_normal32((1 << 28) | (2 << 29) | (0x100 << 19) | (0x100 << 10)).unwrap();
    assert!((flipped[2] + 1.0).abs() < 1e-5, "{flipped:?}");
    assert!(unpack_normal32(3 << 29).is_none());
    // Header vectors rotate into vertex space, never the other way round.
    assert_eq!(studiohdr_to_vertex_space([1.0, 2.0, 3.0]), [2.0, 3.0, 1.0]);
    assert_eq!(rpak::rmdl::vertex_to_studiohdr_space(studiohdr_to_vertex_space([1.0, 2.0, 3.0])), [1.0, 2.0, 3.0]);
}

#[test]
fn model_info_derives_from_a_parsed_model() {
    let (rmdl, vg) = fixture();
    let m = parse_model(&rmdl, &vg).unwrap();
    let info = ModelInfo::from_rmdl(&m, "mdl/props/tri.rmdl", 0xABCD, vec!["common.rpak".into()]);
    assert_eq!(info.path, "mdl/props/tri.rmdl");
    assert_eq!(info.guid, 0xABCD);
    assert!(info.has_collision);
    assert_eq!(info.resident_paks, vec!["common.rpak".to_string()]);
    assert_eq!(info.size(), [1.0, 1.0, 0.0]);
    assert_eq!(info.center(), [0.5, 0.5, 0.0]);
    assert!(info.contains([0.5, 0.5, 0.0]));
    assert!(!info.contains([2.0, 0.0, 0.0]));
}

#[test]
fn hostile_model_bytes_produce_errors_not_panics() {
    let (rmdl, vg) = fixture();
    assert!(parse_model(&rmdl[..100], &vg).is_err());
    assert!(parse_model(&[], &vg).is_err());
    assert!(parse_model(&rmdl, &vg[..40]).is_err());
    // A group table offset that lands past the header.
    let mut bad = rmdl.clone();
    bad[180] = 0xAB;
    bad[181] = 0xCD;
    assert!(parse_model(&bad, &vg).is_err());
    // A vertex stride that overruns the file.
    let mut bad_vg = vg.clone();
    bad_vg[16 + 12] = 0xFF;
    bad_vg[16 + 13] = 0x00;
    assert!(parse_model(&rmdl, &bad_vg).is_err());
    // An index that points past the vertex array.
    let mut bad_vg = vg.clone();
    bad_vg[64] = 0xFF;
    bad_vg[65] = 0x7F;
    assert!(parse_model(&rmdl, &bad_vg).is_err());
    // An out-of-range group count.
    let mut bad_rmdl = rmdl.clone();
    bad_rmdl[182..184].copy_from_slice(&9999u16.to_le_bytes());
    assert!(parse_model(&bad_rmdl, &vg).is_err());
    // Random bit flips anywhere must never panic.
    let mut seed = 0x1234_5678u32;
    for _ in 0..400 {
        let mut b = rmdl.clone();
        let mut g = vg.clone();
        for _ in 0..8 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if seed % 2 == 0 && !b.is_empty() {
                let i = (seed >> 8) as usize % b.len();
                b[i] ^= (seed >> 16) as u8;
            } else if !g.is_empty() {
                let i = (seed >> 8) as usize % g.len();
                g[i] ^= (seed >> 16) as u8;
            }
        }
        let _ = parse_model(&b, &g);
    }
}

#[test]
fn a_model_with_no_geometry_reports_a_clean_error() {
    let (rmdl, vg) = fixture();
    let model = parse_model(&rmdl, &vg).unwrap();
    let mut empty = model.clone();
    empty.meshes.clear();
    assert!(model_to_glb(&empty).is_err());
}

#[test]
fn the_thumbnail_raster_covers_the_model() {
    let (rmdl, vg) = fixture();
    let model = parse_model(&rmdl, &vg).unwrap();
    let img = rpak::thumbnail::render_thumbnail(&model).unwrap();
    assert_eq!(img.width, rpak::thumbnail::THUMB_SIZE);
    assert_eq!(img.height, rpak::thumbnail::THUMB_SIZE);
    assert_eq!(img.pixels.len(), img.width * img.height * 4);
    let lit = img.pixels.chunks_exact(4).filter(|p| p[3] == 255).count();
    assert!(lit > 0, "the raster must draw something");
    assert!(lit < img.width * img.height, "and not fill the frame");
}
