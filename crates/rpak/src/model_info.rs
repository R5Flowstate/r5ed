//! The compiler-facing model description, built from a parsed RMDL.

use serde::{Deserialize, Serialize};

use crate::rmdl::RmdlModel;

/// Everything the compiler needs about one model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub path: String,
    pub guid: u64,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    /// True when the studiohdr declares physics contents (static prop collision).
    pub has_collision: bool,
    /// Every pak the asset and its dependencies were resolved out of.
    pub resident_paks: Vec<String>,
}

/// The studiohdr `contents` field's solid-geometry bit. A model whose
/// contents are zero still draws but carries no collision.
const CONTENTS_SOLID: i32 = 1;

impl ModelInfo {
    /// Builds the info from a parsed model. `path` and `guid` come from the
    /// asset list row the model was exported from.
    pub fn from_rmdl(model: &RmdlModel, path: &str, guid: u64, resident_paks: Vec<String>) -> Self {
        let _ = model;
        Self {
            path: path.to_string(),
            guid,
            bounds_min: model.bounds_min,
            bounds_max: model.bounds_max,
            has_collision: model.contents != 0,
            resident_paks,
        }
    }

    /// The centre of the model's bounding box.
    pub fn center(&self) -> [f32; 3] {
        let mut c = [0.0f32; 3];
        for k in 0..3 {
            c[k] = (self.bounds_min[k] + self.bounds_max[k]) * 0.5;
        }
        c
    }

    /// The full extent of the model's bounding box.
    pub fn size(&self) -> [f32; 3] {
        let mut s = [0.0f32; 3];
        for k in 0..3 {
            s[k] = self.bounds_max[k] - self.bounds_min[k];
        }
        s
    }

    pub fn contains(&self, point: [f32; 3]) -> bool {
        (0..3).all(|k| point[k] >= self.bounds_min[k] && point[k] <= self.bounds_max[k])
    }
}

/// True when a studiohdr `contents` value declares usable collision.
pub fn contents_has_collision(contents: i32) -> bool {
    contents & CONTENTS_SOLID != 0
}
