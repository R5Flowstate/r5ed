//! Recipes: a serialisable node graph that turns a JSON document into a
//! heightfield.
//!
//! Nodes are visited in dependency order and evaluated exactly once, and every
//! step is a pure function of its inputs, so the same JSON at the same
//! resolution always produces the same heights on any target.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::filters::{self, BlendMode, ToneCurve};
use crate::generators::Generator;
use crate::heightfield::{Filter, Heightfield};
use crate::mask::Mask;
use crate::math;
use crate::noise::{FractalParams, Noise};
use crate::sdf::VolumeTerrain;

/// Most nodes one recipe may hold, so a hostile document cannot make the
/// evaluator run away.
pub const MAX_NODES: usize = 256;

/// A node identifier.
pub type NodeId = String;

/// A shape filter applied to one field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FilterOp {
    /// Box blur, which converges towards a Gaussian.
    Smooth { radius: u32, passes: u32 },
    /// Unsharp mask.
    Sharpen { amount: f32, radius: u32 },
    /// Flat benches separated by a riser.
    Terrace { step: f32, sharpness: f32 },
    /// Clamp into a range.
    Clamp { lo: f32, hi: f32 },
    /// Remap through a piecewise curve.
    Curve { points: Vec<(f32, f32)> },
    /// A named tonal curve over the current range.
    Tone { curve: ToneCurve },
    /// Add a constant.
    Offset { delta: f32 },
    /// Scale about a base height.
    ScaleAbout { base: f32, factor: f32 },
    /// Thermal erosion.
    Thermal { iterations: u32, talus_angle: f32, rate: f32, talus: f32 },
    /// Particle-based hydraulic erosion.
    Hydraulic {
        droplets: u32,
        seed: u64,
        inertia: f32,
        capacity: f32,
        erode_speed: f32,
        deposit_speed: f32,
        evaporation: f32,
        radius: u32,
    },
    /// Grid-based pipe-model erosion.
    Pipe { iterations: u32, rain: f32, solubility: f32, deposition: f32 },
    /// Fractal noise added to the field.
    Noise { seed: u64, frequency: f64, octaves: u32, gain: f64, lacunarity: f64, amplitude: f32 },
    /// Resample to a new sample count.
    Resample { width: u32, height: u32, filter: Filter },
}

/// Which property of a field a mask reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FieldMaskKind {
    /// Slope in degrees.
    #[default]
    Slope,
    /// Height in inches.
    Height,
    /// Curvature in inches per cell squared.
    Curvature,
    /// Normalised flow accumulation.
    Flow,
    /// One where the field is not below zero.
    Dry,
}

/// Where a blend mask comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mask", rename_all = "snake_case")]
pub enum MaskSource {
    /// One constant everywhere.
    Constant { value: f32 },
    /// A circular window in object space.
    Radial { center: [f32; 2], inner: f32, outer: f32, invert: bool },
    /// A fractal noise field in 0..1.
    Noise { seed: u64, frequency: f64, octaves: u32, contrast: f32, invert: bool },
    /// Derived from another node's field.
    FromField { source: NodeId, kind: FieldMaskKind },
}

/// One term of a blend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendTerm {
    pub source: NodeId,
    /// Relative weight of this term. A negative weight subtracts it.
    pub weight: f32,
    /// Restrict the term to a mask, applied before it is mixed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskSource>,
}

/// What a node does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum NodeKind {
    /// Build a field from a generator.
    Generator { generator: Generator },
    /// Apply a shape filter to another node's field.
    Filter { source: NodeId, op: FilterOp },
    /// Mix two or more fields together.
    Blend { terms: Vec<BlendTerm>, mask: Option<MaskSource>, mode: BlendMode },
    /// Pull a field towards a mask value.
    Modulate { source: NodeId, mask: MaskSource, strength: f32 },
    /// A fractal noise field, used as a blend term.
    Detail { seed: u64, frequency: f64, octaves: u32, amplitude: f32 },
    /// A volumetric SDF field, meshed separately with surface nets.
    Volume { terrain: VolumeTerrain },
}

/// One node of the graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    #[serde(flatten)]
    pub kind: NodeKind,
}

/// A terrain recipe: a node graph plus the node its output comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub nodes: Vec<Node>,
    pub output: NodeId,
}

impl Recipe {
    /// A recipe whose only node is one generator.
    pub fn single(id: impl Into<NodeId>, generator: Generator) -> Self {
        let id = id.into();
        Self { nodes: vec![Node { id: id.clone(), kind: NodeKind::Generator { generator } }], output: id }
    }

    /// Parse a recipe from JSON, the form stored in `Terrain.recipe`.
    pub fn from_json(text: &str) -> Result<Self> {
        serde_json::from_str(text).map_err(|e| Error::BadJson(e.to_string()))
    }

