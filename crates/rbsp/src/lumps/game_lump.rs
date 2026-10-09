//! LUMP_GAME_LUMP (0x23): a directory of blocks, of which `sprp` (the static
//! prop list) is the one both engines read.

use crate::lumps::game_lump::sprp::StaticProps;
use crate::{Error, Result, Version};

pub mod sprp;

/// Directory header: `i32 count` then `count` fixed 16-byte entries.
pub const DIRECTORY_ENTRY_SIZE: usize = 16;
pub const HEADER_SIZE: usize = 4;
pub const SPRP_ID: [u8; 4] = *b"sprp";
/// The same block under the swapped spelling, which is what every shipped map
/// uses.
pub const PRPS_ID: [u8; 4] = *b"prps";

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GameLumpEntry {
    pub id: [u8; 4],
    pub flags: u16,
    /// Mirrors the BSP version: 0x33 for v51, 0x34 for v52.
    pub version: u16,
    /// Absolute file offset of the block. In a split map it is the lump size of
    /// the directory instead, so it is never recomputed here.
    pub file_offset: i32,
    pub length: i32,
}

impl GameLumpEntry {
    pub fn is_sprp(&self) -> bool {
        self.id == SPRP_ID || self.id == PRPS_ID
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    StaticProps(StaticProps),
    /// A block id no shipped map carries; kept verbatim.
    Other(Vec<u8>),
}

impl Block {
    pub fn as_static_props(&self) -> Option<&StaticProps> {
        match self {
            Block::StaticProps(s) => Some(s),
            Block::Other(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GameLump {
    pub entries: Vec<GameLumpEntry>,
    pub blocks: Vec<Block>,
    /// Bytes after the last block; zero in every shipped map.
    pub tail: Vec<u8>,
}

impl GameLump {
    pub fn static_props(&self) -> Option<&StaticProps> {
        self.entries.iter().position(|e| e.is_sprp()).and_then(|i| self.blocks.get(i)?.as_static_props())
    }

    pub fn version_of_sprp(&self) -> Option<u16> {
        self.entries.iter().find(|e| e.is_sprp()).map(|e| e.version)
    }
}

/// Block pointers come in two forms: absolute against the packed file, or
/// relative to the lump. `base` is the lump's own recorded offset, so the
/// block is looked for at `file_offset - base` first and at `file_offset`
/// itself second. The value read is kept verbatim, so a round trip reproduces
/// whichever form the map used.
pub fn parse_at(data: &[u8], _version: Version, base: u32) -> Result<GameLump> {
    let entries = directory(data)?;
    let dir_end = HEADER_SIZE + entries.len() * DIRECTORY_ENTRY_SIZE;

    let mut blocks = Vec::with_capacity(entries.len());
    let mut end = dir_end;
    let mut sequential = dir_end;
    for e in &entries {
        let (start, stop) = block_range(data, e, base, &mut sequential)?;
        let raw = &data[start..stop];
        blocks.push(if e.is_sprp() { Block::StaticProps(sprp::parse(raw)?) } else { Block::Other(raw.to_vec()) });
        end = end.max(stop);
    }
    Ok(GameLump { entries, blocks, tail: data[end.min(data.len())..].to_vec() })
}

pub fn parse(data: &[u8], version: Version) -> Result<GameLump> {
    parse_at(data, version, 0)
}

/// Where one directory entry's block sits inside the lump. `sequential` is the
/// offset the block would occupy if the directory were ignored, which is where a
/// map whose pointers came from a different packing still resolves.
fn block_range(data: &[u8], e: &GameLumpEntry, base: u32, sequential: &mut usize) -> Result<(usize, usize)> {
    let name = String::from_utf8_lossy(&e.id);
    let len = e.length.max(0) as usize;
    let mut candidates = [e.file_offset as i64 - base as i64, e.file_offset as i64, *sequential as i64];
    candidates[2] = candidates[2].min(data.len() as i64);
    for start in candidates {
        *sequential = (*sequential + len).min(data.len() + 1);
        if start < 0 {
            continue;
        }
        let start = start as usize;
        if start > data.len() || start.saturating_add(len) > data.len() {
            continue;
        }
        return Ok((start, start + len));
    }
    Err(Error::Invalid {
        id: 0x23,
        msg: format!("game lump block {name:?} at {} (length {len}) lies outside a {}-byte lump", e.file_offset, data.len()),
    })
}

/// Reads just the directory, which is all a pointer shift needs.
pub fn directory(data: &[u8]) -> Result<Vec<GameLumpEntry>> {
    if data.len() < HEADER_SIZE {
        return Err(Error::Truncated { what: "game lump header", need: HEADER_SIZE, have: data.len() });
    }
    let count = i32::from_le_bytes(data[0..4].try_into().unwrap());
    if count < 0 {
        return Err(Error::Invalid { id: 0x23, msg: format!("negative game lump count {count}") });
    }
    let dir_end = HEADER_SIZE + (count as usize).checked_mul(DIRECTORY_ENTRY_SIZE).ok_or(Error::Invalid {
        id: 0x23,
        msg: "game lump directory size overflows".to_string(),
    })?;
    if dir_end > data.len() {
        return Err(Error::Truncated { what: "game lump directory", need: dir_end, have: data.len() });
    }
    Ok(data[HEADER_SIZE..dir_end]
        .chunks_exact(DIRECTORY_ENTRY_SIZE)
        .map(bytemuck::pod_read_unaligned::<GameLumpEntry>)
        .collect())
}

/// Shifts every block pointer by the distance the lump itself moved, for a map
/// that has just been packed somewhere other than where it was read from. Only
/// the directory's `file_offset` fields change; the blocks and any padding are
/// untouched. Returns `None` when the lump did not move.
pub fn rebase(data: &[u8], old_base: u32, new_base: u32) -> Result<Option<Vec<u8>>> {
    if old_base == new_base {
        return Ok(None);
    }
    let count = directory(data)?.len();
    let delta = new_base as i64 - old_base as i64;
    let mut out = data.to_vec();
    for i in 0..count {
        let at = HEADER_SIZE + i * DIRECTORY_ENTRY_SIZE + 8;
        let moved = i32::from_le_bytes(out[at..at + 4].try_into().unwrap()) as i64 + delta;
        if moved < 0 || moved > u32::MAX as i64 {
            return Err(Error::Invalid {
                id: 0x23,
                msg: format!("game lump block {i} would move to {moved}, outside the file"),
            });
        }
        out[at..at + 4].copy_from_slice(&(moved as i32).to_le_bytes());
    }
    Ok(Some(out))
}

pub fn write(lump: &GameLump, _version: Version) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_SIZE + lump.entries.len() * DIRECTORY_ENTRY_SIZE);
    out.extend_from_slice(&(lump.entries.len() as i32).to_le_bytes());
    for e in &lump.entries {
        out.extend_from_slice(&e.id);
        out.extend_from_slice(&e.flags.to_le_bytes());
        out.extend_from_slice(&e.version.to_le_bytes());
        out.extend_from_slice(&e.file_offset.to_le_bytes());
        out.extend_from_slice(&e.length.to_le_bytes());
    }
    for b in &lump.blocks {
        out.extend_from_slice(&encode(b));
    }
    out.extend_from_slice(&lump.tail);
    out
}

fn encode(block: &Block) -> Vec<u8> {
    match block {
        Block::StaticProps(s) => s.encode(),
        Block::Other(raw) => raw.clone(),
    }
}

/// The bytes a block occupies, which is what its directory length must be.
pub fn block_len(block: &Block) -> usize {
    encode(block).len()
}
