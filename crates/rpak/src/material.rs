//! RSX material export and the compiler-facing `MaterialInfo` shape.
//!
//! Every numeric field of the export is a `"0x..."` string, so the whole
//! document is read as `serde_json::Value` and pulled apart field by field:
//! the export is a debug dump whose key set grows between asset versions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Which world-vertex layout a surface's material compiles into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VertexFormat {
    Unlit,
    LitFlat,
    LitBump,
    UnlitTs,
}

impl VertexFormat {
    /// The lump id the world compiler writes this format's vertices into.
    pub fn lump_id(self) -> u16 {
        match self {
            Self::Unlit => 0x47,
            Self::LitFlat => 0x48,
            Self::LitBump => 0x49,
            Self::UnlitTs => 0x4A,
        }
    }
}

/// Everything the compiler needs about one material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialInfo {
    pub path: String,
    pub guid: u64,
    pub shader_set: u64,
    pub vertex_format: VertexFormat,
    pub surface_prop: String,
    pub translucent: bool,
    /// A translucent material drawn with depth bias: a world decal.
    #[serde(default)]
    pub decal: bool,
    pub nodraw: bool,
    /// Texture slot -> texture guid. A slot the caller could not resolve is
    /// absent rather than zero, so a real guid is never confused with a miss.
    pub textures: BTreeMap<u32, u64>,
}

/// A material as the export writes it, before texture slots are resolved: the
/// tool writes a guid when it did not export the texture and an exported
/// relative path when it did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawMaterial {
    pub path: String,
    pub shader_set: u64,
    pub shader_type: String,
    pub surface_prop: String,
    pub translucent: bool,
    #[serde(default)]
    pub decal: bool,
    pub nodraw: bool,
    /// Texture slot -> the value exactly as written.
    pub textures: BTreeMap<u32, String>,
}

/// RSX writes the material's shader *suffix* (`sknp`, `wldc`, ...), the
/// lowercase tail of the `_sknp` file-name form. World geometry compiles to
/// bump lighting unless the suffix names an unlit or flat variant; a model or
/// particle suffix still has to bind a world surface, so it takes the lit
/// bump layout.
fn vertex_format_of(suffix: &str) -> Result<VertexFormat> {
    match suffix {
        "wldu" | "wldc" | "rgdu" | "rgdp" | "rgdc" | "sknu" | "sknp" | "sknc" | "ptcu" | "ptcs" | "rgbs" => {
            Ok(VertexFormat::LitBump)
        }
        "unlit" => Ok(VertexFormat::Unlit),
        "lit_flat" => Ok(VertexFormat::LitFlat),
        "unlit_ts" => Ok(VertexFormat::UnlitTs),
        _ => Err(Error::Malformed {
            what: "material",
            msg: format!("unknown shaderType suffix {suffix:?}"),
        }),
    }
}

fn hex_u64(v: &serde_json::Value, key: &str) -> Result<u64> {
    let s = v.get(key).and_then(serde_json::Value::as_str).ok_or(Error::Malformed {
        what: "material",
        msg: format!("missing string field {key}"),
    })?;
    let body = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u64::from_str_radix(body, 16)
        .map_err(|_| Error::Malformed { what: "material", msg: format!("{key} is not a hex number: {s:?}") })
}

fn hex_u32(v: &serde_json::Value, key: &str) -> Result<u32> {
    let s = v.get(key).and_then(serde_json::Value::as_str).ok_or(Error::Malformed {
        what: "material",
        msg: format!("missing string field {key}"),
    })?;
    let body = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u32::from_str_radix(body, 16)
        .map_err(|_| Error::Malformed { what: "material", msg: format!("{key} is not a hex number: {s:?}") })
}

/// `depthStencilFlags` bit 4 is depth write. Opaque world materials set it
/// (0x17 on every dev and tool material); decals and other surfaces that draw
/// over the world clear it (0x7, 0x5). `blendStateMask` is 0x4 on both, so it
/// does not tell them apart.
const DEPTH_WRITE_BIT: u32 = 0x10;
/// The same engine flag set spells NODRAW in bit 3 of the uber buffer flags.
const NODRAW_BIT: u32 = 0x8;
/// `rasterizerFlags` bit 4 is depth bias: set on every shipped world decal (0x16),
/// clear on glass and screens (0x6).
const DEPTH_BIAS_BIT: u32 = 0x10;

/// A texture slot is either `0x<guid>` or a relative export path, so the only
/// characters either form can hold are these.
fn is_plain_slot_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\' | 'x' | 'X')
}

impl MaterialInfo {
    /// Turns a raw document into the compiler-facing shape, mapping every
    /// texture slot through `lookup` and dropping the ones it cannot resolve.
    pub fn resolve(raw: RawMaterial, lookup: impl Fn(&str) -> Option<u64>) -> Self {
        let vertex_format = vertex_format_of(&raw.shader_type).unwrap_or(VertexFormat::LitBump);
        let textures = raw
            .textures
            .iter()
            .filter_map(|(slot, v)| lookup(v).map(|g| (*slot, g)))
            .collect();
        Self {
            path: raw.path,
            guid: 0,
            shader_set: raw.shader_set,
            vertex_format,
            surface_prop: raw.surface_prop,
            translucent: raw.translucent,
            decal: raw.decal,
            nodraw: raw.nodraw,
            textures,
        }
    }
}

