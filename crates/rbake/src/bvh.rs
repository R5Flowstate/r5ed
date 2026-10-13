//! Binned-SAH bounding volume hierarchy over the triangles the bake traces
//! against, plus the ray queries the direct and indirect lights need.
//!
//! The node and triangle layout is plain `glam` vectors and `u32`s in
//! declaration order, which is also what the compute shader reads out of its
//! storage buffers, so one build serves both backends.

use glam::Vec3;

use crate::error::{Error, Result};

/// One triangle the tracer can hit, in world space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tri {
    pub a: Vec3,
    pub b: Vec3,
    pub c: Vec3,
}

impl Tri {
    pub fn new(a: Vec3, b: Vec3, c: Vec3) -> Self {
        Self { a, b, c }
    }

    pub fn e1(&self) -> Vec3 {
        self.b - self.a
    }

    pub fn e2(&self) -> Vec3 {
        self.c - self.a
    }

    /// Twice the area, the unnormalised cross product.
    pub fn normal(&self) -> Vec3 {
        self.e1().cross(self.e2())
    }

    pub fn centroid(&self) -> Vec3 {
        (self.a + self.b + self.c) / 3.0
    }

    pub fn bounds(&self) -> Bounds {
        let lo = self.a.min(self.b).min(self.c);
        let hi = self.a.max(self.b).max(self.c);
        Bounds { lo, hi }
    }

    /// The midpoint of the longest axis of the triangle's own bounds, the
    /// split candidate the builder falls back to.
    pub fn split_point(&self) -> Vec3 {
        self.bounds().center()
    }

    /// Whether the hierarchy keeps the triangle; a hit indexes the kept ones.
    pub fn is_finite(&self) -> bool {
        self.a.is_finite() && self.b.is_finite() && self.c.is_finite()
    }
}

/// An axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub lo: Vec3,
    pub hi: Vec3,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            lo: Vec3::splat(f32::INFINITY),
            hi: Vec3::splat(f32::NEG_INFINITY),
        }
    }
}

impl Bounds {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        !(self.lo.x <= self.hi.x && self.lo.y <= self.hi.y && self.lo.z <= self.hi.z)
    }

    pub fn from_points(pts: &[Vec3]) -> Self {
        let mut b = Bounds::empty();
        for p in pts {
            b.extend(*p);
        }
        b
    }

    pub fn extend(&mut self, p: Vec3) {
        self.lo = self.lo.min(p);
        self.hi = self.hi.max(p);
    }

    pub fn merge(&mut self, o: &Bounds) {
        if o.is_empty() {
            return;
        }
        self.lo = self.lo.min(o.lo);
        self.hi = self.hi.max(o.hi);
    }

    pub fn center(&self) -> Vec3 {
        (self.lo + self.hi) * 0.5
    }

    pub fn extent(&self) -> Vec3 {
        (self.hi - self.lo).max(Vec3::ZERO)
    }

    /// One component of the box, by axis index.
    pub fn at(&self, axis: usize) -> f32 {
        match axis % 3 {
            0 => self.lo.x,
            1 => self.lo.y,
            _ => self.lo.z,
        }
    }

    pub fn surface(&self) -> f32 {
        let e = self.extent();
        if e.x <= 0.0 || e.y <= 0.0 || e.z <= 0.0 {
            return 0.0;
        }
        2.0 * (e.x * e.y + e.y * e.z + e.z * e.x)
    }

    /// Grows the box by a world-space margin.
    pub fn padded(&self, m: f32) -> Self {
        Self {
            lo: self.lo - Vec3::splat(m),
            hi: self.hi + Vec3::splat(m),
        }
    }
}

/// One BVH node. A non-zero `count` marks a leaf holding that many consecutive
/// triangle indices from `left_or_first`; a zero count marks an inner node
/// whose two children sit at `left_or_first` and `left_or_first + 1`. The node
/// list is laid out breadth first so a node's two children are always adjacent
/// and always come after it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Node {
    pub lo: Vec3,
    pub hi: Vec3,
    pub left_or_first: u32,
    pub count: u32,
}

