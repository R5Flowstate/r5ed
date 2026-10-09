//! The `uiia` asset: a UI image, version 2.
//!
//! The container is a self-relocating lump: a magic, the relocation count, the
//! byte offset of every relocated field, a 64-byte subheader, and the raw data.
//! The data itself starts with a 64-byte record (the display extents, a
//! self-pointer to the pixel data and an optional embedded name) followed by the
//! pixel data as whole 32x32 BC7 tiles.
//!
//! A tile grid the engine lays out itself is described by `imgFlags` 0x00C2: the
//! BC1 low table is absent, the BC7 one is present, and with only one of the two
//! present the engine synthesizes the tile table instead of reading one, so
//! nothing but back-to-back tiles has to be written.

use crate::bc7;

/// `'UIIA'` little-endian.
pub const MAGIC: u32 = 0x4149_4955;
/// Bytes in the subheader that follows the relocation table.
pub const SUBHEADER_BYTES: usize = 64;
/// Bytes in the record that opens the raw data.
pub const DATA_BYTES: usize = 64;
/// The value both streaming fields carry when the asset is fully resident.
pub const RESIDENT: u32 = 0xFFFF_FFFF;

/// The crop and anchor floats a subheader ends with. The engine recomputes what
/// it needs from the extents, and the values a shipped asset carries are
/// resolution specific, so these stay at identity.
const IDENTITY_FLOATS: [f32; 8] = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0];

/// Byte offsets inside the subheader.
mod field {
    pub const STREAMED_OFFSET: usize = 0x08;
    pub const STREAMED_SIZE: usize = 0x0C;
    pub const IMG_FLAGS: usize = 0x12;
    pub const WIDTH: usize = 0x18;
    pub const HEIGHT: usize = 0x1A;
    pub const FLOATS: usize = 0x20;
}

/// Byte offsets inside the data record.
mod data {
    pub const HIGH_WIDTH: usize = 0x20;
    pub const HIGH_HEIGHT: usize = 0x22;
    pub const LOW_WIDTH: usize = 0x24;
    pub const LOW_HEIGHT: usize = 0x26;
    pub const POINTER: usize = 0x28;
    pub const NAME: usize = 0x30;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("a uiia needs at least the 8-byte magic")]
    Truncated,
    #[error("magic {0:08X} is not a uiia")]
    BadMagic(u32),
    #[error("{0} relocations is more than a uiia can address")]
    TooManyRelocations(usize),
    #[error("a uiia declares {0} bytes, which is not a whole number of fields")]
    RaggedHeader(usize),
    #[error("an image of {0}x{1} does not fit the 16-bit extents")]
    TooLarge(u32, u32),
    #[error("the image is not a readable PNG: {0}")]
    Image(String),
    #[error("a uiia carries {0} bytes of pixel data, short of the {1} its extents need")]
    ShortData(usize, usize),
    #[error("the {kind} field holds {value}, which is not a usable offset")]
    BadOffset { kind: &'static str, value: u32 },
}

/// A read image, row major RGBA, eight bits per channel.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Rgba {
    /// Decodes a PNG file. Every other container is refused by name rather than
    /// guessed at, because a misread image would be compressed silently.
    pub fn from_png(bytes: &[u8]) -> Result<Self, Error> {
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .map_err(|e| Error::Image(e.to_string()))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        Ok(Self { width: w, height: h, pixels: rgba.into_raw() })
    }
}

/// The eight trailing header floats for an untrimmed image: crop inset, end
/// anchor, then the half-texel start anchor and scale, whose spans are one
/// pixel short of the size because tiles advance 31 pixels, not 32.
fn header_floats(w: u32, h: u32) -> [f32; 8] {
    if w < 2 || h < 2 {
        return IDENTITY_FLOATS;
    }
    let (sw, sh) = ((w - 1) as f32, (h - 1) as f32);
    [-0.0, -0.0, 1.0, 1.0, -0.5 / sw, -0.5 / sh, w as f32 / sw, h as f32 / sh]
}

