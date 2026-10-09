//! Heightfield filters: the shape operators that generators and sculpt tools
//! share. All of them work in place on a borrowed grid.

use crate::error::{Error, Result};
use crate::heightfield::Heightfield;
use crate::math;
use crate::parallel;

/// How a brush combines its value with the surface underneath.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    /// Take the brush value outright.
    Replace,
    /// Add the brush value.
    #[default]
    Add,
    /// Subtract the brush value.
    Subtract,
    /// Multiply by the brush value.
    Multiply,
    /// `1 - (1 - a) * (1 - b)`, the screen blend.
    Screen,
    /// Multiply by the brush value where the brush is below half, and screen
    /// it where it is above.
    Overlay,
    /// Keep whichever value is larger.
    Max,
    /// Keep whichever value is smaller.
    Min,
    /// Interpolate by the brush value.
    Mix,
}

/// Apply one blend of `brush` onto `dst`.
#[inline]
pub fn blend(dst: f32, brush: f32, mode: BlendMode) -> f32 {
    match mode {
        BlendMode::Replace => brush,
        BlendMode::Add => dst + brush,
        BlendMode::Subtract => dst - brush,
        BlendMode::Multiply => dst * brush,
        BlendMode::Screen => 1.0 - (1.0 - dst) * (1.0 - brush),
        BlendMode::Overlay => {
            if brush < 0.5 {
                2.0 * dst * brush
            } else {
                1.0 - 2.0 * (1.0 - dst) * (1.0 - brush)
            }
        }
        BlendMode::Max => dst.max(brush),
        BlendMode::Min => dst.min(brush),
        BlendMode::Mix => dst + (brush - dst) * 0.5,
    }
}

/// Box blur repeated `passes` times, which converges towards a Gaussian.
pub fn smooth(hf: &mut Heightfield, radius: u32, passes: u32) {
    if radius == 0 || passes == 0 || hf.data.is_empty() {
        return;
    }
    let w = hf.width as i64;
    let h = hf.height as i64;
    let r = radius as i64;
    let mut cur = hf.data.clone();
    let mut tmp = vec![0.0f32; cur.len()];
    for _ in 0..passes.max(1) {
        for y in 0..h {
            for x in 0..w {
                let (mut sum, mut n) = (0.0f32, 0i64);
                for d in -r..=r {
                    let sx = x + d;
                    if sx < 0 || sx >= w {
                        continue;
                    }
                    sum += cur[y as usize * w as usize + sx as usize];
                    n += 1;
                }
                tmp[y as usize * w as usize + x as usize] = sum / n as f32;
            }
        }
        for y in 0..h {
            for x in 0..w {
                let (mut sum, mut n) = (0.0f32, 0i64);
                for d in -r..=r {
                    let sy = y + d;
                    if sy < 0 || sy >= h {
                        continue;
                    }
                    sum += tmp[sy as usize * w as usize + x as usize];
                    n += 1;
                }
                cur[y as usize * w as usize + x as usize] = sum / n as f32;
            }
        }
    }
    hf.data = cur;
}

/// Unsharp mask: subtract a blurred copy to bring back fine detail.
pub fn sharpen(hf: &mut Heightfield, amount: f32, radius: u32) {
    if amount == 0.0 || radius == 0 {
        return;
    }
    let mut blurred = hf.clone();
    smooth(&mut blurred, radius, 1);
    for (i, v) in hf.data.iter_mut().enumerate() {
        *v += amount * (*v - blurred.data[i]);
    }
}

/// Clamp every sample into `lo..=hi`.
pub fn clamp(hf: &mut Heightfield, lo: f32, hi: f32) -> Result<()> {
    if !lo.is_finite() || !hi.is_finite() || hi < lo {
        return Err(Error::BadRange(lo, hi));
    }
    for v in hf.data.iter_mut() {
        *v = math::clamp(*v as f64, lo as f64, hi as f64) as f32;
    }
    Ok(())
}

