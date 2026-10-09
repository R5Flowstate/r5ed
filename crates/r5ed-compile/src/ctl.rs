//! `r5ed-compile ctl`: drives the user's open editor through the host relay.
//!
//! The host writes `{port, token}` to its session file on startup; this verb
//! reads it, POSTs one verb call, and prints the page's reply. The HTTP is a
//! tiny client over a loopback TcpStream, which is all a localhost POST needs.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// Where the host leaves `{port, token}`. Mirrors the host's own lookup so
/// the two agree without sharing a crate.
pub fn app_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        return PathBuf::from(d).join("r5ed");
    }
    if let Some(h) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h).join(".config").join("r5ed");
    }
    std::env::temp_dir().join("r5ed")
}

pub fn session_path() -> PathBuf {
    app_dir().join("session.json")
}

#[derive(Debug, PartialEq, Eq)]
pub struct Session {
    pub port: u16,
    pub token: String,
}

/// Parses a session file. A port of 0 or a missing token means no host wrote it.
pub fn parse_session(text: &str) -> Result<Session> {
    let v: serde_json::Value = serde_json::from_str(text).context("the session file is not JSON")?;
    let port = v.get("port").and_then(|p| p.as_u64()).context("the session file has no numeric port")?;
    if port == 0 || port > 65535 {
        bail!("the session file names an unusable port");
    }
    let token = v.get("token").and_then(|t| t.as_str()).context("the session file has no token")?;
    if token.is_empty() {
        bail!("the session file has an empty token");
    }
    Ok(Session { port: port as u16, token: token.to_string() })
}

