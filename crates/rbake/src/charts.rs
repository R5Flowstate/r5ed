//! Lightmap chart packing.
//!
//! Every surface is projected onto its own plane, given one texel of world
//! surface per lightmap texel, padded so bilinear filtering cannot reach across
//! a chart, and packed into pages with a skyline packer. The result carries the
//! per-surface transform the vertex emit applies, so the lightmap coordinate of
//! a vertex is a world position pushed through an affine map.

use glam::{Vec2, Vec3};
use uuid::Uuid;

use crate::error::{Error, Result};

/// Texels of padding a chart keeps on every side.
pub const CHART_PADDING: u32 = 2;
/// The page edge a chart is kept off by.
pub const PAGE_MARGIN: u32 = 1;
/// The page size a bake packs into when the caller names none.
pub const DEFAULT_PAGE_SIZE: u32 = 1024;
/// Texels per lightmap texel used when a surface names no texel size.
pub const DEFAULT_TEXEL_SIZE: f32 = 16.0;
/// The largest chart edge a page is allowed to hold.
pub const MAX_CHART: i64 = 16384;

/// One surface's chart inside a page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chart {
    pub surface: usize,
    /// The surface's object, for the per-object preview.
    pub object: Option<Uuid>,
    pub page: u32,
    /// The chart's own texel window in the page, padding included.
    pub origin: [u32; 2],
    pub size: [u32; 2],
    /// World position to chart texel, excluding the page origin: a 2x2 affine
    /// map whose columns are the plane axes divided by the texel size.
    pub basis: [[f32; 2]; 3],
    /// The world position the chart's texel (0, 0) maps to.
    pub anchor: Vec3,
    /// The chart's usable texel count, padding excluded.
    pub inner: [u32; 2],
    /// The surface normal, summed over its faces.
    pub normal: Vec3,
    /// The chart is the surface's own `uv1` atlas placed whole: a vertex's
    /// texel is `uv1 * inner`, and `basis` and `anchor` are unused.
    pub atlas: bool,
}

impl Chart {
    /// The page-normalised lightmap coordinate of one vertex, given its
    /// position and its coordinate in the surface's own atlas.
    pub fn vertex_uv(&self, p: Vec3, atlas_uv: Vec2, page_size: u32) -> Vec2 {
        if !self.atlas {
            return self.uv(p, page_size);
        }
        let s = page_size as f32;
        Vec2::new(
            (self.origin[0] as f32 + atlas_uv.x * self.inner[0] as f32) / s,
            (self.origin[1] as f32 + atlas_uv.y * self.inner[1] as f32) / s,
        )
    }

    /// The page-normalised lightmap coordinate of a world position.
    pub fn uv(&self, p: Vec3, page_size: u32) -> Vec2 {
        let d = p - self.anchor;
        let x = (self.basis[0][0] * d.x + self.basis[1][0] * d.y + self.basis[2][0] * d.z) + self.origin[0] as f32;
        let y = (self.basis[0][1] * d.x + self.basis[1][1] * d.y + self.basis[2][1] * d.z) + self.origin[1] as f32;
        let s = page_size as f32;
        Vec2::new(x / s, y / s)
    }
}

/// One lightmap page and the rectangles already placed in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub used: Vec<[u32; 4]>,
}

/// Where every surface's chart ended up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartLayout {
    pub page_size: u32,
    pub pages: Vec<Page>,
    pub charts: Vec<Chart>,
    /// The layout is the map's own lightmap atlas rather than a packing: pages
    /// keep their sizes and charts may share rectangles.
    pub fixed: bool,
}

impl ChartLayout {
    pub fn chart(&self, surface: usize) -> Option<&Chart> {
        self.charts.get(surface)
    }

    /// The lightmap coordinate of a vertex of `surface`, or `None` when the
    /// surface has no chart.
    pub fn uv(&self, surface: usize, position: Vec3) -> Option<Vec2> {
        self.chart(surface).map(|c| c.uv(position, self.page_size))
    }

