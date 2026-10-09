//! Entity text: the worldspawn lump (0x00) and every `.ent` partition file.

use crate::{Error, Result, Version};

/// The `ENTITIES0N num_models=N` first line a partition file starts with. The
/// worldspawn lump has no such line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextHeader {
    pub ident: String,
    pub num_models: u32,
}

/// One block. Keys keep their file order and duplicates are preserved: the
/// engine reads them in order and a repeated key is not an error.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entity {
    pub pairs: Vec<(String, String)>,
}

impl Entity {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn classname(&self) -> Option<&str> {
        self.get("classname")
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (String, String)> {
        self.pairs.iter()
    }
}

impl From<Vec<(String, String)>> for Entity {
    fn from(pairs: Vec<(String, String)>) -> Self {
        Self { pairs }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityText {
    pub header: Option<TextHeader>,
    pub entities: Vec<Entity>,
    /// Bytes after the closing `}` of the last block, stored verbatim. Every
    /// shipped lump and partition file holds `\n` then the NUL.
    pub tail: Vec<u8>,
}

impl EntityText {
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    /// The `classname` of an entity, if it has one.
    pub fn classname(&self, index: usize) -> Option<&str> {
        self.entities.get(index).and_then(|e| e.classname())
    }

    pub fn count_of(&self, class: &str) -> usize {
        self.entities.iter().filter(|e| e.classname() == Some(class)).count()
    }

    /// Every key that names a brush model: `*7`, `*coll3`, and any other `*N`.
    pub fn model_keys(&self) -> impl Iterator<Item = &str> {
        self.entities
            .iter()
            .flat_map(|e| e.pairs.iter().map(|(k, _)| k.as_str()))
            .filter(|k| k.starts_with('*'))
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if let Some(h) = &self.header {
            out.push_str(&h.ident);
            out.push_str(" num_models=");
            out.push_str(&h.num_models.to_string());
            out.push('\n');
        }
        // The last block is closed without a newline: whatever followed it,
        // including the separator, is in `tail` exactly as it was stored.
        let last = self.entities.len().saturating_sub(1);
        for (i, e) in self.entities.iter().enumerate() {
            out.push_str("{\n");
            for (k, v) in &e.pairs {
                out.push('"');
                out.push_str(k);
                out.push_str("\" \"");
                out.push_str(v);
                out.push_str("\"\n");
            }
            out.push('}');
            if i != last {
                out.push('\n');
            }
        }
        for &b in &self.tail {
            out.push(b as char);
        }
        out
    }
}

fn parse_header(id: u16, line: &str) -> Result<TextHeader> {
    let mut it = line.split_whitespace();
    let ident = it.next().unwrap_or_default();
    if !ident.starts_with("ENTITIES") {
        return Err(Error::Invalid { id, msg: format!("first line {line:?} is not an ENTITIES header") });
    }
    let mut num_models = None;
    for tok in it {
        if let Some(v) = tok.strip_prefix("num_models=") {
            num_models = v.parse::<u32>().ok();
        }
    }
    let num_models = num_models.ok_or_else(|| Error::Invalid {
        id,
        msg: format!("header {line:?} carries no num_models="),
    })?;
    Ok(TextHeader { ident: ident.to_string(), num_models })
}

/// `"key" "value"`, tolerating the extra spaces the engine allows between tokens.
fn split_pair(id: u16, line: &str) -> Result<(String, String)> {
    let err = |m: String| Error::Invalid { id, msg: format!("{m} in {line:?}") };
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] != b'"' {
        i += 1;
    }
    if i == bytes.len() {
        return Err(err("key is not quoted".to_string()));
    }
    let key_end = i + 1 + line[i + 1..].find('"').ok_or_else(|| err("unterminated key".to_string()))?;
    let key = line[i + 1..key_end].to_string();
    let rest = line[key_end + 1..].trim_start();
    let rest = rest.strip_prefix('"').ok_or_else(|| err("value is not quoted".to_string()))?;
    let val = rest.strip_suffix('"').ok_or_else(|| err("unterminated value".to_string()))?;
    Ok((key, val.to_string()))
}

/// Parses entity text. `id` only reaches the error type.
pub fn parse_id(id: u16, data: &[u8], _version: Version) -> Result<EntityText> {
    let mut out = EntityText::default();
    // The body runs to the last `}`; everything after it is the tail and is
    // never interpreted. A blob with no `}` at all is all body, so an
    // unterminated block is reported rather than silently dropped, while a
    // header-only partition keeps its terminator in the tail.
    let header_end = if data.starts_with(b"ENTITIES") {
        data.iter().position(|&b| b == b'\n').map_or(data.len(), |p| p + 1)
    } else {
        0
    };
    let body_end = match data[header_end..].iter().rposition(|&b| b == b'}') {
        Some(p) => header_end + p + 1,
        None if header_end > 0 => header_end,
        None => data.len(),
    };
    out.tail = data[body_end..].to_vec();
    let text = String::from_utf8_lossy(&data[..body_end]);
    let mut rest: &str = &text;
    if let Some(nl) = rest.find('\n') {
        let first = rest[..nl].trim_end_matches('\r');
        if first.starts_with("ENTITIES") {
            out.header = Some(parse_header(id, first)?);
            rest = &rest[nl + 1..];
        }
    }
    while let Some(open) = rest.find('{') {
        if !rest[..open].trim().is_empty() {
            return Err(Error::Invalid {
                id,
                msg: format!("unexpected {:?} ahead of an entity block", &rest[..open]),
            });
        }
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .ok_or_else(|| Error::Invalid { id, msg: "entity block is not closed".to_string() })?;
        let mut pairs = Vec::new();
        for line in after[..close].lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            pairs.push(split_pair(id, line)?);
        }
        out.entities.push(Entity { pairs });
        rest = &after[close + 1..];
    }
    if !rest.trim().is_empty() {
        return Err(Error::Invalid { id, msg: format!("trailing text {rest:?} after the last entity") });
    }
    Ok(out)
}

pub fn parse(data: &[u8], version: Version) -> Result<EntityText> {
    parse_id(0x00, data, version)
}

pub fn write_id(_id: u16, text: &EntityText, _version: Version) -> Vec<u8> {
    text.to_text().into_bytes()
}

pub fn write(text: &EntityText, version: Version) -> Vec<u8> {
    write_id(0x00, text, version)
}

/// Number of `{` bytes in a raw blob: what an entity parse must match.
pub fn count_blocks(data: &[u8]) -> usize {
    data.iter().filter(|&&b| b == b'{').count()
}

/// The engine's entity-text parser advances two bytes per step and stops on the
/// last byte when it is a NUL, so a blob with blocks has to end `}` `\n` NUL. One
/// byte short and the parser runs a step past the buffer, scanning for a closing
/// quote with no bound.
pub fn terminator_is_intact(data: &[u8]) -> bool {
    if count_blocks(data) == 0 {
        return true;
    }
    data.last() == Some(&0) && data.len() >= 3 && data[data.len() - 3] == b'}'
}

/// What follows the closing brace of the last block in every shipped lump and
/// partition file.
pub const AFTER_LAST_BLOCK: [u8; 2] = [b'\n', 0];

/// `*7` and `*coll3` both name a brush model in the MODELS lump.
pub fn is_model_key(key: &str) -> bool {
    let Some(rest) = key.strip_prefix('*') else { return false };
    let digits = rest.strip_prefix("coll").unwrap_or(rest);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

pub fn model_index(key: &str) -> Option<u32> {
    if !is_model_key(key) {
        return None;
    }
    let rest = key.strip_prefix('*')?;
    let digits = rest.strip_prefix("coll").unwrap_or(rest);
    digits.parse::<u32>().ok()
}