impl Node {
    pub fn is_leaf(&self) -> bool {
        self.count != 0
    }
}

/// A built hierarchy.
#[derive(Clone, Debug, Default)]
pub struct Bvh {
    pub nodes: Vec<Node>,
    /// Triangle index per leaf slot.
    pub indices: Vec<u32>,
    pub triangles: Vec<Tri>,
    pub root: Bounds,
    /// Per triangle, the chance a ray passes through it; empty when every
    /// triangle is opaque.
    pub pass: Vec<f32>,
}

/// A node before the breadth-first renumber, when its children are two explicit
/// indices rather than a pair of adjacent slots.
#[derive(Clone, Copy, Debug, Default)]
struct RawNode {
    lo: Vec3,
    hi: Vec3,
    first: usize,
    count: u32,
    left: Option<usize>,
    right: Option<usize>,
}

const BINS: usize = 16;
const MAX_LEAF: usize = 4;
const TRAVERSAL_CAP: usize = 64;

impl Bvh {
    /// Builds a hierarchy over `tris`. Non-finite vertices are dropped rather
    /// than poisoning the bounds.
    pub fn build(tris: &[Tri]) -> Result<Self> {
        let mut clean: Vec<Tri> = Vec::with_capacity(tris.len());
        for t in tris {
            if t.is_finite() {
                clean.push(*t);
            }
        }
        let mut bvh = Bvh {
            nodes: Vec::new(),
            indices: Vec::new(),
            triangles: clean,
            root: Bounds::empty(),
            pass: Vec::new(),
        };
        if bvh.triangles.is_empty() {
            bvh.nodes.push(Node { lo: Vec3::ZERO, hi: Vec3::ZERO, left_or_first: 0, count: 0 });
            bvh.root = Bounds { lo: Vec3::ZERO, hi: Vec3::ZERO };
            return Ok(bvh);
        }
        bvh.indices = (0..bvh.triangles.len() as u32).collect();
        bvh.root = Bounds::from_points(
            &bvh.triangles.iter().flat_map(|t| [t.a, t.b, t.c]).collect::<Vec<_>>(),
        );
        let centroid_bounds = Bounds::from_points(&bvh.triangles.iter().map(|t| t.centroid()).collect::<Vec<_>>());
        bvh.build_all(centroid_bounds);
        Ok(bvh)
    }

    /// Builds the hierarchy, then lays the nodes out breadth first so every
    /// node's two children are adjacent.
    fn build_all(&mut self, centroid_bounds: Bounds) {
        let mut raw: Vec<RawNode> = Vec::new();
        self.build_raw(0, self.triangles.len(), centroid_bounds, &mut raw);
        let mut order: Vec<usize> = vec![0];
        let mut at = 0usize;
        while at < order.len() {
            let node = raw[order[at]];
            if let (Some(l), Some(r)) = (node.left, node.right) {
                order.push(l);
                order.push(r);
            }
            at += 1;
        }
        let mut index_of = vec![0usize; raw.len()];
        for (new, old) in order.iter().enumerate() {
            index_of[*old] = new;
        }
        self.nodes.reserve(order.len());
        for old in &order {
            let n = &raw[*old];
            let (first, count) = (n.first, n.count);
            match (n.left, n.right) {
                (Some(l), Some(_r)) => self.nodes.push(Node {
                    lo: n.lo,
                    hi: n.hi,
                    left_or_first: index_of[l] as u32,
                    count: 0,
                }),
                _ => self.nodes.push(Node { lo: n.lo, hi: n.hi, left_or_first: first as u32, count }),
            }
        }
    }

