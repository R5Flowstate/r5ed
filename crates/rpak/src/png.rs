//! A minimal PNG writer for thumbnails.
//!
//! Thumbnails are 128x128 and generated per request at most once, so the
//! payload is a single zlib stream of stored (uncompressed) deflate blocks:
//! no compressor, no dependency, and a decoder cannot care which of the two it
//! is handed.

use crate::error::{Error, Result};

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const BIT_DEPTH: u8 = 8;
const COLOUR_TYPE: u8 = 6;
/// zlib header for a 32 KiB window, fastest compression level, no preset
/// dictionary, check bits that make the word a multiple of 31.
const ZLIB_HEADER: [u8; 2] = [0x78, 0x01];
const MAX_STORED: usize = 0xFFFF;
const ADLER_MOD: u32 = 65521;

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut n = 0usize;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[n] = c;
        n += 1;
    }
    t
}

fn crc32(table: &[u32; 256], b: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &x in b {
        c = table[((c ^ x as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

fn adler32(b: &[u8]) -> u32 {
    let (mut a, mut s) = (1u32, 0u32);
    for &x in b {
        a = (a + x as u32) % ADLER_MOD;
        s = (s + a) % ADLER_MOD;
    }
    (s << 16) | a
}

/// Wraps `raw` in a zlib stream of stored deflate blocks.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / MAX_STORED * 5 + 16);
    out.extend_from_slice(&ZLIB_HEADER);
    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    }
    let mut at = 0usize;
    while at < raw.len() {
        let n = (raw.len() - at).min(MAX_STORED);
        let last = at + n == raw.len();
        out.push(u8::from(last));
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&(!(n as u16)).to_le_bytes());
        out.extend_from_slice(&raw[at..at + n]);
        at += n;
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let at = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let table = crc_table();
    out.extend_from_slice(&crc32(&table, &out[at..]).to_be_bytes());
}

/// Encodes row-major RGBA8 as an 8-bit truecolour-with-alpha PNG.
pub fn encode_rgba8(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(Error::Malformed { what: "png", msg: "zero extent".into() });
    }
    let want = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::Malformed { what: "png", msg: "extent overflows".into() })?;
    if pixels.len() != want {
        return Err(Error::Malformed { what: "png", msg: format!("{} bytes for {width}x{height} rgba", pixels.len()) });
    }
    let row_bytes = (width as usize).checked_mul(4).ok_or(Error::Malformed {
        what: "png",
        msg: "row length overflows".into(),
    })?;
    // Every scanline is prefixed with its filter type; 0 means "no filter".
    let mut raw = Vec::with_capacity((row_bytes + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(&pixels[y * row_bytes..(y + 1) * row_bytes]);
    }
    let mut out = Vec::with_capacity(raw.len() + 128);
    out.extend_from_slice(&SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(BIT_DEPTH);
    ihdr.push(COLOUR_TYPE);
    ihdr.extend_from_slice(&[0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(b: &[u8]) -> Vec<[u8; 4]> {
        let mut out = Vec::new();
        let mut at = 8;
        while at + 8 <= b.len() {
            let n = u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
            let mut k = [0u8; 4];
            k.copy_from_slice(&b[at + 4..at + 8]);
            out.push(k);
            at += 8 + n + 4;
        }
        out
    }

    fn idat(b: &[u8]) -> Vec<u8> {
        let mut at = 8;
        let mut out = Vec::new();
        while at + 8 <= b.len() {
            let n = u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
            if &b[at + 4..at + 8] == b"IDAT" {
                out.extend_from_slice(&b[at + 8..at + 8 + n]);
            }
            at += 8 + n + 4;
        }
        out
    }

    #[test]
    fn a_one_pixel_png_has_the_three_chunks_and_a_decodable_stream() {
        let png = encode_rgba8(&[1, 2, 3, 4], 1, 1).unwrap();
        assert_eq!(&png[0..8], &SIGNATURE);
        assert_eq!(chunks(&png), vec![*b"IHDR", *b"IDAT", *b"IEND"]);
        let z = idat(&png);
        assert_eq!(&z[..2], &ZLIB_HEADER);
        // stored block: header byte, LEN, NLEN, filter byte, the pixel
        assert_eq!(z[2], 1, "final block flag");
        assert_eq!(u16::from_le_bytes([z[3], z[4]]), 5);
        assert_eq!(u16::from_le_bytes([z[5], z[6]]), !5u16);
        assert_eq!(z[7], 0, "filter type none");
        assert_eq!(&z[8..12], &[1, 2, 3, 4]);
        assert_eq!(u32::from_be_bytes([z[z.len() - 4], z[z.len() - 3], z[z.len() - 2], z[z.len() - 1]]), adler32(&[0, 1, 2, 3, 4]));
    }

    #[test]
    fn every_crc_covers_its_chunk_type_and_body() {
        let png = encode_rgba8(&[9; 16], 2, 2).unwrap();
        let table = crc_table();
        let mut at = 8;
        while at + 8 <= png.len() {
            let n = u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]) as usize;
            let want = u32::from_be_bytes([
                png[at + 8 + n],
                png[at + 9 + n],
                png[at + 10 + n],
                png[at + 11 + n],
            ]);
            assert_eq!(crc32(&table, &png[at + 4..at + 8 + n]), want);
            at += 8 + n + 4;
        }
    }

    #[test]
    fn a_payload_larger_than_one_stored_block_splits() {
        // Three scanlines of 40000 bytes each is past the 65535 byte block cap.
        let w = 10_000u32;
        let px = vec![7u8; w as usize * 4 * 3];
        let png = encode_rgba8(&px, w, 3).unwrap();
        let z = idat(&png);
        let blocks = z.windows(5).filter(|w| w[0] == 0 || w[0] == 1).count();
        assert!(blocks >= 2, "expected more than one stored block, got {blocks}");
        assert_eq!(chunks(&png), vec![*b"IHDR", *b"IDAT", *b"IEND"]);
    }

    #[test]
    fn rejects_a_pixel_count_that_does_not_match_the_extent() {
        assert!(matches!(encode_rgba8(&[0; 3], 1, 1), Err(Error::Malformed { .. })));
        assert!(matches!(encode_rgba8(&[0; 4], 0, 1), Err(Error::Malformed { .. })));
        assert!(matches!(encode_rgba8(&[0; 4], 1, 0), Err(Error::Malformed { .. })));
        assert!(matches!(encode_rgba8(&[0; 4], u32::MAX, u32::MAX), Err(Error::Malformed { .. })));
    }
}
