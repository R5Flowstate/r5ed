//! BC7 block compression, single subset, mode 6.
//!
//! A block is 16 bytes: a 6-bit mode tag, one shared endpoint precision bit,
//! two RGBA endpoints of 7 bits per channel, one unused bit and sixteen 4-bit
//! indices. With the precision bit clear the endpoints are 7-bit values the
//! decoder expands by bit replication; with it set they are odd 9-bit values,
//! one more bit of precision over the same range. Index 15 is the transparent
//! colour, so a block uses the fifteen intervals its sixteen endpoints form.
//! Both precision bits are tried and the cheaper block wins.
//!
//! An image is compressed as 32x32 tiles of 8x8 blocks, which is the layout the
//! tile format stores, so any tile can be decoded on its own.

/// Bytes one compressed block occupies.
pub const BLOCK_BYTES: usize = 16;
/// Pixels along one edge of a tile.
pub const TILE: usize = 32;
/// Blocks along one edge of a tile.
pub const TILE_BLOCKS: usize = TILE / 4;
/// Bytes one tile occupies.
pub const TILE_BYTES: usize = TILE_BLOCKS * TILE_BLOCKS * BLOCK_BYTES;
/// `imgFlags` for a tile grid the engine builds itself out of BC7 tiles: the
/// BC1 low table is absent and the BC7 one is present, and with only one of the
/// two present the engine synthesizes the table instead of reading one.
pub const IMG_FLAGS_BC7_TILES: u16 = 0x00C2;

const MODE6: u64 = 0b010000;
const INDICES: usize = 15;

/// Bits written least significant first, the order BC7 uses.
struct Bits {
    words: [u64; 2],
    at: usize,
}

impl Bits {
    fn new() -> Self {
        Self { words: [0; 2], at: 0 }
    }

    fn put(&mut self, value: u64, count: usize) {
        debug_assert!(self.at + count <= 128, "a block holds 128 bits");
        for i in 0..count {
            if (value >> i) & 1 == 1 {
                self.words[self.at / 64] |= 1u64 << (self.at % 64);
            }
            self.at += 1;
        }
    }

    fn finish(self) -> [u8; BLOCK_BYTES] {
        let mut out = [0u8; BLOCK_BYTES];
        for (i, b) in out.iter_mut().enumerate() {
            *b = (self.words[i / 8] >> ((i % 8) * 8)) as u8;
        }
        out
    }
}

/// The eight-bit value the decoder reconstructs a seven-bit endpoint as. With
/// the precision bit set the stored value is the top seven bits of an odd
/// nine-bit number, so the low bit comes back as one.
fn expand(e: [u8; 4], p: u32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for c in 0..4 {
        let v = if p == 0 { (e[c] as u16) << 1 | (e[c] as u16) >> 6 } else { (e[c] as u16) << 1 | 1 };
        out[c] = v.min(255) as u8;
    }
    out
}

/// The seven bits an eight-bit channel extreme quantizes to at one precision.
fn quantize_channel(lo: i32, hi: i32, p: u32) -> (u8, u8) {
    if p == 0 {
        (((lo * 127 + 127) / 255).clamp(0, 127) as u8, ((hi * 127 + 127) / 255).clamp(0, 127) as u8)
    } else {
        ((lo / 2).clamp(0, 127) as u8, (hi / 2).clamp(0, 127) as u8)
    }
}

/// The fifteen colours a block's endpoints produce.
fn palette(a: [u8; 4], b: [u8; 4]) -> [[u8; 4]; INDICES] {
    let mut out = [[0u8; 4]; INDICES];
    for (i, slot) in out.iter_mut().enumerate() {
        let w = if i < 9 { i as u32 } else { i as u32 - 8 };
        for c in 0..4 {
            slot[c] = ((a[c] as u32 * (8 - w) + b[c] as u32 * w + 4) >> 3).min(255) as u8;
        }
    }
    out
}