/// Remap heights through a piecewise curve, keeping the ends in place.
///
/// Both axes of `points` are in 0..1 over the field's own range, so the identity
/// curve returns the field unchanged and any curve is read as a shape rather
/// than as absolute heights. The segments are straight lines, so a two point
/// curve is exactly the identity. `points` must be strictly increasing in x with
/// at least two entries.
pub fn curve_remap(hf: &mut Heightfield, points: &[(f32, f32)]) -> Result<()> {
    if points.len() < 2 {
        return Err(Error::BadParams("a curve needs at least two points"));
    }
    for w in points.windows(2) {
        if w[1].0 <= w[0].0 {
            return Err(Error::BadParams("curve x values must strictly increase"));
        }
    }
    let (lo, hi) = hf.range();
    if hi <= lo {
        return Ok(());
    }
    let span = hi - lo;
    for v in hf.data.iter_mut() {
        let t = ((*v - lo) / span).clamp(0.0, 1.0);
        // Find the segment containing t and interpolate smoothly inside it.
        let mut i = 0usize;
        while i + 2 < points.len() && t > points[i + 1].0 {
            i += 1;
        }
        let (x0, y0) = points[i];
        let (x1, y1) = points[i + 1];
        let local = if x1 > x0 { math::saturate(f64::from((t - x0) / (x1 - x0))) as f32 } else { 0.0 };
        let shaped = y0 + (y1 - y0) * local;
        *v = lo + math::saturate(f64::from(shaped)) as f32 * span;
    }
    Ok(())
}

/// Quantise heights into flat steps separated by a steep riser, which is what
/// gives farmland and mesas their bench structure.
pub fn terrace(hf: &mut Heightfield, step: f32, sharpness: f32) -> Result<()> {
    if !step.is_finite() || step <= 0.0 {
        return Err(Error::BadParams("terrace step must be positive"));
    }
    let s = math::clamp(sharpness as f64, 0.0, 1.0);
    for v in hf.data.iter_mut() {
        let t = *v / step;
        let base = t.floor();
        let frac = t - base;
        // A sharp terrace holds its level then jumps at the top of the step; a
        // soft one rounds the riser.
        let shaped = if s >= 1.0 {
            if frac < 1.0 - 1e-4 {
                0.0
            } else {
                1.0
            }
        } else {
            math::smootherstep(frac as f64) as f32
        };
        *v = (base + shaped) * step;
    }
    Ok(())
}

/// A named tonal curve, applied relative to the current sample range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToneCurve {
    /// Raise low values, leaving the top alone.
    Lift,
    /// Push mid values apart, increasing contrast.
    Contrast,
    /// Compress towards the mean, reducing contrast.
    Flatten,
    /// Swap light and dark.
    Invert,
}

impl ToneCurve {
    /// Map a normalised `0..1` value through this curve.
    pub fn apply(&self, t: f32) -> f32 {
        let x = math::saturate(t as f64) as f32;
        match self {
            ToneCurve::Lift => x * x,
            ToneCurve::Contrast => {
                if x < 0.5 {
                    0.5 * (2.0 * x) * (2.0 * x)
                } else {
                    1.0 - 0.5 * (2.0 * (1.0 - x)) * (2.0 * (1.0 - x))
                }
            }
            ToneCurve::Flatten => 0.5 + (x - 0.5) * 0.5,
            ToneCurve::Invert => 1.0 - x,
        }
    }
}

/// Apply a named curve across the current sample range.
pub fn apply_curve(hf: &mut Heightfield, curve: ToneCurve) -> Result<()> {
    let (lo, hi) = hf.range();
    if hi <= lo {
        return Ok(());
    }
    let span = hi - lo;
    for v in hf.data.iter_mut() {
        let t = (*v - lo) / span;
        *v = lo + curve.apply(t) * span;
    }
    Ok(())
}

