//! NUL-separated string blobs (lump 0x0F and the v37 name blob).

use crate::lumps::records::{parse_fixed, write_fixed};
use crate::{Error, Result, Version};

/// A lump that is exactly one run of NUL-terminated strings. The byte offsets
/// other lumps hold into it always land on a string start; the run has no gaps
/// and no trailing bytes in every shipped map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StringTable {
    pub strings: Vec<String>,
    pub tail: Vec<u8>,
}

impl StringTable {
    pub fn len(&self) -> usize {
        self.strings.iter().map(|s| s.len() + 1).sum::<usize>() + self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Byte offset of the `index`th string, or the lump length when out of range.
    pub fn offset_of(&self, index: usize) -> usize {
        let mut at = 0usize;
        for (i, s) in self.strings.iter().enumerate() {
            if i == index {
                return at;
            }
            at += s.len() + 1;
        }
        at.min(self.len())
    }

    /// The string starting at `offset`, if one does.
    pub fn at_offset(&self, offset: usize) -> Option<&str> {
        if offset >= self.len() {
            return None;
        }
        let mut at = 0usize;
        for s in &self.strings {
            if at == offset {
                return Some(s);
            }
            at += s.len() + 1;
        }
        None
    }

    pub fn get(&self, index: usize) -> Option<&str> {
        self.strings.get(index).map(|s| s.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.strings.iter().map(|s| s.as_str())
    }
}

/// The blobs are latin-1 on disk: a byte above 0x7f is a valid character there
/// and has to survive a round trip, so every byte maps to one char.
fn to_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

fn latin1_bytes(s: &str) -> Vec<u8> {
    s.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect()
}

pub fn parse(data: &[u8], _version: Version) -> Result<StringTable> {
    let mut table = StringTable::default();
    let mut start = 0usize;
    for i in 0..data.len() {
        if data[i] == 0 {
            table.strings.push(to_latin1(&data[start..i]));
            start = i + 1;
        }
    }
    if start < data.len() {
        table.tail = data[start..].to_vec();
    }
    Ok(table)
}

pub fn write(table: &StringTable, _version: Version) -> Vec<u8> {
    let mut out = Vec::with_capacity(table.len());
    for s in &table.strings {
        out.extend_from_slice(&latin1_bytes(s));
        out.push(0);
    }
    out.extend_from_slice(&table.tail);
    out
}

/// The `u32` offset form of a name blob: one offset per texdata record.
pub fn parse_offsets(id: u16, data: &[u8], _version: Version) -> Result<Vec<u32>> {
    parse_fixed(id, data)
}

pub fn write_offsets(items: &[u32], _version: Version) -> Vec<u8> {
    write_fixed(items)
}

/// LUMP_TEXTURE_DATA_STRING_TABLE: one offset per texdata record.
pub fn parse_table(data: &[u8], version: Version) -> Result<Vec<u32>> {
    parse_offsets(0x2C, data, version)
}

/// Resolves an offset against a blob, reporting the failure rather than panicking.
pub fn resolve<'a>(table: &'a StringTable, offset: u32, what: &str) -> Result<&'a str> {
    let at = offset as usize;
    if at >= table.len() {
        return Err(Error::Invalid {
            id: 0x0F,
            msg: format!("{what} offset {offset} is past the {} byte name blob", table.len()),
        });
    }
    table
        .at_offset(at)
        .ok_or_else(|| Error::Invalid { id: 0x0F, msg: format!("{what} offset {offset} is not a string start") })
}
