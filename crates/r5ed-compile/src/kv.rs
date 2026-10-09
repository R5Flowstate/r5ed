//! Valve key values, the shape the overview and playlist files use.
//!
//! A document is a list of key/value pairs. A value is either a bare or quoted
//! token, or a braced block of more pairs, and `//` starts a comment. Keys and
//! values are written bare unless they would otherwise be ambiguous, and blocks
//! are indented with tabs, which is how the shipped files read.

/// A parsed value: a token, or a block of pairs.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(String),
    Block(Entries),
}

/// Key/value pairs, in file order.
pub type Entries = Vec<(String, Node)>;

/// Deepest nesting accepted, which keeps a hand-edited file from exhausting the
/// stack.
pub const MAX_DEPTH: usize = 32;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("a quoted token is not closed")]
    Unterminated,
    #[error("key {0:?} has no value")]
    NoValue(String),
    #[error("a block opened for {0:?} is not closed")]
    Unclosed(String),
    #[error("{0} closing braces do not match")]
    Unbalanced(usize),
    #[error("{0:?} is not a key")]
    StrayToken(String),
    #[error("a document cannot nest more than {MAX_DEPTH} blocks")]
    TooDeep,
    #[error("text follows the last pair")]
    Trailing,
    #[error("a key cannot be empty")]
    EmptyKey,
}

impl Node {
    /// A block of pairs.
    pub fn block(entries: Entries) -> Self {
        Node::Block(entries)
    }

    /// A leaf holding one value.
    pub fn leaf(value: impl Into<String>) -> Self {
        Node::Leaf(value.into())
    }

    /// The pair under `key`, when this is a block that has one.
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Block(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            Node::Leaf(_) => None,
        }
    }

    /// The first path in the document, `a.b.c`.
    pub fn path(&self, path: &str) -> Option<&Node> {
        let mut node = self;
        for part in path.split('.') {
            node = node.get(part)?;
        }
        Some(node)
    }

    /// The token, when this is a leaf.
    pub fn token(&self) -> Option<&str> {
        match self {
            Node::Leaf(v) => Some(v.as_str()),
            Node::Block(_) => None,
        }
    }

}

