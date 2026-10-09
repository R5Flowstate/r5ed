//! ReMap project import.
//!
//! Field names mirror the ReMap project model: objects carry `customType`,
//! `parentId`, Unity-metre `position`/`rotation`/`scale`, and an `x-headless`
//! block with the Source-unit position and angles the game output uses.
//! Positions are converted from Unity metres (Y up) to Source inches (Z up).

use glam::{DQuat, DVec3, EulerRot};
use serde::Deserialize;

use crate::brush::box_brush;
use crate::command::Command;
use crate::doc::{new_object, Document};
use crate::model::{Entity, Id, KeyValues, Object, ObjectKind, PathKind, PathObject, Prop, Transform, Volume, VolumeKind};

use super::ImportError;

/// Metres per Source unit.
const METERS_PER_UNIT: f64 = 0.0254;
/// The jump pad model the ReMap tool places for a jump pad.
const JUMP_PAD_MODEL: &str = "mdl/props/octane_jump_pad/octane_jump_pad.rmdl";
/// The editor marker a ReMap sound emitter compiles as.
const SOUND_NODE_MODEL: &str = "mdl/dev/editor_ambient_generic_node.rmdl";
const SOUND_MODEL: &str = "mdl/props/cabinets/industrial1/cabinet1_door.rmdl";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReMapProject {
    #[serde(default)]
    name: String,
    #[serde(default)]
    origin_offset: Float3,
    #[serde(default)]
    objects: Vec<ReMapObject>,
}

#[derive(Deserialize, Default, Clone, Copy, Debug)]
pub struct Float3 {
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub z: f64,
}

