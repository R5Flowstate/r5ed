//! Heightmap import and export.
//!
//! Three source encodings are accepted: 8- or 16-bit greyscale PNG, raw
//! little-endian `R16`, and raw little-endian `R32`. A [`HeightmapSource`]
//! carries the samples inline so a recipe stays pure data with no file handles,
//! which is what lets the browser and the CLI produce the same terrain.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::heightfield::Heightfield;

/// Ceiling on samples accepted from a decoded image, so a hostile PNG cannot
/// ask for an allocation the machine cannot back.
pub const MAX_SAMPLES: u64 = 1 << 26;

/// Which raw encoding a byte slice is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RawFormat {
    /// Little-endian unsigned 16-bit samples, the usual game heightmap layout.
    #[default]
    R16,
    /// Little-endian 32-bit floats.
    R32,
}

/// The PNG signature, so a byte slice can be classified without decoding it.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// A heightmap's samples plus the transform that turns them into a field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeightmapSource {
    pub width: u32,
    pub height: u32,
    /// World units between samples.
    pub cell_size: f64,
    /// Raw little-endian `R16` samples, row-major, `width * height` of them.
    pub samples: Vec<u16>,
    /// Height in inches that a sample of `u16::MAX` represents.
    pub z_scale: f64,
    /// Height in inches that a sample of zero represents.
    pub z_bias: f64,
    /// Seed recorded for provenance; it does not affect the samples.
    pub seed: u64,
}

impl Default for HeightmapSource {
    fn default() -> Self {
        Self {
            width: 2,
            height: 2,
            cell_size: 8.0,
            samples: vec![0; 4],
            z_scale: 4096.0,
            z_bias: 0.0,
            seed: 0,
        }
    }
}

impl HeightmapSource {
    /// Build from a heightfield by quantising into `bits`-wide samples.
    pub fn from_heightfield(hf: &Heightfield, z_scale: f64, z_bias: f64, bits: u8) -> Result<Self> {
        if bits != 16 {
            return Err(Error::UnsupportedHeightmap("only 16-bit heightmap sources are supported"));
        }
        let n = hf.data.len();
        let mut samples = Vec::with_capacity(n);
        let span = if z_scale.is_finite() && z_scale > 0.0 { z_scale } else { 4096.0 };
        for v in &hf.data {
            if !v.is_finite() {
                return Err(Error::NonFinite(0));
            }
            let t = ((*v as f64 - z_bias) / span).clamp(0.0, 1.0);
            samples.push((t * 65535.0).round() as u16);
        }
        Ok(Self {
            width: hf.width,
            height: hf.height,
            cell_size: hf.cell_size as f64,
            samples,
            z_scale: span,
            z_bias,
            seed: 0,
        })
    }

    /// Height in inches represented by the lowest and highest sample.
    pub fn min_height(&self) -> f32 {
        self.z_bias as f32
    }

    pub fn max_height(&self) -> f32 {
        (self.z_bias + self.z_scale) as f32
    }

    /// Expand the samples into a heightfield at the requested cell size.
    pub fn to_heightfield(&self, cell_size: f32) -> Result<Heightfield> {
        let want = self.width as usize * self.height as usize;
        if self.width == 0 || self.height == 0 {
            return Err(Error::ZeroSize);
        }
        if self.samples.len() != want {
            return Err(Error::BadByteLengthU8(want, self.width, self.height, self.samples.len()));
        }
        if !cell_size.is_finite() || cell_size <= 0.0 {
            return Err(Error::InvalidCellSize(cell_size));
        }
        let span = if self.z_scale.is_finite() && self.z_scale > 0.0 { self.z_scale } else { 4096.0 };
        let bias = if self.z_bias.is_finite() { self.z_bias } else { 0.0 };
        let scale = span / 65535.0;
        let data = self.samples.iter().map(|s| (bias + *s as f64 * scale) as f32).collect();
        Heightfield::new(self.width, self.height, cell_size, data)
    }
}

/// What a byte slice turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded {
    /// A greyscale image, one sample per pixel.
    Grey { width: u32, height: u32, samples: Vec<u16> },
    /// Raw samples already at the right bit depth.
    Raw { width: u32, height: u32, samples: Vec<u16> },
}