/// Squared error between two colours.
fn cost(a: [u8; 4], b: [u8; 4]) -> u32 {
    let mut sum = 0u32;
    for c in 0..4 {
        let d = a[c] as i32 - b[c] as i32;
        sum += (d * d) as u32;
    }
    sum
}

/// Compresses one 4x4 block of RGBA pixels.
pub fn encode_block(px: &[[u8; 4]; 16]) -> [u8; BLOCK_BYTES] {
    let mut minima = [255u8; 4];
    let mut maxima = [0u8; 4];
    for p in px {
        for c in 0..4 {
            minima[c] = minima[c].min(p[c]);
            maxima[c] = maxima[c].max(p[c]);
        }
    }
    let mut best: Option<(u32, u32, [u8; 4], [u8; 4], [u8; 16])> = None;
    for p in 0..2u32 {
        let mut low = [0u8; 4];
        let mut high = [0u8; 4];
        for c in 0..4 {
            let (a, b) = quantize_channel(minima[c] as i32, maxima[c] as i32, p);
            low[c] = a;
            high[c] = b;
        }
        let lut = palette(expand(low, p), expand(high, p));
        let mut indices = [0u8; 16];
        let mut error = 0u32;
        for (i, pixel) in px.iter().enumerate() {
            let mut best_index = 0u8;
            let mut best_cost = u32::MAX;
            for (k, entry) in lut.iter().enumerate() {
                let c = cost(*entry, *pixel);
                if c < best_cost {
                    best_cost = c;
                    best_index = k as u8;
                }
            }
            indices[i] = best_index;
            error += best_cost;
        }
        let candidate = (error, p, low, high, indices);
        if best.as_ref().is_none_or(|(e, ..)| error < *e) {
            best = Some(candidate);
        }
    }
    let (_, p, low, high, indices) = best.expect("both precision bits are always tried");
    let mut bits = Bits::new();
    bits.put(MODE6, 6);
    bits.put(p as u64, 1);
    for c in 0..4 {
        bits.put(low[c] as u64, 7);
    }
    for c in 0..4 {
        bits.put(high[c] as u64, 7);
    }
    bits.put(0, 1);
    for i in indices {
        bits.put(i as u64, 4);
    }
    bits.finish()
}

/// Blocks along one edge of a `width` x `height` image, and the padded extent
/// they cover. The engine recomputes this count when it loads the asset, so the
/// rule has to agree with it exactly rather than merely cover the image.
pub fn grid(width: u32, height: u32) -> ((usize, usize), (u32, u32)) {
    let blocks = |n: u32| (((n + 29) / 31) as usize).max(1);
    let (bw, bh) = (blocks(width), blocks(height));
    ((bw, bh), (bw as u32 * TILE as u32, bh as u32 * TILE as u32))
}

/// Successive tiles advance 31 pixels, so neighbours share one pixel of edge:
/// n tiles span 31n+1 pixels, the span [`grid`] inverts.
const TILE_STRIDE: usize = 31;

/// The on-disk slot of the block at (`bx`, `by`) inside a tile: the engine
/// reads a tile's 64 blocks in Morton order (x on the even bits, y on the odd).
fn morton_slot(bx: usize, by: usize) -> usize {
    let mut m = 0;
    for b in 0..3 {
        m |= ((bx >> b) & 1) << (2 * b);
        m |= ((by >> b) & 1) << (2 * b + 1);
    }
    m
}