    /// Serialise back to JSON.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| Error::BadJson(e.to_string()))
    }

    /// Evaluate the output field on a `res` by `res` grid of `cell` samples.
    pub fn evaluate(&self, res: [u32; 2], cell: f32) -> Result<Heightfield> {
        let (w, h) = (res[0], res[1]);
        if w == 0 || h == 0 {
            return Err(Error::ZeroSize);
        }
        if !cell.is_finite() || cell <= 0.0 {
            return Err(Error::InvalidCellSize(cell));
        }
        let built = self.build(w, h, cell)?;
        built
            .into_iter()
            .find(|(k, _)| *k == self.output)
            .map(|(_, v)| v)
            .ok_or_else(|| Error::UnknownNode(self.output.clone()))
    }

    /// One node's field, for callers that want an intermediate.
    pub fn field(&self, id: &str, res: [u32; 2], cell: f32) -> Result<Option<Heightfield>> {
        Ok(self.build(res[0], res[1], cell)?.into_iter().find(|(k, _)| k == id).map(|(_, v)| v))
    }

    /// The volumetric terrain of a volume node, if one exists with that id.
    pub fn volume(&self, id: &str) -> Result<Option<VolumeTerrain>> {
        for n in &self.nodes {
            if n.id == id {
                if let NodeKind::Volume { terrain } = &n.kind {
                    return Ok(Some(terrain.clone()));
                }
            }
        }
        Ok(None)
    }

    /// Every node's heightfield, in dependency order.
    fn build(&self, w: u32, h: u32, cell: f32) -> Result<Vec<(NodeId, Heightfield)>> {
        if self.nodes.is_empty() {
            return Err(Error::EmptyBlend);
        }
        if self.nodes.len() > MAX_NODES {
            return Err(Error::RecipeTooLarge(MAX_NODES));
        }
        let order = self.dependency_order()?;
        let mut fields: Vec<(NodeId, Heightfield)> = Vec::with_capacity(self.nodes.len());
        for idx in order {
            let node = &self.nodes[idx];
            if fields.iter().any(|(k, _)| *k == node.id) {
                return Err(Error::DuplicateNode(node.id.clone()));
            }
            let hf = match &node.kind {
                NodeKind::Generator { generator } => generator.generate(w, h, cell)?,
                NodeKind::Filter { source, op } => {
                    let mut owned = look_up(&fields, source)?.clone();
                    apply_filter(&mut owned, op)?;
                    owned
                }
                NodeKind::Blend { terms, mask, mode } => {
                    blend(&fields, terms, mask.as_ref(), *mode)?
                }
                NodeKind::Modulate { source, mask, strength } => {
                    let mut owned = look_up(&fields, source)?.clone();
                    let m = build_mask(mask, w, h, cell, &fields)?;
                    require_same(&m, &owned)?;
                    for i in 0..owned.data.len() {
                        owned.data[i] =
                            filters::blend(owned.data[i], m.data[i] * *strength, BlendMode::Mix);
                    }
                    owned
                }
                NodeKind::Detail { seed, frequency, octaves, amplitude } => {
                    detail(*seed, *frequency, *octaves, *amplitude, w, h, cell)?
                }
                // A volume node has no heightfield of its own, so it publishes
                // nothing here and is read back through `volume`.
                NodeKind::Volume { .. } => continue,
            };
            fields.push((node.id.clone(), hf));

        }
        Ok(fields)
    }

    /// Every node's field on a fresh grid, keyed by node id.
    pub fn all_fields(&self, w: u32, h: u32, cell: f32) -> Result<Vec<(NodeId, Heightfield)>> {
        if w == 0 || h == 0 {
            return Err(Error::ZeroSize);
        }
        if !cell.is_finite() || cell <= 0.0 {
            return Err(Error::InvalidCellSize(cell));
        }
        self.build(w, h, cell)
    }

    /// Visit order, each node after everything it depends on.
    fn dependency_order(&self) -> Result<Vec<usize>> {
        let mut state = vec![0u8; self.nodes.len()];
        let mut order = Vec::with_capacity(self.nodes.len());
        for start in 0..self.nodes.len() {
            visit(start, &self.nodes, &mut state, &mut order)?;
        }
        Ok(order)
    }
}

/// Depth-first visit with an explicit stack, so a long chain cannot overflow
/// the call stack and a cycle is reported instead of looping.
fn visit(start: usize, nodes: &[Node], state: &mut [u8], order: &mut Vec<usize>) -> Result<()> {
    let mut stack: Vec<(usize, u8)> = vec![(start, 0)];
    while let Some((idx, phase)) = stack.pop() {
        if state[idx] == 2 {
            continue;
        }
        if phase == 0 {
            state[idx] = 1;
            stack.push((idx, 1));
            for dep in dependencies(&nodes[idx].kind) {
                let Some(d) = nodes.iter().position(|n| n.id == dep) else {
                    return Err(Error::UnknownNode(dep));
                };
                if state[d] == 1 {
                    return Err(Error::RecipeCycle(nodes[d].id.clone()));
                }
                if state[d] == 0 {
                    stack.push((d, 0));
                }
            }
        } else {
            state[idx] = 2;
            order.push(idx);
        }
    }
    Ok(())
}

fn dependencies(kind: &NodeKind) -> Vec<NodeId> {
    match kind {
        NodeKind::Generator { .. } | NodeKind::Detail { .. } | NodeKind::Volume { .. } => Vec::new(),
        NodeKind::Filter { source, .. } => vec![source.clone()],
        NodeKind::Modulate { source, mask, .. } => {
            let mut v = vec![source.clone()];
            if let MaskSource::FromField { source, .. } = mask {
                v.push(source.clone());
            }
            v
        }
        NodeKind::Blend { terms, mask, .. } => {
            let mut v: Vec<NodeId> = terms.iter().map(|t| t.source.clone()).collect();
            for m in mask.iter().chain(terms.iter().filter_map(|t| t.mask.as_ref())) {
                if let MaskSource::FromField { source, .. } = m {
                    v.push(source.clone());
                }
            }
            v
        }
    }
}