/// Parses one RSX material json document.
pub fn parse_material_json(text: &str) -> Result<RawMaterial> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| Error::Json(e.to_string()))?;
    if !v.is_object() {
        return Err(Error::Malformed { what: "material", msg: "root is not an object".into() });
    }
    let path = v.get("name").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    let shader_set = hex_u64(&v, "shaderSet")?;
    let shader_type = v.get("shaderType").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    if vertex_format_of(&shader_type).is_err() {
        return Err(Error::Malformed {
            what: "material",
            msg: format!("unknown shaderType suffix {shader_type:?}"),
        });
    }
    let surface_prop = v
        .get("surfaceProp")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("default")
        .to_string();
    let depth_stencil = if v.get("depthStencilFlags").is_some() { hex_u32(&v, "depthStencilFlags")? } else { DEPTH_WRITE_BIT };
    let uber = hex_u32(&v, "uberBufferFlags")?;
    let raster = if v.get("rasterizerFlags").is_some() { hex_u32(&v, "rasterizerFlags")? } else { 0 };
    let mut textures = BTreeMap::new();
    if let Some(map) = v.get("$textures").and_then(serde_json::Value::as_object) {
        for (slot, value) in map {
            let slot_no: u32 = slot.parse().map_err(|_| Error::Malformed {
                what: "material",
                msg: format!("texture slot {slot:?} is not a number"),
            })?;
            let s = value.as_str().ok_or(Error::Malformed {
                what: "material",
                msg: format!("texture slot {slot} is not a string"),
            })?;
            if s.is_empty() || !s.chars().all(is_plain_slot_char) {
                return Err(Error::Malformed {
                    what: "material",
                    msg: format!("texture slot {slot} is not a guid or a relative path"),
                });
            }
            textures.insert(slot_no, s.to_string());
        }
    }
    Ok(RawMaterial {
        path,
        shader_set,
        shader_type,
        surface_prop,
        translucent: depth_stencil & DEPTH_WRITE_BIT == 0,
        decal: depth_stencil & DEPTH_WRITE_BIT == 0 && raster & DEPTH_BIAS_BIT != 0,
        nodraw: uber & NODRAW_BIT != 0,
        textures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real material export; every key below appears verbatim in
    /// that output, with the texture slots written as guids.
    const REAL: &str = r#"{
	"name": "art/mshop/weapons/class/sniper/tripletake/base/skins/epicplus/v19_ironcrown/tripletake_epicp_v19_ironcrown_main_colpass",
	"width": 2048,
	"height": 2048,
	"depth": 0,
	"glueFlags": "0x56000020",
	"blendStates": [ "0xF0000000", "0xF0000000" ],
	"blendStateMask": "0x4",
	"depthStencilFlags": "0x5",
	"rasterizerFlags": "0x6",
	"uberBufferFlags": "0x0",
	"features": "0x1F5A92BD",
	"samplers": "0x1D0300",
	"surfaceProp": "weapon",
	"surfaceProp2": "",
	"shaderType": "sknp",
	"shaderSet": "0xBC3BC3B1C806DCB2",
	"$textures": {
		"0": "0x29FC975FA62CB0FE", "1": "0x4C569C195D04FF88",
		"2": "0x62D6E287C541C253", "3": "0x8F89BD92EA2BACD0",
		"4": "0x8D96F43BAA0C2C7B", "5": "0x9B4E7D9550595C79",
		"6": "0x22E66D03A88F9B0F", "7": "0xCBF46E9C8101631C"
	},
	"$textureTypes": { "0": "albedoTexture", "1": "normalTexture" },
	"$depthShadowMaterial": "0x2B93C99C67CC8B51",
	"$colpassMaterial": "0x0",
	"$textureAnimation": "0x0"
}"#;

    /// The same document as a real export writes it when it also exported the
    /// textures: every slot is a relative path instead of a guid.
    const REAL_PATHS: &str = r#"{
	"name": "models/props/caustic_mug/caustic_mug",
	"blendStateMask": "0x4",
	"uberBufferFlags": "0x0",
	"surfaceProp": "default",
	"shaderType": "rgdp",
	"shaderSet": "0x2A1140474660B075",
	"$textures": {
		"0": "tic_mug_rgdp/caustic_mug_albedoTexture.rpak",
		"1": "tic_mug_rgdp/caustic_mug_normalTexture.rpak"
	}
}"#;

    #[test]
    fn material_json_parse() {
        let raw = parse_material_json(REAL).unwrap();
        assert_eq!(raw.shader_type, "sknp");
        assert_eq!(raw.surface_prop, "weapon");
        assert!(raw.translucent);
        assert!(!raw.nodraw);
        assert_eq!(raw.shader_set, 0xBC3B_C3B1_C806_DCB2);
        assert_eq!(raw.textures.len(), 8);
        let m = MaterialInfo::resolve(raw, |v| {
            let body = v.strip_prefix("0x").unwrap_or(v);
            u64::from_str_radix(body, 16).ok()
        });
        assert_eq!(
            m.path,
            "art/mshop/weapons/class/sniper/tripletake/base/skins/epicplus/v19_ironcrown/tripletake_epicp_v19_ironcrown_main_colpass"
        );
        assert_eq!(m.shader_set, 0xBC3B_C3B1_C806_DCB2);
        assert_eq!(m.surface_prop, "weapon");
        assert_eq!(m.vertex_format, VertexFormat::LitBump);
        assert_eq!(m.vertex_format.lump_id(), 0x49);
        assert!(m.translucent, "depthStencilFlags 0x5 writes no depth");
        assert!(!m.nodraw);
        assert_eq!(m.textures.len(), 8);
        assert_eq!(m.textures[&0], 0x29FC_975F_A62C_B0FE);
        assert_eq!(m.textures[&7], 0xCBF4_6E9C_8101_631C);
    }

    #[test]
    fn texture_slots_written_as_paths_resolve_through_the_caller() {
        let raw = parse_material_json(REAL_PATHS).unwrap();
        assert_eq!(raw.shader_set, 0x2A11_4047_4660_B075);
        assert_eq!(raw.textures[&0], "tic_mug_rgdp/caustic_mug_albedoTexture.rpak");
        let m = MaterialInfo::resolve(raw, |v| {
            let stem = v.strip_suffix(".rpak")?;
            (stem == "tic_mug_rgdp/caustic_mug_albedoTexture").then_some(0xAAAA_AAAA_AAAA_AAAA)
        });
        assert_eq!(m.textures.len(), 1, "a slot the caller cannot resolve is dropped");
        assert_eq!(m.textures[&0], 0xAAAA_AAAA_AAAA_AAAA);
        assert!(m.textures.get(&1).is_none());
        assert_eq!(m.vertex_format, VertexFormat::LitBump);
    }

    #[test]
    fn decal_is_translucent_with_depth_bias() {
        let decal = REAL.replace("\"rasterizerFlags\": \"0x6\"", "\"rasterizerFlags\": \"0x16\"");
        assert!(parse_material_json(&decal).unwrap().decal);
        assert!(!parse_material_json(REAL).unwrap().decal, "translucent without depth bias is not a decal");
        let opaque = decal.replace("\"depthStencilFlags\": \"0x5\"", "\"depthStencilFlags\": \"0x17\"");
        assert!(!parse_material_json(&opaque).unwrap().decal, "a depth-writing material is never a decal");
    }

    #[test]
    fn nodraw_and_unlit_variants() {
        let src = REAL
            .replace(r#""shaderType": "sknp""#, r#""shaderType": "unlit_ts""#)
            .replace(r#""uberBufferFlags": "0x0""#, r#""uberBufferFlags": "0x8""#)
            .replace(r#""depthStencilFlags": "0x5""#, r#""depthStencilFlags": "0x17""#);
        let m = parse_material_json(&src).unwrap();
        assert_eq!(m.shader_type, "unlit_ts");
        assert!(m.nodraw);
        assert!(!m.translucent);
        let m = MaterialInfo::resolve(m, |_| None);
        assert_eq!(m.vertex_format, VertexFormat::UnlitTs);
        assert_eq!(m.vertex_format.lump_id(), 0x4A);
    }

    #[test]
    fn every_vertex_format_has_a_distinct_lump() {
        let ids: Vec<u16> =
            [VertexFormat::Unlit, VertexFormat::LitFlat, VertexFormat::LitBump, VertexFormat::UnlitTs]
                .iter()
                .map(|v| v.lump_id())
                .collect();
        assert_eq!(ids, vec![0x47, 0x48, 0x49, 0x4A]);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4);
    }

    #[test]
    fn rejects_malformed_documents() {
        assert!(matches!(parse_material_json("not json"), Err(Error::Json(_))));
        assert!(matches!(parse_material_json("[]"), Err(Error::Malformed { what: "material", .. })));
        let no_shader = REAL.replace(r#""shaderSet": "0xBC3BC3B1C806DCB2","#, "");
        assert!(matches!(parse_material_json(&no_shader), Err(Error::Malformed { .. })));
        let bad_shader = REAL.replace(r#""shaderType": "sknp""#, r#""shaderType": "nope""#);
        assert!(parse_material_json(&bad_shader).is_err());
        let bad_slot = REAL.replace(r#""0": "0x29FC975FA62CB0FE""#, r#""zero": "0x1""#);
        assert!(parse_material_json(&bad_slot).is_err());
        let bad_value = REAL.replace(r#""0": "0x29FC975FA62CB0FE""#, r#""0": "0x1 \"injected\"""#);
        assert!(parse_material_json(&bad_value).is_err());
        let non_string = REAL.replace(r#""0": "0x29FC975FA62CB0FE""#, r#""0": 5"#);
        assert!(parse_material_json(&non_string).is_err());
    }
}
