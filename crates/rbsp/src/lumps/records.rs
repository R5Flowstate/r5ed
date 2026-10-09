//! Fixed-stride record helpers and the codec macro used by every lump module.

use bytemuck::Pod;

use crate::{Error, Result};

/// Splits `data` into `T` records, rejecting a size that is not a whole number
/// of records. Reads are unaligned so a lump can be sliced straight out of a
/// mmap'd file.
pub fn parse_fixed<T: Pod>(id: u16, data: &[u8]) -> Result<Vec<T>> {
    let stride = std::mem::size_of::<T>();
    if stride == 0 {
        return Err(Error::Invalid { id, msg: "zero-sized record".to_string() });
    }
    if !data.len().is_multiple_of(stride) {
        return Err(Error::Stride { id, size: data.len(), stride });
    }
    let mut out = Vec::with_capacity(data.len() / stride);
    for chunk in data.chunks_exact(stride) {
        out.push(bytemuck::pod_read_unaligned::<T>(chunk));
    }
    Ok(out)
}

pub fn write_fixed<T: Pod>(items: &[T]) -> Vec<u8> {
    if items.is_empty() {
        return Vec::new();
    }
    bytemuck::cast_slice(items).to_vec()
}

/// Emits the `parse(&[u8], Version) -> Result<Vec<T>>` / `write(&[T], Version) -> Vec<u8>`
/// pair for a lump whose record layout does not change between versions.
macro_rules! fixed_codec {
    ($parse:ident, $write:ident, $ty:ty, $id:expr) => {
        pub fn $parse(data: &[u8], _version: $crate::Version) -> $crate::Result<Vec<$ty>> {
            $crate::lumps::records::parse_fixed::<$ty>($id, data)
        }

        pub fn $write(items: &[$ty], _version: $crate::Version) -> Vec<u8> {
            $crate::lumps::records::write_fixed(items)
        }
    };
}

pub(crate) use fixed_codec;
