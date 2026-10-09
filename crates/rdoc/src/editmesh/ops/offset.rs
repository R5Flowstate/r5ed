//! Planar polygon offsetting shared by inset (M5) and bevel (M6): each edge
//! of a loop moves inward in its own plane, so the rim keeps constant width.

use glam::DVec2;

/// What one offset produced: the moved loop, whether the ask was clamped,
/// and how much offset survived clamping.
#[derive(Clone, Debug, PartialEq)]
pub struct OffsetOutcome {
    pub points: Vec<DVec2>,
    pub clamped: bool,
    pub applied: f64,
}

fn signed_area(pts: &[DVec2]) -> f64 {
    (0..pts.len()).map(|k| pts[k].perp_dot(pts[(k + 1) % pts.len()])).sum::<f64>() * 0.5
}

fn line_dist(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let d = b - a;
    let len = d.length();
    if len < 1e-12 {
        return f64::INFINITY;
    }
    (d.perp_dot(p - a)).abs() / len
}

/// Moves a loop inward (negative `amount` moves it outward). `even_offset`
/// intersects the moved edge lines, so the rim width is exactly `amount`
/// everywhere; without it every vertex travels `amount` along its bisector.
/// Returns `None` for a degenerate loop or an offset that collapses to nothing.
pub fn offset_polygon(points: &[DVec2], amount: f64, even_offset: bool) -> Option<OffsetOutcome> {
    if points.len() < 3 || !amount.is_finite() {
        return None;
    }
    let area = signed_area(points);
    if !area.is_finite() || area.abs() < 1e-12 {
        return None;
    }
    if amount.abs() < 1e-12 {
        return Some(OffsetOutcome { points: points.to_vec(), clamped: false, applied: 0.0 });
    }
    let n = points.len();
    let inward = area.signum();
    let mut lefts = Vec::with_capacity(n);
    for k in 0..n {
        let d = points[(k + 1) % n] - points[k];
        let len = d.length();
        if !len.is_finite() || len <= 1e-12 {
            return None;
        }
        let d = d / len;
        lefts.push(DVec2::new(-d.y, d.x) * inward);
    }
    let mut bound = f64::INFINITY;
    for i in 0..n {
        for k in 0..n {
            if k == i || (k + 1) % n == i || k == (i + 1) % n {
                continue;
            }
            bound = bound.min(line_dist(points[i], points[k], points[(k + 1) % n]));
        }
    }
    let mut ask = amount;
    let mut clamped = false;
    if ask.abs() > bound {
        ask = bound.copysign(ask);
        clamped = true;
    }
    let mut applied = ask;
    for _ in 0..64 {
        if let Some(pts) = at(points, &lefts, applied, even_offset) {
            let kept = signed_area(&pts);
            if kept.is_finite() && kept * area > 0.0 && kept.abs() > 1e-12 {
                return Some(OffsetOutcome { points: pts, clamped: clamped || applied != amount, applied });
            }
        }
        clamped = true;
        applied *= 0.5;
        if applied.abs() < 1e-9 {
            return None;
        }
    }
    None
}

fn at(points: &[DVec2], lefts: &[DVec2], amount: f64, even: bool) -> Option<Vec<DVec2>> {
    let n = points.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = (i + n - 1) % n;
        let (lp, ln) = (lefts[prev], lefts[i]);
        let step = if even {
            let denom = 1.0 + lp.dot(ln);
            if denom.abs() < 1e-9 {
                return None;
            }
            (lp + ln) * (amount / denom)
        } else {
            let b = lp + ln;
            let len = b.length();
            if len < 1e-9 {
                lp * amount
            } else {
                b * (amount / len)
            }
        };
        let q = points[i] + step;
        if !q.is_finite() {
            return None;
        }
        out.push(q);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Vec<DVec2> {
        vec![DVec2::new(0.0, 0.0), DVec2::new(64.0, 0.0), DVec2::new(64.0, 64.0), DVec2::new(0.0, 64.0)]
    }

    fn rim_width(loop_pts: &[DVec2], inset: &[DVec2]) -> Vec<f64> {
        inset
            .iter()
            .map(|q| {
                (0..loop_pts.len())
                    .map(|k| line_dist(*q, loop_pts[k], loop_pts[(k + 1) % loop_pts.len()]))
                    .fold(f64::INFINITY, f64::min)
            })
            .collect()
    }

    #[test]
    fn even_inset_is_exact_everywhere() {
        let out = offset_polygon(&square(), 8.0, true).unwrap();
        assert!(!out.clamped);
        assert_eq!(out.applied, 8.0);
        assert_eq!(out.points, vec![DVec2::new(8.0, 8.0), DVec2::new(56.0, 8.0), DVec2::new(56.0, 56.0), DVec2::new(8.0, 56.0)]);
        for w in rim_width(&square(), &out.points) {
            assert!((w - 8.0).abs() < 1e-9, "rim width {w}");
        }
    }

    #[test]
    fn uneven_inset_travels_the_bisector() {
        let out = offset_polygon(&square(), 8.0, false).unwrap();
        let d = 8.0 / 2.0f64.sqrt();
        assert!((out.points[0].x - d).abs() < 1e-9 && (out.points[0].y - d).abs() < 1e-9);
    }

    #[test]
    fn triangle_keeps_constant_width() {
        let tri = vec![DVec2::new(0.0, 0.0), DVec2::new(64.0, 0.0), DVec2::new(32.0, 55.0)];
        let out = offset_polygon(&tri, 4.0, true).unwrap();
        assert!(!out.clamped);
        for w in rim_width(&tri, &out.points) {
            assert!((w - 4.0).abs() < 1e-6, "rim width {w}");
        }
    }

    #[test]
    fn outset_expands() {
        let out = offset_polygon(&square(), -8.0, true).unwrap();
        assert_eq!(out.points[0], DVec2::new(-8.0, -8.0));
        assert_eq!(out.points[2], DVec2::new(72.0, 72.0));
    }

    #[test]
    fn over_thick_clamps_instead_of_inverting() {
        let out = offset_polygon(&square(), 100.0, true).unwrap();
        assert!(out.clamped);
        assert!(out.applied < 100.0 && out.applied > 0.0);
        assert!(signed_area(&out.points) > 0.0);
    }

    #[test]
    fn clockwise_input_insets_the_same() {
        let mut cw = square();
        cw.reverse();
        let out = offset_polygon(&cw, 8.0, true).unwrap();
        assert_eq!(out.points, vec![DVec2::new(8.0, 56.0), DVec2::new(56.0, 56.0), DVec2::new(56.0, 8.0), DVec2::new(8.0, 8.0)]);
    }

    #[test]
    fn degenerate_input_is_none() {
        assert!(offset_polygon(&square()[..2], 8.0, true).is_none());
        let flat = vec![DVec2::new(0.0, 0.0), DVec2::new(1.0, 0.0), DVec2::new(2.0, 0.0)];
        assert!(offset_polygon(&flat, 8.0, true).is_none());
        assert!(offset_polygon(&square(), f64::NAN, true).is_none());
    }
}
