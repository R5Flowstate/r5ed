//! What a Source engine entity, material or model becomes in an Apex map.

use indexmap::IndexMap;

use glam::DVec3;

use crate::model::{KeyValues, LightKind, VolumeKind};

/// The part a Source entity class plays in the imported map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Its solids are ordinary world brushes; the entity itself is not kept.
    WorldBrushes,
    /// Its solids render but never collide.
    RenderBrushes,
    Volume(VolumeKind),
    Spawn,
    Prop { dynamic: bool },
    Light(LightKind),
    /// A bake cubemap sample point.
    Cubemap,
    /// Kept as a point entity of the same class, Source-only keys removed.
    Keep,
    /// A class this table does not know: kept as it was written, solids and all.
    Passthrough,
    /// Editor, logic or client-effect classes that have no Apex counterpart.
    Drop,
}

/// Moving brush entities, kept as static geometry in their spawn pose until
/// entity I/O can drive them.
pub fn is_frozen_mover(classname: &str) -> bool {
    matches!(
        classname,
        "func_door" | "func_door_rotating" | "func_rotating" | "func_button" | "func_rot_button" | "func_tracktrain"
            | "func_train" | "func_movelinear" | "func_platrot" | "momentary_rot_button"
    )
}

/// Source I/O entities: brush solids kept under a passthrough entity when the
/// schema knows the class, dropped otherwise.
pub const IO_ENTITIES: [&str; 12] = [
    "func_door",
    "func_door_rotating",
    "func_button",
    "func_movelinear",
    "func_rotating",
    "func_tracktrain",
    "path_track",
    "logic_relay",
    "logic_timer",
    "logic_auto",
    "math_counter",
    "func_breakable",
];

/// The role an I/O entity plays once the schema is known. Anything else keeps
/// `role`; without a schema the caller keeps the historical role.
pub fn role_in_schema(classname: &str, known: bool) -> Role {
    if IO_ENTITIES.contains(&classname) {
        if known {
            Role::Passthrough
        } else {
            Role::Drop
        }
    } else {
        role(classname)
    }
}

/// A VMF `connections` value or BSP output key as the entity partition writes
/// it: `target,input,parameter,delay,refire`. Source separates the fields with
/// `,` or ESC (`\x1b`); the middle field is free text, so extra fields rejoin
/// into the parameter.
pub fn connection_value(raw: &str) -> String {
    let parts: Vec<&str> = if raw.contains('\x1b') { raw.split('\x1b').collect() } else { raw.split(',').collect() };
    let mut fields: Vec<&str> = parts.iter().map(|f| f.trim()).collect();
    while fields.len() < 5 {
        fields.push("");
    }
    let param = fields[2..fields.len() - 2].join(",");
    [fields[0], fields[1], param.as_str(), fields[fields.len() - 2], fields[fields.len() - 1]].join(",")
}

/// A plain entity key carrying a Source output: `OnXxx` with a comma- or
/// ESC-separated connection value, the shape a BSP lump stores its outputs in.
pub fn is_output_key(key: &str, value: &str) -> bool {
    let upper = key.as_bytes().get(2).is_some_and(|c| c.is_ascii_uppercase());
    key.len() > 2 && key.starts_with("On") && upper && (value.contains(',') || value.contains('\x1b'))
}

