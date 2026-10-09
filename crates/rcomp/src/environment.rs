//! The environment entities every shipped map carries: a colour correction, a
//! world fog controller and a wind volume. A map that authors its own entity of
//! a class keeps it; the rest take canyonlands_staging_mu1's, the map the sky
//! camera and sun defaults already follow.

use glam::{DQuat, DVec3};
use rdoc::model::{Entity, KeyValues, MapDoc, ObjectKind, Partition, Transform};

/// The grade canyonlands_staging_mu1 ships. The install carries every shipped
/// grade loose under `materials/correction/`.
pub const DEFAULT_CORRECTION: &str = "materials/correction/mp_rr_desertlands_hu_hdr.raw_hdr";

/// The wind volume's authored size; `scale` stretches it over the world.
const WIND_VOLUME_SIZE: f64 = 1000.0;

const COLOR_CORRECTION: &[(&str, &str)] = &[
    ("spawnflags", "0"),
    ("minfalloff", "-1"),
    ("maxweight", "1"),
    ("maxfalloff", "-1"),
    ("fadeOutDuration", "0"),
    ("fadeInDuration", "0"),
    ("filename", DEFAULT_CORRECTION),
];

const FOG_CONTROLLER: &[(&str, &str)] = &[
    ("vl_scatter", "0.8"),
    ("vl_intensity", "0.15"),
    ("vl_in_shadow_scatter", "0.0"),
    ("vl_distFalloff", "1500"),
    ("vl_color", "0.9 0.7 0.4"),
    ("disableWhenVfogEnabled", "0"),
    ("fogangles", "0 90 0"),
    ("use_angles", "1"),
    ("spawnflags", "0"),
    ("minfadetime", "3"),
    ("forceontosky", "0"),
    ("fogztop", "65000"),
    ("fogzbottom", "-800"),
    ("foghalfdisttop", "6000"),
    ("foghalfdistbottom", "10000"),
    ("fogenable", "1"),
    ("fogdistoffset", "1500"),
    ("fogdirhalfangle", "80"),
    ("fogdircolorstrength", "0.5"),
    ("fogdircolor", "255 243 229"),
    ("fogdensity", "0.1"),
    ("fogcolorstrength", "0.5"),
    ("fogcolor", "127 208 255"),
];

const WIND_VOLUME: &[(&str, &str)] = &[
    ("windType", "0"),
    ("windSpeed", "14"),
    ("windOrigin", "0 0 0"),
    ("windGustScale", "1"),
    ("windDir", "1 1 1"),
    ("volumeSize", "1000 1000 1000"),
    ("sortPriority", "0"),
    ("shapeType", "1"),
    ("enabled", "1"),
    ("edgeFade", "0"),
];

fn keys(pairs: &[(&str, &str)]) -> KeyValues {
    let mut kv = KeyValues::default();
    for (k, v) in pairs {
        kv.insert((*k).into(), (*v).into());
    }
    kv
}

fn has_class(doc: &MapDoc, classname: &str) -> bool {
    doc.objects.iter().any(|o| matches!(&o.kind, ObjectKind::Entity(e) if e.classname.eq_ignore_ascii_case(classname)))
}

/// `doc` with the baseline environment entities it lacks, placed in the middle
/// of the world box `(lo, hi)`.
pub fn with_baseline(doc: &MapDoc, lo: DVec3, hi: DVec3) -> MapDoc {
    let mut out = doc.clone();
    let layer = out.layers.first().map(|l| l.id).unwrap_or_default();
    let centre = (lo + hi) * 0.5;
    let reach = (hi - lo).max_element().max(WIND_VOLUME_SIZE);
    let mut add = |id: u128, classname: &str, mut kv: KeyValues| {
        if has_class(doc, classname) {
            return;
        }
        kv.insert("targetname".into(), format!("{classname}_baseline"));
        let mut o = rdoc::doc::new_object(
            classname,
            layer,
            ObjectKind::Entity(Entity {
                classname: classname.to_string(),
                keyvalues: kv,
                partition: Some(Partition::Env),
                links: Default::default(),
            }),
        );
        o.id = uuid::Uuid::from_u128(id);
        o.transform = Transform { position: centre, rotation: DQuat::IDENTITY, scale: DVec3::ONE };
        out.objects.push(o);
    };
    add(0x656e_765f_0000_0000_0000_0000_0000_0001, "color_correction", keys(COLOR_CORRECTION));
    add(0x656e_765f_0000_0000_0000_0000_0000_0002, "env_fog_controller", keys(FOG_CONTROLLER));
    let mut wind = keys(WIND_VOLUME);
    wind.insert("scale".into(), format!("{}", reach / WIND_VOLUME_SIZE));
    add(0x656e_765f_0000_0000_0000_0000_0000_0003, "wind_volume", wind);
    out
}

