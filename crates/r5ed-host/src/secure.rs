//! Request admission: the session token, the `Host` and `Origin` checks, and
//! the response headers every page and every API reply carries.
//!
//! These are the defences against a page in the user's browser driving the
//! host: the token cannot be read cross-origin, the `Host` check stops a
//! rebound DNS name from resolving here, and the absence of any CORS header
//! means a foreign page can neither preflight nor read a reply.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::token::Token;

pub const TOKEN_HEADER: &str = "x-r5ed-token";
/// The cookie the editor page receives, so a plain `http://127.0.0.1:<port>/`
/// works without the token in the URL. It is HttpOnly and SameSite=Strict: a
/// foreign page's request never carries it, and script cannot read it.
pub const TOKEN_COOKIE: &str = "r5ed_token";

/// The `Set-Cookie` value that hands the session token to the editor page.
pub fn token_cookie(token: &Token) -> String {
    format!("{TOKEN_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict", token.as_str())
}

/// The strictest policy that still lets the editor's own bundle and its wasm
/// workers run. No remote origin is reachable from the page.
const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
connect-src 'self' ws://127.0.0.1:* ws://localhost:*; \
worker-src 'self' blob:; object-src 'none'; base-uri 'none'; \
form-action 'none'; frame-ancestors 'none'";