/// Encodes an image as a `uiia` container.
pub fn encode(image: &Rgba) -> Result<Vec<u8>, Error> {
    let (w, h) = (image.width, image.height);
    if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
        return Err(Error::TooLarge(w, h));
    }
    let tiles = bc7::encode_tiles(&image.pixels, w, h);
    let mut out = Vec::with_capacity(8 + 4 + SUBHEADER_BYTES + DATA_BYTES + tiles.len());
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    // The one relocated field is the pixel data pointer, at byte 0x28 of the
    // record; its value is the offset the pointer takes once the lump is loaded.
    out.extend_from_slice(&(data::POINTER as u32).to_le_bytes());
    let mut sub = vec![0u8; SUBHEADER_BYTES];
    sub[field::STREAMED_OFFSET..field::STREAMED_OFFSET + 4].copy_from_slice(&RESIDENT.to_le_bytes());
    sub[field::STREAMED_SIZE..field::STREAMED_SIZE + 4].copy_from_slice(&RESIDENT.to_le_bytes());
    sub[field::IMG_FLAGS..field::IMG_FLAGS + 2].copy_from_slice(&bc7::IMG_FLAGS_BC7_TILES.to_le_bytes());
    sub[field::WIDTH..field::WIDTH + 2].copy_from_slice(&(w as u16).to_le_bytes());
    sub[field::HEIGHT..field::HEIGHT + 2].copy_from_slice(&(h as u16).to_le_bytes());
    for (i, f) in header_floats(w, h).iter().enumerate() {
        let at = field::FLOATS + i * 4;
        sub[at..at + 4].copy_from_slice(&f.to_le_bytes());
    }
    out.extend_from_slice(&sub);
    let mut raw = vec![0u8; DATA_BYTES];
    raw[data::HIGH_WIDTH..data::HIGH_WIDTH + 2].copy_from_slice(&(w as u16).to_le_bytes());
    raw[data::HIGH_HEIGHT..data::HIGH_HEIGHT + 2].copy_from_slice(&(h as u16).to_le_bytes());
    raw[data::LOW_WIDTH..data::LOW_WIDTH + 2].copy_from_slice(&(w as u16).to_le_bytes());
    raw[data::LOW_HEIGHT..data::LOW_HEIGHT + 2].copy_from_slice(&(h as u16).to_le_bytes());
    raw[data::POINTER..data::POINTER + 4].copy_from_slice(&(DATA_BYTES as u32).to_le_bytes());
    out.extend_from_slice(&raw);
    out.extend_from_slice(&tiles);
    Ok(out)
}

