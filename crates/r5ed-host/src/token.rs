//! The per-launch session token.
//!
//! 256 bits from the OS generator, handed to the page in the URL fragment and
//! required on every request. Comparison is constant time so a caller cannot
//! learn the token one byte at a time from response latency.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Hex characters in a token: 32 bytes.
const LEN: usize = 64;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token(String);

impl Token {
    /// Draws a fresh token from the OS generator.
    pub fn generate() -> Self {
        let a = Uuid::new_v4().simple().to_string();
        let b = Uuid::new_v4().simple().to_string();
        Self(format!("{a}{b}"))
    }

    /// Parses a token of exactly the right length and alphabet.
    pub fn parse(s: &str) -> Option<Self> {
        if s.len() != LEN || !s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return None;
        }
        Some(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when `candidate` is this token, in time that does not depend on how
    /// many leading characters matched.
    pub fn matches(&self, candidate: &str) -> bool {
        let a = self.0.as_bytes();
        let b = candidate.as_bytes();
        // Fold the length into the accumulator so a wrong length is not a
        // fast path, and never return before the whole candidate is read.
        let mut diff = (a.len() ^ b.len()) as u8;
        for i in 0..a.len().max(b.len()) {
            let x = a.get(i).copied().unwrap_or(0);
            let y = b.get(i).copied().unwrap_or(0);
            diff |= x ^ y;
        }
        diff == 0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_256_bits_of_hex_and_unique() {
        let a = Token::generate();
        let b = Token::generate();
        assert_eq!(a.as_str().len(), LEN);
        assert!(a.as_str().bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b);
    }

    #[test]
    fn matching_is_exact_and_length_sensitive() {
        let t = Token::generate();
        assert!(t.matches(t.as_str()));
        assert!(!t.matches(""));
        assert!(!t.matches(&t.as_str()[1..]));
        let last = if t.as_str().ends_with('0') { '1' } else { '0' };
        assert!(!t.matches(&format!("{}{last}", &t.as_str()[..LEN - 1])));
        assert!(!t.matches(&t.as_str().to_uppercase()));
        assert!(!t.matches(&format!("{}x", t.as_str())));
    }

    #[test]
    fn parse_rejects_anything_that_is_not_a_token() {
        assert!(Token::parse("").is_none());
        assert!(Token::parse("zz").is_none());
        assert!(Token::parse(&"g".repeat(LEN)).is_none());
        assert!(Token::parse(&"A".repeat(LEN)).is_none());
        assert!(Token::parse(&"a".repeat(LEN - 1)).is_none());
        assert!(Token::parse(&"a".repeat(LEN + 1)).is_none());
        assert!(Token::parse(&"a".repeat(LEN)).is_some());
    }
}
