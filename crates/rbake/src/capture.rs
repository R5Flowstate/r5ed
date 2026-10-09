//! Cubemaps captured by the client's `cubemap_capture` command: six float
//! views per cubemap, rendered by the engine itself from the map's cubemap
//! origins. A captured set replaces the baked render of each cubemap it covers.
//!
//! Each view is a 90 degree square rendered along a (pitch, yaw) camera, with
//! the engine's axes: forward (cp*cy, cp*sy, -sp), right (sy, -cy, 0) and up
//! (sp*cy, sp*sy, cp). An output texel direction is projected into whichever
//! view it is most in front of, so the stored face layout never has to match
//! the capture order.

use std::path::{Path, PathBuf};

use glam::Vec3;

use crate::cubemap::{face_direction, BakedCubemap};
use crate::error::{Error, Result};

/// The environment variable naming a capture directory
/// (`platform/cubemaps/<map>` of the install the capture ran in).
pub const CAPTURE_DIR_ENV: &str = "R5ED_CUBEMAP_CAPTURE";

/// A captured origin must sit this close to the cubemap it stands in for.
pub const MATCH_DISTANCE: f32 = 32.0;

struct View {
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    size: usize,
    /// Rows top first.
    pixels: Vec<Vec3>,
}

pub struct CapturedCubemap {
    pub origin: Vec3,
    views: Vec<View>,
}

pub struct CapturedSet {
    pub dir: PathBuf,
    pub cubemaps: Vec<CapturedCubemap>,
}

fn numbers(s: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == 'e' || ch == 'E' || ch == '+' {
            cur.push(ch);
        } else if !cur.is_empty() {
            if let Ok(v) = cur.parse() {
                out.push(v);
            }
            cur.clear();
        }
    }
    if let Ok(v) = cur.parse() {
        out.push(v);
    }
    out
}

/// The text between the brackets that follow `"key":`.
fn array_after<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let at = text.find(&format!("\"{key}\""))?;
    let rest = &text[at..];
    let open = rest.find('[')?;
    let mut depth = 0usize;
    for (i, ch) in rest[open..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn read_pfm(path: &Path) -> Result<(usize, Vec<Vec3>)> {
    let bytes = std::fs::read(path).map_err(|e| Error::lighting(format!("{}: {e}", path.display())))?;
    // "PF\n<w> <h>\n<scale>\n" then float RGB rows, bottom row first.
    let mut fields = Vec::new();
    let mut at = 0usize;
    while fields.len() < 4 && at < bytes.len() {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..at]).into_owned());
    }
    at += 1;
    if fields.len() < 4 || fields[0] != "PF" {
        return Err(Error::lighting(format!("{} is not a colour PFM", path.display())));
    }
    let w: usize = fields[1].parse().unwrap_or(0);
    let h: usize = fields[2].parse().unwrap_or(0);
    let scale: f32 = fields[3].parse().unwrap_or(-1.0);
    if w == 0 || w != h || bytes.len() < at + w * h * 12 {
        return Err(Error::lighting(format!("{} is not a square PFM of the size it declares", path.display())));
    }
    let big_endian = scale > 0.0;
    let mut pixels = vec![Vec3::ZERO; w * h];
    for row in 0..h {
        let y = h - 1 - row;
        for x in 0..w {
            let o = at + (row * w + x) * 12;
            let f = |k: usize| {
                let b = [bytes[o + k], bytes[o + k + 1], bytes[o + k + 2], bytes[o + k + 3]];
                let v = if big_endian { f32::from_be_bytes(b) } else { f32::from_le_bytes(b) };
                if v.is_finite() && v > 0.0 { v } else { 0.0 }
            };
            pixels[y * w + x] = Vec3::new(f(0), f(4), f(8));
        }
    }
    Ok((w, pixels))
}

fn basis(pitch: f32, yaw: f32) -> (Vec3, Vec3, Vec3) {
    let (sp, cp) = pitch.to_radians().sin_cos();
    let (sy, cy) = yaw.to_radians().sin_cos();
    (Vec3::new(cp * cy, cp * sy, -sp), Vec3::new(sy, -cy, 0.0), Vec3::new(sp * cy, sp * sy, cp))
}

