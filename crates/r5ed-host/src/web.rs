//! Serving the editor bundle.
//!
//! The bundle is built by the web workspace, so it is embedded rather than
//! served from disk: the host has one directory to ship and no way to be talked
//! into reading outside it. A missing `index.html` inside the embedded set
//! falls back to a page that says the bundle was not built.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::Response;
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web/dist"]
struct Bundle;

const FALLBACK: &str = "<!doctype html>\n<meta charset=\"utf-8\">\n<title>r5ed</title>\n\
<p>The editor bundle was not built. Build the web workspace first.</p>\n";

/// A byte range is only accepted when it is a simple, satisfiable one; anything
/// else gets the whole file rather than a wrong slice.
fn parse_range(spec: &str, len: usize) -> Option<(usize, usize)> {
    let spec = spec.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    if len == 0 {
        return None;
    }
    if a.is_empty() {
        let n: usize = b.parse().ok()?;
        if n == 0 {
            return None;
        }
        return Some((len.saturating_sub(n), len));
    }
    let start: usize = a.parse().ok()?;
    if start >= len {
        return None;
    }
    let end = if b.is_empty() { len } else { b.parse::<usize>().ok()?.saturating_add(1).min(len) };
    (end > start).then_some((start, end))
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "glb" => "model/gltf-binary",
        "dds" => "image/vnd.ms.dds",
        "txt" | "rson" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn plain(status: StatusCode, mime: &'static str, body: &[u8]) -> Response {
    let mut res = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(mime))
        .body(Body::from(body.to_vec()))
        .expect("static response");
    crate::secure::set_security_headers(&mut res);
    res
}

fn not_found() -> Response {
    plain(StatusCode::NOT_FOUND, "text/plain; charset=utf-8", b"not found")
}

/// A file of the bundle and its etag. `R5ED_WEB_DIR` serves a built bundle
/// from disk instead of the embedded one, so a UI rebuild needs no host restart.
fn load(name: &str) -> Option<(Vec<u8>, String)> {
    if let Some(dir) = std::env::var_os("R5ED_WEB_DIR") {
        let data = std::fs::read(std::path::Path::new(&dir).join(name)).ok()?;
        let len = data.len();
        let stamp = data.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
        return Some((data, format!("{stamp:016x}{len:x}")));
    }
    let file = Bundle::get(name)?;
    let etag = file.metadata.sha256_hash().iter().map(|b| format!("{b:02x}")).collect::<String>();
    Some((file.data.to_vec(), etag))
}

/// Serves one bundle file, or the fallback page for a directory root.
pub fn serve(uri: &Uri, range: Option<&str>) -> Response {
    let path = uri.path();
    let mut name = path.trim_start_matches('/').to_string();
    if name.is_empty() || name.ends_with('/') {
        name.push_str("index.html");
    }
    // A path that tries to climb, or that has an empty segment, is a probe.
    if name.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        return not_found();
    }
    if name.contains('\\') || name.contains(':') {
        return not_found();
    }
    let Some((data, etag)) = load(&name) else {
        return if path == "/" { plain(StatusCode::OK, "text/html; charset=utf-8", FALLBACK.as_bytes()) } else { not_found() };
    };
    let mime = content_type(&name);
    if let Some((start, end)) = range.and_then(|r| parse_range(r, data.len())) {
        let mut res = Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, HeaderValue::from_static(mime))
            .header(header::CONTENT_RANGE, format!("bytes {start}-{}/{}", end - 1, data.len()))
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ETAG, HeaderValue::from_str(&format!("\"{etag}\"")).unwrap_or(HeaderValue::from_static("\"0\"")))
            .body(Body::from(data[start..end].to_vec()))
            .expect("static response");
        crate::secure::set_security_headers(&mut res);
        return res;
    }
    let mut res = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(mime))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, HeaderValue::from_str(&format!("\"{etag}\"")).unwrap_or(HeaderValue::from_static("\"0\"")))
        .body(Body::from(data))
        .expect("static response");
    crate::secure::set_security_headers(&mut res);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_only_honoured_when_they_are_simple_and_satisfiable() {
        assert_eq!(parse_range("bytes=0-3", 10), Some((0, 4)));
        assert_eq!(parse_range("bytes=0-1", 10), Some((0, 2)));
        assert_eq!(parse_range("bytes=5-", 10), Some((5, 10)));
        assert_eq!(parse_range("bytes=-4", 10), Some((6, 10)));
        assert_eq!(parse_range("bytes=0-99", 10), Some((0, 10)));
        // Malformed, unsatisfiable, and the multipart forms that are not worth
        // answering: each of those gets the whole file.
        for bad in [
            "bytes=10-",
            "bytes=9-2",
            "bytes=abc",
            "items=0-1",
            "bytes=",
            "bytes=0-1,3",
            "bytes=-",
            "bytes=1-2-3",
            "bytes=0-1 ",
            "  bytes=0-1",
        ] {
            assert_eq!(parse_range(bad, 10), None, "{bad}");
        }
        assert_eq!(parse_range("bytes=0-0", 0), None);
        assert_eq!(parse_range("bytes=-0", 10), None);
    }

    #[test]
    fn content_types_cover_the_bundle_and_the_asset_routes() {
        assert_eq!(content_type("a.wasm"), "application/wasm");
        assert_eq!(content_type("a.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("a.glb"), "model/gltf-binary");
        assert_eq!(content_type("a.dds"), "image/vnd.ms.dds");
        assert_eq!(content_type("a.unknown"), "application/octet-stream");
        assert_eq!(content_type("noext"), "application/octet-stream");
    }

    #[test]
    fn a_climbing_path_is_not_served_and_the_root_always_is() {
        let uri: Uri = "/../secret".parse().unwrap();
        assert_eq!(serve(&uri, None).status(), StatusCode::NOT_FOUND);
        let uri: Uri = "/a//b".parse().unwrap();
        assert_eq!(serve(&uri, None).status(), StatusCode::NOT_FOUND);
        let uri: Uri = "/".parse().unwrap();
        assert_eq!(serve(&uri, None).status(), StatusCode::OK);
    }
}