    fn build_raw(&mut self, first: usize, count: usize, centroid_bounds: Bounds, out: &mut Vec<RawNode>) -> usize {
        let index = out.len();
        out.push(RawNode::default());
        let mut bounds = Bounds::empty();
        for i in first..first + count {
            bounds.merge(&self.triangles[self.indices[i] as usize].bounds());
        }
        let leaf = |out: &mut Vec<RawNode>, index: usize, bounds: Bounds| {
            out[index] = RawNode {
                lo: bounds.lo,
                hi: bounds.hi,
                first,
                count: count as u32,
                left: None,
                right: None,
            };
        };
        if count <= MAX_LEAF || centroid_bounds.is_empty() {
            leaf(out, index, bounds);
            return index;
        }
        let extent = centroid_bounds.extent();
        let mut axis = 0usize;
        if extent.y > extent.x {
            axis = 1;
        }
        if extent.z > extent[axis] {
            axis = 2;
        }
        if extent[axis] <= f32::EPSILON {
            leaf(out, index, bounds);
            return index;
        }
        let (mut left_n, mut right_n) = self.partition(first, count, centroid_bounds, axis);
        if left_n == 0 || right_n == 0 {
            left_n = self.partition_midpoint(first, count, axis);
            right_n = count - left_n;
        }
        if left_n == 0 || right_n == 0 {
            leaf(out, index, bounds);
            return index;
        }
        let cb = self.child_centroid_bounds(first, left_n);
        let l = self.build_raw(first, left_n, cb, out);
        let cb = self.child_centroid_bounds(first + left_n, right_n);
        let r = self.build_raw(first + left_n, right_n, cb, out);
        out[index] = RawNode {
            lo: bounds.lo,
            hi: bounds.hi,
            first: 0,
            count: 0,
            left: Some(l),
            right: Some(r),
        };
        index
    }

    fn child_centroid_bounds(&self, first: usize, count: usize) -> Bounds {
        let mut b = Bounds::empty();
        for i in first..first + count {
            b.extend(self.triangles[self.indices[i] as usize].centroid());
        }
        b
    }

    /// Binned SAH split, returned as the two side counts.
    fn partition(&mut self, first: usize, count: usize, cb: Bounds, axis: usize) -> (usize, usize) {
        let k = cb.at(axis) / BINS as f32;
        let mut bin_count = [0u32; BINS];
        let mut bin_bounds: [Bounds; BINS] = [Bounds::empty(); BINS];
        for i in first..first + count {
            let t = &self.triangles[self.indices[i] as usize];
            let b = (t.centroid()[axis] - cb.at(axis)) / k;
            let bin = if b.is_finite() {
                (b as isize).clamp(0, BINS as isize - 1) as usize
            } else {
                0
            };
            bin_count[bin] += 1;
            bin_bounds[bin].merge(&t.bounds());
        }
        let mut left_area = [0.0f32; BINS];
        let mut left_count = [0u32; BINS];
        let mut acc = Bounds::empty();
        let mut n = 0u32;
        for i in 0..BINS {
            acc.merge(&bin_bounds[i]);
            n += bin_count[i];
            left_area[i] = acc.surface();
            left_count[i] = n;
        }
        let mut right_area = [0.0f32; BINS];
        let mut right_count = [0u32; BINS];
        let mut acc = Bounds::empty();
        let mut n = 0u32;
        for i in (0..BINS).rev() {
            acc.merge(&bin_bounds[i]);
            n += bin_count[i];
            right_area[i] = acc.surface();
            right_count[i] = n;
        }
        let mut best = (f32::INFINITY, BINS / 2);
        for i in 0..BINS - 1 {
            if left_count[i] == 0 || right_count[i + 1] == 0 {
                continue;
            }
            let cost = left_area[i] * left_count[i] as f32 + right_area[i + 1] * right_count[i + 1] as f32;
            if cost < best.0 {
                best = (cost, i + 1);
            }
        }
        let pivot = cb.lo[axis] + best.1 as f32 * k;
        let mid = self.stable_partition(first, count, axis, pivot);
        (mid - first, first + count - mid)
    }

