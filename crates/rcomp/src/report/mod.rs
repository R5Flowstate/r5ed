//! Compile counters and per-stage timings.

use std::collections::BTreeMap;
use std::time::Instant;

/// A point on the clock the stages are measured against.
///
/// The wasm target has no monotonic clock of its own and the browser's is only
/// reachable through the JS boundary, so a marker taken there is always the
/// same point and every stage measures as zero rather than aborting the
/// compile.
#[derive(Copy, Clone, Debug, Default)]
pub struct Clock(Option<Instant>);

impl Clock {
    /// A marker taken now.
    pub fn now() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            Clock(None)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Clock(Some(Instant::now()))
        }
    }

    /// Microseconds from this marker to one taken later.
    pub fn elapsed(self, later: Self) -> u128 {
        match (self.0, later.0) {
            (Some(start), Some(end)) => end.saturating_duration_since(start).as_micros(),
            _ => 0,
        }
    }
}

/// One measured stage. The duration is a diagnostic only, never an input to
/// any emitted byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage {
    pub name: String,
    pub micros: u128,
}

/// What the geometry, entity and prop stages produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub objects: usize,
    pub brushes: usize,
    pub faces: usize,
    pub render_meshes: usize,
    pub triangles: usize,
    pub surfaces: usize,
    pub collision_solids: usize,
    pub bvh_nodes: usize,
    pub bvh_leaves: usize,
    pub models: usize,
    pub props: usize,
    pub prop_models: usize,
    pub volumes: usize,
    pub lights: usize,
    pub paths: usize,
    pub terrains: usize,
    pub nav_triangles: usize,
    /// Entity count per classname, in classname order.
    pub entities_per_class: BTreeMap<String, usize>,
    pub entities: usize,
    pub partition_files: usize,
    pub vis_cells: usize,
    pub vis_objects: usize,
    pub lightmap_pages: usize,
    pub lightprobes: usize,
    pub cubemaps: usize,
    pub csm_shadow_meshes: usize,
}

impl Stats {
    pub fn count_class(&mut self, classname: &str) {
        *self.entities_per_class.entry(classname.to_string()).or_insert(0) += 1;
        self.entities += 1;
    }

    pub fn merge(&mut self, other: &Stats) {
        for (k, v) in &other.entities_per_class {
            *self.entities_per_class.entry(k.clone()).or_insert(0) += v;
        }
        self.objects += other.objects;
        self.brushes += other.brushes;
        self.faces += other.faces;
        self.render_meshes += other.render_meshes;
        self.triangles += other.triangles;
        self.surfaces += other.surfaces;
        self.collision_solids += other.collision_solids;
        self.bvh_nodes += other.bvh_nodes;
        self.bvh_leaves += other.bvh_leaves;
        self.models += other.models;
        self.props += other.props;
        self.prop_models += other.prop_models;
        self.volumes += other.volumes;
        self.lights += other.lights;
        self.paths += other.paths;
        self.terrains += other.terrains;
        self.nav_triangles += other.nav_triangles;
        self.entities += other.entities;
        self.partition_files += other.partition_files;
        self.vis_cells += other.vis_cells;
        self.vis_objects += other.vis_objects;
        self.lightmap_pages += other.lightmap_pages;
        self.lightprobes += other.lightprobes;
        self.cubemaps += other.cubemaps;
        self.csm_shadow_meshes += other.csm_shadow_meshes;
    }
}

/// What a compile produced and what it had to say about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub warnings: Vec<String>,
    pub stats: Stats,
    pub stages: Vec<Stage>,
    /// Engine validator issues, at the severity they were raised.
    pub validation: Vec<String>,
}

impl Report {
    pub fn warn(&mut self, message: impl Into<String>) {
        let text = message.into();
        if !self.warnings.contains(&text) {
            self.warnings.push(text);
        }
    }

    /// Records a stage and the microseconds it took. Stage order is the order
    /// the compiler ran them in, never sorted, so the report reads top to
    /// bottom.
    pub fn record(&mut self, name: &str, started: Clock) {
        self.stages.push(Stage { name: name.to_string(), micros: started.elapsed(Clock::now()) });
    }

    /// A one-line summary per stage, in run order.
    pub fn timing_lines(&self) -> Vec<String> {
        self.stages.iter().map(|s| format!("{} {}us", s.name, s.micros)).collect()
    }
}