/// A decoded `uiia`, as far as the container describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub width: u16,
    pub height: u16,
    pub low_width: u16,
    pub low_height: u16,
    pub flags: u16,
    pub streamed_offset: u32,
    pub streamed_size: u32,
    pub floats: [f32; 8],
    pub relocations: Vec<u32>,
    /// Offset the pixel data pointer names, from the start of the lump.
    pub data_offset: u32,
    /// Offset the embedded name names, or zero when the asset carries none.
    pub name_offset: u32,
    /// Bytes of pixel data that follow the record.
    pub tile_bytes: usize,
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, Error> {
    let s = b.get(at..at + 2).ok_or(Error::RaggedHeader(b.len()))?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, Error> {
    let s = b.get(at..at + 4).ok_or(Error::RaggedHeader(b.len()))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Reads a container. Every offset is checked against the bytes that are there,
/// so a truncated or hand-edited asset is an error rather than a panic.
pub fn parse(bytes: &[u8]) -> Result<Parsed, Error> {
    if bytes.len() < 8 {
        return Err(Error::Truncated);
    }
    let magic = u32_at(bytes, 0)?;
    if magic != MAGIC {
        return Err(Error::BadMagic(magic));
    }
    let n = u32_at(bytes, 4)? as usize;
    if n > 16 {
        return Err(Error::TooManyRelocations(n));
    }
    let mut relocations = Vec::with_capacity(n);
    for i in 0..n {
        relocations.push(u32_at(bytes, 8 + i * 4)?);
    }
    let sub_at = 8 + n * 4;
    let sub = bytes.get(sub_at..sub_at + SUBHEADER_BYTES).ok_or(Error::RaggedHeader(bytes.len()))?;
    let raw_at = sub_at + SUBHEADER_BYTES;
    let raw = bytes.get(raw_at..raw_at + DATA_BYTES).ok_or(Error::RaggedHeader(bytes.len()))?;
    let mut floats = [0.0f32; 8];
    for (i, f) in floats.iter_mut().enumerate() {
        let at = field::FLOATS + i * 4;
        let bits = u32::from_le_bytes([sub[at], sub[at + 1], sub[at + 2], sub[at + 3]]);
        *f = f32::from_bits(bits);
    }
    let data_offset = u32_at(raw, data::POINTER)?;
    if data_offset == 0 {
        return Err(Error::BadOffset { kind: "pixel data", value: data_offset });
    }
    let name_offset = u32_at(raw, data::NAME)?;
    let tile_bytes = bytes.len() - raw_at - DATA_BYTES;
    let parsed = Parsed {
        width: u16_at(sub, field::WIDTH)?,
        height: u16_at(sub, field::HEIGHT)?,
        low_width: u16_at(raw, data::LOW_WIDTH)?,
        low_height: u16_at(raw, data::LOW_HEIGHT)?,
        flags: u16_at(sub, field::IMG_FLAGS)?,
        streamed_offset: u32_at(sub, field::STREAMED_OFFSET)?,
        streamed_size: u32_at(sub, field::STREAMED_SIZE)?,
        floats,
        relocations,
        data_offset,
        name_offset,
        tile_bytes,
    };
    let ((bw, bh), _) = bc7::grid(parsed.width as u32, parsed.height as u32);
    let need = bw * bh * bc7::TILE_BYTES;
    if parsed.tile_bytes < need {
        return Err(Error::ShortData(parsed.tile_bytes, need));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: u32, h: u32) -> Rgba {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                pixels[o] = (x * 255 / w.max(1)) as u8;
                pixels[o + 1] = (y * 255 / h.max(1)) as u8;
                pixels[o + 2] = 64;
                pixels[o + 3] = 255;
            }
        }
        Rgba { width: w, height: h, pixels }
    }

    #[test]
    fn a_container_is_the_shape_a_reader_walks() {
        let bytes = encode(&ramp(64, 48)).expect("encodes");
        assert_eq!(u32_at(&bytes, 0).unwrap(), MAGIC);
        assert_eq!(u32_at(&bytes, 4).unwrap(), 1, "one relocated field");
        assert_eq!(u32_at(&bytes, 8).unwrap(), 0x28, "the pixel data pointer");
        assert_eq!(&bytes[12..12 + SUBHEADER_BYTES].len(), &SUBHEADER_BYTES);
        let p = parse(&bytes).expect("parses");
        assert_eq!((p.width, p.height), (64, 48));
        assert_eq!((p.low_width, p.low_height), (64, 48));
        assert_eq!(p.flags, bc7::IMG_FLAGS_BC7_TILES);
        assert_eq!(p.streamed_offset, RESIDENT);
        assert_eq!(p.streamed_size, RESIDENT);
        assert_eq!(p.name_offset, 0, "no embedded name");
        assert_eq!(p.data_offset as usize, DATA_BYTES, "the pointer is relative to the raw data");
        assert_eq!(p.relocations, vec![0x28]);
        assert_eq!(p.floats, header_floats(64, 48));
        let ((bw, bh), _) = bc7::grid(64, 48);
        assert_eq!(p.tile_bytes, bw * bh * bc7::TILE_BYTES);
    }

    /// A shipped 1920x1080 loading screen's subheader, field for field: the
    /// streaming sentinels, the extents, the flags and the float block.
    #[test]
    fn the_subheader_fields_match_a_shipped_asset() {
        let bytes = encode(&ramp(1920, 64)).expect("encodes");
        let sub = &bytes[12..12 + SUBHEADER_BYTES];
        assert_eq!(&sub[0..8], &[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&sub[field::STREAMED_OFFSET..field::STREAMED_OFFSET + 4], &RESIDENT.to_le_bytes());
        assert_eq!(&sub[field::STREAMED_SIZE..field::STREAMED_SIZE + 4], &RESIDENT.to_le_bytes());
        assert_eq!(u16_at(sub, 0x10).unwrap(), 0);
        assert_eq!(u16_at(sub, 0x14).unwrap(), 0);
        assert_eq!(u16_at(sub, 0x16).unwrap(), 0, "no width offset");
        assert_eq!(u16_at(sub, field::IMG_FLAGS).unwrap(), 0x00C2);
        assert_eq!(u16_at(sub, field::WIDTH).unwrap(), 1920);
        assert_eq!(u16_at(sub, field::HEIGHT).unwrap(), 64);
        assert_eq!(u16_at(sub, 0x1C).unwrap(), 0);
        // A shipped 1920x1080 screen carries the half-texel pair: -0.5/1919 and 1920/1919.
        let shipped = header_floats(1920, 1080);
        assert!((shipped[4] + 0.000_260_552_36).abs() < 1e-9 && (shipped[6] - 1.000_521_1).abs() < 1e-6);
        for (i, want) in header_floats(1920, 64).iter().enumerate() {
            let at = field::FLOATS + i * 4;
            assert_eq!(f32::from_le_bytes([sub[at], sub[at + 1], sub[at + 2], sub[at + 3]]), *want);
        }
    }

    #[test]
    fn a_short_or_edited_container_is_an_error_not_a_panic() {
        let good = encode(&ramp(32, 32)).expect("encodes");
        assert!(matches!(parse(&good[..4]), Err(Error::Truncated)));
        assert!(matches!(parse(&[0, 0, 0, 0, 1, 0, 0, 0]), Err(Error::BadMagic(0))));
        let mut bad_magic = good.clone();
        bad_magic[0] = 0x50;
        assert!(matches!(parse(&bad_magic), Err(Error::BadMagic(_))));
        let mut many = good.clone();
        many[4..8].copy_from_slice(&9999u32.to_le_bytes());
        assert!(matches!(parse(&many), Err(Error::TooManyRelocations(9999))));
        for cut in [12usize, 40, 76, 100, good.len() - 1] {
            let _ = parse(&good[..cut]);
        }
        assert!(parse(&good[..good.len() - 8]).is_err(), "a short tile blob is refused");
        let mut no_pointer = good.clone();
        let at = 12 + SUBHEADER_BYTES + data::POINTER;
        no_pointer[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(parse(&no_pointer), Err(Error::BadOffset { .. })));
    }

    #[test]
    fn a_zero_extent_is_refused() {
        assert!(matches!(encode(&Rgba { width: 0, height: 4, pixels: Vec::new() }), Err(Error::TooLarge(0, 4))));
        assert!(matches!(encode(&Rgba { width: 4, height: 0, pixels: Vec::new() }), Err(Error::TooLarge(4, 0))));
        assert!(encode(&Rgba { width: 70000, height: 4, pixels: Vec::new() }).is_err());
    }

    #[test]
    fn only_a_png_is_read_as_an_image() {
        let pixels = (0..(8 * 8 * 4)).map(|i| (i % 251) as u8).collect::<Vec<u8>>();
        let png = rpak::encode_rgba8(&pixels, 8, 8).expect("a png");
        let image = Rgba::from_png(&png).expect("reads back");
        assert_eq!((image.width, image.height), (8, 8));
        assert_eq!(image.pixels, pixels);
        assert!(Rgba::from_png(b"not a png").is_err());
        assert!(Rgba::from_png(&[]).is_err());
    }
}