/// The rooms shipped maps keep away from the playable space for character
/// select, the victory ceremony, scene capture and the victory podium. The file
/// is generated from the user's own install by scripts/basement/make_basement.py
/// and named by R5ED_BASEMENT.
fn basement_json() -> Option<&'static str> {
    static FILE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        let Some(path) = std::env::var_os("R5ED_BASEMENT") else {
            eprintln!("[basement] R5ED_BASEMENT is not set: maps get no character select, ceremony or podium rooms \
                       (generate them with scripts/basement/make_basement.py)");
            return None;
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("[basement] cannot read R5ED_BASEMENT {}: {e}", std::path::Path::new(&path).display());
                None
            }
        }
    })
    .as_deref()
}

/// The anchor each room is lifted around; a map that already names it keeps its own room.
const ROOM_ANCHORS: [(&str, &str); 4] = [
    ("char_select", "target_char_sel_camera_new"),
    ("victory", "target_victory_gcards_ref"),
    ("scene_capture", "scene_capture_room_1_ref"),
    ("podium", "podium_info_target"),
];

/// Clearance between the rooms, and between them and the playable space above.
const ROOM_GAP: f64 = 2048.0;
const ROOM_DEPTH: f64 = 4096.0;

#[derive(serde::Deserialize)]
struct BasementFile {
    rooms: std::collections::BTreeMap<String, Vec<RoomItem>>,
}

#[derive(serde::Deserialize)]
struct RoomItem {
    name: String,
    transform: Transform,
    kind: ObjectKind,
}

/// One room placed in a map: where its anchor sits and what it holds.
pub struct PlacedRoom {
    pub name: String,
    pub anchor: DVec3,
    items: Vec<RoomItem>,
}

impl PlacedRoom {
    /// Every object position the room puts in the world.
    pub fn points(&self) -> impl Iterator<Item = DVec3> + '_ {
        self.items.iter().map(|i| self.anchor + i.transform.position)
    }
}

fn names_script(doc: &MapDoc, name: &str) -> bool {
    doc.objects.iter().any(|o| match &o.kind {
        ObjectKind::Entity(e) => e.keyvalues.get("script_name").or_else(|| e.keyvalues.get("targetname")).is_some_and(|v| v == name),
        _ => false,
    })
}

/// Lays the rooms `doc` lacks in a row below the playable box `(lo, hi)`,
/// wrapping onto a new row when the box is too narrow.
pub fn plan_basement(doc: &MapDoc, lo: DVec3, hi: DVec3) -> Vec<PlacedRoom> {
    let Some(json) = basement_json() else { return Vec::new() };
    let file: BasementFile = match serde_json::from_str(json) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[basement] R5ED_BASEMENT does not parse: {e}");
            return Vec::new();
        }
    };
    let mut rooms = file.rooms;
    let mut out = Vec::new();
    let (mut x, mut y, mut row_depth) = (lo.x + ROOM_GAP, lo.y + ROOM_GAP, 0.0f64);
    let z = lo.z - ROOM_DEPTH;
    for (room, anchor) in ROOM_ANCHORS {
        if names_script(doc, anchor) {
            continue;
        }
        let Some(items) = rooms.remove(room) else { continue };
        let (mut rlo, mut rhi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        for i in &items {
            rlo = rlo.min(i.transform.position);
            rhi = rhi.max(i.transform.position);
        }
        let size = rhi - rlo;
        if x + size.x > hi.x - ROOM_GAP && x > lo.x + ROOM_GAP {
            x = lo.x + ROOM_GAP;
            y += row_depth + ROOM_GAP;
            row_depth = 0.0;
        }
        // The room's highest object sits ROOM_DEPTH under the playable floor.
        let anchor_at = DVec3::new(x - rlo.x, y - rlo.y, z - rhi.z);
        out.push(PlacedRoom { name: room.to_string(), anchor: anchor_at, items });
        x += size.x + ROOM_GAP;
        row_depth = row_depth.max(size.y);
    }
    out
}

/// The models the rooms `doc` lacks draw, which the map's own pak has to carry:
/// shipped maps each pack their own copy.
pub fn basement_models(doc: &MapDoc) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for room in plan_basement(doc, DVec3::ZERO, DVec3::ZERO) {
        for item in &room.items {
            if let ObjectKind::Entity(e) = &item.kind {
                // Editor markers (mdl/dev) never draw in a shipped map.
                if let Some(m) = e.keyvalues.get("model").filter(|m| m.ends_with(".rmdl") && !m.starts_with("mdl/dev/")) {
                    if !out.contains(m) {
                        out.push(m.clone());
                    }
                }
            }
        }
    }
    out
}

/// `doc` with the planned rooms' objects added.
pub fn with_basement(doc: &MapDoc, rooms: &[PlacedRoom]) -> MapDoc {
    let mut out = doc.clone();
    let layer = out.layers.first().map(|l| l.id).unwrap_or_default();
    for (r, room) in rooms.iter().enumerate() {
        for (i, item) in room.items.iter().enumerate() {
            let mut o = rdoc::doc::new_object(&item.name, layer, item.kind.clone());
            o.id = uuid::Uuid::from_u128(0x6261_7365_6d65_6e74_0000_0000_0000_0000 | ((r as u128) << 16) | i as u128);
            o.transform = Transform { position: room.anchor + item.transform.position, ..item.transform };
            o.hidden = false;
            out.objects.push(o);
        }
    }
    out
}
