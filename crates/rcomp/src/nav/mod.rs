//! The triangle soup a Recast bake consumes, plus an OBJ exporter for it.

use glam::{DVec3, Vec3};
use rbsp::Bsp;
use rir::{AssetTables, StaticPropInstance};

#[derive(Debug, thiserror::Error)]
pub enum NavError {
    #[error("the map's collision does not decode: {0}")]
    Collision(#[from] crate::collision::CollError),
    #[error("navmesh bounds {min:?}..{max:?} are not a finite box")]
    BadBounds { min: [f64; 3], max: [f64; 3] },
}

/// The soup, as an indexed triangle mesh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavSoup {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    /// Clipped triangles per source: brushes, meshes, terrain and props.
    pub counts: NavCounts,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavCounts {
    pub brush_triangles: usize,
    pub mesh_triangles: usize,
    pub terrain_triangles: usize,
    pub prop_triangles: usize,
}

impl NavSoup {
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for p in &self.positions {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        if lo.x > hi.x {
            return (Vec3::ZERO, Vec3::ZERO);
        }
        (lo, hi)
    }
}

struct Builder {
    soup: NavSoup,
    clip: Option<(DVec3, DVec3)>,
}

impl Builder {
    fn tri(&mut self, a: DVec3, b: DVec3, c: DVec3, which: fn(&mut NavCounts)) {
        if !(a.is_finite() && b.is_finite() && c.is_finite()) {
            return;
        }
        // A triangle with no area contributes nothing a recast can walk on.
        let area = (b - a).cross(c - a).length();
        if area <= 1e-6 {
            return;
        }
        if let Some((lo, hi)) = self.clip {
            let inside = |p: DVec3| p.cmpge(lo).all() && p.cmple(hi).all();
            if !(inside(a) && inside(b) && inside(c)) {
                let centre = (a + b + c) / 3.0;
                if !inside(centre) {
                    return;
                }
            }
        }
        let base = self.soup.positions.len() as u32;
        for p in [a, b, c] {
            self.soup.positions.push(p.as_vec3());
        }
        self.soup.indices.extend_from_slice(&[base, base + 1, base + 2]);
        which(&mut self.soup.counts);
    }
}

/// Builds the soup: every collision triangle the map carries, then a box per
/// static prop that has no collision mesh of its own.
pub fn build(
    bsp: &Bsp,
    props: &[StaticPropInstance],
    assets: &AssetTables,
    bounds: Option<[DVec3; 2]>,
) -> Result<NavSoup, NavError> {
    let clip = match bounds {
        None => None,
        Some([min, max]) => {
            if !(min.is_finite() && max.is_finite() && min.cmple(max).all()) {
                return Err(NavError::BadBounds { min: min.to_array(), max: max.to_array() });
            }
            Some((min, max))
        }
    };
    let mut b = Builder { soup: NavSoup::default(), clip };
    for (_, tris) in crate::collision::decode_collision(bsp)? {
        for t in tris {
            b.tri(t[0], t[1], t[2], |c| c.brush_triangles += 1);
        }
    }
    for p in props {
        if !p.cast_shadows && !p.solid {
            continue;
        }
        let model = assets.models.get(&p.model);
        let collidable = model.map(|m| m.has_collision).unwrap_or(false);
        if collidable {
            continue;
        }
        // The instance carries the bounds the placement resolved, which is
        // better than the table when the model is not in it.
        let (bmin, bmax) = if p.bounds_min.x <= p.bounds_max.x {
            (p.bounds_min, p.bounds_max)
        } else {
            match model {
                Some(m) => (m.bounds_min, m.bounds_max),
                None => (Vec3::splat(-8.0), Vec3::splat(8.0)),
            }
        };
        let (lo, hi) = prop_bounds(p, bmin, bmax);
        let c = [
            DVec3::new(lo.x as f64, lo.y as f64, lo.z as f64),
            DVec3::new(hi.x as f64, hi.y as f64, hi.z as f64),
        ];
        // The six faces of the prop's box, each split into two triangles.
        let v = [
            [c[0].x, c[0].y, c[0].z],
            [c[1].x, c[0].y, c[0].z],
            [c[1].x, c[1].y, c[0].z],
            [c[0].x, c[1].y, c[0].z],
            [c[0].x, c[0].y, c[1].z],
            [c[1].x, c[0].y, c[1].z],
            [c[1].x, c[1].y, c[1].z],
            [c[0].x, c[1].y, c[1].z],
        ];
        let faces = [(0, 1, 2, 3), (7, 6, 5, 4), (0, 4, 5, 1), (1, 5, 6, 2), (2, 6, 7, 3), (3, 7, 4, 0)];
        for (a, bb, c2, d) in faces {
            b.tri(
                DVec3::from_array(v[a]),
                DVec3::from_array(v[bb]),
                DVec3::from_array(v[c2]),
                |n| n.prop_triangles += 1,
            );
            b.tri(
                DVec3::from_array(v[a]),
                DVec3::from_array(v[c2]),
                DVec3::from_array(v[d]),
                |n| n.prop_triangles += 1,
            );
        }
    }
    Ok(b.soup)
}

/// A prop's world box, rotated with its angles when they are not identity.
fn prop_bounds(p: &StaticPropInstance, bmin: Vec3, bmax: Vec3) -> (Vec3, Vec3) {
    let (pitch, yaw, roll) = (p.angles.x.to_radians(), p.angles.y.to_radians(), p.angles.z.to_radians());
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    let (sr, cr) = roll.sin_cos();
    // ZYX order, the same one the document's transform uses.
    let rot = glam::Mat3::from_cols_array(&[
        cy * cr,
        cy * -sr * sp + sy * cp,
        cy * sr * cp + sy * sp,
        sy * cr,
        sy * -sr * sp + cy * cp,
        sy * sr * cp - cy * sp,
        -sr,
        -cr * sp,
        cr * cp,
    ]);
    let scale = if p.scale > 0.0 { p.scale } else { 1.0 };
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for x in [bmin.x, bmax.x] {
        for y in [bmin.y, bmax.y] {
            for z in [bmin.z, bmax.z] {
                let p = rot * Vec3::new(x, y, z) * scale + p.origin;
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
    }
    (lo, hi)
}

/// The soup as a Wavefront OBJ, one vertex and face per record.
pub fn to_obj(soup: &NavSoup) -> String {
    let mut out = String::with_capacity(soup.positions.len() * 32 + soup.indices.len() * 12);
    out.push_str("# r5ed navigation soup\n");
    for p in &soup.positions {
        out.push_str("v ");
        out.push_str(&format!("{} {} {}\n", p.x, p.y, p.z));
    }
    for t in soup.indices.chunks_exact(3) {
        out.push_str("f ");
        for i in t {
            out.push_str(&format!("{} ", i + 1));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbsp::Version;

    fn prop(model: &str, origin: Vec3) -> StaticPropInstance {
        StaticPropInstance {
            model: model.into(),
            origin,
            angles: Vec3::ZERO,
            scale: 1.0,
            skin: 0,
            solid: true,
            cast_shadows: true,
            fade_distance: 0.0,
            bounds_min: Vec3::splat(-16.0),
            bounds_max: Vec3::splat(16.0),
        }
    }

    #[test]
    fn a_prop_without_collision_becomes_a_box() {
        let bsp = Bsp::new(51, 1, 30);
        let soup = build(&bsp, &[prop("mdl/a.rmdl", Vec3::ZERO)], &AssetTables::default(), None)
            .expect("the soup builds");
        assert_eq!(soup.triangles(), 12);
        assert_eq!(soup.counts.prop_triangles, 12);
        let (lo, hi) = soup.bounds();
        assert_eq!(lo, Vec3::splat(-16.0));
        assert_eq!(hi, Vec3::splat(16.0));
    }

    #[test]
    fn clipping_keeps_the_soup_inside_the_bounds() {
        let bsp = Bsp::new(51, 1, 30);
        let props = [prop("mdl/a.rmdl", Vec3::new(0.0, 0.0, 0.0)), prop("mdl/b.rmdl", Vec3::new(4096.0, 0.0, 0.0))];
        let bounds = [DVec3::splat(-512.0), DVec3::splat(512.0)];
        let soup = build(&bsp, &props, &AssetTables::default(), Some(bounds)).expect("the soup builds");
        assert_eq!(soup.triangles(), 12, "the far prop is clipped away");
        let (lo, hi) = soup.bounds();
        assert!(lo.cmpge(Vec3::splat(-512.0)).all() && hi.cmple(Vec3::splat(512.0)).all());
    }

    #[test]
    fn inverted_bounds_are_an_error() {
        let bsp = Bsp::new(51, 1, 30);
        let bounds = [DVec3::splat(512.0), DVec3::splat(-512.0)];
        assert!(matches!(
            build(&bsp, &[], &AssetTables::default(), Some(bounds)),
            Err(NavError::BadBounds { .. })
        ));
    }

    #[test]
    fn obj_export_lists_every_vertex_and_face() {
        let bsp = Bsp::new(51, 1, 30);
        let soup = build(&bsp, &[prop("mdl/a.rmdl", Vec3::ZERO)], &AssetTables::default(), None)
            .expect("the soup builds");
        let obj = to_obj(&soup);
        let verts = obj.lines().filter(|l| l.starts_with("v ")).count();
        let faces = obj.lines().filter(|l| l.starts_with("f ")).count();
        assert_eq!(verts, soup.positions.len());
        assert_eq!(faces, soup.triangles());
        assert!(obj.is_ascii());
        let _ = Version::ClientV51;
    }
}
