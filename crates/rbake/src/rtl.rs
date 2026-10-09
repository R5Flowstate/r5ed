//! The real-time-light index: which light reaches which lightmap texel, and the
//! group table the shader reads to resolve a texel's group and slot into a
//! world light.
//!
//! A texel indexes its light as `group * 63 + slot`, with 63 the unused value,
//! so 0x7A is a flat table of 63-slot groups of world light indices. The table
//! is filled before shading and the same table the pages were written against is
//! the one emitted, so a slot never resolves to a different light than the one
//! that lit the texel.

use crate::error::{Error, Result};

/// Slots one group holds.
pub const GROUP_SLOTS: usize = 63;
/// The value a texel stores for a slot it does not use.
pub const SLOT_UNUSED: u32 = 63;

/// A light's place in the group table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtlIndex {
    pub group: u8,
    pub slot: u32,
    pub light: u32,
}

/// One group's world light indices, exactly `GROUP_SLOTS` long.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LightGroup {
    pub lights: Vec<u16>,
}

impl LightGroup {
    pub fn new(first: u32) -> Self {
        Self { lights: vec![first as u16; GROUP_SLOTS] }
    }

    /// The world light a slot resolves to.
    pub fn resolve(&self, slot: u32) -> Option<u32> {
        self.lights.get(slot as usize).map(|v| *v as u32)
    }
}

/// Assigns every real-time light a group and slot, in scene order. The table
/// is dense: an unused slot repeats the group's first light, so every entry
/// names a light the map actually has.
pub fn build_groups(lights: &[rir::BakeLight], world_count: usize) -> Result<(Vec<LightGroup>, Vec<RtlIndex>)> {
    let mut groups: Vec<LightGroup> = Vec::new();
    let mut index: Vec<RtlIndex> = vec![RtlIndex::default(); lights.len()];
    let mut placed: u32 = 0;
    for (i, l) in lights.iter().enumerate() {
        if !l.realtime {
            continue;
        }
        if l.kind == rir::LightType::Sky {
            continue;
        }
        if placed as usize >= world_count {
            return Err(Error::lighting(format!(
                "light {i} is real-time but the map has only {world_count} world lights to index it"
            )));
        }
        let g = (placed / GROUP_SLOTS as u32) as usize;
        let s = placed % GROUP_SLOTS as u32;
        while groups.len() <= g {
            let first = placed.min(world_count.saturating_sub(1) as u32);
            groups.push(LightGroup::new(first));
        }
        groups[g].lights[s as usize] = placed as u16;
        index[i] = RtlIndex { group: g as u8, slot: s, light: placed };
        placed += 1;
    }
    if groups.is_empty() {
        // A map with no real-time light still needs a table the shader can
        // read, and every entry has to name a light that exists.
        if world_count > 0 {
            groups.push(LightGroup::new(0));
        }
    }
    Ok((groups, index))
}

/// A texel's raw id word: the group in the top 8 bits, then four 6-bit slots.
pub fn pack_ids(group: u8, slots: [u32; 4]) -> u32 {
    let mut word = (group as u32 & 0xFF) << 24;
    for (i, s) in slots.iter().enumerate() {
        word |= (*s).min(SLOT_UNUSED) << (6 * i);
    }
    word
}

/// The four slots a word holds.
pub fn unpack_ids(word: u32) -> (u8, [u32; 4]) {
    let mut slots = [SLOT_UNUSED; 4];
    for (i, slot) in slots.iter_mut().enumerate() {
        *slot = (word >> (6 * i)) & 0x3F;
    }
    ((word >> 24) as u8, slots)
}

/// The table as the 0x7A records, one `u16` per slot.
pub fn group_table(groups: &[LightGroup]) -> Vec<Vec<u16>> {
    groups.iter().map(|g| g.lights.clone()).collect()
}

/// The world light a texel's slot resolves to.
pub fn resolve(groups: &[LightGroup], group: u8, slot: u32) -> Option<u32> {
    groups.get(group as usize).and_then(|g| g.resolve(slot))
}

/// The per-light group and slot assignment the page raster writes with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RtlSlots {
    pub slots: Vec<RtlIndex>,
}

impl RtlSlots {
    /// Where a scene light landed, or `None` when it is not written.
    pub fn of(&self, light: usize) -> Option<RtlIndex> {
        self.slots.get(light).copied().filter(|s| s.slot < GROUP_SLOTS as u32)
    }

    /// Records the real-time lights that reach a texel, strongest first, in the
    /// page's four slots.
    pub fn fill(
        &self,
        page: &mut crate::codec::RtlPage,
        texel: usize,
        ctx: &crate::light::DirectContext,
        lights: &[rir::BakeLight],
        position: glam::Vec3,
        normal: glam::Vec3,
    ) {
        let mut hits: Vec<(f32, RtlIndex)> = Vec::new();
        for (i, l) in lights.iter().enumerate() {
            if !l.realtime || l.kind == rir::LightType::Sky {
                continue;
            }
            let Some(slot) = self.of(i) else { continue };
            let s = ctx.direct(l, position, normal, f32::MAX);
            let w = s.colour.max(glam::Vec3::ZERO).max_element();
            if w > 0.0 {
                hits.push((w, slot));
            }
        }
        hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (w, slot) in hits.into_iter().take(4) {
            let weight = (w.clamp(0.0, 1.0) * 255.0).round() as u8;
            page.set_slot(texel, slot.group, slot.slot, weight.max(1));
        }
    }
}