pub fn role(classname: &str) -> Role {
    match classname {
        "func_detail" | "func_brush" | "func_wall" | "func_wall_toggle" | "func_lod" | "func_breakable"
        | "func_physbox" | "func_clip_vphysics" => Role::WorldBrushes,
        "func_illusionary" | "func_water_analog" => Role::RenderBrushes,
        c if is_frozen_mover(c) => Role::WorldBrushes,
        "trigger_hurt" => Role::Volume(VolumeKind::TriggerHurt),
        "trigger_soundscape" => Role::Volume(VolumeKind::TriggerSoundscape),
        "trigger_multiple" | "trigger_once" | "trigger_push" | "trigger_teleport" | "trigger_gravity" => {
            Role::Volume(VolumeKind::Trigger)
        }
        "func_playerclip" => Role::Volume(VolumeKind::PlayerClip),
        "info_player_start" | "info_player_terrorist" | "info_player_counterterrorist" | "info_player_deathmatch"
        | "info_player_teamspawn" | "info_player_combine" | "info_player_rebel" => Role::Spawn,
        "prop_static" | "prop_detail" => Role::Prop { dynamic: false },
        "prop_dynamic" | "prop_dynamic_override" | "prop_physics" | "prop_physics_multiplayer"
        | "prop_physics_override" => Role::Prop { dynamic: true },
        "light_environment" => Role::Light(LightKind::Sun),
        "light" => Role::Light(LightKind::Point),
        "light_spot" => Role::Light(LightKind::Spot),
        "env_cubemap" => Role::Cubemap,
        "info_teleport_destination" | "info_target" | "info_landmark" | "path_track" | "ambient_generic" => Role::Keep,
        c if c.starts_with("logic_")
            || c.starts_with("filter_")
            || c.starts_with("game_")
            || c.starts_with("weapon_")
            || c.starts_with("item_")
            || c.starts_with("point_")
            || c.starts_with("info_node")
            || c.starts_with("env_soundscape")
            || c.starts_with("func_areaportal") =>
        {
            Role::Drop
        }
        "env_tonemap_controller" | "postprocess_controller" | "shadow_control" | "env_cascade_light"
        | "water_lod_control" | "func_dustmotes" | "func_dustcloud" | "env_fog_controller" | "env_sun"
        | "env_wind" | "env_sprite" | "env_smokestack" | "env_lightglow" | "func_occluder" | "func_viewblocker"
        | "info_overlay" | "info_lighting" | "info_particle_system" | "sky_camera" | "color_correction"
        | "func_precipitation" | "func_buyzone" | "func_bomb_target" | "func_hostage_rescue" | "info_map_parameters"
        | "infodecal" | "move_rope" | "keyframe_rope" | "ambient_music" | "math_counter" | "worldspawn" => Role::Drop,
        _ => Role::Passthrough,
    }
}

/// Keys that survive on a kept entity or volume: names, links and the few
/// behaviour keys the Apex class reads under the same meaning.
fn kept_key(classname: &str, key: &str) -> Option<&'static str> {
    Some(match (classname, key) {
        (_, "targetname") => "targetname",
        (_, "target") => "target",
        (_, "parentname") => "parentname",
        (_, "spawnflags") => "spawnflags",
        (_, "StartDisabled") | (_, "startdisabled") => "StartDisabled",
        ("trigger_teleport", "landmark") => "landmark",
        ("trigger_push", "pushdir") => "pushdir",
        ("trigger_push", "speed") => "pushspeed",
        ("trigger_hurt", "damage") => "damage",
        ("trigger_hurt", "damagecap") => "damagecap",
        _ => return None,
    })
}

pub fn keep_keys(classname: &str, keys: &KeyValues, dropped: &mut IndexMap<String, usize>) -> KeyValues {
    let mut out = KeyValues::new();
    for (k, v) in keys {
        if matches!(k.as_str(), "classname" | "origin" | "angles" | "id") {
            continue;
        }
        match kept_key(classname, k) {
            Some(name) => {
                out.insert(name.to_string(), v.clone());
            }
            None => *dropped.entry(format!("{classname}.{k}")).or_default() += 1,
        }
    }
    out
}

/// The keys every `info_spawnpoint_human` needs to validate and spawn a player.
pub fn spawn_keys() -> KeyValues {
    let mut k = KeyValues::new();
    for (key, value) in [
        ("teamnumber", "0"),
        ("model", "mdl/dev/mp_spawn.rmdl"),
        ("scale", "1"),
        ("gamemode_tdm", "1"),
        ("gamemode_ffa", "1"),
        ("gamemode_ctf", "1"),
        ("gamemode_cp", "1"),
        ("gamemode_at", "1"),
        ("gamemode_fd", "1"),
        ("gamemode_fw", "0"),
        ("gamemode_freelance", "0"),
        ("control_teamnumber", "-1"),
        ("alliance", "-1"),
    ] {
        k.insert(key.into(), value.into());
    }
    for i in 1..=9 {
        k.insert(format!("phase_{i}"), "0".into());
    }
    k
}

