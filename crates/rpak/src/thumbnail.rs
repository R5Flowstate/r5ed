//! A tiny CPU rasteriser that renders a shaded 128x128 preview of a model.
//!
//! No GPU and no image library: triangles are projected orthographically,
//! depth-buffered and lit with a fixed head light, then the framebuffer is
//! encoded as a PNG.

use crate::error::Result;
use crate::rmdl::RmdlModel;

pub const THUMB_SIZE: usize = 128;

/// An RGBA8 image.
pub struct Image {
    pub width: usize,
    pub height: usize,
    /// Row-major RGBA, row 0 at the top.
    pub pixels: Vec<u8>,
}

impl Image {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, pixels: vec![0; width * height * 4] }
    }

    fn put(&mut self, x: usize, y: usize, c: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let o = (y * self.width + x) * 4;
        self.pixels[o..o + 4].copy_from_slice(&c);
    }
}

/// Renders a model's LOD0 with a fixed camera looking down -Z with a slight
/// tilt, head light from the upper left.
pub fn render_thumbnail(model: &RmdlModel) -> Result<Image> {
    let mut img = Image::new(THUMB_SIZE, THUMB_SIZE);
    let mut depth = vec![f32::INFINITY; THUMB_SIZE * THUMB_SIZE];
    // Fit the whole bounding box, not just what LOD0 covers, so a preview is
    // stable when the model is being drawn at a lower LOD.
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for k in 0..3 {
        lo[k] = model.bounds_min[k];
        hi[k] = model.bounds_max[k];
    }
    for m in &model.meshes {
        for p in &m.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    if !lo[0].is_finite() || !hi[0].is_finite() {
        return Ok(img);
    }
    let extent: Vec<f32> = (0..3).map(|k| (hi[k] - lo[k]).max(1e-3)).collect();
    let max_extent = extent.iter().copied().fold(0.0f32, f32::max);
    let scale = (THUMB_SIZE as f32 * 0.82) / max_extent;
    let center = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    let light = normalize([-0.4, -0.5, -0.76]);

    for m in &model.meshes {
        let shade = shade_for(m.material);
        for tri in m.indices.chunks_exact(3) {
            let p = [m.positions[tri[0] as usize], m.positions[tri[1] as usize], m.positions[tri[2] as usize]];
            let v: Vec<[f32; 2]> = p
                .iter()
                .map(|w| project([w[0] - center[0], w[1] - center[1], w[2] - center[2]], scale, THUMB_SIZE))
                .collect();
            let z = [
                (p[0][2] - center[2]) * scale,
                (p[1][2] - center[2]) * scale,
                (p[2][2] - center[2]) * scale,
            ];
            let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
            let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
            let face_n = normalize(cross(e1, e2));
            let lambert = dot(face_n, light).abs().clamp(0.15, 1.0);
            fill_triangle(&mut img, &mut depth, &v, &z, shade, lambert);
        }
    }
    Ok(img)
}


fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-9 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// A fixed tilt so a prop reads as a prop rather than a flat silhouette.
fn project(v: [f32; 3], scale: f32, size: usize) -> [f32; 2] {
    let tilt = 0.35f32;
    let (s, c) = (tilt.sin(), tilt.cos());
    let x = v[0] * c - v[1] * s;
    let y = v[0] * s + v[1] * c - v[2] * 0.25;
    [x * scale + size as f32 * 0.5, size as f32 * 0.5 - y * scale]
}

fn shade_for(material: u16) -> [u8; 3] {
    const PALETTE: [[u8; 3]; 6] = [
        [190, 190, 205],
        [205, 160, 120],
        [140, 180, 150],
        [180, 150, 200],
        [220, 200, 130],
        [130, 190, 200],
    ];
    PALETTE[(material as usize) % PALETTE.len()]
}

fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn fill_triangle(
    img: &mut Image,
    depth: &mut [f32],
    v: &[[f32; 2]],
    z: &[f32],
    shade: [u8; 3],
    lambert: f32,
) {
    let min_x = v.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
    let max_x = (v.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max).ceil() as isize)
        .min(img.width as isize - 1)
        .max(-1) as usize;
    let min_y = v.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
    let max_y = (v.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max).ceil() as isize)
        .min(img.height as isize - 1)
        .max(-1) as usize;
    if min_x > max_x || min_y > max_y {
        return;
    }
    let area = edge(v[0], v[1], v[2]);
    if area.abs() < 1e-9 {
        return;
    }
    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let p = [px as f32 + 0.5, py as f32 + 0.5];
            let w0 = edge(v[1], v[2], p) / area;
            let w1 = edge(v[2], v[0], p) / area;
            let w2 = edge(v[0], v[1], p) / area;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let zz = w0 * z[0] + w1 * z[1] + w2 * z[2];
            let di = py * img.width + px;
            // A smaller depth is nearer the camera, which looks down -Z.
            if zz >= depth[di] {
                continue;
            }
            depth[di] = zz;
            img.put(
                px,
                py,
                [
                    (shade[0] as f32 * lambert).clamp(0.0, 255.0) as u8,
                    (shade[1] as f32 * lambert).clamp(0.0, 255.0) as u8,
                    (shade[2] as f32 * lambert).clamp(0.0, 255.0) as u8,
                    255,
                ],
            );
        }
    }
}
