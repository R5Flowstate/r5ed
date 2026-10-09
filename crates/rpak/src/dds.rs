//! DDS header reader.
//!
//! The host serves texture bytes straight through as `image/vnd-ms.dds`, so
//! only the header is interpreted: enough to report the format, the array
//! size and the mip count, and to know that the payload is BC-compressed
//! data that must not be re-encoded.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const MAGIC: &[u8; 4] = b"DDS ";
/// `"DDS "` plus the 124-byte DDS_HEADER.
const HEADER_END: usize = 4 + 124;
/// The optional 20-byte DDS_HEADER_DXT10 extension.
const DXT10_END: usize = HEADER_END + 20;

const PF_FOURCC: u32 = 0x4;

/// What the payload of a DDS file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DdsFormat {
    /// A BCn block-compressed DXGI format, passed through byte for byte.
    BlockCompressed(u32),
    /// An uncompressed DXGI format.
    Uncompressed(u32),
    /// A legacy DXT1..DXT5 / ATI1 / ATI2 fourcc with no DX10 header.
    LegacyFourcc([u8; 4]),
}

impl DdsFormat {
    /// Texels a BCn block covers; `None` for an uncompressed format.
    pub fn block_extent(self) -> Option<(u32, u32)> {
        match self {
            Self::BlockCompressed(70..=102) | Self::LegacyFourcc(_) => Some((4, 4)),
            _ => None,
        }
    }