    /// True when no two charts share a texel, padding included.
    pub fn is_disjoint(&self) -> bool {
        let mut taken: Vec<[u32; 4]> = Vec::new();
        for c in &self.charts {
            for r in c.rect() {
                for t in &taken {
                    if rects_overlap(&r, t) {
                        return false;
                    }
                }
                taken.push(r);
            }
        }
        true
    }

    /// Every chart that falls inside its page.
    pub fn all_inside(&self) -> bool {
        self.charts.iter().all(|c| {
            let p = self.pages.get(c.page as usize);
            match p {
                Some(p) => {
                    c.origin[0] + c.size[0] <= p.width && c.origin[1] + c.size[1] <= p.height
                }
                None => false,
            }
        })
    }
}

impl Chart {
    fn rect(&self) -> [[u32; 4]; 1] {
        [[self.origin[0], self.origin[1], self.size[0], self.size[1]]]
    }
}

fn rects_overlap(a: &[u32; 4], b: &[u32; 4]) -> bool {
    a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
}

/// Packs every surface of `scene` into lightmap pages.
pub fn pack_charts(scene: &rir::BakeScene) -> Result<ChartLayout> {
    pack_charts_with(scene, DEFAULT_PAGE_SIZE)
}

/// The same packing at an explicit page size.
pub fn pack_charts_with(scene: &rir::BakeScene, page_size: u32) -> Result<ChartLayout> {
    if page_size < 16 {
        return Err(Error::lighting(format!(
            "a lightmap page of {page_size} texels is too small to chart into"
        )));
    }
    let mut layout = ChartLayout { page_size, ..ChartLayout::default() };
    let mut requests: Vec<Request> = Vec::new();
    for (si, s) in scene.surfaces.iter().enumerate() {
        if let Some(req) = chart_for_surface(si, s, scene.settings.texel_size, page_size)? {
            requests.push(req);
        }
    }
    // Tallest first: a skyline packer wastes far less when the long edges go
    // in before the short ones.
    requests.sort_by(|a, b| (b.size[1], b.size[0], b.surface).cmp(&(a.size[1], a.size[0], a.surface)));
    layout.pages.push(Page { index: 0, width: page_size, height: page_size, used: Vec::new() });
    let mut skyline = Skyline::new(page_size, page_size);
    for mut r in requests {
        // A surface's atlas maps to one page, so one larger than a page is
        // scaled down whole, keeping its layout, rather than refused.
        let room = page_size.saturating_sub(2 * PAGE_MARGIN + 2 * CHART_PADDING).max(1);
        if r.atlas && (r.inner[0] > room || r.inner[1] > room) {
            let f = room as f32 / r.inner[0].max(r.inner[1]) as f32;
            r.inner = r.inner.map(|v| ((v as f32 * f).floor() as u32).clamp(1, room));
            r.size = r.inner.map(|v| v + 2 * CHART_PADDING);
        }
        let w = r.size[0] + 2 * PAGE_MARGIN;
        let h = r.size[1] + 2 * PAGE_MARGIN;
        if w > page_size || h > page_size {
            return Err(Error::Chart {
                surface: r.surface,
                chart: 0,
                msg: format!(
                    "a {}x{} chart does not fit a {page_size} texel page; raise the lightmap texel size",
                    r.size[0], r.size[1]
                ),
            });
        }
        let spot = match skyline.place(w, h) {
            Some(s) => s,
            None => {
                let index = layout.pages.len() as u32;
                layout.pages.push(Page { index, width: page_size, height: page_size, used: Vec::new() });
                skyline = Skyline::new(page_size, page_size);
                skyline.place(w, h).ok_or_else(|| Error::Chart {
                    surface: r.surface,
                    chart: 0,
                    msg: "a fresh page did not take the chart".to_string(),
                })?
            }
        };
        if let Some(p) = layout.pages.last_mut() { p.used.push([spot[0], spot[1], w, h]) }
        layout.charts.push(Chart {
            surface: r.surface,
            object: r.object,
            page: index_of(&layout),
            origin: [spot[0] + CHART_PADDING, spot[1] + CHART_PADDING],
            size: r.size,
            basis: r.basis,
            anchor: r.anchor,
            inner: r.inner,
            normal: r.normal,
            atlas: r.atlas,
        });
    }
    if layout.pages.len() > MAX_LIGHTMAP_PAGES {
        return Err(Error::lighting(format!(
            "the charts fill {} lightmap pages and the client binds at most {MAX_LIGHTMAP_PAGES};              raise the texel size, or the lightmap_scale of the meshes nobody walks near",
            layout.pages.len()
        )));
    }
    layout.charts.sort_by_key(|c| c.surface);
    Ok(layout)
}

