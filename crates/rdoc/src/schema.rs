//! The entity schema: what the editor's entity forms are built from.
//!
//! It merges three ground truths -- the engine's server factory table, its
//! per-class datamaps, and every key seen in shipped maps or read by game
//! scripts -- into one table of classes and keys.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::enttext::{self, EntBlock};
use crate::model::{Entity, Id, KeyValues, LightKind, Object, ObjectKind, Partition};
use crate::scriptkeys;

// ---- types -----------------------------------------------------------------

/// Types the editor offers a widget for. Matches the order the UI lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyType {
    Int,
    Float,
    Bool,
    String,
    Vec3,
    Angles,
    Color,
    Model,
    Material,
    Sound,
    Target,
    Flags,
    Enum,
}

impl KeyType {
    pub const ALL: [KeyType; 13] = [
        KeyType::Int,
        KeyType::Float,
        KeyType::Bool,
        KeyType::String,
        KeyType::Vec3,
        KeyType::Angles,
        KeyType::Color,
        KeyType::Model,
        KeyType::Material,
        KeyType::Sound,
        KeyType::Target,
        KeyType::Flags,
        KeyType::Enum,
    ];

    /// Whether a raw entity value fits the type.
    pub fn accepts(self, value: &str) -> bool {
        let v = value.trim();
        match self {
            KeyType::Int => v.parse::<i64>().is_ok(),
            KeyType::Float => v.parse::<f64>().is_ok_and(|n| n.is_finite()),
            KeyType::Bool => matches!(v.to_ascii_lowercase().as_str(), "0" | "1" | "true" | "false"),
            KeyType::Vec3 | KeyType::Angles => numbers(v) == 3,
            KeyType::Color => {
                let parts: Vec<&str> = v.split_whitespace().collect();
                (3..=4).contains(&parts.len())
                    && parts
                        .iter()
                        .all(|p| p.parse::<f64>().is_ok_and(|x| x.is_finite() && (0.0..=255.0).contains(&x)))
            }
            KeyType::Flags => v.parse::<i64>().is_ok_and(|n| n >= 0),
            _ => true,
        }
    }
}

