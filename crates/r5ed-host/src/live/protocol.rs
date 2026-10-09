//! The live link: the editor's live view of a running dedicated server.
//!
//! The server exposes a loopback line socket guarded by a token file. The host
//! speaks that protocol, and turns the page's JSON messages into the script
//! calls the server's own script tree is written against. Nothing from the
//! page is ever concatenated into script text: every value is validated into a
//! fixed grammar first and then formatted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ApiError, Result};

/// The dedicated server's default agent port.
pub const DEFAULT_PORT: u16 = 37019;
/// The token file, relative to the game root.
pub const TOKEN_REL: &str = "platform/agentlink.token";
/// The largest coordinate or angle the editor will send.
pub const LIMIT: f32 = 65536.0;
/// The model path grammar, as a prefix and an extension.
pub const MODEL_PREFIX: &str = "mdl/";
pub const MODEL_EXT: &str = ".rmdl";

/// One message from the page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Message {
    Prop { id: String, model: String, origin: [f32; 3], angles: [f32; 3], scale: f32 },
    Move { id: String, origin: [f32; 3], angles: [f32; 3] },
    Delete { id: String },
    Reload { stem: String },
    Status,
}

/// A validated message: every string is a uuid, every number is in range and
/// every model path matches the grammar, so the text below is total.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Checked {
    Prop { id: String, model: String, origin: [f32; 3], angles: [f32; 3], scale: f32 },
    Move { id: String, origin: [f32; 3], angles: [f32; 3] },
    Delete { id: String },
    Reload { stem: String },
    Status,
}

impl Checked {
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Prop { id, .. } | Self::Move { id, .. } | Self::Delete { id } => Some(id),
            _ => None,
        }
    }
}

fn uuid(s: &str) -> Result<String> {
    let Ok(v) = uuid::Uuid::parse_str(s) else {
        return Err(ApiError::bad("id must be a uuid"));
    };
    Ok(v.hyphenated().to_string())
}

fn vec3(v: [f32; 3]) -> Result<[f32; 3]> {
    for c in v {
        if !c.is_finite() {
            return Err(ApiError::bad("a vector component is not a number"));
        }
        if c.abs() > LIMIT {
            return Err(ApiError::bad(format!("a vector component is outside +-{LIMIT}")));
        }
    }
    Ok(v)
}

fn scale(s: f32) -> Result<f32> {
    if !s.is_finite() || s <= 0.0 || s > 1024.0 {
        return Err(ApiError::bad("scale must be a positive number no larger than 1024"));
    }
    Ok(s)
}

/// A model path is `mdl/` then segments of letters, digits, `_`, `-`, `.` and
/// `/`, ending in `.rmdl`. The segment rules make `..` and a leading `//`
/// impossible, the fixed prefix keeps the call inside the model tree, and the
/// extension may not repeat, so the file the server opens is the one named.
pub fn model_path(s: &str) -> Result<String> {
    let bad = || ApiError::bad("model must be mdl/<segments>.rmdl");
    let rest = s.strip_prefix(MODEL_PREFIX).ok_or_else(bad)?;
    let body = rest.strip_suffix(MODEL_EXT).ok_or_else(bad)?;
    if body.is_empty() || body.len() > 240 {
        return Err(bad());
    }
    if body.to_ascii_lowercase().ends_with(MODEL_EXT) {
        return Err(bad());
    }
    for seg in body.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(bad());
        }
        if !seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.') {
            return Err(bad());
        }
        if seg.starts_with('.') || seg.ends_with('.') {
            return Err(bad());
        }
    }
    Ok(s.to_string())
}

fn stem(s: &str) -> Result<String> {
    if s.is_empty()
        || s.len() > 64
        || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ApiError::bad("stem must be 1-64 characters of A-Z a-z 0-9 _ -"));
    }
    Ok(s.to_string())
}

/// Validates a message from the page. The error names the field, never the
/// value, so a probe learns nothing from the reply.
pub fn check(m: &Message) -> Result<Checked> {
    Ok(match m {
        Message::Prop { id, model, origin, angles, scale: s } => Checked::Prop {
            id: uuid(id)?,
            model: model_path(model)?,
            origin: vec3(*origin)?,
            angles: vec3(*angles)?,
            scale: scale(*s)?,
        },
        Message::Move { id, origin, angles } => {
            Checked::Move { id: uuid(id)?, origin: vec3(*origin)?, angles: vec3(*angles)? }
        }
        Message::Delete { id } => Checked::Delete { id: uuid(id)? },
        Message::Reload { stem: st } => Checked::Reload { stem: stem(st)? },
        Message::Status => Checked::Status,
    })
}

/// Parses a frame off the socket. A frame is one JSON object.
pub fn parse_frame(bytes: &[u8]) -> Result<Message> {
    if bytes.len() > 64 * 1024 {
        return Err(ApiError::BadRequest("live message is too large".into()));
    }
    serde_json::from_slice(bytes)
        .map_err(|_| ApiError::BadRequest("live message is not one of the known operations".into()))
}

/// The script function the server's own script tree changes maps through.
pub const CHANGE_MAP: &str = "GameRules_ChangeMap";
/// The live-prop verbs the server's map editor script defines, in its own
/// argument order: id, model, origin, angles, allow-mantle, scale.
pub const LIVE_PROP: &str = "ReMap_LiveProp";
pub const LIVE_MOVE: &str = "ReMap_LiveMove";
pub const LIVE_DELETE: &str = "ReMap_LiveDelete";

/// A float formatted with a fixed number of decimals, so the same input always
/// produces the same text.
fn f(v: f32) -> String {
    let s = format!("{:.4}", v);
    let t = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if t.is_empty() || t == "-0" {
        "0".to_string()
    } else {
        t
    }
}

