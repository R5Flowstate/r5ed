//! The `sprp` block: the model name table plus the 64-byte static prop records.

use crate::{Error, Result};

pub const MODEL_NAME_SIZE: usize = 128;
pub const STATIC_PROP_SIZE: usize = 64;
pub const COUNTS_SIZE: usize = 12;
pub const TAIL_COUNT_SIZE: usize = 4;
/// One group record behind the trailing count.
pub const GROUP_SIZE: usize = 64;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct StaticProp {
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    pub scale: f32,
    /// Index into [`StaticProps::models`].
    pub model_index: u16,
    pub unknown_0: u8,
    pub unknown_1: u8,
    pub unknown_2: u16,
    pub unknown_3: u16,
    pub unknown_4: f32,
    pub unknown_5: u32,
    pub unknown_6: u32,
    pub unknown_7: u32,
    /// 0xFFFFFFFF is opaque white.
    pub diffuse_modulation: u32,
    /// Zero means the prop takes no wind.
    pub wind_data: u32,
    pub setdress_level: u8,
    pub unknown_8: [u8; 3],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StaticProps {
    /// Fixed-width, NUL-padded asset paths such as `mdl/levels/terrain/x.rmdl`.
    pub models: Vec<String>,
    /// The block's own three counts: prop count first, then two more the
    /// loaders keep alongside it.
    pub counts: [u32; 3],
    pub props: Vec<StaticProp>,
    /// Trailing optional-section count; zero in every shipped map.
    pub tail_count: u32,
    /// The group records the trailing count introduces, one per count, kept as
    /// bytes because nothing in this crate reads their fields.
    pub groups: Vec<u8>,
}

impl StaticProps {
    pub fn prop_count(&self) -> usize {
        self.counts[0] as usize
    }

    /// Model path of a prop, if its index names one.
    pub fn model_of(&self, prop: usize) -> Option<&str> {
        let p = self.props.get(prop)?;
        self.models.get(p.model_index as usize).map(|s| s.as_str())
    }
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

fn pad_name(name: &str) -> Result<[u8; MODEL_NAME_SIZE]> {
    let bytes = name.as_bytes();
    if bytes.len() >= MODEL_NAME_SIZE {
        return Err(Error::Invalid {
            id: 0x23,
            msg: format!("static prop model path is {} bytes, the field holds {}", bytes.len(), MODEL_NAME_SIZE - 1),
        });
    }
    let mut out = [0u8; MODEL_NAME_SIZE];
    out[..bytes.len()].copy_from_slice(bytes);
    Ok(out)
}

pub fn parse(data: &[u8]) -> Result<StaticProps> {
    if data.len() < 4 {
        return Err(Error::Truncated { what: "sprp model count", need: 4, have: data.len() });
    }
    let n = i32::from_le_bytes(data[0..4].try_into().unwrap());
    if n < 0 {
        return Err(Error::Invalid { id: 0x23, msg: format!("negative sprp model count {n}") });
    }
    let n = n as usize;
    let table_end = 4 + n * MODEL_NAME_SIZE;
    if table_end + COUNTS_SIZE > data.len() {
        return Err(Error::Truncated { what: "sprp model name table", need: table_end, have: data.len() });
    }
    let mut models = Vec::with_capacity(n);
    for i in 0..n {
        let at = 4 + i * MODEL_NAME_SIZE;
        let raw = &data[at..at + MODEL_NAME_SIZE];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(MODEL_NAME_SIZE);
        models.push(latin1(&raw[..end]));
    }
    let counts_at = table_end;
    let counts = [
        u32::from_le_bytes(data[counts_at..counts_at + 4].try_into().unwrap()),
        u32::from_le_bytes(data[counts_at + 4..counts_at + 8].try_into().unwrap()),
        u32::from_le_bytes(data[counts_at + 8..counts_at + 12].try_into().unwrap()),
    ];
    let props_at = counts_at + COUNTS_SIZE;
    let props_bytes = (counts[0] as usize).checked_mul(STATIC_PROP_SIZE).ok_or(Error::Invalid {
        id: 0x23,
        msg: "sprp prop count overflows".to_string(),
    })?;
    let props_end = props_at.checked_add(props_bytes).ok_or(Error::Invalid {
        id: 0x23,
        msg: "sprp prop block overflows".to_string(),
    })?;
    if props_end + TAIL_COUNT_SIZE > data.len() {
        return Err(Error::Truncated { what: "sprp props", need: props_end + TAIL_COUNT_SIZE, have: data.len() });
    }
    let props = data[props_at..props_end]
        .chunks_exact(STATIC_PROP_SIZE)
        .map(bytemuck::pod_read_unaligned::<StaticProp>)
        .collect();
    let tail_count = u32::from_le_bytes(data[props_end..props_end + 4].try_into().unwrap());
    let groups = data[props_end + TAIL_COUNT_SIZE..].to_vec();
    if groups.len() != tail_count as usize * GROUP_SIZE {
        return Err(Error::Invalid {
            id: 0x23,
            msg: format!(
                "the sprp trailing count is {tail_count} but {} bytes of groups follow",
                groups.len()
            ),
        });
    }
    Ok(StaticProps { models, counts, props, tail_count, groups })
}

impl StaticProps {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            4 + self.models.len() * MODEL_NAME_SIZE + COUNTS_SIZE + self.props.len() * STATIC_PROP_SIZE + TAIL_COUNT_SIZE,
        );
        out.extend_from_slice(&(self.models.len() as i32).to_le_bytes());
        for m in &self.models {
            out.extend_from_slice(&pad_name(m).unwrap_or([0u8; MODEL_NAME_SIZE]));
        }
        for c in self.counts {
            out.extend_from_slice(&c.to_le_bytes());
        }
        for p in &self.props {
            out.extend_from_slice(bytemuck::bytes_of(p));
        }
        out.extend_from_slice(&self.tail_count.to_le_bytes());
        out.extend_from_slice(&self.groups);
        out
    }
}

/// `unknown_1` flag: the prop is above water. The client's lit opaque pass
/// only draws props carrying it; every shipped prop has it set.
pub const FLAG_ABOVE_WATER: u8 = 0x4;

/// The 64-byte record at every version; exposed so a writer can build one.
pub fn default_prop() -> StaticProp {
    StaticProp { diffuse_modulation: 0xFFFF_FFFF, unknown_1: FLAG_ABOVE_WATER, ..Default::default() }
}

/// The record size the engine reads, as a compile-time check on the layout.
const _: () = assert!(std::mem::size_of::<StaticProp>() == STATIC_PROP_SIZE);

