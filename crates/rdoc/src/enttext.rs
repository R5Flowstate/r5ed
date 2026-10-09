//! Parser for Valve entity text: `.ent` partition sidecars and the ENTITIES
//! lump inside a BSP.
//!
//! A sidecar opens with a header line (`ENTITIES02 num_models=4`); the lump has
//! no header. Both then hold a flat list of blocks:
//!
//! ```text
//! {
//! "classname" "light_spot"
//! "origin" "-4307.51 1684.35 184.5"
//! }
//! ```

use indexmap::IndexMap;

use crate::model::KeyValues;

/// One parsed entity block.
#[derive(Clone, Debug, PartialEq)]
pub struct EntBlock {
    pub keyvalues: KeyValues,
}

impl EntBlock {
    pub fn classname(&self) -> Option<&str> {
        self.keyvalues.get("classname").map(String::as_str)
    }

    /// A `*N` model key names a brush, which is how a compiled map marks a
    /// brush-backed entity.
    pub fn brush_model(&self) -> Option<&str> {
        self.keyvalues.get("model").map(String::as_str).filter(|m| m.starts_with('*'))
    }

    /// Collider companion key written next to a brush model reference.
    pub fn collider_model(&self) -> Option<&str> {
        self.keyvalues.get("*coll").map(String::as_str)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EntError {
    UnterminatedBlock,
    UnterminatedString,
    UnterminatedComment,
    BadEscape,
    MissingKeyValue,
}

/// The header line of a `.ent` sidecar, if present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntHeader {
    pub version: u32,
    pub num_models: u32,
}

impl EntHeader {
    /// Number of model slots the partition references (`*0` .. `*N`).
    pub fn models(&self) -> u32 {
        self.num_models
    }
}

/// Splits a partition or lump into its header and body.
pub fn split_header(text: &str) -> (Option<EntHeader>, &str) {
    let trimmed_start = text.trim_start_matches(['\u{feff}', ' ', '\r', '\n', '\t']);
    if !trimmed_start.starts_with('{') && trimmed_start.starts_with("ENTITIES") {
        let end = trimmed_start.find('\n').unwrap_or(trimmed_start.len());
        let line = &trimmed_start[..end];
        let mut words = line.split_whitespace();
        let version = words.next().and_then(|w| w.strip_prefix("ENTITIES")).and_then(|v| v.parse().ok());
        let num_models = words
            .find_map(|w| w.strip_prefix("num_models="))
            .and_then(|v| v.parse().ok());
        return (match (version, num_models) {
            (Some(v), Some(n)) => Some(EntHeader { version: v, num_models: n }),
            _ => None,
        }, &trimmed_start[end..]);
    }
    (None, text)
}

/// Parses entity blocks, in file order.
pub fn parse(text: &str) -> Result<(Option<EntHeader>, Vec<EntBlock>), EntError> {
    let (header, body) = split_header(text);
    parse_body(body).map(|blocks| (header, blocks))
}

fn parse_body(text: &str) -> Result<Vec<EntBlock>, EntError> {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let mut blocks: Vec<EntBlock> = Vec::new();
    let mut current: Option<EntBlock> = None;
    while pos < bytes.len() {
        match bytes[pos] {
            b'/' if bytes.get(pos + 1) == Some(&b'/') => {
                pos += 2;
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
            }
            b'{' => {
                if current.is_some() {
                    return Err(EntError::MissingKeyValue);
                }
                current = Some(EntBlock { keyvalues: KeyValues::new() });
                pos += 1;
            }
            b'}' => {
                let block = current.take().ok_or(EntError::UnterminatedBlock)?;
                blocks.push(block);
                pos += 1;
            }
            b'"' => {
                let key = read_token(bytes, &mut pos)?;
                skip_space(bytes, &mut pos);
                if bytes.get(pos) != Some(&b'"') {
                    return Err(EntError::MissingKeyValue);
                }
                let value = read_token(bytes, &mut pos)?;
                let block = current.as_mut().ok_or(EntError::UnterminatedBlock)?;
                block.keyvalues.insert(key, value);
            }
            _ => pos += 1,
        }
    }
    if current.is_some() {
        return Err(EntError::UnterminatedBlock);
    }
    Ok(blocks)
}

fn skip_space(bytes: &[u8], pos: &mut usize) {
    while matches!(bytes.get(*pos), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        *pos += 1;
    }
}

fn read_token(bytes: &[u8], pos: &mut usize) -> Result<String, EntError> {
    if bytes.get(*pos) != Some(&b'"') {
        return Err(EntError::UnterminatedString);
    }
    *pos += 1;
    let mut out: Vec<u8> = Vec::new();
    while let Some(&c) = bytes.get(*pos) {
        *pos += 1;
        match c {
            b'"' => return String::from_utf8(out).map_err(|_| EntError::UnterminatedString),
            b'\\' => {
                let Some(&esc) = bytes.get(*pos) else {
                    return Err(EntError::BadEscape);
                };
                *pos += 1;
                match esc {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'\\' => out.push(b'\\'),
                    b'"' => out.push(b'"'),
                    other => {
                        out.push(b'\\');
                        out.push(other);
                    }
                }
            }
            other => out.push(other),
        }
    }
    Err(EntError::UnterminatedString)
}

/// Writes blocks back out in the partition format.
pub fn write(header: Option<EntHeader>, blocks: &[EntBlock]) -> String {
    let mut out = String::new();
    if let Some(h) = header {
        out.push_str(&format!("ENTITIES0{} num_models={}\n", h.version % 10, h.num_models));
    }
    for block in blocks {
        out.push_str("{\n");
        for (k, v) in &block.keyvalues {
            out.push_str(&format!("\"{}\" \"{}\"\n", escape(k), escape(v)));
        }
        out.push_str("}\n");
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Groups blocks by classname into a count per class.
pub fn class_counts(blocks: &[EntBlock]) -> IndexMap<String, usize> {
    let mut out: IndexMap<String, usize> = IndexMap::new();
    for b in blocks {
        if let Some(cn) = b.classname() {
            *out.entry(cn.to_string()).or_insert(0) += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTITION: &str = "ENTITIES02 num_models=4\n\
        {\n\"classname\" \"light_spot\"\n\"origin\" \"-1 2 3\"\n}\n\
        {\n\"model\" \"*9\"\n\"classname\" \"trigger_multiple\"\n}\n";

    #[test]
    fn header_is_split_off() {
        let (header, body) = split_header(PARTITION);
        assert_eq!(header, Some(EntHeader { version: 2, num_models: 4 }));
        assert!(body.trim_start().starts_with('{'));
        let (none, body) = split_header("{\n\"classname\" \"worldspawn\"\n}\n");
        assert!(none.is_none());
        assert!(body.contains("worldspawn"));
    }

    #[test]
    fn blocks_parse_in_order() {
        let (header, blocks) = parse(PARTITION).unwrap();
        assert_eq!(header.unwrap().models(), 4);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].classname(), Some("light_spot"));
        assert_eq!(blocks[0].keyvalues["origin"], "-1 2 3");
        assert_eq!(blocks[1].brush_model(), Some("*9"));
        assert_eq!(blocks[1].collider_model(), None);
    }

    #[test]
    fn round_trips_through_the_writer() {
        let (_, blocks) = parse(PARTITION).unwrap();
        let text = write(parse(PARTITION).unwrap().0, &blocks);
        let (header, again) = parse(&text).unwrap();
        assert_eq!(header, Some(EntHeader { version: 2, num_models: 4 }));
        assert_eq!(again, blocks);
    }

    #[test]
    fn comments_and_escapes_are_handled() {
        let text = "// leading comment\n{ \"a\" \"say \\\"hi\\\"\" /* x */ \"b\" \"c\\\\d\" }\n";
        let (_, blocks) = parse(text).unwrap();
        assert_eq!(blocks[0].keyvalues["a"], "say \"hi\"");
        assert_eq!(blocks[0].keyvalues["b"], "c\\d");
    }

    #[test]
    fn malformed_text_is_an_error_not_a_panic() {
        assert!(parse("{ \"a\" \"b\"").is_err());
        assert!(parse("\"a\" \"b\"").is_err());
        assert!(parse("{ \"a\" }").is_err());
        assert!(parse("{\n}\n{\n").is_err());
    }

    #[test]
    fn class_counts_group_by_classname() {
        let (_, blocks) = parse(PARTITION).unwrap();
        let counts = class_counts(&blocks);
        assert_eq!(counts["light_spot"], 1);
        assert_eq!(counts["trigger_multiple"], 1);
    }
}
