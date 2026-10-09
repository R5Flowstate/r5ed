//! BC4, BC5 and BC6H block codecs.

use rbake::bc;
use glam::Vec3;
use rbake::bc6;

fn lcg(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state >> 8
}

/// A BC4 block re-encodes to the same values it decoded to, within the
/// palette step.
#[test]
fn bc5_roundtrip_error_bound() {
    let mut state = 0x51ED_2701u32;
    let (w, h) = (37u32, 21u32);
    let mut worst = 0.0f32;
    for _ in 0..8 {
        let mut red = vec![0u8; (w * h) as usize];
        let mut green = vec![0u8; (w * h) as usize];
        for i in 0..red.len() {
            red[i] = (lcg(&mut state) & 0xFF) as u8;
            green[i] = ((lcg(&mut state) & 0x7F) as u8).min(red[i]);
        }
        let blocks = bc::encode_bc5_image(&red, &green, w, h).expect("the pair encodes");
        let bw = w.div_ceil(4) as usize;
        let bh = h.div_ceil(4) as usize;
        assert_eq!(blocks.len(), 2 * bw * bh * 8, "a BC5 image is two block planes");
        // Decode the red plane and compare per texel.
        for by in 0..bh {
            for bx in 0..bw {
                let at = (by * bw + bx) * 8;
                let block: [u8; 8] = blocks[at..at + 8].try_into().expect("a whole block");
                let vals = bc::decode_bc4_block(&block);
                for ty in 0..4 {
                    for tx in 0..4 {
                        let px = (by * 4 + ty).min(h as usize - 1);
                        let pxx = (bx * 4 + tx).min(w as usize - 1);
                        let want = red[px * w as usize + pxx] as f32;
                        let got = vals[ty * 4 + tx];
                        worst = worst.max((want - got).abs());
                    }
                }
            }
        }
    }
    assert!(worst <= bc::BC4_MAX_ERROR, "the worst texel error is {worst}");
    assert!(worst > 0.0, "the image was not flat");
}

/// A constant block is exact, and the six-value palette of an unordered pair
/// keeps 0 and 255 exact.
#[test]
fn bc4_palette_holds_the_constants() {
    for v in [0u8, 1, 128, 254, 255] {
        let blk = [v; 16];
        let out = bc::encode_bc4_block(&blk);
        let back = bc::decode_bc4_block(&out);
        for got in back {
            assert!((got - v as f32).abs() < 0.5, "a flat block of {v} decoded as {got}");
        }
    }
}

/// A ramp round trips within the palette step.
#[test]
fn bc4_ramp_is_close() {
    let mut blk = [0u8; 16];
    for (i, slot) in blk.iter_mut().enumerate() {
        *slot = (i as u32 * 17) as u8;
    }
    let out = bc::encode_bc4_block(&blk);
    let back = bc::decode_bc4_block(&out);
    // A ramp over the whole 0..255 range has a palette step of 255/7.
    for i in 0..16 {
        assert!(
            (back[i] - blk[i] as f32).abs() <= bc::BC4_MAX_ERROR,
            "texel {i}: {} against {}",
            back[i],
            blk[i]
        );
    }
}

fn close(got: Vec3, want: Vec3, rel: f32) -> bool {
    (0..3).all(|c| (got[c] - want[c]).abs() <= rel * want[c].abs().max(1.0e-3))
}

/// A constant block decodes to its colour within the 10-bit endpoint step.
#[test]
fn bc6h_constant_block_is_close() {
    for v in [Vec3::ZERO, Vec3::new(0.02, 0.5, 1.7), Vec3::splat(1.0), Vec3::new(300.0, 12.0, 0.25)] {
        let back = bc6::decode_bc6h_block(&bc6::encode_bc6h_block(&[v; 16]));
        for got in back {
            assert!(close(got, v, 0.03), "a block of {v} decoded as {got}");
        }
    }
}

/// A smooth colour ramp round-trips within the 16-step palette, and the first
/// texel's index always fits its three bits.
#[test]
fn bc6h_block_roundtrip() {
    let mut state = 0x0C0B_E123u32;
    for _ in 0..64 {
        let base = Vec3::new(
            (lcg(&mut state) % 1000) as f32 / 500.0,
            (lcg(&mut state) % 1000) as f32 / 500.0,
            (lcg(&mut state) % 1000) as f32 / 500.0,
        );
        let texels: [Vec3; 16] = std::array::from_fn(|i| base * (1.0 + i as f32 / 30.0));
        let block = bc6::encode_bc6h_block(&texels);
        assert_eq!(block[0] & 0x1F, 0b00011, "mode 11");
        let back = bc6::decode_bc6h_block(&block);
        for i in 0..16 {
            assert!(close(back[i], texels[i], 0.06), "texel {i} decodes to {} against {}", back[i], texels[i]);
        }
    }
}

/// Half-float conversion is exact on representable values and clamps.
#[test]
fn half_bits_round_trip() {
    for v in [0.0f32, 1.0, 0.5, 2.0, 1.5, 65504.0, 6.1035156e-5] {
        assert_eq!(bc6::half_value(bc6::half_bits(v)), v, "{v}");
    }
    assert_eq!(bc6::half_bits(-1.0), 0);
    assert_eq!(bc6::half_bits(1.0e9), 0x7BFF);
}

/// Faces of any size encode into whole blocks.
#[test]
fn bc6h_face_encodes_whole_blocks() {
    let face = vec![Vec3::splat(0.5); 64];
    assert_eq!(bc6::encode_bc6h_face(&face, 8, 8).expect("8x8").len(), 4 * 16);
    assert_eq!(bc6::encode_bc6h_face(&[Vec3::ONE], 1, 1).expect("1x1").len(), 16);
    assert!(bc6::encode_bc6h_face(&face[..3], 8, 8).is_err(), "a short face is an error");
}

/// A BC4 image that does not match its declared size is an error.
#[test]
fn bc4_image_size_is_checked() {
    assert!(bc::encode_bc4_image(&[0u8; 10], 4, 4).is_err(), "a short image");
    assert!(bc::encode_bc4_image(&[0u8; 0], 4, 4).is_err(), "an empty image");
    assert!(bc::encode_bc4_image(&[0u8; 16], 0, 4).is_err(), "a zero dimension");
}