    /// Splits at the triangle midpoint when the binned split degenerates, and
    /// returns the size of the lower side.
    /// Splits a run at its median centroid on `axis`. The fallback when the
    /// binned split puts everything on one side, so it has to halve the run:
    /// a split that peels off one triangle at a time makes the tree as deep
    /// as the run is long, which a shipped map's triangle count overflows the
    /// stack with.
    fn partition_midpoint(&mut self, first: usize, count: usize, axis: usize) -> usize {
        self.indices[first..first + count].sort_by(|a, b| {
            let ka = self.triangles[*a as usize].centroid()[axis];
            let kb = self.triangles[*b as usize].centroid()[axis];
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });
        count / 2
    }

    fn stable_partition(&mut self, first: usize, count: usize, axis: usize, pivot: f32) -> usize {
        let mut lo = first;
        let mut hi = first + count;
        while lo < hi {
            let key = self.triangles[self.indices[lo] as usize].centroid()[axis];
            if key < pivot {
                lo += 1;
            } else {
                hi -= 1;
                self.indices.swap(lo, hi);
            }
        }
        lo
    }

    /// Builds over the scene geometry, keeping its partial occluders when no
    /// triangle was dropped (the pass table is indexed like `triangles`).
    pub fn build_scene(g: &SceneGeometry) -> Result<Self> {
        let mut bvh = Self::build(&g.tris)?;
        if g.pass.iter().any(|p| *p > 0.0) && bvh.triangles.len() == g.pass.len() {
            bvh.pass = g.pass.clone();
        }
        Ok(bvh)
    }

    /// Whether `ray` goes through triangle `tri` at a hole: drawn per ray and
    /// triangle, so many rays average to the triangle's coverage.
    fn passes(&self, ray: &Ray, tri: u32) -> bool {
        let Some(&p) = self.pass.get(tri as usize) else { return false };
        if p <= 0.0 {
            return false;
        }
        let mut h = tri.wrapping_mul(0x9E37_79B9);
        for v in [ray.origin.x, ray.origin.y, ray.origin.z, ray.dir.x, ray.dir.y, ray.dir.z] {
            h ^= v.to_bits();
            h = h.wrapping_mul(0x85EB_CA6B);
            h ^= h >> 13;
        }
        h = h.wrapping_mul(0xC2B2_AE35);
        h ^= h >> 16;
        ((h >> 8) as f32 / (1u32 << 24) as f32) < p
    }

    /// The closest hit along `ray` within `t_max`, as (distance, triangle).
    pub fn closest_hit(&self, ray: &Ray) -> Option<Hit> {
        if self.nodes.is_empty() || self.triangles.is_empty() {
            return None;
        }
        let mut best = Hit { t: ray.t_max, tri: 0, front: true };
        let mut found = false;
        let mut stack = [0u32; TRAVERSAL_CAP];
        let mut sp = 0usize;
        stack[sp] = 0;
        sp += 1;
        while sp > 0 {
            sp -= 1;
            let n = stack[sp];
            let Some(node) = self.nodes.get(n as usize).copied() else { continue };
            if !intersect_bounds(ray, node.lo, node.hi) {
                continue;
            }
            if node.is_leaf() {
                for i in 0..node.count as usize {
                    let ti = node.left_or_first as usize + i;
                    let Some(&tri_idx) = self.indices.get(ti) else { continue };
                    if let Some((t, front)) = intersect_tri(ray, &self.triangles[tri_idx as usize]) {
                        if t < best.t && !self.passes(ray, tri_idx) {
                            best = Hit { t, tri: tri_idx, front };
                            found = true;
                        }
                    }
                }
                continue;
            }
            let l = node.left_or_first;
            let r = l + 1;
            if sp + 2 <= TRAVERSAL_CAP {
                stack[sp] = l;
                stack[sp + 1] = r;
                sp += 2;
            } else {
                stack[sp] = l;
                sp += 1;
            }
        }
        if found {
            Some(best)
        } else {
            None
        }
    }

