//! The GPU backend: the same visibility pass the CPU does, run as a compute
//! shader over the lightmap texels.
//!
//! The node and triangle buffers are the ones the CPU hierarchy already uses,
//! so both backends walk the same tree. With the `gpu` feature off the module
//! still exists and reports that no adapter was used, which is what the wasm
//! build needs.

use glam::Vec3;

use crate::charts::ChartLayout;
use crate::error::Result;
use crate::light::DirectContext;

/// One 16-byte box-and-child record, matching the CPU node layout.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuNode {
    pub lo: [f32; 4],
    pub hi: [f32; 4],
    /// Non-zero `count` marks a leaf; a zero count marks two children at
    /// `left_or_first` and `left_or_first + 1`.
    pub left_or_first: u32,
    pub count: u32,
}

impl GpuNode {
    pub fn from(n: &crate::bvh::Node) -> Self {
        Self {
            lo: pad4(n.lo),
            hi: pad4(n.hi),
            left_or_first: n.left_or_first,
            count: n.count,
        }
    }
}

/// One page's dimensions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub width: u32,
    pub height: u32,
}

fn pad4(v: Vec3) -> [f32; 4] {
    [v.x, v.y, v.z, 0.0]
}

/// A batch of texel origins, one ray each, with the page row each belongs to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RayBatch {
    pub pages: Vec<Page>,
    /// `x`, `y`, `z` of the texel centre.
    pub origins: Vec<[f32; 3]>,
    /// The page and row each origin lands in, parallel to `origins`.
    pub targets: Vec<[u32; 2]>,
    pub direction: [f32; 3],
}

impl RayBatch {
    pub fn is_empty(&self) -> bool {
        self.origins.is_empty()
    }
}

/// One page of the compute pass's output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VisibilityPage {
    pub width: u32,
    pub height: u32,
    pub values: Vec<f32>,
}

/// The compute pass's output, one page per lightmap page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Visibility {
    pub pages: Vec<VisibilityPage>,
}

impl Visibility {
    /// The visibility of a page texel, or `None` when the page is not in the set.
    pub fn get(&self, page: u32, x: u32, y: u32) -> Option<f32> {
        let p = self.pages.get(page as usize)?;
        let at = y as usize * p.width as usize + x as usize;
        p.values.get(at).copied()
    }
}

/// The rays the compute pass traces: one per lightmap texel a chart covers.
pub fn rays(ctx: &DirectContext, layout: &ChartLayout, scene: &rir::BakeScene) -> Option<RayBatch> {
    let sun = ctx.lights.iter().find(|l| l.kind == rir::LightType::Sun)?;
    let dir = sun.direction.normalize_or_zero();
    if dir.length_squared() < 0.5 {
        return None;
    }
    let pages: Vec<Page> = layout
        .pages
        .iter()
        .map(|p| Page {
            width: p.width.min(u16::MAX as u32),
            height: p.height.min(u16::MAX as u32),
        })
        .collect();
    let mut out = RayBatch {
        pages,
        direction: [dir.x, dir.y, dir.z],
        ..RayBatch::default()
    };
    for chart in &layout.charts {
        let Some(page) = out.pages.get(chart.page as usize) else { continue };
        let Some(surface) = scene.surfaces.get(chart.surface) else { continue };
        let iw = chart.inner[0] as usize;
        let texels = crate::light::rasterize(chart, surface);
        for (i, t) in texels.iter().enumerate() {
            let x = chart.origin[0] as usize + i % iw;
            let y = chart.origin[1] as usize + i / iw;
            if x >= page.width as usize || y >= page.height as usize {
                continue;
            }
            let p = t.map(|s| s.position).unwrap_or(chart.anchor);
            out.origins.push([p.x, p.y, p.z]);
            out.targets.push([chart.page, y as u32 * page.width + x as u32]);
        }
    }
    Some(out)
}

/// Runs the compute pass over every lightmap texel. Returns `None` when the
/// `gpu` feature is off or no adapter was available, in which case the caller
/// keeps the CPU result.
pub fn sun_visibility_batch(
    ctx: &DirectContext,
    layout: &ChartLayout,
    scene: &rir::BakeScene,
) -> Result<Option<Visibility>> {
    let Some(batch) = rays(ctx, layout, scene) else {
        return Ok(None);
    };
    #[cfg(feature = "gpu")]
    {
        match crate::gpu::device::run(ctx, &batch) {
            Ok(v) => Ok(Some(v)),
            Err(_) => Ok(None),
        }
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = (ctx, batch);
        Ok(None)
    }
}

/// The CPU answer for the same batch, which is what the two backends are
/// compared on.
pub fn cpu_visibility(ctx: &DirectContext, batch: &RayBatch) -> Visibility {
    let mut pages: Vec<VisibilityPage> = batch
        .pages
        .iter()
        .map(|p| VisibilityPage {
            width: p.width,
            height: p.height,
            values: vec![1.0; (p.width * p.height) as usize],
        })
        .collect();
    let dir = Vec3::new(batch.direction[0], batch.direction[1], batch.direction[2]).normalize_or_zero();
    for (i, o) in batch.origins.iter().enumerate() {
        let [page, at] = batch.targets[i];
        let origin = Vec3::new(o[0], o[1], o[2]);
        let v = ctx.sun_visibility(origin, dir, f32::MAX);
        if let Some(p) = pages.get_mut(page as usize) {
            if let Some(slot) = p.values.get_mut(at as usize) {
                *slot = v;
            }
        }
    }
    Visibility { pages }
}

#[cfg(feature = "gpu")]
pub mod device;

pub const SHADER: &str = include_str!("traverse.wgsl");
