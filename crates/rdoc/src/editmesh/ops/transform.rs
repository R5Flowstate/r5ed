//! translate / rotate / scale of a selection: pivot median | cursor |
//! individual, with optional proportional falloff using the six formulas the
//! web proportional edit uses (constant, linear, sharp, root, sphere, smooth).

use std::collections::BTreeSet;

use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};

use super::{clamp_finite, clamp_vec3, parse_params, selected_verts, OpResult};
use crate::editmesh::{bad, Adjacency, EditMesh};
use crate::editmesh::select::{SelMode, Selection};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pivot {
    #[default]
    Median,
    Cursor,
    Individual,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FalloffName {
    Constant,
    Linear,
    Sharp,
    Root,
    Sphere,
    #[default]
    Smooth,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Proportional {
    #[serde(default = "default_radius")]
    pub radius: f64,
    #[serde(default)]
    pub falloff: FalloffName,
}

impl Default for Proportional {
    fn default() -> Self {
        Self { radius: default_radius(), falloff: FalloffName::Smooth }
    }
}

fn default_radius() -> f64 {
    512.0
}

fn default_scale() -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    #[serde(default = "default_vec")]
    pub translate: [f64; 3],
    #[serde(default = "default_axis")]
    pub axis: [f64; 3],
    #[serde(default)]
    pub angle_deg: f64,
    #[serde(default = "default_scale")]
    pub scale: [f64; 3],
    #[serde(default)]
    pub pivot: Pivot,
    #[serde(default = "default_vec")]
    pub cursor: [f64; 3],
    #[serde(default)]
    pub proportional: Option<Proportional>,
}

fn default_vec() -> [f64; 3] {
    [0.0; 3]
}

fn default_axis() -> [f64; 3] {
    [0.0, 0.0, 1.0]
}

pub fn falloff_weight(name: FalloffName, t: f64) -> f64 {
    if t >= 1.0 {
        return 0.0;
    }
    let x = (1.0 - t).clamp(0.0, 1.0);
    match name {
        FalloffName::Constant => 1.0,
        FalloffName::Linear => x,
        FalloffName::Sharp => x * x,
        FalloffName::Root => x.sqrt(),
        FalloffName::Sphere => (1.0 - (1.0 - x) * (1.0 - x)).max(0.0).sqrt(),
        FalloffName::Smooth => x * x * (3.0 - 2.0 * x),
    }
}

pub fn run(kind: &str, mesh: &mut EditMesh, sel: &Selection, params: &serde_json::Value) -> OpResult {
    let p: Params = parse_params(params)?;
    let translate = clamp_vec3(p.translate, -1e6, 1e6, "translate")?;
    let axis = DVec3::new(p.axis[0], p.axis[1], p.axis[2]);
    if !axis.is_finite() {
        return Err(bad("param axis is not finite"));
    }
    let angle = clamp_finite(p.angle_deg, -3600.0, 3600.0, "angle_deg")?;
    let scale = clamp_vec3(p.scale, -1e4, 1e4, "scale")?;
    let cursor = DVec3::new(p.cursor[0], p.cursor[1], p.cursor[2]);
    if !cursor.is_finite() {
        return Err(bad("param cursor is not finite"));
    }
    let adj = Adjacency::build(mesh);
    let mut picked = selected_verts(mesh, &adj, sel);
    if picked.is_empty() {
        picked = (0..mesh.positions.len() as u32).collect();
    }
    if picked.is_empty() {
        return Err(bad("the mesh has no vertices to move"));
    }
    let median = picked.iter().map(|v| mesh.positions[*v as usize]).sum::<DVec3>() / picked.len() as f64;
    let center = match p.pivot {
        Pivot::Median => median,
        Pivot::Cursor => cursor,
        Pivot::Individual => median,
    };
    let (prop_radius, prop_name) = match p.proportional {
        Some(pr) => (clamp_finite(pr.radius, 0.0, 1e6, "proportional.radius")?, pr.falloff),
        None => (0.0, FalloffName::Smooth),
    };
    let prop_on = p.proportional.is_some() && prop_radius > 0.0;
    let targets: BTreeSet<u32> = if prop_on {
        (0..mesh.positions.len() as u32)
            .filter(|v| {
                picked.contains(v) || (mesh.positions[*v as usize] - center).length() < prop_radius
            })
            .collect()
    } else {
        picked.clone()
    };
    let axis_n = axis.normalize_or_zero();
    if kind == "rotate" && axis_n == DVec3::ZERO {
        return Err(bad("param axis has no direction"));
    }
    let mut plan: Vec<(u32, DVec3, f64)> = Vec::with_capacity(targets.len());
    for v in &targets {
        let w = if picked.contains(v) {
            1.0
        } else {
            falloff_weight(prop_name, (mesh.positions[*v as usize] - center).length() / prop_radius)
        };
        let pivot = match p.pivot {
            Pivot::Individual => match sel.mode {
                SelMode::Vertex => mesh.positions[*v as usize],
                SelMode::Edge => match adj.edges.iter().find(|e| e.a == *v || e.b == *v) {
                    Some(e) => (mesh.positions[e.a as usize] + mesh.positions[e.b as usize]) * 0.5,
                    None => mesh.positions[*v as usize],
                },
                SelMode::Face => match adj.faces_of(*v).first() {
                    Some(f) => {
                        let pts = mesh.face_points(*f as usize);
                        if pts.is_empty() {
                            mesh.positions[*v as usize]
                        } else {
                            pts.iter().copied().sum::<DVec3>() / pts.len() as f64
                        }
                    }
                    None => mesh.positions[*v as usize],
                },
            },
            _ => center,
        };
        plan.push((*v, pivot, w));
    }
    for (v, pivot, w) in plan {
        if w <= 0.0 {
            continue;
        }
        let mut pos = mesh.positions[v as usize];
        match kind {
            "translate" => pos += DVec3::from_array(translate) * w,
            "rotate" => {
                let r = DQuat::from_axis_angle(axis_n, angle.to_radians() * w);
                pos = pivot + r * (pos - pivot);
            }
            "scale" => {
                let s = DVec3::from_array(scale);
                pos = pivot + (pos - pivot) * (DVec3::ONE + (s - DVec3::ONE) * w);
            }
            _ => return Err(bad(format!("unknown transform {kind}"))),
        }
        mesh.positions[v as usize] = pos;
    }
    Ok((Selection { mode: sel.mode, ids: sel.ids.clone() }, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falloffs_match_the_web_proportional_edit() {
        assert_eq!(falloff_weight(FalloffName::Constant, 0.5), 1.0);
        assert_eq!(falloff_weight(FalloffName::Constant, 1.5), 0.0);
        assert!((falloff_weight(FalloffName::Linear, 0.5) - 0.5).abs() < 1e-12);
        assert!((falloff_weight(FalloffName::Sharp, 0.5) - 0.25).abs() < 1e-12);
        assert!((falloff_weight(FalloffName::Root, 0.5) - 0.5f64.sqrt()).abs() < 1e-12);
        assert!((falloff_weight(FalloffName::Sphere, 0.0) - 1.0).abs() < 1e-12);
        assert_eq!(falloff_weight(FalloffName::Linear, 0.0), 1.0);
        assert!((falloff_weight(FalloffName::Sphere, 0.5) - (1.0 - 0.25f64).sqrt()).abs() < 1e-12);
        assert!((falloff_weight(FalloffName::Smooth, 0.5) - 0.5).abs() < 1e-12);
        assert_eq!(falloff_weight(FalloffName::Smooth, 2.0), 0.0);
    }
}