/// Lightmap pages the S21 client can bind: its per-page texture tables hold
/// 33 entries and page N binds entry N + 1, so a 33rd page reads past them.
pub const MAX_LIGHTMAP_PAGES: usize = 32;

fn index_of(layout: &ChartLayout) -> u32 {
    layout.pages.len().saturating_sub(1) as u32
}

/// A chart request: how big a surface wants to be and what it is.
#[derive(Clone, Debug)]
struct Request {
    surface: usize,
    object: Option<Uuid>,
    /// Texel window the chart occupies, padding included.
    size: [u32; 2],
    /// Usable texel count, padding excluded.
    inner: [u32; 2],
    /// Rows are world axes, columns are chart axes; the value is the axis
    /// length in texels.
    basis: [[f32; 2]; 3],
    /// The world position chart texel (0, 0) maps to.
    anchor: Vec3,
    /// The surface normal.
    normal: Vec3,
    atlas: bool,
}

/// An orthonormal pair spanning a surface's plane, taken from the surface's own
/// tangent when that one lies in the plane.
fn plane_basis(n: Vec3, tangent: Option<Vec3>) -> (Vec3, Vec3) {
    if let Some(t) = tangent {
        if t.is_finite() && t.length_squared() > 1.0e-12 {
            let t = t.normalize();
            if t.dot(n).abs() < 0.95 {
                return (t, n.cross(t).normalize_or_zero());
            }
        }
    }
    let up = if n.z.abs() > 0.9 { Vec3::X } else { Vec3::Z };
    let t = up.cross(n);
    if t.length_squared() < 1.0e-12 {
        return (Vec3::X, Vec3::Y);
    }
    let t = t.normalize();
    (t, n.cross(t).normalize_or_zero())
}