impl Float3 {
    fn is_finite(&self) -> bool {
        [self.x, self.y, self.z].iter().all(|v| v.is_finite())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReMapObject {
    #[serde(default)]
    id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    parent_id: String,
    #[serde(default)]
    is_group: bool,
    #[serde(default)]
    game_model_path: String,
    #[serde(default)]
    custom_type: String,
    #[serde(default)]
    custom_role: String,
    #[serde(default)]
    zipline_start_id: String,
    #[serde(default)]
    zipline_end_id: String,
    #[serde(default)]
    zipline_mode: String,
    #[serde(default)]
    zipline_width: f64,
    #[serde(default)]
    zipline_speed: f64,
    #[serde(default)]
    zipline_length_scale: f64,
    #[serde(default)]
    zipline_fade_distance: f64,
    #[serde(default)]
    zipline_drop_to_bottom: bool,
    #[serde(default)]
    zipline_preserve_velocity: bool,
    #[serde(default)]
    jump_pad_launch_velocity: f64,
    #[serde(default)]
    jump_pad_radius: f64,
    #[serde(default)]
    jump_pad_double_jump: bool,
    #[serde(default)]
    trigger_radius: f64,
    #[serde(default)]
    trigger_half_height: f64,
    #[serde(default)]
    spawn_point_team: i64,
    #[serde(default)]
    door_type: String,
    #[serde(default)]
    door_gold: bool,
    #[serde(default)]
    sound_name: String,
    #[serde(default)]
    sound_radius: f64,
    #[serde(default)]
    allow_mantle: bool,
    #[serde(default)]
    fade_distance: f64,
    #[serde(default)]
    solid: bool,
    #[serde(default)]
    position: Float3,
    #[serde(default)]
    rotation: Float3,
    #[serde(default)]
    scale: Float3,
    #[serde(default)]
    headless: Option<Headless>,
    #[serde(default, rename = "x-headless")]
    x_headless: Option<Headless>,
}

#[derive(Deserialize, Default, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
struct Headless {
    #[serde(default)]
    pos_apex: [f64; 3],
    #[serde(default)]
    ang_apex: [f64; 3],
}

impl Headless {
    fn is_finite(&self) -> bool {
        self.pos_apex.iter().chain(self.ang_apex.iter()).all(|v| v.is_finite())
    }
}

/// Unity metres, Y up, to Source units, Z up.
pub fn unity_to_apex(v: Float3) -> DVec3 {
    DVec3::new(v.x / METERS_PER_UNIT, v.z / METERS_PER_UNIT, v.y / METERS_PER_UNIT)
}

/// Unity scale, Y up, to Source scale, Z up.
pub fn unity_to_apex_scale(v: Float3) -> DVec3 {
    if v.x.is_finite() && v.y.is_finite() && v.z.is_finite() && v.x.abs() > 1e-6 && v.y.abs() > 1e-6 && v.z.abs() > 1e-6
    {
        DVec3::new(v.x, v.z, v.y)
    } else {
        DVec3::ONE
    }
}

/// Unity euler angles to the Source `pitch yaw roll` the engine reads.
pub fn unity_angles_to_source(x: f64, y: f64, z: f64) -> DVec3 {
    let u = DQuat::from_euler(EulerRot::YXZ, y.to_radians(), x.to_radians(), z.to_radians());
    let source = DQuat::from_xyzw(-u.x, -u.z, -u.y, u.w);
    let f = source * DVec3::X;
    let side = source * DVec3::Y;
    let up = source * DVec3::Z;
    let planar = (f.x * f.x + f.y * f.y).sqrt();
    let pitch = (-f.z).atan2(planar);
    let yaw = if planar > 1e-5 { f.y.atan2(f.x) } else { (-side.x).atan2(side.y) };
    let roll = if planar > 1e-5 { side.z.atan2(up.z) } else { 0.0 };
    DVec3::new(pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees())
}

fn num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// True for the custom types that only position or skin a part of their
/// parent, and are folded into it on import.
fn is_folded(custom_type: &str) -> bool {
    custom_type.ends_with("-component")
        || matches!(
            custom_type,
            "zipline-endpoint"
                | "curved-zipline-point"
                | "ziprail-point"
                | "sound-point"
                | "trigger-teleport-target"
                | "button-teleport-target"
        )
}

/// Turns a ReMap project into one batch of edits.
pub fn import(doc: &Document, text: &str) -> Result<Command, ImportError> {
    let project: ReMapProject = serde_json::from_str(text).map_err(|e| ImportError::Unsupported(e.to_string()))?;
    if project.objects.len() > 100_000 {
        return Err(ImportError::Unsupported("the project has more than 100000 objects".into()));
    }
    let layer = doc
        .layer_ids()
        .first()
        .copied()
        .ok_or_else(|| ImportError::Unsupported("document has no layer".into()))?;
    let offset = if project.origin_offset.is_finite() {
        unity_to_apex(project.origin_offset)
    } else {
        DVec3::ZERO
    };

    let by_id: std::collections::HashMap<&str, &ReMapObject> =
        project.objects.iter().map(|o| (o.id.as_str(), o)).collect();
    let remap: std::collections::HashMap<&str, Id> = project
        .objects
        .iter()
        .map(|o| (o.id.as_str(), crate::doc::new_id()))
        .collect();

    let mut objects: Vec<Object> = Vec::new();
    for source in &project.objects {
        if is_folded(&source.custom_type) {
            continue;
        }
        let position = position_of(source, offset);
        let angles = angles_of(source);
        let name = if source.display_name.is_empty() { source.custom_type.clone() } else { source.display_name.clone() };
        let parent = if source.parent_id.is_empty() { None } else { remap.get(source.parent_id.as_str()).copied() };
        let mut object = new_object(name, layer, ObjectKind::Group);
        object.id = remap[source.id.as_str()];
        object.parent = parent;
        object.transform = Transform::from_source_angles(position, angles);
        object.transform.scale = unity_to_apex_scale(source.scale);
        object.kind = kind_of(source, &project, &by_id);
        objects.push(object);
    }
    if objects.is_empty() {
        return Err(ImportError::Unsupported("the project contains no placeable objects".into()));
    }
    let label = if project.name.is_empty() { "remap".to_string() } else { format!("remap {}", project.name) };
    Ok(Command::Batch { label: format!("import {label}"), commands: vec![Command::AddObjects { objects }] })
}

fn position_of(source: &ReMapObject, offset: DVec3) -> DVec3 {
    let headless = source.x_headless.or(source.headless);
    if let Some(h) = headless {
        if h.is_finite() {
            return DVec3::new(h.pos_apex[0], h.pos_apex[1], h.pos_apex[2]) + offset;
        }
    }
    if source.position.is_finite() {
        return unity_to_apex(source.position) + offset;
    }
    DVec3::ZERO
}

fn angles_of(source: &ReMapObject) -> DVec3 {
    let headless = source.x_headless.or(source.headless);
    if let Some(h) = headless {
        if h.is_finite() {
            return DVec3::new(h.ang_apex[0], h.ang_apex[1], h.ang_apex[2]);
        }
    }
    if source.rotation.is_finite() {
        return unity_angles_to_source(source.rotation.x, source.rotation.y, source.rotation.z);
    }
    DVec3::ZERO
}

fn prop_kind(model: &str, fade_distance: f64, allow_mantle: bool, solid: bool) -> ObjectKind {
    ObjectKind::Prop(Prop {
        model: model.to_string(),
        skin: 0,
        static_prop: false,
        solid,
        cast_shadows: true,
        fade_distance: if fade_distance.is_finite() { fade_distance as f32 } else { -1.0 },
        keyvalues: [("can_mantle".to_string(), if allow_mantle { "1".into() } else { "0".to_string() })]
            .into_iter()
            .collect(),
    })
}

fn kind_of(source: &ReMapObject, project: &ReMapProject, by_id: &std::collections::HashMap<&str, &ReMapObject>) -> ObjectKind {
    let model = if source.game_model_path.is_empty() { "mdl/dev/editor_models/editor_cube.rmdl" } else { source.game_model_path.as_str() };
    match source.custom_type.as_str() {
        "" => {
            if source.is_group || source.game_model_path.is_empty() {
                ObjectKind::Group
            } else {
                prop_kind(model, source.fade_distance, source.allow_mantle, source.solid)
            }
        }
        "zipline" => {
            let mut points: Vec<DVec3> = Vec::new();
            for id in [&source.zipline_start_id, &source.zipline_end_id] {
                if id.is_empty() {
                    continue;
                }
                if let Some(end) = by_id.get(id.as_str()) {
                    points.push(position_of(end, DVec3::ZERO));
                }
            }
            if points.len() < 2 {
                points = vec![DVec3::ZERO, DVec3::new(0.0, 0.0, 128.0)];
            }
            ObjectKind::Path(PathObject {
                kind: PathKind::Zipline,
                points,
                keyvalues: zipline_keys(source),
            })
        }
        "curved-zipline" => {
            let mut points: Vec<DVec3> = project
                .objects
                .iter()
                .filter(|c| c.parent_id == source.id && c.custom_type == "curved-zipline-point")
                .filter_map(|c| {
                    let role: usize = c.custom_role.parse().ok()?;
                    Some((role, position_of(c, DVec3::ZERO)))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(_, p)| p)
                .collect();
            if points.len() < 2 {
                points = vec![DVec3::ZERO, DVec3::new(0.0, 0.0, 128.0)];
            }
            ObjectKind::Path(PathObject { kind: PathKind::Spline, points, keyvalues: zipline_keys(source) })
        }
        "ziprail" => {
            let mut points: Vec<DVec3> = project
                .objects
                .iter()
                .filter(|c| c.parent_id == source.id && c.custom_type == "ziprail-point")
                .filter_map(|c| {
                    let role: usize = c.custom_role.parse().ok()?;
                    Some((role, position_of(c, DVec3::ZERO)))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(_, p)| p)
                .collect();
            if points.len() < 2 {
                points = vec![DVec3::ZERO, DVec3::new(128.0, 0.0, 0.0)];
            }
            ObjectKind::Path(PathObject { kind: PathKind::Ziprail, points, keyvalues: zipline_keys(source) })
        }
        "trigger" => {
            let radius = if source.trigger_radius.is_finite() && source.trigger_radius > 0.0 { source.trigger_radius } else { 100.0 };
            let half = if source.trigger_half_height.is_finite() && source.trigger_half_height > 0.0 { source.trigger_half_height } else { 50.0 };
            let brush = box_brush(
                DVec3::new(-radius, -radius, -half),
                DVec3::new(radius, radius, half),
                "trigger/hurt",
            );
            let keyvalues: KeyValues = [("magnitude".to_string(), "0".to_string())].into_iter().collect();
            ObjectKind::Volume(Volume {
                kind: VolumeKind::Trigger,
                classname: "trigger_multiple".into(),
                brushes: brush.into_iter().collect(),
                keyvalues,
            })
        }
        "jump-pad" => ObjectKind::Prop(Prop {
            model: JUMP_PAD_MODEL.into(),
            skin: 0,
            static_prop: false,
            solid: true,
            cast_shadows: true,
            fade_distance: 0.0,
            keyvalues: [
                ("JumpPadLaunchVelocity".to_string(), num(source.jump_pad_launch_velocity)),
                ("JumpPadRadius".to_string(), num(source.jump_pad_radius)),
                ("JumpPadDoubleJump".to_string(), if source.jump_pad_double_jump { "1".into() } else { "0".to_string() }),
            ]
            .into_iter()
            .collect(),
        }),
        "door" => ObjectKind::Entity(Entity {
            classname: "prop_door".into(),
            keyvalues: [
                ("model".to_string(), if source.game_model_path.is_empty() { SOUND_MODEL.to_string() } else { source.game_model_path.clone() }),
                ("skin".to_string(), if source.door_gold { "1".into() } else { "0".to_string() }),
                ("doorType".to_string(), if source.door_type.is_empty() { "single".into() } else { source.door_type.clone() }),
            ]
            .into_iter()
            .collect(),
            partition: None,
            links: Default::default(),
        }),
        "spawn-point" => ObjectKind::Entity(Entity {
            classname: "info_spawnpoint_human".into(),
            keyvalues: [("spawnPointTeam".to_string(), num(source.spawn_point_team as f64))]
                .into_iter()
                .collect(),
            partition: None,
            links: Default::default(),
        }),
        "sound" => ObjectKind::Entity(Entity {
            classname: "ambient_generic".into(),
            keyvalues: [
                ("model".to_string(), SOUND_NODE_MODEL.to_string()),
                ("soundName".to_string(), source.sound_name.clone()),
                ("radius".to_string(), num(source.sound_radius)),
            ]
            .into_iter()
            .collect(),
            partition: None,
            links: Default::default(),
        }),
        _ => {
            if source.is_group {
                ObjectKind::Group
            } else {
                prop_kind(model, source.fade_distance, source.allow_mantle, source.solid)
            }
        }
    }
}

fn zipline_keys(source: &ReMapObject) -> KeyValues {
    [
        ("ZiplineVertical".to_string(), if source.zipline_mode == "vertical" { "1".into() } else { "0".to_string() }),
        ("Width".to_string(), num(source.zipline_width)),
        ("ZiplineSpeedScale".to_string(), num(source.zipline_speed)),
        ("ZiplineLengthScale".to_string(), num(source.zipline_length_scale)),
        ("ZiplineFadeDistance".to_string(), num(source.zipline_fade_distance)),
        ("ZiplineDropToBottom".to_string(), if source.zipline_drop_to_bottom { "1".into() } else { "0".to_string() }),
        ("ZiplinePreserveVelocity".to_string(), if source.zipline_preserve_velocity { "1".into() } else { "0".to_string() }),
        ("Material".to_string(), "cable/zipline.vmt".to_string()),
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, MemoryFileStore};
    use crate::model::MapDoc;

    const SMALL_PROJECT: &str = r#"{
 "schemaVersion": 1,
 "name": "duel_plateau",
 "coordinateSystem": "unity-y-up-meters",
 "gameTarget": "r5flowstate",
 "editingMap": "mp_rr_canyonlands_staging_mu1",
 "targetMaps": ["mp_rr_canyonlands_staging_mu1"],
 "originOffset": { "x": 0, "y": 0, "z": 0 },
 "objects": [
  {
   "id": "000000000000000000000000000000a8",
   "displayName": "pad_00",
   "parentId": "",
   "isGroup": false,
   "gameModelPath": "mdl/canyonlands/firingrange_mu1_vert_course_platform_01.rmdl",
   "customType": "",
   "position": { "x": -13.0048, "y": -2.105025, "z": 19.5072 },
   "rotation": { "x": 0.0, "y": -0.0, "z": 0.0 },
   "scale": { "x": 1.0, "y": 1.0, "z": 1.0 },
   "fadeDistance": 50000.0,
   "realmId": -1,
   "allowMantle": true,
   "solid": true,
   "x-headless": { "posApex": [-512.0, 768.0, -82.875], "angApex": [0.0, 0.0, 0.0] }
  },
  {
   "id": "000000000000000000000000000000a9",
   "displayName": "desks",
   "parentId": "",
   "isGroup": true,
   "gameModelPath": "",
   "customType": "",
   "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
   "rotation": { "x": 0.0, "y": 0.0, "z": 0.0 },
   "scale": { "x": 1.0, "y": 1.0, "z": 1.0 }
  },
  {
   "id": "000000000000000000000000000000aa",
   "displayName": "chair",
   "parentId": "000000000000000000000000000000a9",
   "isGroup": false,
   "gameModelPath": "mdl/props/canyonlands/chair_01.rmdl",
   "customType": "",
   "position": { "x": 1.0, "y": 0.0, "z": 0.0 },
   "rotation": { "x": 0.0, "y": 0.0, "z": 90.0 },
   "scale": { "x": 1.0, "y": 1.0, "z": 1.0 }
  },
  {
   "id": "000000000000000000000000000000ab",
   "displayName": "cable",
   "parentId": "",
   "isGroup": true,
   "customType": "zipline",
   "ziplineStartId": "000000000000000000000000000000ac",
   "ziplineEndId": "000000000000000000000000000000ad",
   "ziplineMode": "vertical",
   "ziplineWidth": 2.0,
   "ziplineSpeed": 1.0,
   "ziplineLengthScale": 1.0,
   "ziplineFadeDistance": -1.0,
   "ziplineDropToBottom": true,
   "ziplinePreserveVelocity": false,
   "position": { "x": 0.0, "y": 0.0, "z": 0.0 }
  },
  {
   "id": "000000000000000000000000000000ac",
   "displayName": "cable_start",
   "parentId": "000000000000000000000000000000ab",
   "customType": "zipline-endpoint",
   "customRole": "start",
   "position": { "x": 0.0, "y": 0.0, "z": 3.0 }
  },
  {
   "id": "000000000000000000000000000000ad",
   "displayName": "cable_end",
   "parentId": "000000000000000000000000000000ab",
   "customType": "zipline-endpoint",
   "customRole": "end",
   "position": { "x": 0.0, "y": 0.0, "z": 8.0 }
  },
  {
   "id": "000000000000000000000000000000ae",
   "displayName": "hotzone",
   "parentId": "",
   "isGroup": true,
   "customType": "trigger",
   "triggerRadius": 45.0,
   "triggerHalfHeight": 50.0,
   "position": { "x": 2.54, "y": 0.0, "z": 0.0 }
  },
  {
   "id": "000000000000000000000000000000af",
   "displayName": "launcher",
   "parentId": "",
   "isGroup": false,
   "gameModelPath": "mdl/props/octane_jump_pad/octane_jump_pad.rmdl",
   "customType": "jump-pad",
   "jumpPadLaunchVelocity": 1000.0,
   "jumpPadRadius": 45.0,
   "jumpPadDoubleJump": true,
   "position": { "x": 5.08, "y": 0.0, "z": 0.0 }
  },
  {
   "id": "000000000000000000000000000000b0",
   "displayName": "gate",
   "parentId": "",
   "isGroup": true,
   "customType": "door",
   "doorType": "double",
   "doorGold": true,
   "gameModelPath": "mdl/props/cabinets/industrial1/cabinet1_door.rmdl",
   "position": { "x": 7.62, "y": 0.0, "z": 0.0 }
  },
  {
   "id": "000000000000000000000000000000b1",
   "displayName": "human_spawn",
   "parentId": "",
   "isGroup": false,
   "customType": "spawn-point",
   "spawnPointTeam": 1,
   "position": { "x": 10.16, "y": 0.0, "z": 0.0 }
  }
 ]
}"#;

    fn doc() -> Document {
        Document::new(MapDoc::default(), Box::new(MemoryFileStore::new()))
    }

    #[test]
    fn remap_import_small() {
        let mut d = doc();
        let (_, ids) = d.apply(import(&d, SMALL_PROJECT).unwrap()).unwrap();
        assert_eq!(ids.len(), 8, "the two zipline endpoints fold into the path");
        let names: Vec<&str> = d.doc.objects.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["pad_00", "desks", "chair", "cable", "hotzone", "launcher", "gate", "human_spawn"]
        );

        let pad = &d.doc.objects[0];
        let ObjectKind::Prop(p) = &pad.kind else { panic!("expected a prop") };
        assert_eq!(p.model, "mdl/canyonlands/firingrange_mu1_vert_course_platform_01.rmdl");
        assert!(p.solid);
        assert_eq!(pad.transform.position, DVec3::new(-512.0, 768.0, -82.875));

        let desks = d.doc.objects.iter().find(|o| o.name == "desks").unwrap();
        assert!(desks.kind == ObjectKind::Group);
        let chair = d.doc.objects.iter().find(|o| o.name == "chair").unwrap();
        assert_eq!(chair.parent, Some(desks.id), "hierarchy is preserved");
        assert!(matches!(chair.kind, ObjectKind::Prop(_)));

        let cable = d.doc.objects.iter().find(|o| o.name == "cable").unwrap();
        let ObjectKind::Path(path) = &cable.kind else { panic!("expected a path") };
        assert_eq!(path.kind, PathKind::Zipline);
        assert_eq!(path.points.len(), 2);
        // Unity metres, Y up, so the vertical run lands on the Source Y axis.
        assert!((path.points[0].y - 118.1102362).abs() < 1e-4, "{:?}", path.points[0]);
        assert!((path.points[1].y - 314.9606).abs() < 1e-4, "{:?}", path.points[1]);
        assert!(path.points[0].x.abs() < 1e-9 && path.points[1].z.abs() < 1e-9);
        assert_eq!(path.keyvalues["ZiplineVertical"], "1");
        assert_eq!(path.keyvalues["Width"], "2");

        let hotzone = d.doc.objects.iter().find(|o| o.name == "hotzone").unwrap();
        let ObjectKind::Volume(v) = &hotzone.kind else { panic!("expected a volume") };
        assert_eq!(v.classname, "trigger_multiple");
        assert_eq!(v.brushes.len(), 1);
        assert!(crate::brush::Solid::from_faces(&v.brushes[0].faces).is_ok());
        assert!((hotzone.transform.position.x - 100.0).abs() < 1e-6);

        let launcher = d.doc.objects.iter().find(|o| o.name == "launcher").unwrap();
        let ObjectKind::Prop(p) = &launcher.kind else { panic!("expected a jump pad prop") };
        assert_eq!(p.model, JUMP_PAD_MODEL);
        assert_eq!(p.keyvalues["JumpPadLaunchVelocity"], "1000");

        let gate = d.doc.objects.iter().find(|o| o.name == "gate").unwrap();
        let ObjectKind::Entity(e) = &gate.kind else { panic!("expected a door entity") };
        assert_eq!(e.classname, "prop_door");
        assert_eq!(e.keyvalues["skin"], "1");
        assert_eq!(e.keyvalues["doorType"], "double");

        let spawn = d.doc.objects.iter().find(|o| o.name == "human_spawn").unwrap();
        let ObjectKind::Entity(e) = &spawn.kind else { panic!("expected a spawn entity") };
        assert_eq!(e.classname, "info_spawnpoint_human");
        assert_eq!(e.keyvalues["spawnPointTeam"], "1");
        assert!((spawn.transform.position.x - 400.0).abs() < 1e-6);
    }

    #[test]
    fn unity_and_source_coordinates_agree() {
        // 2.54 m is exactly 100 Source units.
        assert!((unity_to_apex(Float3 { x: 2.54, y: 0.0, z: 0.0 }).x - 100.0).abs() < 1e-9);
        // Unity Y is up, Source Z is up.
        let p = unity_to_apex(Float3 { x: 0.0, y: 5.08, z: 0.0 });
        assert!((p.z - 200.0).abs() < 1e-9);
        assert!((p.x - 0.0).abs() < 1e-9);
        assert_eq!(unity_angles_to_source(0.0, 0.0, 0.0), DVec3::ZERO);
    }

    #[test]
    fn a_yaw_of_ninety_degrees_survives_the_unity_fallback() {
        // Unity yaws about +Y; the Source frame swaps the axes and the handedness.
        let source = unity_angles_to_source(0.0, 90.0, 0.0);
        assert!((source.y + 90.0).abs() < 1e-6, "{source:?}");
        assert!(source.x.abs() < 1e-6 && source.z.abs() < 1e-6, "{source:?}");
    }

    #[test]
    fn remap_bad_input_is_rejected_without_a_panic() {
        let d = doc();
        for bad in ["", "[]", "null", "{\"objects\": []}", "{\"objects\": 5}", "{}"] {
            assert!(import(&d, bad).is_err(), "{bad:?} should not import");
        }
    }
}