/// Adds the headers every response needs, whatever the route.
pub fn set_security_headers(res: &mut Response) {
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(HeaderName::from_static("cross-origin-opener-policy"), HeaderValue::from_static("same-origin"));
    h.insert(HeaderName::from_static("cross-origin-embedder-policy"), HeaderValue::from_static("require-corp"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static("DENY"),
    );
}

/// Answers a preflight or any other method the API does not implement. There
/// is no CORS support at all, so a foreign page gets nothing usable.
pub fn no_cors<T>(_: T) -> Response {
    let mut res = StatusCode::METHOD_NOT_ALLOWED.into_response();
    set_security_headers(&mut res);
    res
}

/// The `Host` values this server answers to: the two names the socket is
/// reachable under. Anything else is a rebound name or a proxy that has no
/// business here.
pub fn host_allowed(host: &str, port: u16) -> bool {
    if host.is_empty() {
        return false;
    }
    let host = host.split_once(',').map_or(host, |(h, _)| h).trim();
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

/// The `Origin` values a browser may legitimately send: this host's own two
/// names, and nothing else. An absent Origin is allowed because a non-browser
/// client (the CLI, curl) does not send one and still needs the token.
pub fn origin_allowed(origin: &str, port: u16) -> bool {
    if origin.is_empty() {
        return true;
    }
    let o = origin.trim();
    o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
}

/// Strips a control character out of a message that is about to be logged.
pub fn scrub(msg: &str) -> String {
    msg.chars().filter(|c| !c.is_control()).take(200).collect()
}

/// The state every guarded route needs.
#[derive(Clone)]
pub struct Guard {
    pub token: Arc<Token>,
    pub port: u16,
}

impl Guard {
    pub fn new(token: Token, port: u16) -> Self {
        Self { token: Arc::new(token), port }
    }
}

/// Rejects a request that did not come from this host's own page.
pub fn check_host(headers: &HeaderMap, port: u16) -> Result<(), ApiError> {
    // More than one Host header is not a request, it is an attempt to have this
    // server and something in front of it disagree about who the caller is.
    let mut hosts = headers.get_all(header::HOST).iter();
    let host = hosts.next().and_then(|v| v.to_str().ok()).unwrap_or("");
    if hosts.next().is_some() {
        return Err(ApiError::Forbidden);
    }
    if !host_allowed(host, port) {
        return Err(ApiError::Forbidden);
    }
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !origin_allowed(origin, port) {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

/// The token from the header, or from the query string for the WebSocket
/// handshake, which cannot set headers.
pub fn request_token(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    if let Some(v) = headers.get(TOKEN_HEADER) {
        return v.to_str().ok().map(str::to_string);
    }
    for v in headers.get_all(header::COOKIE) {
        let Ok(text) = v.to_str() else { continue };
        for part in text.split(';') {
            if let Some(t) = part.trim().strip_prefix(TOKEN_COOKIE).and_then(|r| r.strip_prefix('=')) {
                return Some(t.to_string());
            }
        }
    }
    let q = uri.query()?;
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("token=") {
            return Some(percent_decode(v));
        }
    }
    None
}

/// Decodes the two escapes a token could plausibly carry. Anything else is left
/// alone and will simply fail the comparison.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None,
            };
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The admission middleware for everything under `/api`.
pub async fn admit(
    State(guard): State<Guard>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    match admit_inner(&guard, &req) {
        Ok(()) => {
            let mut res = next.run(req).await;
            set_security_headers(&mut res);
            res
        }
        Err(e) => {
            let mut res = e.into_response();
            set_security_headers(&mut res);
            res
        }
    }
}

fn admit_inner(guard: &Guard, req: &axum::extract::Request) -> Result<(), ApiError> {
    // No CORS preflight is ever answered, so an OPTIONS from a page is a
    // foreign caller rather than the editor.
    if req.method() == Method::OPTIONS {
        return Err(ApiError::Forbidden);
    }
    check_host(req.headers(), guard.port)?;
    let given = request_token(req.headers(), req.uri()).ok_or(ApiError::Unauthorized)?;
    if !guard.token.matches(&given) {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

/// A minimal response for the body-limit layer's rejection, so an oversize
/// body answers 413 with the same headers as everything else.
pub fn payload_too_large() -> Response {
    let mut res = ApiError::BadRequest("request body is too large".into()).into_response();
    *res.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
    set_security_headers(&mut res);
    res
}

/// Body caps. A file PUT is the largest thing the API accepts; everything with
/// json in it is an order of magnitude smaller.
pub const MAX_FILE_BYTES: usize = 64 << 20;
pub const MAX_JSON_BYTES: usize = 1 << 20;
pub const MAX_CHUNK_BYTES: usize = 96 << 20;

/// An empty body with a status, for handlers that only report success.
pub fn empty(status: StatusCode) -> Response {
    let mut res = Response::builder().status(status).body(Body::empty()).expect("static response");
    set_security_headers(&mut res);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_hosts_own_names_are_allowed() {
        assert!(host_allowed("127.0.0.1:7797", 7797));
        assert!(host_allowed("localhost:7797", 7797));
        assert!(host_allowed("localhost:7797, 127.0.0.1:7797", 7797));
        for bad in [
            "",
            "evil.test:7797",
            "127.0.0.1",
            "127.0.0.1:80",
            "127.0.0.2:7797",
            "[::1]:7797",
            "127.0.0.1:7797.evil.test",
        ] {
            assert!(!host_allowed(bad, 7797), "{bad:?}");
        }
    }

    #[test]
    fn two_host_headers_are_refused_rather_than_picked_from() {
        let mut h = HeaderMap::new();
        assert!(check_host(&h, 7797).is_err(), "no host at all");
        h.append(header::HOST, HeaderValue::from_static("127.0.0.1:7797"));
        assert!(check_host(&h, 7797).is_ok());
        h.append(header::HOST, HeaderValue::from_static("evil.test"));
        assert!(check_host(&h, 7797).is_err(), "two names is not a request");
    }

    #[test]
    fn only_this_hosts_own_origins_are_allowed() {
        assert!(origin_allowed("", 7797));
        assert!(origin_allowed("http://127.0.0.1:7797", 7797));
        assert!(origin_allowed("http://localhost:7797", 7797));
        for bad in [
            "http://evil.test",
            "https://localhost:7797",
            "http://127.0.0.1:80",
            "null",
            "http://127.0.0.1:7797.evil.test",
        ] {
            assert!(!origin_allowed(bad, 7797), "{bad:?}");
        }
    }

    #[test]
    fn the_token_is_read_from_the_header_or_the_query() {
        let mut h = HeaderMap::new();
        h.insert(TOKEN_HEADER, HeaderValue::from_static("abc"));
        let uri: Uri = "/api/live".parse().unwrap();
        assert_eq!(request_token(&h, &uri).as_deref(), Some("abc"));
        let uri: Uri = "/api/live?token=def".parse().unwrap();
        assert_eq!(request_token(&h, &uri).as_deref(), Some("abc"), "the header wins");
        let empty = HeaderMap::new();
        assert_eq!(request_token(&empty, &uri).as_deref(), Some("def"));
        let uri: Uri = "/api/live".parse().unwrap();
        assert!(request_token(&empty, &uri).is_none());
    }

    #[test]
    fn percent_escapes_in_the_query_are_decoded() {
        assert_eq!(percent_decode("%61%62"), "ab");
        assert_eq!(percent_decode("ab"), "ab");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%4"), "%4");
    }

    #[test]
    fn the_security_headers_are_all_present() {
        let mut res = Response::new(Body::empty());
        set_security_headers(&mut res);
        let h = res.headers();
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h[HeaderName::from_static("cross-origin-opener-policy")], "same-origin");
        assert_eq!(h[HeaderName::from_static("cross-origin-embedder-policy")], "require-corp");
        let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("wasm-unsafe-eval"));
        assert!(!csp.contains("'unsafe-eval'"));
        assert!(csp.contains("object-src 'none'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(h.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    }

    #[test]
    fn scrubbing_drops_control_characters() {
        assert_eq!(scrub("a\nb\0c"), "abc");
        assert_eq!(scrub(&"x".repeat(500)).len(), 200);
    }
}