/// `models/foo/bar.mdl` -> `mdl/source/foo/bar.rmdl`, the path the model
/// converter publishes the Apex model under.
pub fn model_path(source: &str) -> String {
    let s = source.trim().replace('\\', "/").to_ascii_lowercase();
    let s = s.strip_prefix("models/").unwrap_or(&s);
    let s = s.strip_suffix(".mdl").unwrap_or(s);
    format!("mdl/source/{s}.rmdl")
}

/// A Source texture path as every importer stage files it: lowercased, slash
/// separated, with no empty segment. A degenerate texname like `/altflag`
/// would otherwise namespace to `world/source//altflag`, which no asset
/// path may hold.
pub fn clean_name(source: &str) -> String {
    source.trim().replace('\\', "/").to_ascii_lowercase().split('/').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("/")
}

/// A Source brush material as the Apex material the importer references.
/// Tool textures map onto the engine's own tool materials; everything else
/// onto the world material the material converter builds for it.
pub fn material(source: &str) -> String {
    let s = clean_name(source);
    match s.as_str() {
        "tools/toolsskybox" | "tools/toolsskybox2d" | "tools/toolsskyfog" => "tools/toolsskybox".into(),
        "tools/toolsnodraw" | "tools/toolsblack" | "tools/toolsoccluder" | "tools/toolsareaportal"
        | "tools/toolsskip" | "tools/toolshint" => "tools/toolsnodraw".into(),
        // Invisible walls collide but must not cast shadows the way nodraw does.
        "tools/toolsinvisible" | "tools/toolsinvisibleladder" => "tools/toolsinvisible".into(),
        "tools/toolsclip" | "tools/toolsplayerclip" | "tools/toolsnpcclip" | "tools/toolsgrenadeclip" => {
            "tools/toolsplayerclip".into()
        }
        s if s.starts_with("tools/toolstrigger") => "tools/toolstrigger".into(),
        // The converter builds no world/source/tools/* asset and the gate
        // exempts engine-side tools/*, so anything else Source-side stays
        // engine-side under its own name instead of failing the ship.
        s if s.starts_with("tools/") => s.into(),
        // Source sky faces are sky, not world geometry the converter builds.
        s if s.starts_with("skybox/") => "tools/toolsskybox".into(),
        "" => crate::model::DEFAULT_MATERIAL.into(),
        s => format!("world/source/{s}"),
    }
}

/// Source's bullet-only collision tool, as the importer names it.
pub const BLOCK_BULLETS: &str = "tools/toolsblockbullets";

pub fn is_clip(material: &str) -> bool {
    material == "tools/toolsplayerclip"
}

/// The way a Source sun or spot shines, as VRAD reads it: a non-zero `pitch`
/// replaces the angles' pitch, and -90 is straight down.
pub fn light_direction(angles: Option<&str>, pitch: Option<&str>, angle: Option<&str>) -> DVec3 {
    let angles: Vec<f64> = angles.unwrap_or("0 0 0").split_whitespace().filter_map(|t| t.parse().ok()).collect();
    let key = |v: Option<&str>| v.and_then(|t| t.trim().parse::<f64>().ok()).filter(|v| *v != 0.0);
    let pitch = key(pitch).unwrap_or_else(|| angles.first().copied().unwrap_or(0.0)).to_radians();
    let yaw = key(angle).unwrap_or_else(|| angles.get(1).copied().unwrap_or(0.0)).to_radians();
    DVec3::new(pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin())
}

/// `_light "r g b brightness"` as linear colour and an Apex intensity; Source
/// brightness 200 is taken as intensity 1.
pub fn light_value(text: Option<&str>) -> ([f32; 3], f32) {
    let v: Vec<f64> = text.unwrap_or("").split_whitespace().filter_map(|t| t.parse().ok()).collect();
    if v.len() < 3 {
        return ([1.0; 3], 1.0);
    }
    let c = |x: f64| ((x.clamp(0.0, 255.0) / 255.0).powf(2.2)) as f32;
    let brightness = v.get(3).copied().unwrap_or(200.0).max(0.0);
    ([c(v[0]), c(v[1]), c(v[2])], (brightness / 200.0) as f32)
}