pub fn read_session(path: &Path) -> Result<Session> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}; is the host running?", path.display()))?;
    parse_session(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Verbs are path segments on the relay; anything outside this grammar is refused.
pub fn valid_verb(verb: &str) -> bool {
    !verb.is_empty()
        && verb.len() <= 64
        && verb.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// Standard base64, no new dependency for one verb.
pub fn decode_base64(text: &str) -> Result<Vec<u8>> {
    let mut vals: Vec<u8> = Vec::new();
    for b in text.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 64,
            _ => bail!("base64 holds an illegal character"),
        };
        vals.push(v);
    }
    if vals.len() % 4 != 0 {
        bail!("base64 length is not a multiple of 4");
    }
    let mut out = Vec::with_capacity(vals.len() / 4 * 3);
    for q in vals.chunks_exact(4) {
        if q[0] == 64 || q[1] == 64 {
            bail!("base64 padding is misplaced");
        }
        let pad = q.iter().rev().take_while(|&&v| v == 64).count();
        if pad > 2 {
            bail!("base64 padding is misplaced");
        }
        let n = ((q[0] as u32) << 18) | ((q[1] as u32) << 12) | ((q[2] as u32 & 63) << 6) | (q[3] as u32 & 63);
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[derive(Debug, PartialEq, Eq)]
struct HttpReply {
    status: u16,
    body: Vec<u8>,
}

/// Splits one raw HTTP/1.1 response. Content-Length wins; chunked is decoded;
/// otherwise the body runs to the end of the bytes.
fn parse_http_response(raw: &[u8]) -> Result<HttpReply> {
    let head_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("the response has no header terminator")?;
    let head = std::str::from_utf8(&raw[..head_end]).context("the response head is not ASCII")?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().context("the response is empty")?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .context("the status line has no code")?
        .parse()
        .context("the status code is not a number")?;
    let mut length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        if let Some(v) = line.strip_prefix("Content-Length:").or_else(|| line.strip_prefix("content-length:")) {
            length = Some(v.trim().parse().context("Content-Length is not a number")?);
        }
        if line.to_ascii_lowercase().starts_with("transfer-encoding:") && line.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
    }
    let mut rest = &raw[head_end + 4..];
    if let Some(n) = length {
        if rest.len() < n {
            bail!("the body is shorter than Content-Length");
        }
        rest = &rest[..n];
        return Ok(HttpReply { status, body: rest.to_vec() });
    }
    if chunked {
        return Ok(HttpReply { status, body: decode_chunked(rest)? });
    }
    Ok(HttpReply { status, body: rest.to_vec() })
}

fn decode_chunked(mut raw: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = raw.windows(2).position(|w| w == b"\r\n").context("a chunk has no size line")?;
        let size_text = std::str::from_utf8(&raw[..end]).context("a chunk size is not ASCII")?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).context("a chunk size is not hex")?;
        raw = &raw[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if raw.len() < size + 2 {
            bail!("a chunk runs past the end of the body");
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

fn post_verb(session: &Session, verb: &str, params: &str) -> Result<serde_json::Value> {
    let addr = SocketAddr::from(([127, 0, 0, 1], session.port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .with_context(|| format!("connecting to 127.0.0.1:{}; is the host running?", session.port))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.set_read_timeout(Some(Duration::from_secs(70)))?;
    let request = format!(
        "POST /api/editor/{verb} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-R5ED-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        port = session.port,
        token = session.token,
        len = params.len()
    );
    stream.write_all(request.as_bytes())?;
    stream.write_all(params.as_bytes())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let reply = parse_http_response(&raw)?;
    if !(200..300).contains(&reply.status) {
        let text = String::from_utf8_lossy(&reply.body);
        bail!("the host answered {}: {}", reply.status, text.trim());
    }
    serde_json::from_slice(&reply.body).context("the host did not answer with JSON")
}

fn read_params(arg: Option<&str>) -> Result<String> {
    match arg {
        None => Ok("{}".to_string()),
        Some("-") => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(if s.trim().is_empty() { "{}".to_string() } else { s })
        }
        Some(s) => {
            let path = Path::new(s);
            if path.is_file() {
                Ok(std::fs::read_to_string(path).with_context(|| format!("reading {s}"))?)
            } else {
                Ok(s.to_string())
            }
        }
    }
}

/// Runs `ctl <verb> [json] [--out]`: POSTs the verb and prints the reply. A
/// screenshot reply also lands on disk, decoded from its base64 PNG.
pub fn run_ctl(verb: &str, json: Option<&str>, out: Option<&Path>) -> Result<()> {
    if !valid_verb(verb) {
        bail!("{verb:?} is not an editor verb");
    }
    if verb == "screenshot" && out.is_none() {
        bail!("screenshot needs --out <file.png>");
    }
    let session = read_session(&session_path())?;
    let params = read_params(json)?;
    let parsed: serde_json::Value = serde_json::from_str(&params).context("the params are not JSON")?;
    let reply = post_verb(&session, verb, &parsed.to_string())?;
    if verb == "screenshot" {
        let png = reply.get("png").and_then(|p| p.as_str()).context("the screenshot reply has no png")?;
        let bytes = decode_base64(png)?;
        let dest = out.expect("checked above");
        if let Some(dir) = dest.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
    }
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_parses_and_a_broken_one_names_its_fault() {
        let s = parse_session(r#"{"port": 7797, "token": "abc"}"#).unwrap();
        assert_eq!(s, Session { port: 7797, token: "abc".into() });
        for bad in [
            r#"{"port": 0, "token": "abc"}"#,
            r#"{"port": 99999, "token": "abc"}"#,
            r#"{"port": "7797", "token": "abc"}"#,
            r#"{"port": 7797}"#,
            r#"{"port": 7797, "token": ""}"#,
            "not json",
        ] {
            assert!(parse_session(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn verbs_are_path_segments_and_nothing_else() {
        for ok in ["state", "edit.select", "a-b_c9"] {
            assert!(valid_verb(ok), "{ok}");
        }
        for bad in ["", "a/b", "a b", "state;rm", "../x", &"x".repeat(65)] {
            assert!(!valid_verb(bad), "{bad}");
        }
    }

    #[test]
    fn base64_decodes_the_rfc_vectors_and_rejects_junk() {
        assert_eq!(decode_base64("").unwrap(), b"");
        assert_eq!(decode_base64("Zg==").unwrap(), b"f");
        assert_eq!(decode_base64("Zm9v").unwrap(), b"foo");
        assert_eq!(decode_base64("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(decode_base64(" Zg==\n").unwrap(), b"f");
        for bad in ["Zg", "Zm9!", "=====", "Z==="] {
            assert!(decode_base64(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_content_length_reply_is_cut_to_size() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\n{\"a\":1}trailing";
        let r = parse_http_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"{\"a\":1}");
    }

    #[test]
    fn a_chunked_reply_is_joined() {
        let raw = b"HTTP/1.1 503 Slow\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let r = parse_http_response(raw).unwrap();
        assert_eq!(r.status, 503);
        assert_eq!(r.body, b"Wikipedia");
    }

    #[test]
    fn a_reply_without_a_length_runs_to_the_end() {
        let raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}";
        let r = parse_http_response(raw).unwrap();
        assert_eq!(r.body, b"{}");
    }

    #[test]
    fn a_truncated_or_garbled_reply_is_an_error() {
        assert!(parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\"").is_err());
        assert!(parse_http_response(b"no headers here").is_err());
        assert!(parse_http_response(b"HTTP/1.1 OK\r\n\r\n{}").is_err());
    }
}