/// Stamp a brush heightfield onto `dst` at a sample position.
///
/// `strength` scales the brush, `falloff` fades its edges, and the blend mode
/// decides how it meets the surface. Returns the rect that changed.
#[allow(clippy::too_many_arguments)]
pub fn stamp(
    dst: &mut Heightfield,
    brush: &Heightfield,
    at_x: i64,
    at_y: i64,
    strength: f32,
    rotation_rad: f32,
    falloff: f32,
    mode: BlendMode,
) -> crate::heightfield::Rect {
    let rad = math::clamp(falloff as f64, 0.0, 1.0) as f32;
    let (sin, cos) = (math::sin(rotation_rad as f64) as f32, math::cos(rotation_rad as f64) as f32);
    // Stamp the brush's bounding box expanded by a cell for the rotation.
    let pad = 1;
    let x0 = (at_x - brush.width as i64 - pad).max(0) as u32;
    let y0 = (at_y - brush.height as i64 - pad).max(0) as u32;
    let x1 = ((at_x + brush.width as i64 + pad) as u64).min(dst.width as u64) as u32;
    let y1 = ((at_y + brush.height as i64 + pad) as u64).min(dst.height as u64) as u32;
    if x0 >= x1 || y0 >= y1 {
        return crate::heightfield::Rect::new(x0, y0, x0, y0);
    }

    let cx = (brush.width as f32 - 1.0) * 0.5;
    let cy = (brush.height as f32 - 1.0) * 0.5;
    let radius = (cx * cx + cy * cy).max(1.0).sqrt();

    for y in y0..y1 {
        for x in x0..x1 {
            // Rotate the destination sample into brush space about its centre.
            let dx = (x as f32 - at_x as f32) - cx;
            let dy = (y as f32 - at_y as f32) - cy;
            let rx = (dx * cos + dy * sin) + cx;
            let ry = (-dx * sin + dy * cos) + cy;
            if rx < 0.0 || ry < 0.0 || rx > brush.width as f32 - 1.0 || ry > brush.height as f32 - 1.0 {
                continue;
            }
            let b = brush.sample_bilinear(rx, ry) * strength;
            // Fade to zero at the brush's circular edge for a soft footprint.
            let d2 = (rx - cx) * (rx - cx) + (ry - cy) * (ry - cy);
            let dist = math::sqrt(d2 as f64) as f32 / radius;
            let fade = 1.0 - math::smooth_range(dist as f64, rad as f64, 1.0) as f32;
            let i = dst.index(x, y);
            dst.data[i] = blend(dst.data[i], b, mode) * fade + dst.data[i] * (1.0 - fade);
        }
    }
    crate::heightfield::Rect::new(x0, y0, x1, y1)
}

/// Scale heights about `base` by `factor`, which raises and flattens together.
pub fn scale_about(hf: &mut Heightfield, base: f32, factor: f32) {
    parallel::for_each_mut(&mut hf.data, |v| {
        *v = base + (*v - base) * factor;
    });
}