    /// True when anything blocks the ray before `t_max`.
    pub fn occluded(&self, ray: &Ray) -> bool {
        if self.nodes.is_empty() || self.triangles.is_empty() {
            return false;
        }
        let mut stack = [0u32; TRAVERSAL_CAP];
        let mut sp = 0usize;
        stack[sp] = 0;
        sp += 1;
        while sp > 0 {
            sp -= 1;
            let n = stack[sp];
            let Some(node) = self.nodes.get(n as usize).copied() else { continue };
            if !intersect_bounds(ray, node.lo, node.hi) {
                continue;
            }
            if node.is_leaf() {
                for i in 0..node.count as usize {
                    let ti = node.left_or_first as usize + i;
                    let Some(&tri_idx) = self.indices.get(ti) else { continue };
                    if intersect_tri(ray, &self.triangles[tri_idx as usize]).is_some() && !self.passes(ray, tri_idx) {
                        return true;
                    }
                }
                continue;
            }
            let l = node.left_or_first;
            if sp + 2 <= TRAVERSAL_CAP {
                stack[sp] = l;
                stack[sp + 1] = l + 1;
                sp += 2;
            } else {
                stack[sp] = l;
                sp += 1;
            }
        }
        false
    }

    /// True when the point sits inside solid geometry: most of the faces it
    /// sees, looking every way, are seen from behind. A ray parity count read
    /// the inside of every closed hollow building as solid, and a mesh map's
    /// halls are exactly that.
    pub fn inside_solid(&self, p: Vec3) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        const D: f32 = 0.577_350_3;
        let dirs = [
            Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z,
            Vec3::new(D, D, D), Vec3::new(-D, D, D), Vec3::new(D, -D, D), Vec3::new(-D, -D, D),
            Vec3::new(D, D, -D), Vec3::new(-D, D, -D), Vec3::new(D, -D, -D), Vec3::new(-D, -D, -D),
        ];
        let (mut back, mut front) = (0u32, 0u32);
        for d in dirs {
            let ray = Ray::new(p, d, 0.01, 1.0e9);
            let Some(hit) = self.closest_hit(&ray) else { continue };
            let t = &self.triangles[hit.tri as usize];
            // The engine fronts clockwise faces: (c - a) x (b - a).
            if d.dot((t.c - t.a).cross(t.b - t.a)) > 0.0 {
                back += 1;
            } else {
                front += 1;
            }
        }
        back > front
    }
}

/// A ray in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    pub origin: Vec3,
    /// Not normalised: `t` is in units of `dir`.
    pub dir: Vec3,
    pub t_min: f32,
    pub t_max: f32,
}

impl Ray {
    pub fn new(origin: Vec3, dir: Vec3, t_min: f32, t_max: f32) -> Self {
        Self { origin, dir, t_min, t_max }
    }

    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }
}

/// A closest hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub t: f32,
    /// Index into the triangle list the hierarchy was built from.
    pub tri: u32,
    pub front: bool,
}

impl Hit {
    pub fn point(&self, ray: &Ray) -> Vec3 {
        ray.at(self.t)
    }

    /// The surface normal at the hit, always turned against the incoming ray.
    pub fn normal(&self, ray: &Ray, tris: &[Tri]) -> Vec3 {
        let n = tris.get(self.tri as usize).map(|t| t.normal()).unwrap_or(Vec3::ZERO);
        let len = n.length();
        if len <= 0.0 {
            return Vec3::Z;
        }
        let n = n / len;
        if n.dot(ray.dir) > 0.0 {
            -n
        } else {
            n
        }
    }
}

