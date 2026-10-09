//! Shared tokenizer for the Valve text formats (`.vmf` and Valve 220 `.map`).

use super::ImportError;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Str(String),
    Num(f64),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Equals,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("identifier {s:?}"),
            Tok::Str(s) => format!("string {s:?}"),
            Tok::Num(n) => format!("number {n}"),
            Tok::LParen => "`(`".into(),
            Tok::RParen => "`)`".into(),
            Tok::LBrace => "`{`".into(),
            Tok::RBrace => "`}`".into(),
            Tok::LBracket => "`[`".into(),
            Tok::RBracket => "`]`".into(),
            Tok::Equals => "`=`".into(),
        }
    }
}

pub fn parse_f64(t: &str) -> Result<f64, ImportError> {
    let v: f64 = t.parse().map_err(|_| ImportError::BadNumber(t.to_string()))?;
    if !v.is_finite() {
        return Err(ImportError::BadNumber(t.to_string()));
    }
    Ok(v)
}

/// Tokenizes both formats. `//` and `/* */` comments are dropped, and quoted
/// strings keep their escapes resolved.
pub fn tokenize(text: &str) -> Result<Vec<Tok>, ImportError> {
    let bytes = text.as_bytes();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' | b',' => i += 1,
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(ImportError::Eof);
                }
                i += 2;
            }
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'{' => {
                out.push(Tok::LBrace);
                i += 1;
            }
            b'}' => {
                out.push(Tok::RBrace);
                i += 1;
            }
            b'[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            b']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            b'=' => {
                out.push(Tok::Equals);
                i += 1;
            }
            b'"' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\n' {
                        return Err(ImportError::Eof);
                    }
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(ImportError::Eof);
                }
                out.push(Tok::Str(unescape(&text[start..i])));
                i += 1;
            }
            _ if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' => {
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'-' || bytes[i] == b'+') {
                    i += 1;
                }
                let raw = &text[start..i];
                out.push(Tok::Num(parse_f64(raw)?));
            }
            _ if c.is_ascii_alphanumeric() || c == b'_' || c == b'/' || c == b'\\' || c == b'*' || c == b'#' => {
                let start = i;
                while i < bytes.len()
                    && !bytes[i].is_ascii_whitespace()
                    && !matches!(bytes[i], b'"' | b'(' | b')' | b'{' | b'}' | b'[' | b']' | b'=' | b',')
                {
                    i += 1;
                }
                if i == start {
                    i += 1;
                    continue;
                }
                out.push(Tok::Ident(text[start..i].to_string()));
            }
            _ => i += 1,
        }
    }
    Ok(out)
}

fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// A cursor over a token list.
pub struct Cursor {
    pub tokens: Vec<Tok>,
    pub pos: usize,
}

impl Cursor {
    pub fn new(tokens: Vec<Tok>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    pub fn take(&mut self) -> Result<Tok, ImportError> {
        let t = self.tokens.get(self.pos).cloned().ok_or(ImportError::Eof)?;
        self.pos += 1;
        Ok(t)
    }

    pub fn expect(&mut self, want: &Tok) -> Result<(), ImportError> {
        let got = self.take()?;
        if &got == want {
            return Ok(());
        }
        Err(ImportError::Unexpected(got.describe()))
    }

    /// `[ x y z off ]` or `( x y z )`; the offset component is optional.
    pub fn vector(&mut self, open: &Tok, close: &Tok) -> Result<Vec<f64>, ImportError> {
        self.expect(open)?;
        let mut out = Vec::with_capacity(4);
        while let Some(t) = self.peek() {
            match t {
                Tok::Num(n) => {
                    out.push(*n);
                    self.pos += 1;
                }
                _ if *t == *close => {
                    self.pos += 1;
                    return Ok(out);
                }
                other => return Err(ImportError::Unexpected(other.describe())),
            }
        }
        Err(ImportError::Eof)
    }

    /// Skips tokens until the brace depth returns to zero.
    pub fn skip_block(&mut self) -> Result<(), ImportError> {
        let mut depth = 0usize;
        while self.pos < self.tokens.len() {
            match self.tokens[self.pos] {
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        self.pos += 1;
                        return Ok(());
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        Err(ImportError::Eof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_punctuation_and_strings() {
        let toks = tokenize("a { \"x y\" = 12 -3.5 } // tail").unwrap();
        assert_eq!(toks[0], Tok::Ident("a".into()));
        assert_eq!(toks[1], Tok::LBrace);
        assert_eq!(toks[2], Tok::Str("x y".into()));
        assert_eq!(toks[3], Tok::Equals);
        assert_eq!(toks[4], Tok::Num(12.0));
        assert_eq!(toks[5], Tok::Num(-3.5));
        assert_eq!(toks[6], Tok::RBrace);
    }

    #[test]
    fn reads_vectors() {
        let mut c = Cursor::new(tokenize("( 1 2 3 ) [ 4 5 6 7 ]").unwrap());
        assert_eq!(c.vector(&Tok::LParen, &Tok::RParen).unwrap(), vec![1.0, 2.0, 3.0]);
        assert_eq!(c.vector(&Tok::LBracket, &Tok::RBracket).unwrap(), vec![4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn skips_nested_blocks() {
        let mut c = Cursor::new(tokenize("skip { a { b } c } after").unwrap());
        c.skip_block().unwrap();
        assert_eq!(c.peek(), Some(&Tok::Ident("after".into())));
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        assert!(tokenize("\"unterminated").is_err());
        assert!(tokenize("/* unterminated").is_err());
        assert!(Cursor::new(tokenize("( 1 2").unwrap()).vector(&Tok::LParen, &Tok::RParen).is_err());
        assert!(tokenize("1e999").is_err());
        let mut c = Cursor::new(Vec::new());
        assert!(c.take().is_err());
        assert!(c.vector(&Tok::LParen, &Tok::RParen).is_err());
        assert!(c.skip_block().is_err());
    }
}