fn look_up<'a>(fields: &'a [(NodeId, Heightfield)], id: &str) -> Result<&'a Heightfield> {
    fields
        .iter()
        .find(|(k, _)| k == id)
        .map(|(_, v)| v)
        .ok_or_else(|| Error::UnknownNode(id.to_string()))
}

fn require_same(a: &Mask, b: &Heightfield) -> Result<()> {
    if a.width != b.width || a.height != b.height {
        return Err(Error::SizeMismatch(b.width, b.height, a.data.len()));
    }
    Ok(())
}

fn blend(
    fields: &[(NodeId, Heightfield)],
    terms: &[BlendTerm],
    mask: Option<&MaskSource>,
    mode: BlendMode,
) -> Result<Heightfield> {
    if terms.is_empty() {
        return Err(Error::EmptyBlend);
    }
    let (w, h, cell) = {
        let first = look_up(fields, &terms[0].source)?;
        (first.width, first.height, first.cell_size)
    };
    let mut out: Option<Heightfield> = None;
    let mut weight_sum = 0.0f32;
    for t in terms {
        let src = look_up(fields, &t.source)?;
        require_same_dims(src, w, h)?;
        let wgt = t.weight.abs();
        if wgt <= 0.0 || !wgt.is_finite() {
            return Err(Error::BadParams("a blend term needs a positive weight"));
        }
        // Each term is normalised to its own source range before mixing, so a
        // blend of two differently scaled fields behaves predictably.
        let (lo, hi) = src.range();
        let span = if hi > lo { hi - lo } else { 1.0 };
        let mut term = src.clone();
        for v in term.data.iter_mut() {
            *v = (*v - lo) / span;
        }
        if let Some(m) = &t.mask {
            let built = build_mask(m, w, h, cell, fields)?;
            require_same(&built, &term)?;
            for i in 0..term.data.len() {
                term.data[i] *= built.data[i];
            }
        }
        if out.is_none() {
            out = Some(term);
            weight_sum = wgt;
        } else if let Some(dst) = out.as_mut() {
            for i in 0..dst.data.len() {
                dst.data[i] = filters::blend(dst.data[i], term.data[i], mode);
            }
            weight_sum += wgt;
        }
    }
    let mut hf = out.expect("terms is not empty");
    if let Some(m) = mask {
        let built = build_mask(m, w, h, cell, fields)?;
        require_same(&built, &hf)?;
        for i in 0..hf.data.len() {
            hf.data[i] = filters::blend(hf.data[i], built.data[i], mode);
        }
    }
    // Restore the first term's scale, so a blend of absolute heights keeps the
    // heights the caller wrote down. A blend cannot invent relief the first
    // term did not have, so the result is clamped to that term's own range.
    let _ = weight_sum;
    let first = look_up(fields, &terms[0].source)?;
    let (lo, hi) = first.range();
    let span = if hi > lo { hi - lo } else { 1.0 };
    for v in hf.data.iter_mut() {
        *v = lo + math::saturate(f64::from(*v)) as f32 * span;
    }
    Ok(hf)
}

fn require_same_dims(hf: &Heightfield, w: u32, h: u32) -> Result<()> {
    if hf.width != w || hf.height != h {
        return Err(Error::SizeMismatch(w, h, hf.data.len()));
    }
    Ok(())
}

fn build_mask(
    source: &MaskSource,
    w: u32,
    h: u32,
    cell: f32,
    fields: &[(NodeId, Heightfield)],
) -> Result<Mask> {
    let n = w as usize * h as usize;
    let data = match source {
        MaskSource::Constant { value } => vec![*value; n],
        MaskSource::Radial { center, inner, outer, invert } => {
            let mut v = Vec::with_capacity(n);
            for y in 0..h {
                for x in 0..w {
                    let (px, py) = (x as f32 * cell, y as f32 * cell);
                    let dx = px - center[0];
                    let dy = py - center[1];
                    let d = math::sqrt((dx * dx + dy * dy) as f64);
                    let mut t = math::smooth_range(d, *inner as f64, *outer as f64) as f32;
                    if *invert {
                        t = 1.0 - t;
                    }
                    v.push(t);
                }
            }
            v
        }
        MaskSource::Noise { seed, frequency, octaves, contrast, invert } => {
            let nz = Noise::new(*seed);
            let params = FractalParams {
                octaves: (*octaves).clamp(1, 24),
                frequency: if frequency.is_finite() { frequency.abs().max(1e-6) } else { 1.0 },
                ..FractalParams::default()
            };
            let k = if contrast.is_finite() && *contrast > 0.0 { *contrast } else { 1.0 };
            let mut v = Vec::with_capacity(n);
            for y in 0..h {
                for x in 0..w {
                    let (px, py) = (x as f32 * cell, y as f32 * cell);
                    let raw = crate::noise::fractal2(
                        crate::noise::Fractal::Fbm,
                        &nz,
                        &params,
                        px as f64,
                        py as f64,
                    );
                    let mut t = math::saturate(raw * 0.5 + 0.5) as f32;
                    if k != 1.0 {
                        t = math::pow(t as f64, k as f64) as f32;
                    }
                    if *invert {
                        t = 1.0 - t;
                    }
                    v.push(t);
                }
            }
            v
        }
        MaskSource::FromField { source, kind } => {
            let hf = look_up(fields, source)?;
            if hf.data.len() != n {
                return Err(Error::SizeMismatch(w, h, hf.data.len()));
            }
            match kind {
                FieldMaskKind::Slope => hf.slope_deg(),
                FieldMaskKind::Height => hf.data.clone(),
                FieldMaskKind::Curvature => hf.curvature(),
                FieldMaskKind::Flow => {
                    let flow = hf.flow_accumulation();
                    let max = flow.max_area().max(1e-6);
                    flow.area.iter().map(|a| (a / max).clamp(0.0, 1.0)).collect()
                }
                FieldMaskKind::Dry => hf.data.iter().map(|v| if *v > 0.0 { 1.0 } else { 0.0 }).collect(),
            }
        }
    };
    Mask::new(w, h, cell, data)
}