/// How many whitespace-separated finite numbers the value holds, 0 when it is
/// not a number list.
fn numbers(v: &str) -> usize {
    let parts: Vec<&str> = v.split_whitespace().collect();
    if parts.is_empty() || !parts.iter().all(|p| p.parse::<f64>().is_ok_and(|n| n.is_finite())) {
        return 0;
    }
    parts.len()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassKind {
    Point,
    Brush,
    Both,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyDef {
    pub name: String,
    pub ty: KeyType,
    /// Most frequent values seen in shipped maps, most frequent first.
    pub observed_values: Vec<String>,
    /// The most common value, or `None` when no map shows the key.
    pub default: Option<String>,
    /// Present on at least 95% of the observed instances.
    pub required: bool,
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassSources {
    pub engine_factory: bool,
    pub engine_datamap: bool,
    /// Instances of this class found in the map set.
    pub maps: usize,
    /// Key reads in game scripts attributed to this class.
    pub scripts: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassDef {
    pub classname: String,
    pub cpp_class: String,
    /// C++ classes this class's key set is built from, most derived first.
    pub base_chain: Vec<String>,
    pub kind: ClassKind,
    pub partitions: BTreeMap<String, usize>,
    pub keys: BTreeMap<String, KeyDef>,
    /// Input handler names, i.e. what another entity can fire into it.
    pub inputs: Vec<String>,
    /// Output names, i.e. what the entity fires.
    pub outputs: Vec<String>,
    pub sources: ClassSources,
    pub example_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EntitySchema {
    pub classes: BTreeMap<String, ClassDef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    pub object: Option<Id>,
    pub message: String,
}

impl EntitySchema {
    pub fn load(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("entity schema does not parse: {e}"))
    }

    /// Pretty printed, two-space indent, LF, trailing newline.
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("EntitySchema always serializes");
        text.push('\n');
        text
    }

    pub fn class(&self, classname: &str) -> Option<&ClassDef> {
        self.classes.get(classname)
    }

    pub fn len(&self) -> usize {
        self.classes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Partition holding the majority of the class's observed instances.
    pub fn home_partition(&self, classname: &str) -> Option<Partition> {
        let def = self.classes.get(classname)?;
        Partition::ALL
            .into_iter()
            .filter_map(|p| def.partitions.get(p.name()).copied().map(|n| (p, n)))
            .filter(|(_, n)| *n > 0)
            .max_by_key(|(p, n)| (*n, std::cmp::Reverse(partition_rank(*p))))
            .map(|(p, _)| p)
    }

    pub fn keys(&self, classname: &str) -> Option<&BTreeMap<String, KeyDef>> {
        self.class(classname).map(|c| &c.keys)
    }

    /// Unknown class, unknown key, wrong value type, missing required key.
    pub fn validate_entity(&self, entity: &Entity) -> Vec<Issue> {
        self.validate_keys(None, &entity.classname, &entity.keyvalues)
    }

    /// The same checks for a document object, including its transform keys.
    pub fn validate_object(&self, object: &Object) -> Vec<Issue> {
        let Some((classname, keys)) = entity_of(object) else {
            return Vec::new();
        };
        // Lights, volumes, paths and props get their engine keys from the
        // compiler, so only a raw Entity must spell out the required ones.
        let mut satisfied: Vec<&str> = vec!["classname", "origin", "angles"];
        if !matches!(object.kind, crate::model::ObjectKind::Entity(_)) {
            if let Some(def) = self.class(classname) {
                satisfied.extend(def.keys.keys().map(|k| k.as_str()));
            }
        }
        let mut issues = self.validate_inner(Some(object.id), classname, keys, &satisfied);
        let Some(def) = self.class(classname) else { return issues };
        if let Some(k) = def.keys.get("origin") {
            let p = object.transform.position;
            issues.extend(check_value(Some(object.id), k.ty, "origin", &format!("{} {} {}", p.x, p.y, p.z)));
        }
        if let Some(k) = def.keys.get("angles") {
            let a = object.transform.to_source_angles();
            issues.extend(check_value(Some(object.id), k.ty, "angles", &format!("{} {} {}", a.x, a.y, a.z)));
        }
        issues
    }

    pub fn validate_keys(&self, object: Option<Id>, classname: &str, keys: &KeyValues) -> Vec<Issue> {
        self.validate_inner(object, classname, keys, &[])
    }

    /// `satisfied` lists keys the caller supplies outside the keyvalue block,
    /// which `validate_object` uses for the transform-derived ones.
    fn validate_inner(
        &self,
        object: Option<Id>,
        classname: &str,
        keys: &KeyValues,
        satisfied: &[&str],
    ) -> Vec<Issue> {
        let mut issues = Vec::new();
        let Some(def) = self.class(classname) else {
            issues.push(Issue {
                severity: Severity::Error,
                object,
                message: format!("classname {classname} is not a known entity class"),
            });
            return issues;
        };
        for (name, value) in keys {
            match def.keys.get(name) {
                Some(k) => issues.extend(check_value(object, k.ty, name, value)),
                None => issues.push(Issue {
                    severity: Severity::Warning,
                    object,
                    message: format!("{classname} has no key named {name}"),
                }),
            }
        }
        for (name, k) in &def.keys {
            if k.required && !keys.contains_key(name) && !satisfied.contains(&name.as_str()) {
                issues.push(Issue {
                    severity: Severity::Error,
                    object,
                    message: format!("{classname} requires the key {name}"),
                });
            }
        }
        issues
    }
}

fn partition_rank(p: Partition) -> usize {
    Partition::ALL.iter().position(|q| *q == p).unwrap_or(0)
}

/// Classname a document object compiles as.
pub fn entity_of(object: &Object) -> Option<(&str, &crate::model::KeyValues)> {
    match &object.kind {
        ObjectKind::Entity(e) => Some((e.classname.as_str(), &e.keyvalues)),
        ObjectKind::Volume(v) => Some((v.classname.as_str(), &v.keyvalues)),
        ObjectKind::Light(l) => {
            let classname = match l.kind {
                LightKind::Sun | LightKind::Area => "light_environment",
                LightKind::Point => "light",
                LightKind::Spot => "light_spot",
                LightKind::Sky => "sky_camera",
            };
            Some((classname, &l.keyvalues))
        }
        _ => None,
    }
}

fn check_value(object: Option<Id>, ty: KeyType, name: &str, value: &str) -> Vec<Issue> {
    if ty.accepts(value) {
        return Vec::new();
    }
    vec![Issue {
        severity: Severity::Error,
        object,
        message: format!("{name} expects {ty:?} but got {value:?}"),
    }]
}

// ---- generation ------------------------------------------------------------

/// One entity partition or ENTITIES lump of a map.
pub struct MapSource {
    pub path: String,
    pub partition: Partition,
    pub text: String,
}

/// One game script.
pub struct ScriptSource {
    pub path: String,
    pub text: String,
}

pub struct SchemaInput<'a> {
    pub factories_json: &'a str,
    pub datamaps_json: &'a str,
    pub maps: &'a [MapSource],
    pub scripts: &'a [ScriptSource],
}

/// Values recorded per key of a class, plus how many instances carried it.
#[derive(Default)]
struct Observations {
    instances: usize,
    partitions: BTreeMap<String, usize>,
    present: BTreeMap<String, usize>,
    values: BTreeMap<String, BTreeMap<String, usize>>,
    brush_instances: usize,
    point_instances: usize,
}

/// How many distinct values a key records.
const MAX_OBSERVED_VALUES: usize = 16;
/// How many script paths a key records.
const MAX_KEY_SOURCES: usize = 12;
/// Keys naming another entity, whose values are looked up in the target set.
const TARGET_KEYS: [&str; 3] = ["targetname", "script_name", "link_guid"];

/// Classes the engine registers but gives no datamap of its own, mapped to the
/// closest ancestor that has one. Everything else resolves by name prefix, and
/// then by falling back to `CBaseEntity`.
const ANCESTORS: &[(&str, &str)] = &[
    ("CTriggerOnce", "CTriggerMultiple"),
    ("CTriggerLocationSP", "CTriggerLocation"),
    ("CTriggerNoGrapple", "CBaseTrigger"),
    ("CTriggerNoZipline", "CBaseTrigger"),
    ("CNPC_Spectre", "CAI_BaseNPC"),
    ("CNPC_SuperSpectre", "CAI_BaseNPC"),
    ("CNPCProwlerSpawnPoint", "CAI_BaseNPC"),
    ("CSimplePhysicsProp", "CPhysicsProp"),
    ("CSimplePhysicsBrush", "CPhysicsProp"),
    ("CPushable", "CPhysicsProp"),
    ("CBaseGrenade", "CBaseAnimating"),
    ("CMissile", "CBaseAnimating"),
    ("CHealthKit", "CBaseAnimating"),
    ("CWindowPane", "CFuncBrush"),
    ("ScriptMoverLightweight", "CScriptMover"),
    ("DoorMover", "CScriptMover"),
];

const FALLBACK_CLASS: &str = "CBaseEntity";

#[derive(Deserialize)]
struct EngineFactories {
    #[serde(default)]
    factories: Vec<FactoryEntry>,
}

#[derive(Deserialize)]
struct FactoryEntry {
    #[serde(default)]
    cpp_class: String,
    #[serde(default)]
    classname: String,
}

#[derive(Deserialize)]
struct DatamapEntry {
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    fields: Vec<FieldEntry>,
}

#[derive(Deserialize)]
struct FieldEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: Vec<String>,
    #[serde(default, rename = "field_type")]
    field_type: i64,
}

#[derive(Clone, Debug)]
struct DatamapField {
    name: String,
    key: bool,
    input: bool,
    output: bool,
    field_type: i64,
}

struct ScriptHit {
    class: Option<String>,
    key: String,
    path: String,
}

/// Merges the engine tables, the map set and the script set into a schema.
pub fn generate(input: &SchemaInput) -> Result<EntitySchema, String> {
    let factories: EngineFactories = serde_json::from_str(input.factories_json).map_err(|e| e.to_string())?;
    let datamaps: BTreeMap<String, DatamapEntry> =
        serde_json::from_str(input.datamaps_json).map_err(|e| e.to_string())?;

    let mut fields: BTreeMap<String, Vec<DatamapField>> = BTreeMap::new();
    for (class, entry) in &datamaps {
        fields.insert(
            class.clone(),
            entry
                .fields
                .iter()
                .filter(|f| !f.name.is_empty())
                .map(|f| DatamapField {
                    name: f.name.clone(),
                    key: f.kind.iter().any(|k| k == "key"),
                    input: f.kind.iter().any(|k| k == "input"),
                    output: f.kind.iter().any(|k| k == "output"),
                    field_type: f.field_type,
                })
                .collect(),
        );
    }

    let mut observed: BTreeMap<String, Observations> = BTreeMap::new();
    let mut targets: BTreeSet<String> = BTreeSet::new();
    let mut script_names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for source in input.maps {
        let Ok((_, blocks)) = enttext::parse(&source.text) else {
            continue;
        };
        for block in &blocks {
            absorb(&mut observed, &mut targets, &mut script_names, source, block);
        }
    }

    let mut classnames: BTreeSet<String> = BTreeSet::new();
    let mut cpp_of: BTreeMap<String, String> = BTreeMap::new();
    for f in &factories.factories {
        if f.classname.is_empty() {
            continue;
        }
        classnames.insert(f.classname.clone());
        cpp_of.entry(f.classname.clone()).or_insert_with(|| f.cpp_class.clone());
    }
    for name in observed.keys() {
        classnames.insert(name.clone());
    }

    let hits = scan_scripts(input.scripts, &script_names, &classnames);

    let mut classes: BTreeMap<String, ClassDef> = BTreeMap::new();
    for classname in &classnames {
        classes.insert(classname.clone(), build_class(classname, &cpp_of, &datamaps, &fields, &observed, &targets, &hits));
    }
    Ok(EntitySchema { classes })
}

#[allow(clippy::too_many_arguments)]
fn build_class(
    classname: &str,
    cpp_of: &BTreeMap<String, String>,
    datamaps: &BTreeMap<String, DatamapEntry>,
    fields: &BTreeMap<String, Vec<DatamapField>>,
    observed: &BTreeMap<String, Observations>,
    targets: &BTreeSet<String>,
    hits: &[ScriptHit],
) -> ClassDef {
    let cpp = cpp_of.get(classname).cloned().unwrap_or_default();
    let obs = observed.get(classname);
    let (chain, own_datamap) = if cpp.is_empty() {
        (Vec::new(), false)
    } else {
        base_chain(&cpp, datamaps)
    };

    let mut keys: BTreeMap<String, KeyDef> = BTreeMap::new();
    let mut inputs: BTreeSet<String> = BTreeSet::new();
    let mut outputs: BTreeSet<String> = BTreeSet::new();
    // The chain is most derived first, so the first definition of a key wins.
    for class in &chain {
        let Some(list) = fields.get(class) else { continue };
        for f in list {
            if f.output {
                outputs.insert(f.name.clone());
            }
            if f.input && !f.key {
                inputs.insert(f.name.clone());
            }
            if f.key {
                keys.entry(f.name.clone()).or_insert_with(|| KeyDef {
                    name: f.name.clone(),
                    ty: engine_type(f.field_type),
                    observed_values: Vec::new(),
                    default: None,
                    required: false,
                    sources: vec!["engine_datamap".to_string()],
                });
            }
        }
    }

    let mut script_hits = 0usize;
    for hit in hits {
        let for_class = match &hit.class {
            Some(c) => c == classname,
            None => keys.contains_key(&hit.key),
        };
        if !for_class {
            continue;
        }
        script_hits += 1;
        if hit.class.is_some() {
            let entry = keys.entry(hit.key.clone()).or_insert_with(|| KeyDef {
                name: hit.key.clone(),
                ty: KeyType::String,
                observed_values: Vec::new(),
                default: None,
                required: false,
                sources: Vec::new(),
            });
            if entry.sources.len() < MAX_KEY_SOURCES && !entry.sources.iter().any(|s| s == &hit.path) {
                entry.sources.push(hit.path.clone());
            }
        }
    }

    if let Some(o) = obs {
        for name in o.values.keys() {
            keys.entry(name.clone()).or_insert_with(|| KeyDef {
                name: name.clone(),
                ty: KeyType::String,
                observed_values: Vec::new(),
                default: None,
                required: false,
                sources: vec!["maps".to_string()],
            });
        }
        for (name, values) in &o.values {
            let Some(def) = keys.get_mut(name) else { continue };
            def.observed_values = top_values(values);
            def.default = def.observed_values.first().cloned();
            let present = o.present.get(name).copied().unwrap_or(0) as u64;
            def.required = present * 100 >= (o.instances as u64) * 95;
            def.ty = infer_type(name, &def.observed_values, def.ty, targets);
            if !def.sources.iter().any(|s| s == "maps") {
                def.sources.push("maps".to_string());
            }
        }
    } else {
        for def in keys.values_mut() {
            def.ty = infer_type(&def.name, &[], def.ty, targets);
        }
    }

    let kind = match obs {
        Some(o) if o.brush_instances > 0 && o.point_instances > 0 => ClassKind::Both,
        Some(o) if o.brush_instances > 0 => ClassKind::Brush,
        Some(_) => ClassKind::Point,
        None if looks_like_brush(classname) => ClassKind::Brush,
        None => ClassKind::Point,
    };

    ClassDef {
        classname: classname.to_string(),
        cpp_class: cpp,
        base_chain: chain,
        kind,
        partitions: obs.map(|o| o.partitions.clone()).unwrap_or_default(),
        keys,
        inputs: inputs.into_iter().collect(),
        outputs: outputs.into_iter().collect(),
        sources: ClassSources {
            engine_factory: cpp_of.contains_key(classname),
            engine_datamap: own_datamap,
            maps: obs.map(|o| o.instances).unwrap_or(0),
            scripts: script_hits,
        },
        example_count: obs.map(|o| o.instances).unwrap_or(0),
    }
}

fn absorb(
    observed: &mut BTreeMap<String, Observations>,
    targets: &mut BTreeSet<String>,
    script_names: &mut BTreeMap<String, BTreeSet<String>>,
    source: &MapSource,
    block: &EntBlock,
) {
    let Some(classname) = block.classname() else { return };
    for key in TARGET_KEYS {
        if let Some(v) = block.keyvalues.get(key) {
            targets.insert(v.clone());
        }
    }
    if let Some(v) = block.keyvalues.get("script_name") {
        script_names.entry(v.clone()).or_default().insert(classname.to_string());
    }
    // A `*N` model key names a brush, so it is how a compiled map marks a
    // brush-backed entity.
    let brush = block.brush_model().is_some() || block.collider_model().is_some();
    let entry = observed.entry(classname.to_string()).or_default();
    entry.instances += 1;
    *entry.partitions.entry(source.partition.name().to_string()).or_insert(0) += 1;
    if brush {
        entry.brush_instances += 1;
    } else {
        entry.point_instances += 1;
    }
    for (name, value) in &block.keyvalues {
        *entry.present.entry(name.clone()).or_insert(0) += 1;
        *entry.values.entry(name.clone()).or_default().entry(value.clone()).or_insert(0) += 1;
    }
}

fn scan_scripts(
    sources: &[ScriptSource],
    script_names: &BTreeMap<String, BTreeSet<String>>,
    classnames: &BTreeSet<String>,
) -> Vec<ScriptHit> {
    let mut out = Vec::new();
    for source in sources {
        for hit in scriptkeys::scan(&source.path, &source.text) {
            let Some(scope) = hit.scope else {
                out.push(ScriptHit { class: None, key: hit.key, path: source.path.clone() });
                continue;
            };
            // A script-name filter resolves through the maps; a class filter is
            // already a classname. Anything else stays unattributed.
            let class = if let Some(classes) = script_names.get(&scope) {
                classes.iter().min().cloned()
            } else if classnames.contains(&scope) {
                Some(scope.clone())
            } else {
                None
            };
            out.push(ScriptHit { class, key: hit.key, path: source.path.clone() });
        }
    }
    out
}

/// Most frequent values first, ties broken by the value so runs are identical.
fn top_values(values: &BTreeMap<String, usize>) -> Vec<String> {
    let mut ranked: Vec<(&String, &usize)> =
        values.iter().filter(|(v, _)| v.is_ascii() && !v.is_empty()).collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    ranked.into_iter().take(MAX_OBSERVED_VALUES).map(|(v, _)| v.clone()).collect()
}

/// Name convention for classes the map set never instantiates: every brush
/// entity in the shipped maps matches one of these.
fn looks_like_brush(classname: &str) -> bool {
    classname.starts_with("trigger_")
        || classname.starts_with("func_")
        || classname.contains("volume")
        || classname.contains("brush")
}

/// The datamap's own field type, used until a value in a map says otherwise.
/// The codes are read off the key names the engine ships: 1 is a float
/// (`radius`, `HDRColorScale`), 3 a vector (`velocity`, `fogangles`), 9 a
/// colour (`rendercolor`, `fogcolor`) and 19 a sound (`StartSound`).
fn engine_type(field_type: i64) -> KeyType {
    match field_type {
        1 => KeyType::Float,
        3 => KeyType::Vec3,
        5 | 7 | 8 => KeyType::Int,
        6 => KeyType::Bool,
        9 => KeyType::Color,
        19 => KeyType::Sound,
        _ => KeyType::String,
    }
}

fn is_model_value(v: &str) -> bool {
    (v.starts_with('*') && v.len() > 1 && v[1..].chars().all(|c| c.is_ascii_digit()))
        || v.ends_with(".rmdl")
        || v.ends_with(".mdl")
        || v.starts_with("mdl/")
        || v.starts_with("models/")
}

fn looks_like_sound(name: &str, values: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.contains("soundname") || lower == "play_event" || lower == "playsound" || lower == "stopsound" {
        return true;
    }
    values.iter().all(|v| v.to_ascii_lowercase().ends_with(".wav"))
}

/// Ordered rules; the first that matches wins. The engine's field type is the
/// starting guess, and observed values refine it.
fn infer_type(name: &str, values: &[String], engine: KeyType, targets: &BTreeSet<String>) -> KeyType {
    let lower = name.to_ascii_lowercase();
    if lower == "spawnflags" {
        return KeyType::Flags;
    }
    if lower == "angles" || lower.ends_with("_angles") {
        return KeyType::Angles;
    }
    if lower == "origin" {
        return KeyType::Vec3;
    }
    if values.is_empty() {
        return engine;
    }
    let all = |f: fn(&str) -> bool| values.iter().all(|v| f(v));
    if values.iter().all(|v| targets.contains(v)) {
        return KeyType::Target;
    }
    if all(is_model_value) {
        return KeyType::Model;
    }
    let count = numbers(&values[0]);
    if lower.contains("color") && (3..=4).contains(&count) && values.iter().all(|v| numbers(v) == count) {
        return KeyType::Color;
    }
    if looks_like_sound(name, values) {
        return KeyType::Sound;
    }
    if all(|v| numbers(v) == 3) {
        return KeyType::Vec3;
    }
    if engine == KeyType::Bool {
        return KeyType::Bool;
    }
    let has_zero = values.iter().any(|v| v.trim() == "0");
    let has_one = values.iter().any(|v| v.trim() == "1");
    if has_zero && has_one && all(|v| matches!(v.trim(), "0" | "1")) {
        return KeyType::Bool;
    }
    if all(|v| v.trim().parse::<i64>().is_ok()) {
        return KeyType::Int;
    }
    if all(|v| v.trim().parse::<f64>().is_ok()) {
        return KeyType::Float;
    }
    engine
}

fn base_chain(cpp: &str, datamaps: &BTreeMap<String, DatamapEntry>) -> (Vec<String>, bool) {
    let mut chain: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cursor = Some(cpp.to_string());
    let own = datamaps.contains_key(cpp);
    while let Some(class) = cursor {
        if !seen.insert(class.clone()) {
            break;
        }
        chain.push(class.clone());
        cursor = match datamaps.get(&class).and_then(|d| d.base.clone()) {
            Some(base) if !base.is_empty() => Some(base),
            _ => None,
        };
    }
    if !own {
        if let Some(first) = chain.first_mut() {
            *first = resolve_ancestor(cpp, datamaps);
        }
        chain.truncate(1);
        let mut seen: BTreeSet<String> = BTreeSet::new();
        seen.insert(chain[0].clone());
        let mut cursor = datamaps.get(&chain[0]).and_then(|d| d.base.clone());
        while let Some(class) = cursor {
            if !seen.insert(class.clone()) {
                break;
            }
            cursor = datamaps.get(&class).and_then(|d| d.base.clone());
            chain.push(class);
        }
    }
    (chain, own)
}

fn resolve_ancestor(cpp: &str, datamaps: &BTreeMap<String, DatamapEntry>) -> String {
    let mut best: Option<&String> = None;
    for class in datamaps.keys() {
        if class == cpp || !cpp.starts_with(class.as_str()) {
            continue;
        }
        if best.map(|b| class.len() > b.len()).unwrap_or(true) {
            best = Some(class);
        }
    }
    if let Some(b) = best {
        return b.clone();
    }
    for (class, ancestor) in ANCESTORS {
        if *class == cpp {
            return (*ancestor).to_string();
        }
    }
    FALLBACK_CLASS.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec3;

    #[test]
    fn key_type_accepts_matching_values() {
        assert!(KeyType::Int.accepts("12"));
        assert!(!KeyType::Int.accepts("1.5"));
        assert!(KeyType::Float.accepts("-1.5e3"));
        assert!(KeyType::Bool.accepts("1"));
        assert!(!KeyType::Bool.accepts("2"));
        assert!(KeyType::Vec3.accepts("1 2 3"));
        assert!(!KeyType::Vec3.accepts("1 2"));
        assert!(KeyType::Color.accepts("255 128 0"));
        assert!(!KeyType::Color.accepts("1 2 3 4 5"));
        assert!(!KeyType::Color.accepts("1 2 999"));
        assert!(KeyType::Flags.accepts("4096"));
        assert!(!KeyType::Flags.accepts("-1"));
        assert!(KeyType::Target.accepts("anything"));
    }

    fn trigger_schema() -> EntitySchema {
        let mut schema = EntitySchema::default();
        schema.classes.insert(
            "trigger_multiple".into(),
            ClassDef {
                classname: "trigger_multiple".into(),
                cpp_class: "CTriggerMultiple".into(),
                base_chain: vec!["CTriggerMultiple".into(), "CBaseTrigger".into()],
                kind: ClassKind::Brush,
                partitions: BTreeMap::from([("main".to_string(), 4)]),
                keys: BTreeMap::from([
                    (
                        "spawnflags".to_string(),
                        KeyDef {
                            name: "spawnflags".into(),
                            ty: KeyType::Flags,
                            observed_values: vec!["0".into()],
                            default: Some("0".into()),
                            required: true,
                            sources: vec!["engine_datamap".into()],
                        },
                    ),
                    (
                        "magnitude".to_string(),
                        KeyDef {
                            name: "magnitude".into(),
                            ty: KeyType::Float,
                            observed_values: vec![],
                            default: None,
                            required: false,
                            sources: vec!["engine_datamap".into()],
                        },
                    ),
                ]),
                inputs: vec!["Trigger".into()],
                outputs: vec!["OnTrigger".into()],
                sources: ClassSources::default(),
                example_count: 4,
            },
        );
        schema
    }

    #[test]
    fn schema_round_trips_through_json() {
        let schema = trigger_schema();
        let text = schema.to_json();
        assert!(text.ends_with("}\n"));
        assert_eq!(EntitySchema::load(&text).unwrap(), schema);
        assert_eq!(schema.home_partition("trigger_multiple"), Some(Partition::Main));
        assert_eq!(schema.home_partition("nope"), None);
        assert!(EntitySchema::load("{").is_err());
        assert!(EntitySchema::load("[]").is_err());
    }

    #[test]
    fn validate_reports_unknown_wrong_and_missing() {
        let schema = trigger_schema();
        let good = Entity {
            classname: "trigger_multiple".into(),
            keyvalues: [("spawnflags".to_string(), "1".to_string())].into_iter().collect(),
            partition: None,
            links: Default::default(),
        };
        assert!(schema.validate_entity(&good).is_empty());

        let missing = Entity { keyvalues: Default::default(), ..good.clone() };
        let issues = schema.validate_entity(&missing);
        assert!(issues.iter().any(|i| i.severity == Severity::Error && i.message.contains("spawnflags")));

        let wrong = Entity {
            keyvalues: [
                ("spawnflags".to_string(), "abc".to_string()),
                ("magnitude".to_string(), "nope".to_string()),
            ]
            .into_iter()
            .collect(),
            ..good.clone()
        };
        let issues = schema.validate_entity(&wrong);
        assert_eq!(issues.iter().filter(|i| i.severity == Severity::Error).count(), 2);

        let unknown_key = Entity {
            keyvalues: [("spawnflags".into(), "0".into()), ("not_a_key".into(), "1".into())].into_iter().collect(),
            ..good.clone()
        };
        let issues = schema.validate_entity(&unknown_key);
        assert!(issues.iter().any(|i| i.severity == Severity::Warning && i.message.contains("not_a_key")));

        let unknown_class = Entity { classname: "not_a_class".into(), ..good };
        assert_eq!(schema.validate_entity(&unknown_class).len(), 1);
    }

    #[test]
    fn validate_object_covers_the_transform() {
        let mut schema = trigger_schema();
        let def = schema.classes.get_mut("trigger_multiple").unwrap();
        def.keys.insert(
            "origin".into(),
            KeyDef {
                name: "origin".into(),
                ty: KeyType::Vec3,
                observed_values: vec![],
                default: None,
                required: true,
                sources: vec![],
            },
        );
        def.keys.insert(
            "angles".into(),
            KeyDef {
                name: "angles".into(),
                ty: KeyType::Angles,
                observed_values: vec![],
                default: None,
                required: true,
                sources: vec![],
            },
        );
        let object = Object {
            id: Id::new_v4(),
            name: "t".into(),
            layer: Id::new_v4(),
            parent: None,
            transform: Default::default(),
            hidden: false,
            locked: false,
            kind: ObjectKind::Entity(Entity {
                classname: "trigger_multiple".into(),
                keyvalues: [("spawnflags".to_string(), "0".to_string())].into_iter().collect(),
                partition: None,
                links: Default::default(),
            }),
        };
        let issues = schema.validate_object(&object);
        assert!(issues.is_empty(), "{issues:?}");
        let broken = Object {
            transform: crate::model::Transform {
                position: DVec3::splat(f64::NAN),
                ..Default::default()
            },
            ..object
        };
        let issues = schema.validate_object(&broken);
        assert!(issues.iter().any(|i| i.severity == Severity::Error && i.message.contains("origin")));
    }

    const FACTORIES: &str =
        r#"{"factories":[{"cpp_class":"CBaseEntity","classname":"info_target"},{"cpp_class":"CTriggerOnce","classname":"trigger_once"}]}"#;
    const DATAMAPS: &str = r#"{
        "CBaseEntity":{"base":null,"fields":[
            {"field":"m_iSpawnFlags","name":"spawnflags","kind":["key"],"field_type":5},
            {"field":"m_ModelName","name":"model","kind":["key"],"field_type":7}]},
        "CTriggerMultiple":{"base":"CBaseEntity","fields":[
            {"field":"m_OnTrigger","name":"OnTrigger","kind":["key","output"],"field_type":11},
            {"field":"m_InputName","name":"Trigger","kind":["input"],"field_type":0}]}}"#;

    #[test]
    fn generation_is_deterministic_for_equal_input() {
        let maps = vec![MapSource {
            path: "m".into(),
            partition: Partition::Main,
            text: "ENTITIES02 num_models=1\n{\n\"classname\" \"info_target\"\n\"targetname\" \"t1\"\n\"spawnflags\" \"4\"\n}\n"
                .into(),
        }];
        let input = SchemaInput { factories_json: FACTORIES, datamaps_json: DATAMAPS, maps: &maps, scripts: &[] };
        let a = generate(&input).unwrap();
        let b = generate(&input).unwrap();
        assert_eq!(a.to_json(), b.to_json());
        let def = a.class("info_target").unwrap();
        assert!(def.sources.engine_factory);
        assert!(def.sources.engine_datamap);
        assert_eq!(def.example_count, 1);
        assert_eq!(a.home_partition("info_target"), Some(Partition::Main));
        let flags = &def.keys["spawnflags"];
        assert_eq!(flags.ty, KeyType::Flags);
        assert!(flags.required);
        assert_eq!(flags.default.as_deref(), Some("4"));
    }

    #[test]
    fn a_factory_class_without_a_datamap_inherits_one() {
        let input =
            SchemaInput { factories_json: FACTORIES, datamaps_json: DATAMAPS, maps: &[], scripts: &[] };
        let schema = generate(&input).unwrap();
        let def = schema.class("trigger_once").expect("class is present");
        assert_eq!(def.base_chain, vec!["CTriggerMultiple".to_string(), "CBaseEntity".to_string()]);
        assert!(def.keys.contains_key("spawnflags"), "inherited from the ancestor");
        assert_eq!(def.outputs, vec!["OnTrigger".to_string()]);
        assert_eq!(def.inputs, vec!["Trigger".to_string()]);
        assert!(!def.sources.engine_datamap, "the class itself ships no datamap");
    }

    #[test]
    fn brush_and_point_classes_come_from_the_maps() {
        let text = "{\n\"model\" \"*3\"\n\"classname\" \"trigger_hurt\"\n}\n\
                    {\n\"model\" \"*4\"\n\"classname\" \"trigger_hurt\"\n}\n\
                    {\n\"classname\" \"info_target\"\n}\n";
        let maps = vec![MapSource { path: "m".into(), partition: Partition::Main, text: text.into() }];
        let input = SchemaInput { factories_json: FACTORIES, datamaps_json: DATAMAPS, maps: &maps, scripts: &[] };
        let schema = generate(&input).unwrap();
        assert_eq!(schema.class("trigger_hurt").unwrap().kind, ClassKind::Brush);
        assert_eq!(schema.class("info_target").unwrap().kind, ClassKind::Point);
        assert_eq!(schema.class("trigger_hurt").unwrap().example_count, 2);
    }

    #[test]
    fn script_keys_reach_the_classes_they_belong_to() {
        let scripts = vec![ScriptSource {
            path: "vscripts/a.nut".into(),
            text: r#"local e = GetEntArrayByClass_Expensive("info_target")
                      foreach (t in e) { t.GetValueForKey("mykey") t.GetValueForKey("spawnflags") }"#
                .into(),
        }];
        let input =
            SchemaInput { factories_json: FACTORIES, datamaps_json: DATAMAPS, maps: &[], scripts: &scripts };
        let schema = generate(&input).unwrap();
        let def = schema.class("info_target").unwrap();
        assert_eq!(def.sources.scripts, 2);
        assert_eq!(def.keys["mykey"].sources, vec!["vscripts/a.nut".to_string()]);
        assert!(def.keys["spawnflags"].sources.contains(&"engine_datamap".to_string()));
    }
}