/// Reads the capture in `dir`, if there is one. Cubemaps whose faces are not
/// all present are left out, so the bake renders them instead.
pub fn load(dir: &Path) -> Result<Option<CapturedSet>> {
    let manifest = dir.join("manifest.json");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return Ok(None);
    };
    let faces = numbers(array_after(&text, "faces").unwrap_or(""));
    let origins = numbers(array_after(&text, "cubemaps").unwrap_or(""));
    if faces.len() != 18 || !origins.len().is_multiple_of(3) {
        return Err(Error::lighting(format!("{} does not list six faces and whole origins", manifest.display())));
    }
    let mut cubemaps = Vec::new();
    for (i, o) in origins.chunks_exact(3).enumerate() {
        let mut views = Vec::with_capacity(6);
        for (f, angles) in faces.chunks_exact(3).enumerate() {
            let path = dir.join(format!("cubemap_{i:03}_{f}.pfm"));
            if !path.is_file() {
                break;
            }
            let (size, pixels) = read_pfm(&path)?;
            let (forward, right, up) = basis(angles[0], angles[1]);
            views.push(View { forward, right, up, size, pixels });
        }
        if views.len() == 6 {
            cubemaps.push(CapturedCubemap { origin: Vec3::new(o[0], o[1], o[2]), views });
        }
    }
    Ok(Some(CapturedSet { dir: dir.to_path_buf(), cubemaps }))
}

/// The capture directory named by [`CAPTURE_DIR_ENV`], if it holds a capture.
pub fn from_env() -> Result<Option<CapturedSet>> {
    match std::env::var_os(CAPTURE_DIR_ENV) {
        Some(dir) if !dir.is_empty() => load(Path::new(&dir)),
        _ => Ok(None),
    }
}

impl CapturedSet {
    /// The capture taken at (or within [`MATCH_DISTANCE`] of) `position`.
    pub fn at(&self, position: Vec3) -> Option<&CapturedCubemap> {
        self.cubemaps
            .iter()
            .filter(|c| c.origin.distance(position) <= MATCH_DISTANCE)
            .min_by(|a, b| a.origin.distance(position).total_cmp(&b.origin.distance(position)))
    }
}