/// The light VRAD bakes into an HDR map: `hdr` when it names its own colour
/// (not `-1 -1 -1 1`), else `ldr`, either scaled by `scale`. CS:GO bakes only
/// HDR, so a sun with `_lightscaleHDR 0` does not shine there at all.
pub fn hdr_light_value(ldr: Option<&str>, hdr: Option<&str>, scale: Option<&str>) -> ([f32; 3], f32) {
    let scale = scale.and_then(|t| t.trim().parse::<f32>().ok()).unwrap_or(1.0).max(0.0);
    let own = hdr.filter(|t| t.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()).is_some_and(|v| v >= 0.0));
    let (color, intensity) = light_value(own.or(ldr));
    (color, intensity * scale)
}

/// A stock Apex sun is `_light` brightness ~216 (skygarden), intensity ~1.1.
pub const STOCK_SUN_MAX: f32 = 1.25;

/// A Source sun as Apex should see it: at the brightness VRAD bakes into HDR.
/// A CS:GO map with `_lightscaleHDR 0` has no sun in HDR, but S21 has no LDR
/// path to fall back on, so the map would lose its sun outright: that sun keeps
/// its LDR colour, held to a stock sun's brightness so light surfaces do not
/// clip to white.
pub fn sun_value(ldr: Option<&str>, hdr: Option<&str>, scale: Option<&str>) -> ([f32; 3], f32) {
    let (color, intensity) = hdr_light_value(ldr, hdr, scale);
    if intensity > 0.0 {
        return (color, intensity);
    }
    let (color, intensity) = light_value(ldr);
    (color, intensity.min(STOCK_SUN_MAX))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_sun_off_in_hdr_keeps_a_stock_strength_sun() {
        let (_, i) = super::sun_value(Some("224 224 190 800"), Some("-1 -1 -1 1"), Some("0"));
        assert_eq!(i, super::STOCK_SUN_MAX);
        let (_, i) = super::sun_value(Some("255 255 255 200"), Some("-1 -1 -1 1"), Some("1"));
        assert_eq!(i, 1.0);
    }

    /// Only a sun that is off in HDR is held to a stock sun: one that is on
    /// keeps the brightness VRAD bakes (Surf Castle: 640 / 200 = 3.2).
    #[test]
    fn a_sun_on_in_hdr_keeps_its_own_brightness() {
        let (_, i) = super::sun_value(Some("255 238 198 640"), Some("-1 -1 -1 1"), Some("1"));
        assert!((i - 3.2).abs() < 1e-4, "{i}");
        let (_, i) = super::sun_value(Some("255 238 198 640"), Some("255 238 198 400"), Some("1"));
        assert!((i - 2.0).abs() < 1e-4, "{i}");
    }

    #[test]
    fn an_hdr_scale_of_zero_turns_the_light_off() {
        let (_, i) = super::hdr_light_value(Some("224 224 190 800"), Some("-1 -1 -1 1"), Some("0"));
        assert_eq!(i, 0.0);
        let (_, i) = super::hdr_light_value(Some("255 255 255 200"), Some("-1 -1 -1 1"), Some("1"));
        assert_eq!(i, 1.0);
        let (_, i) = super::hdr_light_value(Some("255 255 255 200"), Some("255 255 255 400"), None);
        assert_eq!(i, 2.0);
    }

    use super::*;

    #[test]
    fn light_pitch_minus_ninety_shines_straight_down() {
        let d = light_direction(Some("-90 0 0"), Some("-90"), None);
        assert!((d - DVec3::NEG_Z).length() < 1e-9, "{d}");
        let d = light_direction(Some("-45 90 0"), None, None);
        assert!(d.z < -0.7 && d.y > 0.7 && d.x.abs() < 1e-9, "{d}");
    }

    #[test]
    fn materials_map_tools_and_prefix_the_rest() {
        assert_eq!(material("TOOLS/TOOLSSKYBOX"), "tools/toolsskybox");
        assert_eq!(material("tools/toolstrigger-dx10"), "tools/toolstrigger");
        assert_eq!(material("TOOLS/TOOLSPLAYERCLIP"), "tools/toolsplayerclip");
        assert_eq!(material("CONCRETE/Concrete_Black"), "world/source/concrete/concrete_black");
    }

    #[test]
    fn unknown_tools_stay_engine_side_and_degenerate_names_clean_up() {
        assert_eq!(material("TOOLS/TOOLSBLOCKBULLETS"), "tools/toolsblockbullets");
        assert_eq!(material("tools/toolsblocklight"), "tools/toolsblocklight");
        assert_eq!(material("skybox/sky_day01_01rt"), "tools/toolsskybox");
        assert_eq!(material("/ALTFLAG"), "world/source/altflag");
        assert_eq!(material("a//b"), "world/source/a/b");
        assert_eq!(material(""), crate::model::DEFAULT_MATERIAL);
    }

    #[test]
    fn model_paths_drop_the_models_folder_and_extension() {
        assert_eq!(model_path("models/surf/castle_arrow.mdl"), "mdl/source/surf/castle_arrow.rmdl");
    }

    #[test]
    fn roles_cover_the_common_classes() {
        assert_eq!(role("info_player_terrorist"), Role::Spawn);
        assert_eq!(role("trigger_push"), Role::Volume(VolumeKind::Trigger));
        assert_eq!(role("prop_static"), Role::Prop { dynamic: false });
        assert_eq!(role("func_detail"), Role::WorldBrushes);
        assert_eq!(role("logic_auto"), Role::Drop);
        assert_eq!(role("func_door"), Role::WorldBrushes);
        assert_eq!(role("weapon_ak47"), Role::Drop);
        assert_eq!(role("func_water_analog"), Role::RenderBrushes);
    }

    #[test]
    fn io_entities_follow_the_schema() {
        for class in IO_ENTITIES {
            assert_eq!(role_in_schema(class, true), Role::Passthrough);
            assert_eq!(role_in_schema(class, false), Role::Drop);
        }
        assert_eq!(role_in_schema("trigger_multiple", true), role("trigger_multiple"));
        assert_eq!(role_in_schema("trigger_multiple", false), role("trigger_multiple"));
    }

    #[test]
    fn connection_values_normalize_to_five_comma_fields() {
        assert_eq!(connection_value("door,Open,,0,-1"), "door,Open,,0,-1");
        assert_eq!(connection_value("door\x1bOpen\x1b\x1b0\x1b-1"), "door,Open,,0,-1");
        assert_eq!(connection_value("t,Fire"), "t,Fire,,,");
        assert_eq!(
            connection_value("e,AddOutput,targetname p,upper,0,-1"),
            "e,AddOutput,targetname p,upper,0,-1"
        );
        assert_eq!(connection_value("surf\x1bRunScriptCode\x1bboost_island()\x1b0\x1b-1"), "surf,RunScriptCode,boost_island(),0,-1");
    }

    #[test]
    fn output_keys_are_on_uppercase_names_with_separators() {
        assert!(is_output_key("OnTrigger", "a,b,,0,-1"));
        assert!(is_output_key("OnMapSpawn", "a\x1bB\x1b\x1b0\x1b-1"));
        assert!(!is_output_key("only_spawn_in_freelance", "0"));
        assert!(!is_output_key("targetname", "door,main"));
        assert!(!is_output_key("OnTrigger", "0"));
    }

    #[test]
    fn push_speed_is_renamed_and_source_keys_are_counted() {
        let mut keys = KeyValues::new();
        keys.insert("speed".into(), "900".into());
        keys.insert("FallingSpeedThreshold".into(), "0".into());
        keys.insert("origin".into(), "0 0 0".into());
        let mut dropped = IndexMap::new();
        let kept = keep_keys("trigger_push", &keys, &mut dropped);
        assert_eq!(kept.get("pushspeed").map(String::as_str), Some("900"));
        assert_eq!(kept.len(), 1);
        assert_eq!(dropped.get("trigger_push.FallingSpeedThreshold"), Some(&1));
    }
}