/// Add a constant, then clamp, the usual two-step height adjustment.
pub fn offset_clamped(hf: &mut Heightfield, delta: f32, lo: f32, hi: f32) -> Result<()> {
    hf.offset(delta);
    clamp(hf, lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heightfield::Rect;

    fn spike(w: u32, h: u32) -> Heightfield {
        Heightfield::from_fn(w, h, 8.0, |x, y| {
            let d = (x as f32 - (w as f32 - 1.0) * 0.5).abs() + (y as f32 - (h as f32 - 1.0) * 0.5).abs();
            100.0 + (25.0 - d).max(0.0) * 10.0
        })
        .unwrap()
    }

    fn rough(g: &Heightfield) -> f64 {
        let mut sum = 0.0f64;
        for y in 1..g.height - 1 {
            for x in 1..g.width - 1 {
                sum += (g.get(x + 1, y).unwrap() - g.get(x, y).unwrap()).abs() as f64;
            }
        }
        sum
    }

    #[test]
    fn every_blend_mode_computes_its_own_formula() {
        assert_eq!(blend(10.0, 4.0, BlendMode::Replace), 4.0);
        assert_eq!(blend(10.0, 4.0, BlendMode::Add), 14.0);
        assert_eq!(blend(10.0, 4.0, BlendMode::Subtract), 6.0);
        assert_eq!(blend(10.0, 4.0, BlendMode::Multiply), 40.0);
        assert!((blend(0.5, 0.5, BlendMode::Screen) - 0.75).abs() < 1e-6);
        // Overlay splits on the brush value: below half it multiplies.
        assert!((blend(0.5, 0.25, BlendMode::Overlay) - 0.25).abs() < 1e-6);
        assert!(blend(0.5, 0.75, BlendMode::Overlay) > 0.5);
        assert_eq!(blend(10.0, 4.0, BlendMode::Max), 10.0);
        assert_eq!(blend(10.0, 40.0, BlendMode::Min), 10.0);
        assert_eq!(blend(10.0, 20.0, BlendMode::Mix), 15.0);
        assert_eq!(BlendMode::default(), BlendMode::Add);
    }

    #[test]
    fn smooth_flattens_and_a_zero_pass_is_a_no_op() {
        let mut hf = spike(24, 24);
        let before = rough(&hf);
        smooth(&mut hf, 2, 3);
        assert!(rough(&hf) < before, "smoothing did not reduce the roughness");
        assert!(hf.is_finite());
        let unchanged = hf.clone();
        smooth(&mut hf, 0, 3);
        smooth(&mut hf, 2, 0);
        assert_eq!(hf, unchanged);
    }

    #[test]
    fn sharpen_undoes_a_blur() {
        let mut hf = spike(24, 24);
        let original = hf.clone();
        smooth(&mut hf, 2, 2);
        let blurred = hf.clone();
        sharpen(&mut hf, 1.0, 2);
        // An unsharp mask of a spike puts the peak back above the blur.
        assert!(hf.get(12, 12).unwrap() > blurred.get(12, 12).unwrap());
        assert!(rough(&hf) > rough(&blurred));
        // A zero amount or radius changes nothing.
        let mut again = blurred.clone();
        sharpen(&mut again, 0.0, 2);
        sharpen(&mut again, 1.0, 0);
        assert_eq!(again, blurred);
        assert_ne!(original.data, blurred.data);
    }

    #[test]
    fn clamp_bounds_the_field() {
        let mut hf = spike(8, 8);
        clamp(&mut hf, 0.0, 120.0).unwrap();
        assert!(hf.max() <= 120.0 && hf.min() >= 0.0);
        assert!(matches!(clamp(&mut hf, 1.0, 0.0), Err(Error::BadRange(_, _))));
        assert!(matches!(clamp(&mut hf, f32::NAN, 1.0), Err(Error::BadRange(_, _))));
        // A clamp that changes nothing still succeeds.
        let before = hf.clone();
        clamp(&mut hf, 0.0, 120.0).unwrap();
        assert_eq!(hf, before);
    }

    #[test]
    fn curve_remap_keeps_the_ends_and_rejects_bad_points() {
        let mut hf = Heightfield::from_fn(5, 1, 1.0, |x, _| x as f32 * 25.0).unwrap();
        // The identity curve leaves the field alone.
        let before = hf.clone();
        curve_remap(&mut hf, &[(0.0, 0.0), (1.0, 1.0)]).unwrap();
        for (a, b) in hf.data.iter().zip(before.data.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
        // A curve that only holds the low half pushes the top up to it.
        let mut flat_top = hf.clone();
        curve_remap(&mut flat_top, &[(0.0, 0.0), (0.5, 0.5), (1.0, 0.5)]).unwrap();
        assert!((flat_top.get(4, 0).unwrap() - 50.0).abs() < 1.0, "{}", flat_top.get(4, 0).unwrap());
        assert!(matches!(curve_remap(&mut hf, &[(0.0, 0.0)]), Err(Error::BadParams(_))));
        assert!(matches!(
            curve_remap(&mut hf, &[(0.0, 0.0), (0.0, 1.0)]),
            Err(Error::BadParams(_))
        ));
        // A flat field has no range to map, so the curve does nothing.
        let mut flat = Heightfield::filled(4, 4, 1.0, 7.0).unwrap();
        let keep = flat.clone();
        curve_remap(&mut flat, &[(0.0, 1.0), (1.0, 0.0)]).unwrap();
        assert_eq!(flat, keep);
    }

    #[test]
    fn terrace_quantises_into_benches() {
        let mut hf = Heightfield::from_fn(9, 1, 1.0, |x, _| x as f32 * 10.0).unwrap();
        terrace(&mut hf, 25.0, 1.0).unwrap();
        for v in &hf.data {
            let level = (*v / 25.0).round();
            assert!((*v / 25.0 - level).abs() < 1e-3, "{v} is off the bench grid");
        }
        // A hard bench is flat: a whole run of samples shares one value.
        assert_eq!(hf.get(0, 0).unwrap(), hf.get(1, 0).unwrap());
        // A soft terrace keeps the ramp between the benches, so its samples are
        // off the grid and the peak stays within a step of the original.
        let mut soft = Heightfield::from_fn(9, 1, 1.0, |x, _| x as f32 * 10.0).unwrap();
        terrace(&mut soft, 25.0, 0.0).unwrap();
        let off = soft.data.iter().filter(|v| (*v / 25.0 - (*v / 25.0).round()).abs() > 1e-3).count();
        assert!(off > 4, "the soft terrace is as sharp as the hard one");
        assert!(80.0 - soft.max() < 25.0, "the soft terrace lost the top step");
        assert!(matches!(terrace(&mut soft, 0.0, 1.0), Err(Error::BadParams(_))));
        assert!(matches!(terrace(&mut soft, f32::NAN, 1.0), Err(Error::BadParams(_))));
    }

    #[test]
    fn tone_curves_map_the_unit_range() {
        assert_eq!(ToneCurve::Lift.apply(0.0), 0.0);
        assert_eq!(ToneCurve::Lift.apply(1.0), 1.0);
        assert!(ToneCurve::Lift.apply(0.5) < 0.5);
        assert!(ToneCurve::Contrast.apply(0.25) < 0.25);
        assert!(ToneCurve::Contrast.apply(0.75) > 0.75);
        assert!((ToneCurve::Flatten.apply(0.5) - 0.5).abs() < 1e-6);
        assert!((ToneCurve::Flatten.apply(1.0) - 0.75).abs() < 1e-6);
        assert!((ToneCurve::Invert.apply(0.25) - 0.75).abs() < 1e-6);
        // Out of range values clamp instead of wrapping.
        assert_eq!(ToneCurve::Invert.apply(-4.0), 1.0);
        assert_eq!(ToneCurve::Invert.apply(9.0), 0.0);

        let mut hf = Heightfield::from_fn(5, 1, 1.0, |x, _| x as f32 * 25.0).unwrap();
        apply_curve(&mut hf, ToneCurve::Invert).unwrap();
        assert!((hf.get(0, 0).unwrap() - 100.0).abs() < 1e-3, "invert did not flip the range");
        let mut flat = Heightfield::filled(4, 4, 1.0, 3.0).unwrap();
        let keep = flat.clone();
        apply_curve(&mut flat, ToneCurve::Contrast).unwrap();
        assert_eq!(flat, keep);
    }

    #[test]
    fn stamp_writes_a_brush_and_reports_its_rect() {
        let brush = Heightfield::from_fn(5, 5, 8.0, |x, y| {
            if x + y < 5 {
                30.0
            } else {
                0.0
            }
        })
        .unwrap();
        let mut dst = Heightfield::filled(24, 24, 8.0, 100.0).unwrap();
        let r = stamp(&mut dst, &brush, 12, 12, 1.0, 0.0, 1.0, BlendMode::Add);
        assert!(r.contains(12, 12), "the stamp rect misses its centre: {r:?}");
        // The centre of the brush sits on the destination sample the brush
        // centre is stamped at, and a full weight falloff leaves it at full
        // weight.
        let centre = dst.get(14, 14).unwrap();
        assert!((centre - 130.0).abs() < 1e-3, "the stamp centre is {centre}");
        assert!(dst.max() > 100.0, "the brush never reached the surface");
        // A stamp far off the grid changes nothing and reports an empty rect.
        let mut away = Heightfield::filled(24, 24, 8.0, 100.0).unwrap();
        let r = stamp(&mut away, &brush, 900, 900, 1.0, 0.0, 0.0, BlendMode::Add);
        assert!(r.is_empty());
        assert!(away.data.iter().all(|v| *v == 100.0));
        // A falloff of zero feathers the brush out from its centre, so it lifts
        // less of the surface than the full weight stamp.
        let mut soft = Heightfield::filled(24, 24, 8.0, 100.0).unwrap();
        stamp(&mut soft, &brush, 12, 12, 1.0, 0.0, 0.0, BlendMode::Add);
        let lifted = |g: &Heightfield| g.data.iter().map(|v| *v as f64 - 100.0).sum::<f64>();
        assert!(lifted(&soft) < lifted(&dst), "the feathered falloff did not fade");
        assert!((soft.get(14, 14).unwrap() - 130.0).abs() < 1e-3, "the brush centre moved");
    }

    #[test]
    fn scale_about_and_offset_clamped() {
        let mut hf = Heightfield::filled(4, 4, 1.0, 110.0).unwrap();
        scale_about(&mut hf, 100.0, 2.0);
        assert_eq!(hf.get(0, 0).unwrap(), 120.0);
        scale_about(&mut hf, 100.0, 0.0);
        assert_eq!(hf.get(0, 0).unwrap(), 100.0);

        let mut hf = Heightfield::filled(4, 4, 1.0, 50.0).unwrap();
        offset_clamped(&mut hf, 20.0, 0.0, 60.0).unwrap();
        assert_eq!(hf.get(0, 0).unwrap(), 60.0);
        assert!(matches!(offset_clamped(&mut hf, 1.0, 10.0, 0.0), Err(Error::BadRange(_, _))));
    }

    #[test]
    fn rect_helper_is_re_exported() {
        // The stamp signature names the rect type, so it has to be nameable.
        let r = Rect::new(1, 2, 3, 4);
        assert_eq!((r.width(), r.height()), (2, 2));
    }
}