fn intersect_bounds(ray: &Ray, lo: Vec3, hi: Vec3) -> bool {
    let mut t0 = ray.t_min;
    let mut t1 = ray.t_max;
    for axis in 0..3 {
        let o = ray.origin[axis];
        let d = ray.dir[axis];
        if d.abs() < 1.0e-20 {
            if o < lo[axis] || o > hi[axis] {
                return false;
            }
            continue;
        }
        let inv = 1.0 / d;
        let mut a = (lo[axis] - o) * inv;
        let mut b = (hi[axis] - o) * inv;
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        t0 = t0.max(a);
        t1 = t1.min(b);
        if t0 > t1 {
            return false;
        }
    }
    true
}

/// Moller-Trumbore, returning the distance and whether the ray entered the
/// front face.
fn intersect_tri(ray: &Ray, t: &Tri) -> Option<(f32, bool)> {
    let e1 = t.e1();
    let e2 = t.e2();
    let p = ray.dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1.0e-12 {
        return None;
    }
    let inv_det = 1.0 / det;
    let s = ray.origin - t.a;
    let u = s.dot(p) * inv_det;
    if !(-1.0e-7..=1.0 + 1.0e-7).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = ray.dir.dot(q) * inv_det;
    if v < -1.0e-7 || u + v > 1.0 + 1.0e-7 {
        return None;
    }
    let dist = e2.dot(q) * inv_det;
    if !dist.is_finite() || dist < ray.t_min || dist > ray.t_max {
        return None;
    }
    Some((dist, det > 0.0))
}

/// The world-space triangles of a scene's surfaces and prop occluders, with
/// per-triangle flags the direct pass needs.
#[derive(Clone, Debug, Default)]
pub struct SceneGeometry {
    pub tris: Vec<Tri>,
    /// 0 opaque, 1 translucent, 2 a prop occluder, 3 a shadow-only face.
    pub kinds: Vec<u8>,
    /// Index into `scenes`'s surfaces per triangle, `u32::MAX` for a prop.
    pub surface: Vec<u32>,
    /// Index into the scene's prop occluders per triangle.
    pub prop: Vec<u32>,
    /// Chance a ray passes each triangle (0 = opaque).
    pub pass: Vec<f32>,
}

/// Builds the tracer geometry of a scene.
pub fn scene_geometry(scene: &rir::BakeScene) -> Result<SceneGeometry> {
    let mut out = SceneGeometry::default();
    let clear: std::collections::HashSet<u32> = scene.see_through.iter().copied().collect();
    let partial: std::collections::HashMap<u32, f32> = scene.shadow_transmission.iter().copied().collect();
    for (si, s) in scene.surfaces.iter().enumerate() {
        let pass = partial.get(&(si as u32)).copied().unwrap_or(0.0);
        if clear.contains(&(si as u32)) {
            continue;
        }
        if s.positions.len() != s.normals.len() {
            return Err(Error::scene(format!(
                "surface {si} has {} positions and {} normals",
                s.positions.len(),
                s.normals.len()
            )));
        }
        if s.indices.len() % 3 != 0 {
            return Err(Error::scene(format!("surface {si} has {} indices, not a multiple of 3", s.indices.len())));
        }
        for c in s.indices.chunks_exact(3) {
            let (Some(&a), Some(&b), Some(&d)) = (
                s.positions.get(c[0] as usize),
                s.positions.get(c[1] as usize),
                s.positions.get(c[2] as usize),
            ) else {
                return Err(Error::scene(format!("surface {si} names a vertex it does not have")));
            };
            out.tris.push(Tri::new(a, b, d));
            out.kinds.push(0);
            out.surface.push(si as u32);
            out.prop.push(u32::MAX);
            out.pass.push(pass);
        }
    }
    for (pi, (_, positions, indices)) in scene.prop_occluders.iter().enumerate() {
        if indices.len() % 3 != 0 {
            return Err(Error::scene(format!("prop occluder {pi} has {} indices, not a multiple of 3", indices.len())));
        }
        for c in indices.chunks_exact(3) {
            let (Some(&a), Some(&b), Some(&d)) = (
                positions.get(c[0] as usize),
                positions.get(c[1] as usize),
                positions.get(c[2] as usize),
            ) else {
                return Err(Error::scene(format!("prop occluder {pi} names a vertex it does not have")));
            };
            out.tris.push(Tri::new(a, b, d));
            out.kinds.push(2);
            out.surface.push(u32::MAX);
            out.prop.push(pi as u32);
            out.pass.push(0.0);
        }
    }
    for (bi, (positions, indices)) in scene.shadow_only.iter().enumerate() {
        if indices.len() % 3 != 0 {
            return Err(Error::scene(format!("shadow-only batch {bi} has {} indices, not a multiple of 3", indices.len())));
        }
        for c in indices.chunks_exact(3) {
            let (Some(&a), Some(&b), Some(&d)) = (
                positions.get(c[0] as usize),
                positions.get(c[1] as usize),
                positions.get(c[2] as usize),
            ) else {
                return Err(Error::scene(format!("shadow-only batch {bi} names a vertex it does not have")));
            };
            out.tris.push(Tri::new(a, b, d));
            out.kinds.push(3);
            out.surface.push(u32::MAX);
            out.prop.push(u32::MAX);
            out.pass.push(0.0);
        }
    }
    Ok(out)
}