/// Compresses an RGBA image into whole 32x32 tiles, row major, repeating the
/// edge pixel over the padding the block grid adds.
pub fn encode_tiles(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let ((bw, bh), _) = grid(width, height);
    let mut out = vec![0u8; bw * bh * TILE_BYTES];
    for ty in 0..bh {
        for tx in 0..bw {
            let mut pixels = [[0u8; 4]; 16];
            for by in 0..TILE_BLOCKS {
                for bx in 0..TILE_BLOCKS {
                    let (sx, sy) = ((tx * TILE_STRIDE + bx * 4) as u32, (ty * TILE_STRIDE + by * 4) as u32);
                    for row in 0..4 {
                        for col in 0..4 {
                            let at = row * 4 + col;
                            pixels[at] = pixel(rgba, width, height, sx + col as u32, sy + row as u32);
                        }
                    }
                    let off = (ty * bw + tx) * TILE_BYTES + morton_slot(bx, by) * BLOCK_BYTES;
                    out[off..off + BLOCK_BYTES].copy_from_slice(&encode_block(&pixels));
                }
            }
        }
    }
    out
}

/// One pixel, or the nearest inside edge when the coordinate is in the padding.
fn pixel(rgba: &[u8], width: u32, height: u32, x: u32, y: u32) -> [u8; 4] {
    if width == 0 || height == 0 {
        return [0, 0, 0, 255];
    }
    let sx = x.min(width - 1);
    let sy = y.min(height - 1);
    let o = (sy as usize * width as usize + sx as usize) * 4;
    match rgba.get(o..o + 4) {
        Some(s) => [s[0], s[1], s[2], s[3]],
        None => [0, 0, 0, 255],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_stores_its_blocks_in_morton_order() {
        assert_eq!(morton_slot(0, 0), 0);
        assert_eq!(morton_slot(1, 0), 1);
        assert_eq!(morton_slot(0, 1), 2);
        assert_eq!(morton_slot(2, 0), 4);
        assert_eq!(morton_slot(7, 7), 63);
        let mut seen = [false; 64];
        for by in 0..8 {
            for bx in 0..8 {
                seen[morton_slot(bx, by)] = true;
            }
        }
        assert!(seen.iter().all(|s| *s), "every slot is used once");
    }

    #[test]
    fn the_second_tile_starts_31_pixels_in() {
        // A 63 px wide image is two tiles; the second's first block is the pixels at x 31..35.
        let (w, h) = (63u32, 4u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                rgba[o..o + 4].copy_from_slice(&[if x >= 31 { 255 } else { 0 }, 0, 0, 255]);
            }
        }
        let tiles = encode_tiles(&rgba, w, h);
        let red = [[255u8, 0, 0, 255]; 16];
        assert_eq!(&tiles[TILE_BYTES..TILE_BYTES + BLOCK_BYTES], &encode_block(&red), "tile 1 block 0 samples x 31..");
    }

    /// The mode 6 decoder a conformant reader implements, for the round trip.
    fn decode_block(block: &[u8; 16]) -> [[u8; 4]; 16] {
        let word = |at: usize| -> u64 {
            let mut v = 0u64;
            for i in 0..8 {
                v |= (block[at + i] as u64) << (i * 8);
            }
            v
        };
        let (low, high) = (word(0), word(8));
        let take = |at: usize, count: usize| -> u64 {
            let w = if at < 64 { low } else { high };
            (w >> (at % 64)) & ((1u64 << count) - 1)
        };
        assert_eq!(take(0, 6), MODE6, "the block is mode 6");
        let p = take(6, 1);
        let mut a = [0u8; 4];
        let mut b = [0u8; 4];
        for c in 0..4 {
            a[c] = take(7 + c * 7, 7) as u8;
            b[c] = take(35 + c * 7, 7) as u8;
        }
        assert_eq!(take(63, 1), 0, "the interval shift bit is clear");
        let expand = |e: u8| -> u8 {
            let v = if p == 0 { (e as u16) << 1 | (e as u16) >> 6 } else { (e as u16) << 1 | 1 };
            v.min(255) as u8
        };
        let (ea, eb): ([u8; 4], [u8; 4]) = (a.map(expand), b.map(expand));
        let mut out = [[0u8; 4]; 16];
        for (i, slot) in out.iter_mut().enumerate() {
            let idx = take(64 + i * 4, 4) as u8;
            if idx as usize >= INDICES {
                *slot = [0, 0, 0, 0];
                continue;
            }
            let w = if idx < 9 { idx as u32 } else { idx as u32 - 8 };
            for c in 0..4 {
                slot[c] = ((ea[c] as u32 * (8 - w) + eb[c] as u32 * w + 4) >> 3).min(255) as u8;
            }
        }
        out
    }

    /// A block with a ramp in one channel and flat channels elsewhere: the
    /// fifteen intervals of a single endpoint pair cannot do better than one
    /// step of that ramp.
    #[test]
    fn a_gradient_block_stays_inside_the_quantisation_error() {
        let mut px = [[0u8; 4]; 16];
        for (i, p) in px.iter_mut().enumerate() {
            let v = (i as u32 * 17) as u8;
            *p = [v, 100, 200, 255];
        }
        let d = decode_block(&encode_block(&px));
        let mut worst = 0u32;
        for (i, (a, b)) in d.iter().zip(px.iter()).enumerate() {
            for c in 1..3 {
                let e = (a[c] as i32 - b[c] as i32).unsigned_abs();
                assert!(e <= 1, "pixel {i} flat channel {c}: {} vs {}", a[c], b[c]);
            }
            worst = worst.max((a[0] as i32 - b[0] as i32).unsigned_abs());
        }
        assert!(worst <= 16, "error {worst}");
    }

    /// One colour in, the same colour out, up to the endpoints' own precision:
    /// a seven-bit endpoint cannot carry every eight-bit value exactly.
    #[test]
    fn a_block_of_one_colour_decodes_to_that_colour() {
        for v in [0u8, 1, 127, 128, 254, 255] {
            let d = decode_block(&encode_block(&[[v, v, v, 255]; 16]));
            for c in 0..4 {
                let want = [v, v, v, 255][c];
                let e = (d[0][c] as i32 - want as i32).unsigned_abs();
                assert!(e <= 1, "grey {v} channel {c}: {} vs {want}", d[0][c]);
            }
        }
    }
    #[test]
    fn the_grid_matches_the_block_count_the_engine_recomputes() {
        assert_eq!(grid(1920, 1080), ((62, 35), (1984, 1120)));
        assert_eq!(grid(4096, 4096), ((133, 133), (4256, 4256)));
        assert_eq!(grid(32, 32), ((1, 1), (32, 32)));
        assert_eq!(grid(33, 33), ((2, 2), (64, 64)));
        assert_eq!(grid(1, 1), ((1, 1), (32, 32)));
        // The padded extent always covers the image, which is what the padding
        // rule is for.
        for n in [1u32, 31, 32, 33, 63, 2048, 4097] {
            let ((_, _), (pw, ph)) = grid(n, n);
            assert!(pw >= n && ph >= n, "{n} -> {pw}x{ph}");
        }
    }
    #[test]
    fn tiles_are_whole_and_decode_back() {
        let (w, h) = (64u32, 48u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                rgba[o] = (x * 4) as u8;
                rgba[o + 1] = (y * 5) as u8;
                rgba[o + 2] = 128;
                rgba[o + 3] = 255;
            }
        }
        let ((bw, bh), _) = grid(w, h);
        let tiles = encode_tiles(&rgba, w, h);
        assert_eq!(tiles.len(), bw * bh * TILE_BYTES);
        for chunk in tiles.chunks_exact(BLOCK_BYTES) {
            let mut b = [0u8; 16];
            b.copy_from_slice(chunk);
            let d = decode_block(&b);
            assert_eq!(d.len(), 16);
        }
        let mut first = [0u8; 16];
        first.copy_from_slice(&tiles[..BLOCK_BYTES]);
        let d = decode_block(&first);
        assert!(d[0][0] <= 1, "red {}", d[0][0]);
        assert!(d[0][1] <= 1, "green {}", d[0][1]);
        assert_eq!(d[0][3], 255, "alpha survives");
    }
}
