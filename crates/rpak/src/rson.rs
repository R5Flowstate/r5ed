//! Reader for `platform/scripts/surfaceproperties.rson`.
//!
//! RSON is a brace-delimited key/value dialect. Only what the compiler needs
//! is read: each leaf table's `name -> id` mapping.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};

/// Surface prop name -> engine id.
pub type SurfaceProps = BTreeMap<String, u32>;

/// One `{ ... }` block, remembering the table name it was opened under.
struct Block {
    name: String,
    id: Option<u32>,
    key: Option<String>,
}

/// Parses a surfaceproperties document.
///
/// The grammar that matters is a repeated `name :` followed by a brace block
/// of `key : value` pairs. A block carrying an `id` is a leaf surface; one
/// without is a namespace and contributes nothing.
pub fn parse_surface_props(text: &str) -> Result<SurfaceProps> {
    let b = text.as_bytes();
    let mut out = SurfaceProps::new();
    let mut stack: Vec<Block> = Vec::new();
    // The token seen most recently outside any block.
    let mut outside: Option<String> = None;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'{' => {
                let name = outside.take().unwrap_or_default();
                stack.push(Block { name, id: None, key: None });
                i += 1;
            }
            b'}' => {
                let blk = stack.pop().ok_or(Error::Malformed {
                    what: "surfaceproperties",
                    msg: "unbalanced '}'".into(),
                })?;
                if let (Some(id), false) = (blk.id, blk.name.is_empty()) {
                    out.insert(blk.name, id);
                }
                i += 1;
            }
            b':' => i += 1,
            b'"' => {
                let (s, next) = read_quoted(b, i)?;
                accept(&mut stack, &mut outside, s);
                i = next;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => i = find_newline(b, i),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = match b[i + 2..].iter().position(|&c| c == b'*') {
                    Some(p) => i + 2 + p + 2,
                    None => b.len(),
                };
            }
            c if c.is_ascii_whitespace() || c == b',' => i += 1,
            _ => {
                let (tok, next) = read_token(b, i);
                accept(&mut stack, &mut outside, tok);
                i = next;
            }
        }
    }
    if !stack.is_empty() {
        return Err(Error::Malformed { what: "surfaceproperties", msg: "unclosed '{'".into() });
    }
    Ok(out)
}

/// Routes one lexed token by position: a table name outside any block, a key
/// or a value inside one.
fn accept(stack: &mut [Block], outside: &mut Option<String>, tok: String) {
    let Some(top) = stack.last_mut() else {
        *outside = Some(tok);
        return;
    };
    match top.key.take() {
        None => top.key = Some(tok),
        Some(k) => {
            if k == "id" {
                top.id = tok.parse::<u32>().ok();
            }
            // A value token ends the pair; the next token opens a new key.
            top.key = None;
        }
    }
}

fn read_quoted(b: &[u8], at: usize) -> Result<(String, usize)> {
    let mut i = at + 1;
    let mut out = String::new();
    while i < b.len() && b[i] != b'"' {
        if b[i] == b'\\' && i + 1 < b.len() {
            i += 1;
        }
        out.push(b[i] as char);
        i += 1;
    }
    if i >= b.len() {
        return Err(Error::Malformed { what: "surfaceproperties", msg: "unterminated string".into() });
    }
    Ok((out, i + 1))
}

fn read_token(b: &[u8], at: usize) -> (String, usize) {
    let mut i = at;
    while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'{' | b'}' | b',' | b'"' | b'/' | b':') {
        i += 1;
    }
    (String::from_utf8_lossy(&b[at..i]).into_owned(), i)
}

fn find_newline(b: &[u8], at: usize) -> usize {
    b[at..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| at + p + 1)
}

/// Reads `surfaceproperties.rson` from a game install.
pub fn read_surface_props_file(path: &Path) -> Result<SurfaceProps> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    parse_surface_props(&text)
}