/// A broad-phase box of the tracer geometry, for the compute backend's
/// culling of a whole batch.
pub fn geometry_bounds(g: &SceneGeometry) -> Bounds {
    let mut b = Bounds::empty();
    for t in &g.tris {
        b.extend(t.a);
        b.extend(t.b);
        b.extend(t.c);
    }
    if b.is_empty() {
        Bounds { lo: Vec3::ZERO, hi: Vec3::ZERO }
    } else {
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slab(pass: f32) -> Bvh {
        let g = SceneGeometry {
            tris: vec![
                Tri::new(Vec3::new(-100.0, -100.0, 10.0), Vec3::new(100.0, -100.0, 10.0), Vec3::new(100.0, 100.0, 10.0)),
                Tri::new(Vec3::new(-100.0, -100.0, 10.0), Vec3::new(100.0, 100.0, 10.0), Vec3::new(-100.0, 100.0, 10.0)),
            ],
            kinds: vec![0, 0],
            surface: vec![0, 0],
            prop: vec![u32::MAX, u32::MAX],
            pass: vec![pass, pass],
        };
        Bvh::build_scene(&g).unwrap()
    }

    fn blocked_share(bvh: &Bvh) -> f32 {
        let mut blocked = 0;
        let n = 4000;
        for i in 0..n {
            let x = (i % 63) as f32 - 31.0;
            let y = (i / 63) as f32 - 31.0;
            let dir = Vec3::new(0.01 * (i % 7) as f32, 0.01 * (i % 5) as f32, 1.0).normalize();
            if bvh.occluded(&Ray::new(Vec3::new(x, y, 0.0), dir, 0.01, 100.0)) {
                blocked += 1;
            }
        }
        blocked as f32 / n as f32
    }

    #[test]
    fn a_partial_occluder_blocks_its_coverage() {
        assert_eq!(blocked_share(&slab(0.0)), 1.0, "an opaque slab blocks every ray");
        assert_eq!(blocked_share(&slab(1.0)), 0.0, "a fully open slab blocks none");
        let half = blocked_share(&slab(0.6));
        assert!((half - 0.4).abs() < 0.05, "a slab passing 60% blocks about 40%, got {half}");
        let first = slab(0.6);
        let ray = Ray::new(Vec3::new(3.0, 4.0, 0.0), Vec3::Z, 0.01, 100.0);
        assert_eq!(first.occluded(&ray), first.occluded(&ray), "the same ray always gets the same answer");
        let hit = first.closest_hit(&ray).is_some();
        assert_eq!(hit, first.occluded(&ray), "closest_hit and occluded agree on a hole");
    }
}