fn chart_for_surface(si: usize, s: &rir::RenderSurface, texel_size: f32, page_size: u32) -> Result<Option<Request>> {
    if s.positions.is_empty() || s.indices.len() < 3 {
        return Ok(None);
    }
    let mut texel = if texel_size.is_finite() && texel_size > 0.0 {
        texel_size
    } else if s.lightmap_texel.is_finite() && s.lightmap_texel > 0.0 {
        s.lightmap_texel
    } else {
        DEFAULT_TEXEL_SIZE
    };
    // The chart plane comes from the summed triangle normals, so a surface made
    // of several planes still charts as one sheet.
    let mut n = Vec3::ZERO;
    for c in s.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&d)) = (
            s.positions.get(c[0] as usize),
            s.positions.get(c[1] as usize),
            s.positions.get(c[2] as usize),
        ) else {
            return Err(Error::Chart {
                surface: si,
                chart: 0,
                msg: "the surface names a vertex it does not have".to_string(),
            });
        };
        n += crate::light::front_normal(a, b, d);
    }
    if n.length_squared() < 1.0e-18 {
        n = s.normals.first().copied().unwrap_or(Vec3::Z);
    }
    let n = n.normalize_or_zero();
    if let Some(inner) = atlas_size(s) {
        // A surface that brings its own atlas keeps it: every face already
        // has its own plane and texels there, so the chart is the atlas.
        return Ok(Some(Request {
            surface: si,
            object: s.object,
            size: [inner[0] + 2 * CHART_PADDING, inner[1] + 2 * CHART_PADDING],
            inner,
            basis: [[0.0; 2]; 3],
            anchor: s.positions[0],
            normal: n,
            atlas: true,
        }));
    }
    if n.length_squared() < 0.5 {
        return Ok(None);
    }
    let tangent = s.tangents.first().map(|t| Vec3::new(t.x, t.y, t.z));
    let (ax, ay) = plane_basis(n, tangent);
    let (lo_u, lo_v, hi_u, hi_v) = match projected_bounds(s, ax, ay) {
        Some(v) => v,
        None => return Ok(None),
    };
    // A surface wider than a page at its texel size (a shipped map's terrain
    // runs to tens of thousands of units) takes the coarsest texel that fits
    // it on one page, rather than failing the bake.
    let room = page_size.saturating_sub(2 * (PAGE_MARGIN + CHART_PADDING)).max(1) as f32;
    let span = (hi_u - lo_u).max(hi_v - lo_v);
    if span / texel > room {
        texel = span / room * 1.001;
    }
    let w = (((hi_u - lo_u) / texel).ceil() as i64).clamp(1, MAX_CHART) as u32;
    let h = (((hi_v - lo_v) / texel).ceil() as i64).clamp(1, MAX_CHART) as u32;
    let inv = 1.0 / texel;
    let pad = CHART_PADDING as f32 * texel;
    // The anchor is placed so the surface's lowest chart-axis coordinate lands
    // on the first texel inside the padding.
    let first = s
        .positions
        .iter()
        .find(|p| p.is_finite())
        .copied()
        .unwrap_or(Vec3::ZERO);
    let anchor = first + ax * ((lo_u - pad) - first.dot(ax)) + ay * ((lo_v - pad) - first.dot(ay));
    Ok(Some(Request {
        surface: si,
        object: s.object,
        size: [w + 2 * CHART_PADDING, h + 2 * CHART_PADDING],
        inner: [w, h],
        basis: [[ax.x * inv, ay.x * inv], [ax.y * inv, ay.y * inv], [ax.z * inv, ay.z * inv]],
        anchor,
        normal: n,
        atlas: false,
    }))
}

/// The texel size of a surface's `uv1` atlas, when it carries a usable one.
fn atlas_size(s: &rir::RenderSurface) -> Option<[u32; 2]> {
    let [w, h] = s.lightmap_size;
    if w == 0 || h == 0 || w as i64 > MAX_CHART || h as i64 > MAX_CHART || s.uv1.len() != s.positions.len() {
        return None;
    }
    let inside = |v: f32| v.is_finite() && (-1.0e-4..=1.0 + 1.0e-4).contains(&v);
    s.uv1.iter().all(|u| inside(u.x) && inside(u.y)).then_some([w, h])
}

/// The smallest and largest chart-axis coordinate over a surface's vertices.
fn projected_bounds(s: &rir::RenderSurface, ax: Vec3, ay: Vec3) -> Option<(f32, f32, f32, f32)> {
    let mut lo_u = f32::INFINITY;
    let mut lo_v = f32::INFINITY;
    let mut hi_u = f32::NEG_INFINITY;
    let mut hi_v = f32::NEG_INFINITY;
    let mut any = false;
    for p in &s.positions {
        if !p.is_finite() {
            continue;
        }
        let (u, v) = (p.dot(ax), p.dot(ay));
        any = true;
        lo_u = lo_u.min(u);
        lo_v = lo_v.min(v);
        hi_u = hi_u.max(u);
        hi_v = hi_v.max(v);
    }
    if !any || !lo_u.is_finite() || !hi_v.is_finite() {
        return None;
    }
    Some((lo_u, lo_v, hi_u, hi_v))
}