fn bilinear(view: &View, u: f32, v: f32) -> Vec3 {
    let n = view.size;
    let fx = (u * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
    let fy = (v * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(n - 1), (y0 + 1).min(n - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let p = |x: usize, y: usize| view.pixels[y * n + x];
    let top = p(x0, y0).lerp(p(x1, y0), tx);
    let bottom = p(x0, y1).lerp(p(x1, y1), tx);
    top.lerp(bottom, ty)
}

impl CapturedCubemap {
    /// The face size the capture was written at.
    pub fn face_size(&self) -> u32 {
        self.views.first().map(|v| v.size as u32).unwrap_or(0)
    }

    /// The captured radiance along a world direction.
    pub fn sample(&self, dir: Vec3) -> Vec3 {
        let view = self
            .views
            .iter()
            .max_by(|a, b| dir.dot(a.forward).total_cmp(&dir.dot(b.forward)))
            .expect("a captured cubemap has six views");
        let depth = dir.dot(view.forward).max(1e-6);
        let x = dir.dot(view.right) / depth;
        let y = dir.dot(view.up) / depth;
        bilinear(view, (x + 1.0) * 0.5, (1.0 - y) * 0.5)
    }

    /// The cubemap in the layout the texture stores, at `size` per face.
    pub fn to_baked(&self, position: Vec3, size: u32, ambient_scale: f32) -> BakedCubemap {
        let n = size as usize;
        let mut faces = Vec::with_capacity(6);
        for face in 0..6 {
            let mut buf = vec![Vec3::ZERO; n * n];
            for y in 0..n {
                for x in 0..n {
                    let s = 2.0 * (x as f32 + 0.5) / n as f32 - 1.0;
                    let t = 2.0 * (y as f32 + 0.5) / n as f32 - 1.0;
                    buf[y * n + x] = self.sample(face_direction(face, s, t));
                }
            }
            faces.push(buf);
        }
        BakedCubemap { position, ambient_scale, faces, size }
    }
}

/// The engine's reflection normaliser for a cubemap: one over the light the
/// capture point receives -- open sky scaled by the sky's luminance, plus the
/// local ambient -- so a reflection can be relit by the ambient at the pixel.
pub fn ambient_scale(sky_open: f32, sky_colour: Vec3, ambient: Vec3) -> f32 {
    let lum = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    1.0 / (sky_open.max(0.0) * lum(sky_colour) + lum(ambient)).max(0.001)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_face_direction_maps_back_to_its_texel() {
        for face in 0..6 {
            for (s, t) in [(-0.9f32, -0.9f32), (0.3, -0.5), (0.0, 0.0), (0.7, 0.8)] {
                let (f, s2, t2) = crate::cubemap::direction_to_face(face_direction(face, s, t));
                assert_eq!(f, face, "face {face} s {s} t {t}");
                assert!((s2 - s).abs() < 1e-5 && (t2 - t).abs() < 1e-5, "face {face}: {s},{t} -> {s2},{t2}");
            }
        }
    }

    #[test]
    fn a_face_centre_looks_down_its_own_axis() {
        for (pitch, yaw, expect) in [
            (0.0, 0.0, Vec3::X),
            (0.0, 90.0, Vec3::Y),
            (0.0, 180.0, Vec3::NEG_X),
            (-90.0, 0.0, Vec3::Z),
            (90.0, 0.0, Vec3::NEG_Z),
        ] {
            let (f, r, u) = basis(pitch, yaw);
            assert!(f.distance(expect) < 1e-5, "pitch {pitch} yaw {yaw} forward {f}");
            assert!(r.dot(f).abs() < 1e-5 && u.dot(f).abs() < 1e-5 && r.cross(u).dot(f) < 0.0);
        }
    }

    #[test]
    fn a_direction_lands_in_the_view_it_faces() {
        let mk = |pitch: f32, yaw: f32, colour: Vec3| {
            let (forward, right, up) = basis(pitch, yaw);
            View { forward, right, up, size: 4, pixels: vec![colour; 16] }
        };
        let c = CapturedCubemap {
            origin: Vec3::ZERO,
            views: vec![
                mk(0.0, 0.0, Vec3::X),
                mk(0.0, 180.0, Vec3::Y),
                mk(0.0, 90.0, Vec3::Z),
                mk(0.0, 270.0, Vec3::ONE),
                mk(-90.0, 0.0, Vec3::splat(2.0)),
                mk(90.0, 0.0, Vec3::splat(3.0)),
            ],
        };
        assert_eq!(c.sample(Vec3::X), Vec3::X);
        assert_eq!(c.sample(Vec3::NEG_X), Vec3::Y);
        assert_eq!(c.sample(Vec3::Y), Vec3::Z);
        assert_eq!(c.sample(Vec3::Z), Vec3::splat(2.0));
        assert_eq!(c.sample(Vec3::NEG_Z), Vec3::splat(3.0));
    }

    #[test]
    fn the_right_edge_of_a_view_is_its_right_vector() {
        let (forward, right, up) = basis(0.0, 0.0);
        let mut pixels = vec![Vec3::ZERO; 16];
        for y in 0..4 {
            pixels[y * 4 + 3] = Vec3::ONE;
        }
        let v = View { forward, right, up, size: 4, pixels };
        let c = CapturedCubemap { origin: Vec3::ZERO, views: (0..6).map(|_| View { forward: v.forward, right: v.right, up: v.up, size: 4, pixels: v.pixels.clone() }).collect() };
        // Engine yaw 0 looks down +X with right = -Y.
        assert!(c.sample((Vec3::X + Vec3::NEG_Y * 0.9).normalize()).x > 0.5);
        assert!(c.sample((Vec3::X + Vec3::Y * 0.9).normalize()).x < 0.5);
    }
}