impl Decoded {
    pub fn width(&self) -> u32 {
        match self {
            Decoded::Grey { width, .. } | Decoded::Raw { width, .. } => *width,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Decoded::Grey { height, .. } | Decoded::Raw { height, .. } => *height,
        }
    }

    /// Read sample `(x, y)`, or zero outside the image.
    pub fn sample(&self, x: u32, y: u32) -> u16 {
        if x >= self.width() || y >= self.height() {
            return 0;
        }
        let i = y as usize * self.width() as usize + x as usize;
        match self {
            Decoded::Grey { samples, .. } | Decoded::Raw { samples, .. } => samples[i],
        }
    }
}

fn check_image_size(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(Error::ZeroSize);
    }
    if width as u64 * height as u64 > MAX_SAMPLES {
        return Err(Error::HeightmapTooLarge(width, height));
    }
    Ok(())
}

/// True when the slice starts with the PNG signature.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[..8] == PNG_MAGIC
}

/// Decode a greyscale PNG. 8-bit images are widened to 16 bits by scaling, so
/// both bit depths land on the same 16-bit sample grid.
pub fn decode_png(bytes: &[u8]) -> Result<Decoded> {
    if !is_png(bytes) {
        return Err(Error::UnsupportedHeightmap("not a PNG"));
    }
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|_| Error::UnsupportedHeightmap("malformed PNG"))?;
    let (w, h) = (img.width(), img.height());
    check_image_size(w, h)?;
    let samples = match &img {
        image::DynamicImage::ImageLuma16(b) => b.as_raw().clone(),
        other => {
            // Everything else is reduced to luminance and widened onto the
            // 16-bit grid, so an 8-bit or hand-painted RGB heightmap imports.
            let luma = other.to_luma8();
            luma.as_raw().iter().map(|v| (*v as u16) * 257).collect::<Vec<u16>>()
        }
    };
    Ok(Decoded::Grey { width: w, height: h, samples })
}

/// Parse raw little-endian `R16` or `R32` samples.
pub fn decode_raw(bytes: &[u8], width: u32, height: u32, format: RawFormat) -> Result<Decoded> {
    check_image_size(width, height)?;
    let want = width as usize * height as usize;
    let stride = match format {
        RawFormat::R16 => 2usize,
        RawFormat::R32 => 4usize,
    };
    if bytes.len() != want * stride {
        return Err(Error::BadByteLengthU8(want * stride, width, height, bytes.len()));
    }
    let samples = match format {
        RawFormat::R16 => bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect(),
        RawFormat::R32 => {
            let mut out = Vec::with_capacity(want);
            for c in bytes.chunks_exact(4) {
                let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                if !v.is_finite() {
                    return Err(Error::BadHeightmapBytes(bytes.len()));
                }
                let t = crate::math::saturate(v as f64) * 65535.0;
                out.push(t.round() as u16);
            }
            out
        }
    };
    Ok(Decoded::Raw { width, height, samples })
}

/// Decode either a PNG or a raw buffer, choosing by the PNG signature.
pub fn decode_auto(bytes: &[u8], width: u32, height: u32, format: RawFormat) -> Result<Decoded> {
    if is_png(bytes) {
        decode_png(bytes)
    } else {
        decode_raw(bytes, width, height, format)
    }
}

/// Build a heightfield from decoded samples.
pub fn to_heightfield(decoded: &Decoded, cell_size: f32, z_scale: f64, z_bias: f64) -> Result<Heightfield> {
    let w = decoded.width();
    let h = decoded.height();
    check_image_size(w, h)?;
    if !cell_size.is_finite() || cell_size <= 0.0 {
        return Err(Error::InvalidCellSize(cell_size));
    }
    let span = if z_scale.is_finite() && z_scale > 0.0 { z_scale } else { 4096.0 };
    let bias = if z_bias.is_finite() { z_bias } else { 0.0 };
    let scale = span / 65535.0;
    let n = w as usize * h as usize;
    let mut data = Vec::with_capacity(n);
    for y in 0..h {
        for x in 0..w {
            data.push((bias + decoded.sample(x, y) as f64 * scale) as f32);
        }
    }
    Heightfield::new(w, h, cell_size, data)
}