/// A Squirrel vector literal, in the spelling the server's own scripts use.
fn vec(v: [f32; 3]) -> String {
    format!("< {}, {}, {} >", f(v[0]), f(v[1]), f(v[2]))
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The exact `SCRIPT` payload for a validated message.
///
/// One line, because the agent link is a line protocol: a payload with a
/// newline in it is split into several requests and the fragments are read as
/// unknown operations. Every interpolated value has been through a grammar, and
/// the free-text ones (a uuid, a model path, a stem) cannot hold a quote, a
/// backslash or a newline, so the text below is total.
pub fn script(m: &Checked) -> String {
    match m {
        Checked::Prop { id, model, origin, angles, scale: s } => format!(
            "{LIVE_PROP}( {}, ${}, {}, {}, true, {} );",
            q(id),
            q(model),
            vec(*origin),
            vec(*angles),
            f(*s)
        ),
        Checked::Move { id, origin, angles } => {
            format!("{LIVE_MOVE}( {}, {}, {} );", q(id), vec(*origin), vec(*angles))
        }
        Checked::Delete { id } => format!("{LIVE_DELETE}( {} );", q(id)),
        Checked::Reload { stem: st } => format!("{CHANGE_MAP}( {}, GetCurrentPlaylistName());", q(st)),
        Checked::Status => concat!(
            "AgentLink_Reply( \"{\\\"map\\\":\\\"\" + GetMapName() + ",
            "\"\\\",\\\"playlist\\\":\\\"\" + GetCurrentPlaylistName() + \"\\\"}\" );"
        )
        .to_string(),
    }
}

/// Where a live target's token file and socket live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub game_root: PathBuf,
    pub port: u16,
}

impl Target {
    pub fn new(game_root: &Path) -> Self {
        Self { game_root: game_root.to_path_buf(), port: DEFAULT_PORT }
    }

    pub fn token_path(&self) -> PathBuf {
        self.game_root.join("platform").join("agentlink.token")
    }

    /// Reads the launch token. The server writes exactly 64 hex characters.
    pub fn token(&self) -> Result<String> {
        let p = self.token_path();
        let text = std::fs::read_to_string(&p)
            .map_err(|_| ApiError::Unavailable("the agent link token file is not there yet".into()))?;
        let t = text.trim();
        if t.len() != 64 || !t.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Err(ApiError::Unavailable("the agent link token is malformed".into()));
        }
        Ok(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn a_model_path_must_match_the_grammar() {
        for ok in [
            "mdl/props/caustic_mug/caustic_mug_v.rmdl",
            "mdl/x.rmdl",
            "mdl/a-b_c.d/e.rmdl",
        ] {
            assert_eq!(model_path(ok).unwrap(), ok);
        }
        for bad in [
            "",
            "mdl",
            "mdl/",
            "mdl/x",
            "mdl//x.rmdl",
            "mdl/../x.rmdl",
            "mdl/a/../../b.rmdl",
            "/mdl/x.rmdl",
            "MDL/x.rmdl",
            "mdl/a b.rmdl",
            "mdl/a;b.rmdl",
            "mdl/a\"b.rmdl",
            "mdl/a\\b.rmdl",
            "mdl/a\nb.rmdl",
            "mdl/.hidden.rmdl",
            "mdl/x.rmdl.rmdl",
            "mdl/.rmdl",
            "mdl/trailing..rmdl",
        ] {
            assert!(model_path(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(model_path(&format!("mdl/{}.rmdl", "x".repeat(300))).is_err());
    }

    #[test]
    fn numbers_must_be_finite_and_in_range() {
        assert!(vec3([0.0, -1.5, 65536.0]).is_ok());
        assert!(vec3([f32::NAN, 0.0, 0.0]).is_err());
        assert!(vec3([f32::INFINITY, 0.0, 0.0]).is_err());
        assert!(vec3([65537.0, 0.0, 0.0]).is_err());
        assert!(scale(1.0).is_ok());
        assert!(scale(0.0).is_err());
        assert!(scale(-1.0).is_err());
        assert!(scale(2000.0).is_err());
        assert!(scale(f32::NAN).is_err());
    }

    #[test]
    fn ids_must_be_uuids() {
        let u = fresh_id();
        assert_eq!(uuid(&u).unwrap(), u);
        assert!(uuid("1").is_err());
        assert!(uuid("'; Drop() --").is_err());
        assert!(uuid("").is_err());
    }

    #[test]
    fn every_message_validates_into_its_checked_form() {
        let u = fresh_id();
        let cases = [
            Message::Prop {
                id: u.clone(),
                model: "mdl/props/a.rmdl".into(),
                origin: [1.0, 2.0, 3.0],
                angles: [0.0, 90.0, 0.0],
                scale: 1.0,
            },
            Message::Move { id: u.clone(), origin: [0.0; 3], angles: [0.0; 3] },
            Message::Delete { id: u.clone() },
            Message::Reload { stem: "mp_rr_gauntlet".into() },
            Message::Status,
        ];
        for c in &cases {
            assert!(check(c).is_ok(), "{c:?}");
        }
    }

    #[test]
    fn a_frame_that_is_not_a_known_message_is_refused() {
        assert!(parse_frame(br#"{"op":"prop","id":"nope"}"#).is_err());
        assert!(parse_frame(br#"{"op":"exec","code":"rm -rf /"}"#).is_err());
        assert!(parse_frame(b"not json").is_err());
        assert!(parse_frame(&vec![b'x'; 70 * 1024]).is_err());
    }
}
