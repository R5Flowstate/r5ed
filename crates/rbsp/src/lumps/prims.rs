//! The float triple shared by every position/normal vertex lump.

use bytemuck::{Pod, Zeroable};

use crate::lumps::records::fixed_codec;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Vec3f {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3f {
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
}

fixed_codec!(parse, write, Vec3f, 0x03);