/// A skyline packer: a height per column, and each placement raises the span it
/// covers.
struct Skyline {
    width: u32,
    height: u32,
    tops: Vec<u32>,
}

impl Skyline {
    fn new(width: u32, height: u32) -> Self {
        Self { width, height, tops: vec![0; width as usize] }
    }

    /// The lowest position a `w` by `h` rectangle can take, as (x, y).
    fn place(&mut self, w: u32, h: u32) -> Option<[u32; 2]> {
        if w == 0 || h == 0 || w > self.width || h > self.height {
            return None;
        }
        let mut best: Option<(u32, u32)> = None;
        for x in 0..=(self.width - w) {
            let top = self.tops[x as usize..(x + w) as usize].iter().copied().max().unwrap_or(0);
            if top + h > self.height {
                continue;
            }
            let better = match best {
                None => true,
                Some((by, bx)) => top < by || (top == by && x < bx),
            };
            if better {
                best = Some((top, x));
            }
        }
        let (y, x) = best?;
        for slot in &mut self.tops[x as usize..(x + w) as usize] {
            *slot = y + h;
        }
        Some([x, y])
    }
}

/// The layout a map's existing lightmap atlas already gives its surfaces.
///
/// `pages` are the map's page sizes and `page_of` names each surface's page,
/// `None` for a surface with no lightmap. A surface's `uv1` is page-normalised
/// on the way in; it comes out normalised to the surface's own rectangle, which
/// is the chart it is shaded in, so it should be one connected chart of the
/// atlas or the rectangle spans every chart it holds.
pub fn fixed_layout(scene: &mut rir::BakeScene, pages: &[[u32; 2]], page_of: &[Option<u32>]) -> Result<ChartLayout> {
    let mut layout = ChartLayout { page_size: PAGE_SIZE_FIXED, fixed: true, ..ChartLayout::default() };
    for (i, [w, h]) in pages.iter().enumerate() {
        layout.pages.push(Page { index: i as u32, width: *w, height: *h, used: Vec::new() });
    }
    for (si, s) in scene.surfaces.iter_mut().enumerate() {
        let Some(page) = page_of.get(si).copied().flatten() else { continue };
        let Some(&[pw, ph]) = pages.get(page as usize) else {
            return Err(Error::Chart { surface: si, chart: 0, msg: format!("the surface names lightmap page {page}, the map has {}", pages.len()) });
        };
        if s.indices.is_empty() || s.uv1.len() != s.positions.len() {
            continue;
        }
        let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
        for &i in &s.indices {
            let t = s.uv1[i as usize] * Vec2::new(pw as f32, ph as f32);
            lo = lo.min(t);
            hi = hi.max(t);
        }
        if !lo.is_finite() || !hi.is_finite() {
            continue;
        }
        let x0 = (lo.x.floor().max(0.0) as u32).min(pw.saturating_sub(1));
        let y0 = (lo.y.floor().max(0.0) as u32).min(ph.saturating_sub(1));
        let x1 = (hi.x.ceil() as u32).clamp(x0 + 1, pw);
        let y1 = (hi.y.ceil() as u32).clamp(y0 + 1, ph);
        let inner = [x1 - x0, y1 - y0];
        for uv in s.uv1.iter_mut() {
            let t = *uv * Vec2::new(pw as f32, ph as f32);
            *uv = Vec2::new((t.x - x0 as f32) / inner[0] as f32, (t.y - y0 as f32) / inner[1] as f32);
        }
        s.lightmap_size = inner;
        layout.charts.push(Chart {
            surface: si,
            object: s.object,
            page,
            origin: [x0, y0],
            size: inner,
            basis: [[0.0; 2]; 3],
            anchor: Vec3::ZERO,
            inner,
            normal: Vec3::ZERO,
            atlas: true,
        });
    }
    Ok(layout)
}

/// The page size a fixed layout reports; its pages carry their own sizes.
pub const PAGE_SIZE_FIXED: u32 = 1024;