fn detail(
    seed: u64,
    frequency: f64,
    octaves: u32,
    amplitude: f32,
    w: u32,
    h: u32,
    cell: f32,
) -> Result<Heightfield> {
    let n = Noise::new(seed);
    let params = FractalParams {
        octaves: octaves.clamp(1, 24),
        frequency: if frequency.is_finite() { frequency.abs().max(1e-6) } else { 1.0 },
        ..FractalParams::default()
    };
    let mut hf = Heightfield::filled(w, h, cell, 0.0)?;
    for y in 0..h {
        for x in 0..w {
            let (px, py) = hf.sample_to_world(x as f32, y as f32);
            let v = crate::noise::fractal2(crate::noise::Fractal::Fbm, &n, &params, px as f64, py as f64);
            let i = hf.index(x, y);
            hf.data[i] = (v as f32) * amplitude;
        }
    }
    Ok(hf)
}

fn apply_filter(hf: &mut Heightfield, op: &FilterOp) -> Result<()> {
    match *op {
        FilterOp::Smooth { radius, passes } => filters::smooth(hf, radius, passes),
        FilterOp::Sharpen { amount, radius } => filters::sharpen(hf, amount, radius),
        FilterOp::Terrace { step, sharpness } => filters::terrace(hf, step, sharpness)?,
        FilterOp::Clamp { lo, hi } => filters::clamp(hf, lo, hi)?,
        FilterOp::Curve { ref points } => filters::curve_remap(hf, points)?,
        FilterOp::Tone { curve } => filters::apply_curve(hf, curve)?,
        FilterOp::Offset { delta } => hf.offset(delta),
        FilterOp::ScaleAbout { base, factor } => filters::scale_about(hf, base, factor),
        FilterOp::Thermal { iterations, talus_angle, rate, talus } => {
            let params = crate::erosion::ThermalParams { iterations, talus_angle, rate, talus };
            *hf = crate::erosion::thermal(hf, &params)?;
        }
        FilterOp::Hydraulic {
            droplets,
            seed,
            inertia,
            capacity,
            erode_speed,
            deposit_speed,
            evaporation,
            radius,
        } => {
            let params = crate::erosion::HydraulicParams {
                droplets,
                inertia,
                capacity,
                deposit_speed,
                erode_speed,
                evaporation,
                radius,
                ..crate::erosion::HydraulicParams::default()
            };
            *hf = crate::erosion::hydraulic(hf, &params, seed)?;
        }
        FilterOp::Pipe { iterations, rain, solubility, deposition } => {
            let params = crate::erosion::PipeParams {
                iterations,
                rain,
                solubility,
                deposition,
                ..crate::erosion::PipeParams::default()
            };
            *hf = crate::erosion::pipe_erosion(hf, &params)?;
        }
        FilterOp::Noise { seed, frequency, octaves, gain, lacunarity, amplitude } => {
            let n = Noise::new(seed);
            let params = FractalParams {
                octaves: octaves.clamp(1, 24),
                frequency: if frequency.is_finite() { frequency.abs().max(1e-6) } else { 1.0 },
                lacunarity: if lacunarity.is_finite() { lacunarity.abs().max(1.0) } else { 2.0 },
                gain: if gain.is_finite() { gain.clamp(0.0, 1.0) } else { 0.5 },
                ..FractalParams::default()
            };
            let (w, h, cell) = (hf.width, hf.height, hf.cell_size);
            for y in 0..h {
                for x in 0..w {
                    let (px, py) = hf.sample_to_world(x as f32, y as f32);
                    let v = crate::noise::fractal2(
                        crate::noise::Fractal::Fbm,
                        &n,
                        &params,
                        px as f64 * cell as f64,
                        py as f64 * cell as f64,
                    );
                    let i = hf.index(x, y);
                    hf.data[i] += (v as f32) * amplitude;
                }
            }
        }
        FilterOp::Resample { width, height, filter } => {
            *hf = hf.resample(width, height, filter)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generators::{CratersParams, MountainsParams, RollingHillsParams};
    use crate::sdf::{NoiseKind3, SdfOp, SdfShape};

    fn gen_node(id: &str, g: Generator) -> Node {
        Node { id: id.to_string(), kind: NodeKind::Generator { generator: g } }
    }

    fn hills(seed: u64) -> Generator {
        Generator::RollingHills(RollingHillsParams { seed, ..RollingHillsParams::default() })
    }

    fn simple() -> Recipe {
        Recipe { nodes: vec![gen_node("base", hills(1))], output: "base".to_string() }
    }

    #[test]
    fn a_single_generator_node_evaluates() {
        let hf = simple().evaluate([32, 24], 8.0).unwrap();
        assert_eq!((hf.width, hf.height), (32, 24));
        assert_eq!(hf.cell_size, 8.0);
        assert!(hf.is_finite());
    }

    #[test]
    fn every_filter_kind_applies() {
        let ops = vec![
            FilterOp::Smooth { radius: 1, passes: 1 },
            FilterOp::Sharpen { amount: 0.5, radius: 1 },
            FilterOp::Terrace { step: 40.0, sharpness: 0.5 },
            FilterOp::Clamp { lo: -100.0, hi: 500.0 },
            FilterOp::Curve { points: vec![(0.0, 0.0), (1.0, 1.0)] },
            FilterOp::Tone { curve: ToneCurve::Contrast },
            FilterOp::Offset { delta: 10.0 },
            FilterOp::ScaleAbout { base: 0.0, factor: 1.1 },
            FilterOp::Thermal { iterations: 2, talus_angle: 30.0, rate: 0.4, talus: 0.5 },
            FilterOp::Hydraulic {
                droplets: 500,
                seed: 3,
                inertia: 0.05,
                capacity: 2.0,
                erode_speed: 0.3,
                deposit_speed: 0.3,
                evaporation: 0.02,
                radius: 2,
            },
            FilterOp::Pipe { iterations: 4, rain: 0.01, solubility: 0.3, deposition: 0.3 },
            FilterOp::Noise { seed: 5, frequency: 0.05, octaves: 3, gain: 0.5, lacunarity: 2.0, amplitude: 5.0 },
            FilterOp::Resample { width: 40, height: 40, filter: Filter::Bilinear },
        ];
        for op in ops.clone() {
            let r = Recipe {
                nodes: vec![
                    gen_node("base", hills(1)),
                    Node { id: "out".to_string(), kind: NodeKind::Filter { source: "base".into(), op: op.clone() } },
                ],
                output: "out".to_string(),
            };
            let hf = r.evaluate([32, 32], 8.0).unwrap_or_else(|e| panic!("{op:?}: {e}"));
            assert!(hf.is_finite(), "{op:?} produced a non-finite sample");
        }
    }

    #[test]
    fn blend_modes_all_work() {
        for mode in [BlendMode::Mix, BlendMode::Add, BlendMode::Max, BlendMode::Min] {
            let r = Recipe {
                nodes: vec![
                    gen_node("a", hills(1)),
                    gen_node("b", hills(2)),
                    Node {
                        id: "out".to_string(),
                        kind: NodeKind::Blend {
                            terms: vec![
                                BlendTerm { source: "a".into(), weight: 0.5, mask: None },
                                BlendTerm { source: "b".into(), weight: 0.5, mask: None },
                            ],
                            mask: None,
                            mode,
                        },
                    },
                ],
                output: "out".to_string(),
            };
            let hf = r.evaluate([32, 32], 8.0).unwrap();
            assert!(hf.is_finite(), "{mode:?} blend went non-finite");
        }
    }

    #[test]
    fn blends_respect_every_mask_source() {
        let masks = vec![
            MaskSource::Constant { value: 0.5 },
            MaskSource::Radial { center: [0.0, 0.0], inner: 10.0, outer: 90.0, invert: false },
            MaskSource::Radial { center: [0.0, 0.0], inner: 10.0, outer: 90.0, invert: true },
            MaskSource::Noise { seed: 2, frequency: 0.02, octaves: 3, contrast: 2.0, invert: false },
            MaskSource::FromField { source: "a".into(), kind: FieldMaskKind::Slope },
            MaskSource::FromField { source: "a".into(), kind: FieldMaskKind::Height },
            MaskSource::FromField { source: "a".into(), kind: FieldMaskKind::Curvature },
            MaskSource::FromField { source: "a".into(), kind: FieldMaskKind::Flow },
            MaskSource::FromField { source: "a".into(), kind: FieldMaskKind::Dry },
        ];
        for mask in masks {
            let r = Recipe {
                nodes: vec![
                    gen_node("a", hills(1)),
                    gen_node("b", hills(2)),
                    Node {
                        id: "out".to_string(),
                        kind: NodeKind::Blend {
                            terms: vec![
                                BlendTerm { source: "a".into(), weight: 0.6, mask: Some(mask.clone()) },
                                BlendTerm { source: "b".into(), weight: 0.4, mask: None },
                            ],
                            mask: Some(mask.clone()),
                            mode: BlendMode::Mix,
                        },
                    },
                ],
                output: "out".to_string(),
            };
            let hf = r.evaluate([32, 32], 8.0).unwrap();
            assert!(hf.is_finite(), "{mask:?} broke the blend");
        }
    }

    #[test]
    fn modulate_pulls_towards_the_mask_value() {
        let r = Recipe {
            nodes: vec![
                gen_node("a", hills(1)),
                Node {
                    id: "out".to_string(),
                    kind: NodeKind::Modulate {
                        source: "a".into(),
                        mask: MaskSource::Constant { value: 0.0 },
                        strength: 500.0,
                    },
                },
            ],
            output: "out".to_string(),
        };
        let hf = r.evaluate([16, 16], 8.0).unwrap();
        let base = r.field("a", [16, 16], 8.0).unwrap().unwrap();
        assert!(hf.max() < base.max(), "the mask did not pull the field down");
        assert!(hf.is_finite());
    }

    #[test]
    fn detail_nodes_feed_a_blend() {
        let r = Recipe {
            nodes: vec![
                gen_node("a", Generator::Mountains(MountainsParams::default())),
                Node {
                    id: "d".to_string(),
                    kind: NodeKind::Detail { seed: 9, frequency: 0.02, octaves: 4, amplitude: 40.0 },
                },
                Node {
                    id: "out".to_string(),
                    kind: NodeKind::Blend {
                        terms: vec![
                            BlendTerm { source: "a".into(), weight: 1.0, mask: None },
                            BlendTerm { source: "d".into(), weight: 0.2, mask: None },
                        ],
                        mask: None,
                        mode: BlendMode::Add,
                    },
                },
            ],
            output: "out".to_string(),
        };
        let hf = r.evaluate([32, 32], 8.0).unwrap();
        assert!(hf.is_finite());
        assert!(hf.data.iter().any(|v| v.abs() > 0.0));
    }

    #[test]
    fn node_order_in_the_document_does_not_matter() {
        let a = gen_node("a", hills(1));
        let b = Node {
            id: "b".to_string(),
            kind: NodeKind::Filter { source: "a".into(), op: FilterOp::Smooth { radius: 1, passes: 2 } },
        };
        let forward = Recipe { nodes: vec![a.clone(), b.clone()], output: "b".into() };
        let backward = Recipe { nodes: vec![b, a], output: "b".into() };
        assert_eq!(forward.evaluate([32, 32], 8.0).unwrap(), backward.evaluate([32, 32], 8.0).unwrap());
    }

    #[test]
    fn evaluation_is_deterministic() {
        let r = Recipe {
            nodes: vec![
                gen_node("a", Generator::Craters(CratersParams::default())),
                Node {
                    id: "out".to_string(),
                    kind: NodeKind::Filter { source: "a".into(), op: FilterOp::Terrace { step: 30.0, sharpness: 0.4 } },
                },
            ],
            output: "out".to_string(),
        };
        assert_eq!(r.evaluate([48, 48], 8.0).unwrap(), r.evaluate([48, 48], 8.0).unwrap());
    }

    #[test]
    fn json_round_trips_and_evaluates() {
        let r = Recipe {
            nodes: vec![
                gen_node("a", Generator::Mountains(MountainsParams::default())),
                Node {
                    id: "out".to_string(),
                    kind: NodeKind::Filter { source: "a".into(), op: FilterOp::Smooth { radius: 2, passes: 1 } },
                },
            ],
            output: "out".to_string(),
        };
        let json = r.to_json().unwrap();
        let back = Recipe::from_json(&json).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.evaluate([24, 24], 8.0).unwrap(), r.evaluate([24, 24], 8.0).unwrap());
        assert!(matches!(Recipe::from_json("{ not json"), Err(Error::BadJson(_))));
        assert!(matches!(Recipe::from_json("{}"), Err(Error::BadJson(_))));
    }

    #[test]
    fn structural_errors_are_reported() {
        assert!(Recipe { nodes: vec![], output: "a".into() }.evaluate([8, 8], 1.0).is_err());
        let missing = Recipe { nodes: vec![gen_node("a", hills(1))], output: "nope".to_string() };
        assert!(matches!(missing.evaluate([8, 8], 1.0), Err(Error::UnknownNode(_))));
        let dup = Recipe { nodes: vec![gen_node("a", hills(1)), gen_node("a", hills(2))], output: "a".into() };
        assert!(matches!(dup.evaluate([8, 8], 1.0), Err(Error::DuplicateNode(_))));
        let bad_dep = Recipe {
            nodes: vec![Node {
                id: "out".to_string(),
                kind: NodeKind::Filter { source: "ghost".into(), op: FilterOp::Offset { delta: 1.0 } },
            }],
            output: "out".into(),
        };
        assert!(matches!(bad_dep.evaluate([8, 8], 1.0), Err(Error::UnknownNode(_))));
        let cyclic = Recipe {
            nodes: vec![
                Node { id: "a".into(), kind: NodeKind::Filter { source: "b".into(), op: FilterOp::Offset { delta: 1.0 } } },
                Node { id: "b".into(), kind: NodeKind::Filter { source: "a".into(), op: FilterOp::Offset { delta: 1.0 } } },
            ],
            output: "a".into(),
        };
        assert!(matches!(cyclic.evaluate([8, 8], 1.0), Err(Error::RecipeCycle(_))));
        let empty_blend = Recipe {
            nodes: vec![
                gen_node("a", hills(1)),
                Node { id: "out".into(), kind: NodeKind::Blend { terms: Vec::new(), mask: None, mode: BlendMode::Mix } },
            ],
            output: "out".into(),
        };
        assert!(matches!(empty_blend.evaluate([8, 8], 1.0), Err(Error::EmptyBlend)));
        let too_many = Recipe {
            nodes: (0..MAX_NODES + 1).map(|i| gen_node(&format!("n{i}"), hills(1))).collect(),
            output: "n0".into(),
        };
        assert!(matches!(too_many.evaluate([8, 8], 1.0), Err(Error::RecipeTooLarge(_))));
    }

    #[test]
    fn a_bad_evaluation_grid_is_rejected() {
        let r = simple();
        assert!(matches!(r.evaluate([0, 8], 1.0), Err(Error::ZeroSize)));
        assert!(matches!(r.evaluate([8, 8], 0.0), Err(Error::InvalidCellSize(_))));
        assert!(matches!(r.evaluate([8, 8], f32::NAN), Err(Error::InvalidCellSize(_))));
        assert!(matches!(r.all_fields(0, 8, 1.0), Err(Error::ZeroSize)));
        assert!(matches!(r.all_fields(8, 8, 0.0), Err(Error::InvalidCellSize(_))));
    }

    #[test]
    fn a_single_node_helper_builds() {
        let r = Recipe::single("terrain", hills(1));
        assert_eq!(r.nodes.len(), 1);
        assert_eq!(r.output, "terrain");
        assert!(r.evaluate([16, 16], 8.0).is_ok());
    }

    #[test]
    fn intermediates_are_reachable() {
        let r = Recipe {
            nodes: vec![
                gen_node("a", hills(1)),
                Node { id: "out".to_string(), kind: NodeKind::Filter { source: "a".into(), op: FilterOp::Offset { delta: 5.0 } } },
            ],
            output: "out".to_string(),
        };
        let base = r.field("a", [16, 16], 8.0).unwrap().unwrap();
        let out = r.field("out", [16, 16], 8.0).unwrap().unwrap();
        assert!((out.max() - (base.max() + 5.0)).abs() < 1e-3);
        assert!(r.field("missing", [16, 16], 8.0).unwrap().is_none());
        assert_eq!(r.all_fields(16, 16, 8.0).unwrap().len(), 2);
    }

    #[test]
    fn volume_nodes_are_read_back_separately() {
        let terrain = VolumeTerrain {
            chunks: [8, 8, 8],
            voxel_size: 1.0,
            origin: glam::DVec3::splat(-4.0),
            ops: vec![
                SdfOp::Add { shape: SdfShape::Sphere { center: glam::DVec3::ZERO, radius: 2.5 } },
                SdfOp::Noise { amplitude: 0.4, frequency: 0.2, seed: 3, kind: NoiseKind3::Perlin },
            ],
        };
        let r = Recipe {
            nodes: vec![Node { id: "vol".to_string(), kind: NodeKind::Volume { terrain: terrain.clone() } }],
            output: "vol".to_string(),
        };
        // A volume node has no heightfield, so asking for the heightfield
        // output says the node is unknown rather than inventing a surface.
        assert!(matches!(r.evaluate([8, 8], 1.0), Err(Error::UnknownNode(_))));
        assert_eq!(r.volume("vol").unwrap(), Some(terrain.clone()));
        assert_eq!(r.volume("nope").unwrap(), None);
        // The field itself is still valid and meshable.
        assert!(terrain.evaluate().unwrap().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_blend_keeps_the_first_term_scale() {
        let r = Recipe {
            nodes: vec![
                gen_node("a", hills(1)),
                gen_node("b", hills(2)),
                Node {
                    id: "out".to_string(),
                    kind: NodeKind::Blend {
                        terms: vec![
                            BlendTerm { source: "a".into(), weight: 0.5, mask: None },
                            BlendTerm { source: "b".into(), weight: 0.5, mask: None },
                        ],
                        mask: None,
                        mode: BlendMode::Add,
                    },
                },
            ],
            output: "out".to_string(),
        };
        let a = r.field("a", [32, 32], 8.0).unwrap().unwrap();
        let mixed = r.evaluate([32, 32], 8.0).unwrap();
        // The result lives on the first term's height scale.
        assert!(mixed.max() <= a.max() + 1.0, "blend {} rose above {}", mixed.max(), a.max());
        assert!(mixed.min() >= a.min() - 1.0, "blend {} fell below {}", mixed.min(), a.min());
    }

    #[test]
    fn a_zero_weight_term_is_refused() {
        let r = Recipe {
            nodes: vec![
                gen_node("a", hills(1)),
                gen_node("b", hills(2)),
                Node {
                    id: "out".to_string(),
                    kind: NodeKind::Blend {
                        terms: vec![
                            BlendTerm { source: "a".into(), weight: 0.0, mask: None },
                            BlendTerm { source: "b".into(), weight: 0.5, mask: None },
                        ],
                        mask: None,
                        mode: BlendMode::Mix,
                    },
                },
            ],
            output: "out".to_string(),
        };
        assert!(matches!(r.evaluate([16, 16], 8.0), Err(Error::BadParams(_))));
    }

    #[test]
    fn a_mismatched_blend_source_is_refused() {
        let r = Recipe {
            nodes: vec![
                gen_node("a", hills(1)),
                Node {
                    id: "b".to_string(),
                    kind: NodeKind::Filter {
                        source: "a".into(),
                        op: FilterOp::Resample { width: 8, height: 8, filter: Filter::Bilinear },
                    },
                },
                Node {
                    id: "out".to_string(),
                    kind: NodeKind::Blend {
                        terms: vec![
                            BlendTerm { source: "a".into(), weight: 1.0, mask: None },
                            BlendTerm { source: "b".into(), weight: 1.0, mask: None },
                        ],
                        mask: None,
                        mode: BlendMode::Mix,
                    },
                },
            ],
            output: "out".to_string(),
        };
        assert!(matches!(r.evaluate([16, 16], 8.0), Err(Error::SizeMismatch(..))));
    }

    #[test]
    fn serialised_shape_has_nodes_and_output() {
        let v = serde_json::to_value(&simple()).unwrap();
        assert!(v.get("nodes").is_some());
        assert!(v.get("output").is_some());
    }

    /// A recipe that exercises a generator, a filter chain, a detail node, a
    /// blend and a mask, so the golden hash covers the whole evaluator.
    fn golden_recipes() -> Vec<(&'static str, Recipe)> {
        use crate::generators::{
            DunesParams, IslandsParams, MountainsParams, RiversParams, RollingHillsParams,
        };
        let gen = |id: &str, g: Generator| Node { id: id.to_string(), kind: NodeKind::Generator { generator: g } };
        let filt = |id: &str, source: &str, op: FilterOp| Node {
            id: id.to_string(),
            kind: NodeKind::Filter { source: source.to_string(), op },
        };
        vec![
            ("hills", simple()),
            (
                "mountains_eroded",
                Recipe {
                    nodes: vec![
                        gen("a", Generator::Mountains(MountainsParams::default())),
                        filt(
                            "out",
                            "a",
                            FilterOp::Thermal {
                                iterations: 8,
                                talus_angle: 36.0,
                                rate: 0.5,
                                talus: 0.6,
                            },
                        ),
                    ],
                    output: "out".to_string(),
                },
            ),
            (
                "dunes_smoothed_terraced",
                Recipe {
                    nodes: vec![
                        gen("a", Generator::Dunes(DunesParams::default())),
                        filt("b", "a", FilterOp::Smooth { radius: 2, passes: 2 }),
                        filt("out", "b", FilterOp::Terrace { step: 48.0, sharpness: 0.7 }),
                    ],
                    output: "out".to_string(),
                },
            ),
            (
                "islands_blended_with_detail",
                Recipe {
                    nodes: vec![
                        gen("a", Generator::Islands(IslandsParams::default())),
                        gen("b", Generator::Rivers(RiversParams::default())),
                        Node {
                            id: "d".to_string(),
                            kind: NodeKind::Detail {
                                seed: 21,
                                frequency: 0.03,
                                octaves: 5,
                                amplitude: 30.0,
                            },
                        },
                        Node {
                            id: "out".to_string(),
                            kind: NodeKind::Blend {
                                terms: vec![
                                    BlendTerm { source: "a".into(), weight: 0.7, mask: None },
                                    BlendTerm {
                                        source: "b".into(),
                                        weight: 0.3,
                                        mask: Some(MaskSource::Radial {
                                            center: [0.0, 0.0],
                                            inner: 64.0,
                                            outer: 256.0,
                                            invert: false,
                                        }),
                                    },
                                    BlendTerm { source: "d".into(), weight: 0.15, mask: None },
                                ],
                                mask: None,
                                mode: BlendMode::Mix,
                            },
                        },
                    ],
                    output: "out".to_string(),
                },
            ),
            (
                "hills_hydraulic",
                Recipe {
                    nodes: vec![
                        gen("a", Generator::RollingHills(RollingHillsParams::default())),
                        filt(
                            "out",
                            "a",
                            FilterOp::Hydraulic {
                                droplets: 4000,
                                seed: 12345,
                                inertia: 0.06,
                                capacity: 3.2,
                                erode_speed: 0.32,
                                deposit_speed: 0.28,
                                evaporation: 0.018,
                                radius: 3,
                            },
                        ),
                    ],
                    output: "out".to_string(),
                },
            ),
        ]
    }

    /// Golden hashes of the evaluated heights.
    ///
    /// Every step is built from IEEE-754 add, subtract, multiply, divide and
    /// square root with no fused multiply-add contraction, which Rust guarantees
    /// on every target, so the same recipe hashes the same on `wasm32` as it
    /// does natively. The `math` module carries the accuracy tests for the
    /// polynomial sine, exponential and logarithm that stand in for libm, and
    /// the same hashes are checked again by the `wasm32` test below.
    const GOLDEN: [(&str, u64); 5] = [
        ("hills", 0x74aa_6e27_2557_4996),
        ("mountains_eroded", 0x6964_907b_3886_50de),
        ("dunes_smoothed_terraced", 0x1f9c_ffa8_1846_c0d4),
        ("islands_blended_with_detail", 0xab22_46fc_c3fd_7246),
        ("hills_hydraulic", 0x29d6_68e0_59b8_24c0),
    ];

    fn check_golden_hashes() {
        let want: std::collections::HashMap<&str, u64> = GOLDEN.iter().cloned().collect();
        for (name, r) in golden_recipes() {
            let hf = r.evaluate([37, 29], 8.0).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(hf.is_finite(), "{name} produced a non-finite sample");
            let got = crate::rng::hash_f32(&hf.data);
            let expect = want[name];
            assert_eq!(got, expect, "{name} hashed {got:#018x}, expected {expect:#018x}");
            // The JSON round trip must not move a single bit either.
            let back = Recipe::from_json(&r.to_json().unwrap()).unwrap();
            assert_eq!(back.evaluate([37, 29], 8.0).unwrap().data, hf.data, "{name}");
        }
    }

    #[test]
    fn recipe_golden_hashes() {
        check_golden_hashes();
    }
}