    pub fn is_block_compressed(self) -> bool {
        !matches!(self, Self::Uncompressed(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DdsInfo {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub mip_count: u32,
    pub array_size: u32,
    pub format: DdsFormat,
    /// Byte offset of the first payload byte, i.e. the header size to skip.
    pub data_offset: u32,
}

fn u32_at(b: &[u8], o: usize, what: &'static str) -> Result<u32> {
    if b.len() < o + 4 {
        return Err(Error::Truncated { what, need: o + 4, have: b.len() });
    }
    Ok(u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]))
}

/// Reads the header of a DDS file. The payload is never touched.
pub fn parse_dds_header(b: &[u8]) -> Result<DdsInfo> {
    if b.len() < 4 {
        return Err(Error::Truncated { what: "dds magic", need: 4, have: b.len() });
    }
    if &b[0..4] != MAGIC {
        return Err(Error::Malformed { what: "dds", msg: "missing DDS magic".into() });
    }
    if b.len() < HEADER_END {
        return Err(Error::Truncated { what: "dds header", need: HEADER_END, have: b.len() });
    }
    let header_size = u32_at(b, 4, "dds header")?;
    if header_size != 124 {
        return Err(Error::Malformed { what: "dds", msg: format!("header size {header_size} is not 124") });
    }
    let height = u32_at(b, 12, "dds height")?;
    let width = u32_at(b, 16, "dds width")?;
    let depth = u32_at(b, 24, "dds depth")?.max(1);
    let mip_count = u32_at(b, 28, "dds mips")?.max(1);
    if width == 0 || height == 0 {
        return Err(Error::Malformed { what: "dds", msg: "zero extent".into() });
    }
    if mip_count > 32 {
        return Err(Error::Malformed { what: "dds", msg: format!("mip count {mip_count} exceeds 32") });
    }
    let pf_flags = u32_at(b, 80, "dds pixel format")?;
    let fourcc = [b[84], b[85], b[86], b[87]];
    if &fourcc == b"DX10" {
        if b.len() < DXT10_END {
            return Err(Error::Truncated { what: "dds dx10 header", need: DXT10_END, have: b.len() });
        }
        let dxgi = u32_at(b, 128, "dds dx10 format")?;
        let array_size = u32_at(b, 140, "dds dx10 array")?.max(1);
        let format = if (70..=102).contains(&dxgi) {
            DdsFormat::BlockCompressed(dxgi)
        } else {
            DdsFormat::Uncompressed(dxgi)
        };
        return Ok(DdsInfo { width, height, depth, mip_count, array_size, format, data_offset: DXT10_END as u32 });
    }
    if pf_flags & PF_FOURCC != 0 {
        if fourcc == [0, 0, 0, 0] {
            return Err(Error::Malformed { what: "dds", msg: "FOURCC set but empty".into() });
        }
        return Ok(DdsInfo {
            width,
            height,
            depth,
            mip_count,
            array_size: 1,
            format: DdsFormat::LegacyFourcc(fourcc),
            data_offset: HEADER_END as u32,
        });
    }
    let bit_count = u32_at(b, 88, "dds bit count")?;
    let dxgi = match bit_count {
        32 => 28,
        16 => 49,
        8 => 61,
        _ => {
            return Err(Error::Malformed {
                what: "dds",
                msg: format!("uncompressed bit_count {bit_count} is not a supported width"),
            })
        }
    };
    Ok(DdsInfo {
        width,
        height,
        depth,

        mip_count,
        array_size: 1,
        format: DdsFormat::Uncompressed(dxgi),
        data_offset: HEADER_END as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn data(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join(name)
    }

    #[test]
    fn reads_a_dx10_bc7_header() {
        let b = std::fs::read(data("bc7_mip.dds")).unwrap();
        let i = parse_dds_header(&b).unwrap();
        assert_eq!((i.width, i.height), (1024, 1024));
        assert_eq!(i.mip_count, 11);
        assert_eq!(i.array_size, 1);
        assert_eq!(i.format, DdsFormat::BlockCompressed(98));
        assert!(i.format.is_block_compressed());
        assert_eq!(i.format.block_extent(), Some((4, 4)));
        assert_eq!(i.data_offset, 148);
        assert_eq!(i.depth, 1, "the linear size before the depth field is not a depth");
    }

    #[test]
    fn reads_a_dx10_array_and_its_mip_count() {
        let b = std::fs::read(data("bc1_array.dds")).unwrap();
        let i = parse_dds_header(&b).unwrap();
        assert_eq!(i.format, DdsFormat::BlockCompressed(71));
        assert_eq!(i.array_size, 4);
        assert_eq!(i.mip_count, 9);
        assert_eq!(i.data_offset, 148);
    }

    #[test]
    fn reads_a_legacy_fourcc_header() {
        let b = std::fs::read(data("bc3_legacy.dds")).unwrap();
        let i = parse_dds_header(&b).unwrap();
        assert_eq!(i.format, DdsFormat::LegacyFourcc(*b"DXT5"));
        assert!(i.format.is_block_compressed());
        assert_eq!(i.data_offset, 128);
        assert_eq!(i.array_size, 1);
    }

    #[test]
    fn reads_an_uncompressed_header_by_bit_count() {
        let b = std::fs::read(data("rgba8.dds")).unwrap();
        let i = parse_dds_header(&b).unwrap();
        assert_eq!(i.format, DdsFormat::Uncompressed(28));
        assert!(!i.format.is_block_compressed());
        assert_eq!(i.format.block_extent(), None);
    }

    #[test]
    fn rejects_hostile_input() {
        assert!(matches!(parse_dds_header(&[]), Err(Error::Truncated { .. })));
        assert!(matches!(parse_dds_header(b"NOT"), Err(Error::Truncated { .. })));
        let mut b = std::fs::read(data("bc7_mip.dds")).unwrap();
        b[1] = b'X';
        assert!(matches!(parse_dds_header(&b), Err(Error::Malformed { what: "dds", .. })));
        let b = std::fs::read(data("bc7_mip.dds")).unwrap();
        assert!(matches!(parse_dds_header(&b[..100]), Err(Error::Truncated { what: "dds header", .. })));
        assert!(matches!(parse_dds_header(&b[..140]), Err(Error::Truncated { what: "dds dx10 header", .. })));
        let mut b = b.clone();
        b[4] = 123;
        assert!(matches!(parse_dds_header(&b), Err(Error::Malformed { .. })));
        let mut b = b.clone();
        b[16] = 0;
        b[17] = 0;
        assert!(matches!(parse_dds_header(&b), Err(Error::Malformed { .. })));
        let mut b = b;
        b[28] = 99;
        assert!(matches!(parse_dds_header(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn a_zero_mip_count_reads_as_one() {
        let mut b = std::fs::read(data("bc7_mip.dds")).unwrap();
        b[28..32].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse_dds_header(&b).unwrap().mip_count, 1);
    }
}
