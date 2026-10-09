//! Key reads performed by game scripts (`.nut` / `.gnut`).
//!
//! A key read is one of `GetValueForKey("x")`, `HasKey("x")`, `ent.kv.x` or
//! `kv["x"]`. When the enclosing scope is filtered by entity class, the read is
//! attributed to that class; otherwise it stays class-less evidence that the
//! key is live.

/// One key read found in a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyHit {
    /// Entity class or script name the surrounding scope filters by.
    pub scope: Option<String>,
    pub key: String,
}

/// Calls whose string argument names the entity class or script name a scope
/// iterates over.
const FILTERS: [&str; 3] = ["GetEntArrayByClass_Expensive", "GetEntArrayByClass", "GetEntArrayByScriptName"];
/// Calls whose string argument names an entity key.
const READERS: [&str; 2] = ["GetValueForKey", "HasKey"];
/// Bare `kv` table access, either `kv["x"]` or `kv.x`.
const KV: &str = "kv";

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Strips `//` and `/* */` comments, keeping string literals intact.
fn strip_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    if bytes[i] == b'\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                out.push(' ');
            }
            b'"' | b'\'' => {
                let quote = bytes[i];
                out.push(quote as char);
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        out.push(bytes[i] as char);
                        out.push(bytes[i + 1] as char);
                        i += 2;
                        continue;
                    }
                    let c = bytes[i];
                    out.push(c as char);
                    i += 1;
                    if c == quote {
                        break;
                    }
                }
            }
            other => {
                out.push(other as char);
                i += 1;
            }
        }
    }
    out
}

fn read_string(bytes: &[u8], pos: &mut usize) -> Option<String> {
    let quote = *bytes.get(*pos)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    *pos += 1;
    let mut out: Vec<u8> = Vec::new();
    while let Some(&c) = bytes.get(*pos) {
        *pos += 1;
        if c == b'\\' {
            if let Some(&esc) = bytes.get(*pos) {
                *pos += 1;
                out.push(esc);
                continue;
            }
            break;
        }
        if c == quote {
            return String::from_utf8(out).ok();
        }
        out.push(c);
    }
    None
}

fn skip_space(bytes: &[u8], pos: &mut usize) {
    while matches!(bytes.get(*pos), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        *pos += 1;
    }
}

/// Argument of `name(...)` starting at the identifier's first byte, or `None`
/// when the identifier is not followed by a string literal.
fn call_argument(bytes: &[u8], start: usize, name_len: usize) -> Option<String> {
    let mut pos = start + name_len;
    skip_space(bytes, &mut pos);
    if bytes.get(pos) != Some(&b'(') {
        return None;
    }
    pos += 1;
    skip_space(bytes, &mut pos);
    read_string(bytes, &mut pos)
}

/// Whether the identifier starting at `pos` is a standalone token, which a
/// member access after a dot still is.
fn token_start(bytes: &[u8], pos: usize) -> bool {
    match pos.checked_sub(1).and_then(|i| bytes.get(i)) {
        None => true,
        Some(&c) => !is_ident(c) && c != b'"' && c != b'\'',
    }
}

/// Scans one script for key reads. `scope` carries the class filter from the
/// enclosing function, which is reset at every function definition.
pub fn scan(path: &str, text: &str) -> Vec<KeyHit> {
    let clean = strip_comments(text);
    let bytes = clean.as_bytes();
    let mut hits: Vec<KeyHit> = Vec::new();
    let mut scope: Option<String> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' {
            let mut pos = i;
            let _ = read_string(bytes, &mut pos);
            i = pos.max(i + 1);
            continue;
        }
        if !token_start(bytes, i) {
            i += 1;
            continue;
        }
        if is_ident_start(c) {
            let start = i;
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            let ident = &clean[start..i];
            if ident == "function" {
                scope = None;
                continue;
            }
            if let Some(arg) = call_argument(bytes, start, ident.len()) {
                if FILTERS.contains(&ident) {
                    scope = Some(arg);
                    continue;
                }
                if READERS.contains(&ident) {
                    hits.push(KeyHit { scope: scope.clone(), key: arg });
                    continue;
                }
            }
            if ident == KV {
                let mut pos = i;
                skip_space(bytes, &mut pos);
                match bytes.get(pos) {
                    Some(b'[') => {
                        pos += 1;
                        skip_space(bytes, &mut pos);
                        if let Some(key) = read_string(bytes, &mut pos) {
                            hits.push(KeyHit { scope: scope.clone(), key });
                        }
                    }
                    Some(b'.') => {
                        pos += 1;
                        if matches!(bytes.get(pos), Some(&c) if is_ident_start(c)) {
                            let key_start = pos;
                            while pos < bytes.len() && is_ident(bytes[pos]) {
                                pos += 1;
                            }
                            hits.push(KeyHit { scope: scope.clone(), key: clean[key_start..pos].to_string() });
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        i += 1;
    }
    hits.retain(|h| !h.key.is_empty() && h.key.len() <= 64);
    hits.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.scope.cmp(&b.scope)));
    hits.dedup();
    let _ = path;
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_reader_form() {
        let src = r#"
            local e = GetEntArrayByClass_Expensive("prop_dynamic")
            foreach (ent in e) {
                print(ent.GetValueForKey("model"))
                if (ent.HasKey("solid")) print(ent.kv.rendermode)
                local s = ent.kv["start_active"]
            }
        "#;
        let hits = scan("a.nut", src);
        // Hits come back sorted, so a caller can diff two scans of one script.
        let keys: Vec<&str> = hits.iter().map(|h| h.key.as_str()).collect();
        assert_eq!(keys, vec!["model", "rendermode", "solid", "start_active"]);
        assert!(hits.iter().all(|h| h.scope.as_deref() == Some("prop_dynamic")));
    }

    #[test]
    fn unfiltered_reads_have_no_scope() {
        let hits = scan("b.nut", r#"local v = ent.GetValueForKey("angles");"#);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].scope, None);
        assert_eq!(hits[0].key, "angles");
    }

    #[test]
    fn comments_do_not_produce_hits() {
        let src = "// ent.GetValueForKey(\"nope\")\n/* ent.GetValueForKey(\"also_nope\") */\nreal.GetValueForKey(\"yes\")";
        let hits = scan("c.nut", src);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "yes");
    }

    #[test]
    fn scope_resets_at_a_function_definition() {
        let src = r#"
            function First() { local a = GetEntArrayByClass("light_spot") foreach (e in a) { e.GetValueForKey("brightness") } }
            function Second() { local e2 = GetValueForKey("orphan") }
        "#;
        let hits = scan("d.nut", src);
        let scoped = hits.iter().find(|h| h.key == "brightness").unwrap();
        assert_eq!(scoped.scope.as_deref(), Some("light_spot"));
        let orphan = hits.iter().find(|h| h.key == "orphan").unwrap();
        assert_eq!(orphan.scope, None);
    }

    #[test]
    fn script_name_filters_are_recorded_verbatim() {
        let hits = scan("e.nut", r#"GetEntArrayByScriptName("relic_stand_script_name") ent.kv.model"#);
        assert_eq!(hits[0].scope.as_deref(), Some("relic_stand_script_name"));
        assert_eq!(hits[0].key, "model");
    }

    #[test]
    fn unterminated_input_does_not_panic() {
        for src in ["ent.GetValueForKey(", "kv[", "ent.kv.", "\"", "kv[", "function"] {
            let _ = scan("f.nut", src);
        }
    }
}