/// Load a heightmap from bytes, decoding PNG or raw automatically.
pub fn load(
    bytes: &[u8],
    width: u32,
    height: u32,
    format: RawFormat,
    cell_size: f32,
    z_scale: f64,
    z_bias: f64,
) -> Result<Heightfield> {
    let decoded = decode_auto(bytes, width, height, format)?;
    to_heightfield(&decoded, cell_size, z_scale, z_bias)
}

/// Encode a heightfield as a 16-bit greyscale PNG, quantising into `z_scale`.
pub fn encode_png16(hf: &Heightfield, z_bias: f64, z_scale: f64) -> Result<Vec<u8>> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    let span = if z_scale.is_finite() && z_scale > 0.0 { z_scale } else { 4096.0 };
    let bias = if z_bias.is_finite() { z_bias } else { 0.0 };
    let mut buf = image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::new(hf.width, hf.height);
    for y in 0..hf.height {
        for x in 0..hf.width {
            let v = hf.get(x, y)?;
            if !v.is_finite() {
                return Err(Error::NonFinite(hf.index(x, y)));
            }
            let t = ((v as f64 - bias) / span).clamp(0.0, 1.0);
            buf.put_pixel(x, y, image::Luma([(t * 65535.0).round() as u16]));
        }
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageLuma16(buf)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|_| Error::UnsupportedHeightmap("PNG encoding failed"))?;
    Ok(out)
}

/// Encode a heightfield as raw little-endian `R16` samples.
pub fn encode_raw16(hf: &Heightfield) -> Result<Vec<u8>> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    let mut out = Vec::with_capacity(hf.data.len() * 2);
    for v in &hf.data {
        if !v.is_finite() {
            return Err(Error::NonFinite(0));
        }
        let q = (crate::math::saturate(*v as f64 / 4096.0) * 65535.0).round() as u16;
        out.extend_from_slice(&q.to_le_bytes());
    }
    Ok(out)
}