/// One token with where it started, for error messages.
struct Lexer<'a> {
    text: &'a [u8],
    at: usize,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str) -> Self {
        Self { text: text.as_bytes(), at: 0 }
    }

    fn skip_space(&mut self) {
        while let Some(c) = self.text.get(self.at) {
            match c {
                b' ' | b'\t' | b'\r' | b'\n' => self.at += 1,
                b'/' if self.text.get(self.at + 1) == Some(&b'/') => {
                    while self.at < self.text.len() && self.text[self.at] != b'\n' {
                        self.at += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_space();
        self.text.get(self.at).copied()
    }

    /// The next token, or `None` at the end of the document.
    fn token(&mut self) -> Result<Option<String>, Error> {
        match self.peek() {
            None => Ok(None),
            Some(b'"') => {
                self.at += 1;
                let mut out = Vec::new();
                loop {
                    let Some(&c) = self.text.get(self.at) else {
                        return Err(Error::Unterminated);
                    };
                    self.at += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let Some(&e) = self.text.get(self.at) else {
                                return Err(Error::Unterminated);
                            };
                            self.at += 1;
                            out.push(match e {
                                b'n' => b'\n',
                                b't' => b'\t',
                                b'r' => b'\r',
                                other => other,
                            });
                        }
                        other => out.push(other),
                    }
                }
                String::from_utf8(out).map(Some).map_err(|_| Error::Unterminated)
            }
            Some(b'{') | Some(b'}') => {
                let at = self.at;
                self.at += 1;
                Err(Error::StrayToken((self.text[at] as char).to_string()))
            }
            Some(_) => {
                let start = self.at;
                while let Some(&c) = self.text.get(self.at) {
                    if c.is_ascii_whitespace() || matches!(c, b'{' | b'}' | b'"') {
                        break;
                    }
                    self.at += 1;
                }
                let raw = std::str::from_utf8(&self.text[start..self.at]).map_err(|_| Error::Unterminated)?;
                Ok(Some(raw.to_string()))
            }
        }
    }

    fn open(&mut self) -> bool {
        if self.peek() == Some(b'{') {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn close(&mut self) -> bool {
        if self.peek() == Some(b'}') {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn eof(&mut self) -> bool {
        self.peek().is_none()
    }

    fn entries(&mut self, depth: usize, top: bool) -> Result<Entries, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        let mut out = Entries::new();
        loop {
            if self.close() {
                if top {
                    return Err(Error::Unbalanced(1));
                }
                return Ok(out);
            }
            if self.eof() {
                if top {
                    return Ok(out);
                }
                let last = out.last().map(|(k, _)| k.clone()).unwrap_or_default();
                return Err(Error::Unclosed(last));
            }
            let Some(key) = self.token()? else {
                if top {
                    return Ok(out);
                }
                return Err(Error::Unclosed(String::new()));
            };
            if key.is_empty() {
                return Err(Error::EmptyKey);
            }
            let node = if self.open() {
                Node::Block(self.entries(depth + 1, false)?)
            } else {
                match self.token()? {
                    Some(v) => Node::Leaf(v),
                    None => return Err(Error::NoValue(key)),
                }
            };
            out.push((key, node));
        }
    }
}

/// Parses a whole document.
pub fn parse(text: &str) -> Result<Entries, Error> {
    let mut lexer = Lexer::new(text);
    let entries = lexer.entries(0, true)?;
    if !lexer.eof() {
        return Err(Error::Trailing);
    }
    Ok(entries)
}

fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s.starts_with("//")
        || s.chars().any(|c| c.is_whitespace() || matches!(c, '{' | '}' | '"'))
}

fn quote(s: &str) -> String {
    if !needs_quotes(s) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn write_entries(entries: &Entries, depth: usize, out: &mut String) {
    let pad = "\t".repeat(depth);
    for (key, node) in entries {
        match node {
            Node::Leaf(v) => {
                out.push_str(&pad);
                out.push_str(&quote(key));
                out.push(' ');
                out.push_str(&quote(v));
                out.push('\n');
            }
            Node::Block(inner) => {
                out.push_str(&pad);
                out.push_str(&quote(key));
                out.push('\n');
                out.push_str(&pad);
                out.push_str("{\n");
                write_entries(inner, depth + 1, out);
                out.push_str(&pad);
                out.push_str("}\n");
            }
        }
    }
}

/// Writes a document in the shipped layout: tabs for depth, a trailing newline.
pub fn write(entries: &Entries) -> String {
    let mut out = String::new();
    write_entries(entries, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OVERVIEW: &str = "\"mp_rr_district\"\n{\n\tassets\n\t{\n\t\tminimap overviews\\mp_rr_district\n\t}\n\n\tpos_x -43008\n\tpos_y 43008\n\tscale 21\n\tdisplayDist 2048\n}\n";

    #[test]
    fn a_shipped_overview_parses() {
        let doc = parse(OVERVIEW).expect("parses");
        assert_eq!(doc.len(), 1);
        let (name, root) = &doc[0];
        assert_eq!(name, "mp_rr_district");
        assert_eq!(root.path("assets.minimap").unwrap().token(), Some("overviews\\mp_rr_district"));
        assert_eq!(root.path("pos_x").unwrap().token(), Some("-43008"));
        assert_eq!(root.path("scale").unwrap().token(), Some("21"));
        assert!(root.path("nope").is_none());
        assert!(root.get("assets").unwrap().get("nope").is_none());
        assert_eq!(root.token(), None);
        assert!(root.token().is_none());
    }

    #[test]
    fn writing_and_reading_is_stable() {
        let doc = parse(OVERVIEW).expect("parses");
        let once = write(&doc);
        // A block sits on its own line at the depth of its key, which is the
        // shape the shipped file uses.
        assert!(once.starts_with("mp_rr_district\n{\n\tassets\n\t{\n\t\tminimap overviews\\mp_rr_district\n\t}\n"), "{once}");
        assert_eq!(parse(&once).unwrap(), doc, "the values survive the write");
        assert_eq!(write(&parse(&once).unwrap()), once, "and writing twice is the same");
    }
    #[test]
    fn a_playlist_patch_round_trips() {
        let text = "playlists\n{\n\tPlaylists\n\t{\n\t\tmp_rr_test__one\n\t\t{\n\t\t\tinherit survival_dev\n\t\t\tgamemodes { survival { maps { mp_rr_test__one 1 } } }\n\t\t}\n\t}\n}\n";
        let doc = parse(text).expect("parses");
        let entry = &doc[0].1.path("Playlists.mp_rr_test__one").expect("the entry is there");
        assert_eq!(entry.path("inherit").unwrap().token(), Some("survival_dev"));
        assert_eq!(entry.path("gamemodes.survival.maps.mp_rr_test__one").unwrap().token(), Some("1"));
        // Written back, a block sits on its own line at the key's depth, which
        // is the shape the shipped playlist patch uses.
        let written = write(&doc);
        assert!(written.contains("\t\tmp_rr_test__one\n\t\t{\n"), "{written}");
        assert!(written.contains("\t\t\t\t\tmaps\n\t\t\t\t\t{\n\t\t\t\t\t\tmp_rr_test__one 1\n"), "{written}");
        assert_eq!(parse(&written).unwrap(), doc);
    }

    #[test]
    fn comments_quotes_and_escapes_are_handled() {
        let text = "// a comment\nname\n{\n\t\"a b\" \"say \\\"hi\\\"\"\n\tc \"d\\\\e\"\n}\n";
        let doc = parse(text).expect("parses");
        assert_eq!(doc[0].0, "name");
        assert_eq!(doc[0].1.get("a b").unwrap().token(), Some("say \"hi\""));
        assert_eq!(doc[0].1.get("c").unwrap().token(), Some("d\\e"));
        // A value that needs quoting comes back quoted.
        let written = write(&doc);
        assert!(written.contains("\"a b\" \"say \\\"hi\\\"\""), "{written}");
        assert_eq!(parse(&written).unwrap(), doc);
    }

    #[test]
    fn malformed_text_is_an_error_not_a_panic() {
        for bad in ["{ \"a\"", "\"a\"", "{ \"a\" }", "a b }", "{ a { b }", "\"unclosed", "{ } extra"] {
            assert!(parse(bad).is_err(), "{bad:?} should be refused");
        }
        let deep = format!("{}a 1{}", "b {".repeat(MAX_DEPTH + 2), "}".repeat(MAX_DEPTH + 2));
        assert!(matches!(parse(&deep), Err(Error::TooDeep)));
        assert_eq!(parse("").unwrap().len(), 0);
        assert!(parse("   // only a comment\n").unwrap().is_empty());
    }
}