/// Encode a heightfield as raw little-endian `R32` samples normalised to 0..1.
pub fn encode_raw32(hf: &Heightfield) -> Result<Vec<u8>> {
    if hf.data.is_empty() {
        return Err(Error::ZeroSize);
    }
    let mut out = Vec::with_capacity(hf.data.len() * 4);
    for v in &hf.data {
        if !v.is_finite() {
            return Err(Error::NonFinite(0));
        }
        out.extend_from_slice(&(crate::math::saturate(*v as f64 / 4096.0) as f32).to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png16_round_trip_is_exact() {
        let hf = Heightfield::from_fn(9, 7, 4.0, |x, y| {
            // Land on the 16-bit grid so the round trip is lossless.
            (x as f32 * 4096.0 + y as f32 * 137.0) / 65535.0
        })
        .unwrap();
        let png = encode_png16(&hf, 0.0, 4096.0).unwrap();
        assert!(is_png(&png));
        let back = load(&png, 0, 0, RawFormat::R16, 4.0, 4096.0, 0.0).unwrap();
        assert_eq!((back.width, back.height), (9, 7));
        for y in 0..7u32 {
            for x in 0..9u32 {
                let a = hf.get(x, y).unwrap();
                let b = back.get(x, y).unwrap();
                assert!((a - b).abs() <= 4096.0 / 65535.0 + 1e-4, "at {x},{y}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn png8_widens_to_the_same_grid() {
        let mut png8 = Vec::new();
        let img = image::DynamicImage::ImageLuma8(image::ImageBuffer::from_fn(4, 3, |x, y| {
            image::Luma([(x * 60 + y * 7) as u8])
        }));
        img.write_to(&mut std::io::Cursor::new(&mut png8), image::ImageFormat::Png).unwrap();
        let decoded = decode_png(&png8).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (4, 3));
        // The 8-bit values are widened by 257, so 0 stays 0 and 255 reaches
        // the full 16-bit range.
        assert_eq!(decoded.sample(0, 0), 0);
        assert_eq!(decoded.sample(0, 2), 14u16 * 257);
        let hf = to_heightfield(&decoded, 1.0, 4096.0, 0.0).unwrap();
        assert!(hf.is_finite());
        assert!(hf.max() <= 4096.0 + 1e-3);
    }

    #[test]
    fn rgb_png_reduces_to_luminance() {
        let mut png = Vec::new();
        let img = image::DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(3, 2, |x, y| {
            image::Rgb([(x * 80) as u8, (y * 100) as u8, 40])
        }));
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let decoded = decode_png(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (3, 2));
        assert!(decoded.sample(2, 1) > 0);
    }

    #[test]
    fn raw_r16_and_r32_round_trip() {
        let samples: Vec<u16> = (0..16u32).map(|i| (i * 4096) as u16).collect();
        let src = HeightmapSource {
            width: 4,
            height: 4,
            cell_size: 2.0,
            samples,
            z_scale: 1024.0,
            z_bias: 0.0,
            seed: 0,
        };
        let hf = src.to_heightfield(2.0).unwrap();
        let bytes = encode_raw16(&hf).unwrap();
        assert_eq!(bytes.len(), 16 * 2);
        let back = load(&bytes, 4, 4, RawFormat::R16, 2.0, 4096.0, 0.0).unwrap();
        assert_eq!(back.data.len(), 16);
        let f32_bytes = encode_raw32(&hf).unwrap();
        assert_eq!(f32_bytes.len(), 16 * 4);
        let back32 = load(&f32_bytes, 4, 4, RawFormat::R32, 2.0, 4096.0, 0.0).unwrap();
        assert!(back32.is_finite());
    }

    #[test]
    fn source_validates_shape_and_cell_size() {
        let bad = HeightmapSource { width: 4, height: 4, samples: vec![0; 5], ..Default::default() };
        assert!(matches!(bad.to_heightfield(1.0), Err(Error::BadByteLengthU8(..))));
        let zero = HeightmapSource { width: 0, height: 4, samples: vec![], ..Default::default() };
        assert!(matches!(zero.to_heightfield(1.0), Err(Error::ZeroSize)));
        let ok = HeightmapSource::default();
        assert!(ok.to_heightfield(0.0).is_err());
        assert!(ok.to_heightfield(f32::NAN).is_err());
        assert!(ok.to_heightfield(1.0).is_ok());
    }

    #[test]
    fn source_clamps_out_of_range_scales() {
        let full = HeightmapSource {
            width: 2,
            height: 2,
            samples: vec![0, 65535, 65535, 65535],
            ..Default::default()
        };
        let hf = full.to_heightfield(1.0).unwrap();
        assert_eq!(hf.min(), 0.0);
        assert_eq!(hf.max(), 4096.0);
        // A scale or bias that is not finite falls back to the documented
        // default rather than producing garbage.
        let bad = HeightmapSource {
            z_scale: f64::NAN,
            z_bias: f64::NEG_INFINITY,
            ..full.clone()
        };
        let hb = bad.to_heightfield(1.0).unwrap();
        assert!(hb.is_finite());
        assert_eq!(hb.min(), 0.0);
        assert_eq!(hb.max(), 4096.0);
    }

    #[test]
    fn decode_rejects_truncated_and_oversized_input() {
        assert!(matches!(decode_png(b"not a png at all"), Err(Error::UnsupportedHeightmap(_))));
        assert!(matches!(decode_raw(&[0, 1, 2], 4, 4, RawFormat::R16), Err(Error::BadByteLengthU8(..))));
        assert!(matches!(decode_raw(&[0; 8], 0, 0, RawFormat::R16), Err(Error::ZeroSize)));
        // A NaN in an R32 buffer is refused rather than imported as a height.
        let mut bad = Vec::new();
        bad.extend_from_slice(&f32::NAN.to_le_bytes());
        bad.extend_from_slice(&0.0f32.to_le_bytes());
        assert!(matches!(decode_raw(&bad, 1, 2, RawFormat::R32), Err(Error::BadHeightmapBytes(_))));
    }

    #[test]
    fn oversize_images_are_refused() {
        assert!(matches!(decode_raw(&[], 1 << 20, 1 << 20, RawFormat::R16), Err(Error::HeightmapTooLarge(..))));
    }

    #[test]
    fn from_heightfield_rejects_bad_input() {
        let mut hf = Heightfield::filled(4, 4, 1.0, 10.0).unwrap();
        assert!(matches!(HeightmapSource::from_heightfield(&hf, 100.0, 0.0, 8), Err(Error::UnsupportedHeightmap(_))));
        let src = HeightmapSource::from_heightfield(&hf, 100.0, 0.0, 16).unwrap();
        assert_eq!(src.samples.len(), 16);
        assert!(src.samples.iter().all(|s| *s > 0));
        hf.data[0] = f32::NAN;
        assert!(HeightmapSource::from_heightfield(&hf, 100.0, 0.0, 16).is_err());
    }

    #[test]
    fn encoders_reject_empty_and_non_finite() {
        let empty = Heightfield { width: 2, height: 2, cell_size: 1.0, data: Vec::new() };
        assert!(matches!(encode_png16(&empty, 0.0, 1.0), Err(Error::ZeroSize)));
        assert!(matches!(encode_raw16(&empty), Err(Error::ZeroSize)));
        assert!(matches!(encode_raw32(&empty), Err(Error::ZeroSize)));
        let mut hf = Heightfield::filled(3, 3, 1.0, 0.0).unwrap();
        hf.data[4] = f32::INFINITY;
        assert!(encode_png16(&hf, 0.0, 1.0).is_err());
        assert!(encode_raw16(&hf).is_err());
        assert!(encode_raw32(&hf).is_err());
    }

    #[test]
    fn decoded_reads_outside_the_image_as_zero() {
        let d = Decoded::Raw { width: 2, height: 2, samples: vec![1, 2, 3, 4] };
        assert_eq!(d.sample(0, 0), 1);
        assert_eq!(d.sample(1, 1), 4);
        assert_eq!(d.sample(5, 5), 0);
    }

    /// A 16-bit greyscale PNG is the interchange format a heightmap author hands
    /// over, so the round trip has to keep every one of the 65536 levels rather
    /// than collapsing them into eight bits.
    #[test]
    fn heightmap_import_png16() {
        // One sample per level, walked up and back down so the encoder and the
        // decoder both see the whole range.
        let levels: Vec<u16> = (0..=u16::MAX as u32).map(|v| v as u16).collect();
        let w = 256u32;
        let h = (levels.len() as u32).div_ceil(w);
        let mut samples = levels.clone();
        samples.resize(w as usize * h as usize, 0);
        let mut buf = image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::new(w, h);
        for y in 0..h {
            for x in 0..w {
                buf.put_pixel(x, y, image::Luma([samples[(y * w + x) as usize]]));
            }
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma16(buf)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(is_png(&png));
        assert_eq!(&png[..8], &PNG_MAGIC);

        let decoded = decode_png(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (w, h));
        // Every level survives the file, so the encoding is 16-bit end to end.
        for y in 0..h {
            for x in 0..w {
                assert_eq!(
                    decoded.sample(x, y),
                    samples[(y * w + x) as usize],
                    "level {x},{y} did not survive"
                );
            }
        }
        let back = decoded_to_field(&decoded, 8.0, 4096.0, 0.0);
        assert!(back.is_finite());
        assert!((back.min() - 0.0).abs() < 1e-3);
        assert!((back.max() - 4096.0).abs() < 1e-2);
        // The height step of one level is the span over 65535, so neighbouring
        // levels differ by that much and no more.
        let step = 4096.0 / 65535.0;
        for y in 0..h {
            for x in 1..w {
                let a = back.get(x - 1, y).unwrap();
                let b = back.get(x, y).unwrap();
                assert!((b - a - step).abs() < 1e-3, "level step lost at {x},{y}: {a} -> {b}");
            }
        }
        // A generator reading the same samples produces the same field, so a
        // recipe can carry a heightmap without touching the filesystem.
        let src = HeightmapSource {
            width: w,
            height: h,
            cell_size: 8.0,
            samples,
            z_scale: 4096.0,
            z_bias: 0.0,
            seed: 0,
        };
        assert_eq!(src.to_heightfield(8.0).unwrap(), back);

        // An eight-bit image still imports, widened onto the same grid.
        let mut png8 = Vec::new();
        let small = image::DynamicImage::ImageLuma8(image::ImageBuffer::from_fn(4, 4, |x, y| {
            image::Luma([(x * 60 + y * 5) as u8])
        }));
        small.write_to(&mut std::io::Cursor::new(&mut png8), image::ImageFormat::Png).unwrap();
        let eight = decode_png(&png8).unwrap();
        assert_eq!((eight.width(), eight.height()), (4, 4));
        assert_eq!(eight.sample(3, 3), 195u16 * 257);
    }

    fn decoded_to_field(d: &Decoded, cell: f32, scale: f64, bias: f64) -> Heightfield {
        to_heightfield(d, cell, scale, bias).unwrap()
    }
}
