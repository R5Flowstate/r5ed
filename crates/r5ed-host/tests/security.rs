//! The admission surface: token, `Host`, `Origin`, path traversal, body caps
//! and the bind address.

mod common;

use common::HarnessBuilder;
use r5ed_host::api;
use r5ed_host::secure::{host_allowed, origin_allowed};

#[tokio::test(flavor = "multi_thread")]
async fn rejects_missing_token() {
    let h = HarnessBuilder::new("no-token").start().await.expect("harness");
    // A correct Host, no token at all.
    let r = h
        .client
        .request("GET", "/api/session", &[("Host", format!("127.0.0.1:{}", h.addr.port()))], None)
        .expect("request");
    assert_eq!(r.status, 401, "{}", r.text());

    // A token of the right shape but the wrong value is the same answer.
    let bad = vec![
        ("X-R5ED-Token", "0".repeat(64)),
        ("Host", format!("127.0.0.1:{}", h.addr.port())),
    ];
    let r = h.client.request("GET", "/api/session", &bad, None).expect("request");
    assert_eq!(r.status, 401, "{}", r.text());

    // And so is a prefix of the real one.
    let short = h.token[..32].to_string();
    let r = h
        .client
        .request("GET", "/api/session", &[("X-R5ED-Token", short), ("Host", format!("127.0.0.1:{}", h.addr.port()))], None)
        .expect("request");
    assert_eq!(r.status, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_bad_host() {
    let h = HarnessBuilder::new("bad-host").start().await.expect("harness");
    for host in [
        "evil.test".to_string(),
        format!("127.0.0.1:{}", h.addr.port() + 1),
        "127.0.0.1".to_string(),
        "".to_string(),
        "localhost".to_string(),
    ] {
        let headers = vec![("X-R5ED-Token", h.token.clone()), ("Host", host.clone())];
        let r = h.client.request("GET", "/api/session", &headers, None).expect("request");
        assert_eq!(r.status, 403, "host {host:?} answered {}", r.status);
    }
    // The port the server actually bound is the one it accepts.
    let r = h.get("/api/session");
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!host_allowed("evil.test", 1), "the check must reject a rebound name");

    // Two Host headers is not a request, and the right one does not save it.
    let both = vec![
        ("X-R5ED-Token", h.token.clone()),
        ("Host", format!("127.0.0.1:{}", h.addr.port())),
        ("Host", "evil.test".to_string()),
    ];
    let r = h.client.request("GET", "/api/session", &both, None).expect("request");
    assert_eq!(r.status, 403, "{}", r.status);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_foreign_origin() {
    let h = HarnessBuilder::new("bad-origin").start().await.expect("harness");
    for origin in [
        "http://evil.test",
        "https://localhost:7797",
        "null",
        "http://127.0.0.1:1",
    ] {
        let headers = h.with("Origin", origin.to_string());
        let r = h.client.request("GET", "/api/session", &headers, None).expect("request");
        assert_eq!(r.status, 403, "origin {origin:?} answered {}", r.status);
    }
    // No CORS headers ever go out, preflight or otherwise.
    let r = h.get("/api/session");
    assert!(r.header("access-control-allow-origin").is_none());
    let r = h.client.request("OPTIONS", "/api/session", &h.auth(), None).expect("request");
    assert!(r.status == 403 || r.status == 405, "{}", r.status);
    assert!(r.header("access-control-allow-origin").is_none());
    assert!(origin_allowed("http://localhost:1", 1));
    assert!(!origin_allowed("http://localhost:2", 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_path_traversal() {
    let h = HarnessBuilder::new("traversal").start().await.expect("harness");
    // A project to aim at.
    let r = h.post("/api/projects", br#"{"name":"victim"}"#);
    assert_eq!(r.status, 200, "{}", r.text());

    // Everything a caller could try to smuggle through the query, the body and
    // the path, percent-encoded and not.
    let attempts = [
        "/api/projects/victim/files/../../../etc",
        "/api/projects/..%2f..%2fetc/files",
        "/api/projects/victim/file?path=..%2f..%2fhost.json",
        "/api/projects/victim/file?path=%2e%2e%2f%2e%2e%2fhost.json",
        "/api/projects/victim/file?path=..\\..\\host.json",
        "/api/projects/victim/file?path=C:%5CWindows%5Cwin.ini",
        "/api/projects/victim/file?path=%5C%5Cserver%5Cshare%5Cx",
        "/api/projects/victim/file?path=map.json%3Astream",
        "/api/projects/victim/file?path=CON",
        "/api/projects/victim/file?path=nul.txt",
        "/api/projects/victim/file?path=",
        "/api/projects/C:%5Cevil/files",
        "/api/projects/victim%00/file?path=map.json",
    ];
    for a in attempts {
        let r = h.get(a);
        assert!(
            matches!(r.status, 400 | 404),
            "{a} answered {} {}",
            r.status,
            r.text()
        );
    }
    // Creating a project under a traversal name is refused outright.
    for name in ["..", "a/b", "a\\b", "a.b", "CON", "a:b", "a b"] {
        let body = format!("{{\"name\":\"{name}\"}}");
        let r = h.post("/api/projects", body.as_bytes());
        assert!(matches!(r.status, 400 | 409), "{name:?} answered {}", r.status);
    }
    // Nothing landed outside the projects root.
    assert!(!std::path::Path::new(&h.dir).join("evil").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_junction_escape() {
    let h = HarnessBuilder::new("junction").start().await.expect("harness");
    h.post("/api/projects", br#"{"name":"victim"}"#);
    let secret = h.dir.join("secret");
    std::fs::create_dir_all(&secret).expect("secret");
    std::fs::write(secret.join("key.txt"), b"the token is not in the project").expect("key");

    // A directory junction is the way a Windows process points a project
    // directory at somewhere else without needing a symlink privilege.
    let link = h.config.projects_root.join("victim").join("out");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&secret)
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !made {
        eprintln!("this machine will not create a directory junction; nothing to escape through");
        return;
    }
    // Both a read and a write through the junction are refused, because the
    // path resolves outside the project once the link is followed.
    let r = h.get("/api/projects/victim/file?path=out%2Fkey.txt");
    assert!(matches!(r.status, 400 | 404), "{} {}", r.status, r.text());
    let r = h.get("/api/projects/victim/files");
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!r.text().contains("key.txt"), "a link must not appear in the listing");
    let r = h.put("/api/projects/victim/file?path=out%2Fnew.txt", b"x");
    assert!(matches!(r.status, 400 | 404), "{} {}", r.status, r.text());
    assert!(!secret.join("new.txt").exists(), "nothing was written through the junction");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_oversize_body() {
    let h = HarnessBuilder::new("oversize").start().await.expect("harness");
    assert_eq!(h.post("/api/projects", br#"{"name":"p"}"#).status, 200);

    // A json body past the json cap.
    let big = format!("{{\"name\":\"{}\"}}", "a".repeat(2 << 20));
    let r = h.client.request("POST", "/api/projects", &h.auth(), Some(big.as_bytes())).expect("request");
    assert!(matches!(r.status, 400 | 413), "{}", r.status);

    // A file put past the file cap.
    let payload = vec![b'x'; 65 << 20];
    // The server may answer and hang up before the client has finished
    // sending, which the client sees as a broken pipe or a reset. That is a
    // refusal too.
    match h.client.request("PUT", "/api/projects/p/file?path=big.bin", &h.auth(), Some(&payload)) {
        Ok(r) => assert!(matches!(r.status, 400 | 413), "{}", r.status),
        Err(e) => assert!(
            matches!(e.kind(), std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset),
            "{e}"
        ),
    }
    assert!(!h.config.projects_root.join("p").join("big.bin").exists());

    // A small put still works, so the cap is a cap and not a wall.
    let r = h.put("/api/projects/p/file?path=small.bin", b"hello");
    assert_eq!(r.status, 204, "{}", r.text());
    let r = h.get("/api/projects/p/file?path=small.bin");
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn binds_loopback_only() {
    // The address the server asks for is loopback, never a wildcard.
    let a = api::bind_addr(0);
    assert!(a.ip().is_loopback());
    assert_eq!(a.ip().to_string(), "127.0.0.1");
    assert!(!a.ip().is_unspecified());
    assert!(!a.ip().is_multicast());

    // And the socket it really gets is reachable on loopback and nowhere else.
    let h = HarnessBuilder::new("bind").start().await.expect("harness");
    assert_eq!(h.addr.ip().to_string(), "127.0.0.1");
    assert!(h.addr.port() > 0);
    assert!(std::net::TcpStream::connect(h.addr).is_ok());

    // Asking for the same port again fails, which proves nothing else took it.
    assert!(std::net::TcpListener::bind(h.addr).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn every_response_carries_the_security_headers() {
    let h = HarnessBuilder::new("headers").start().await.expect("harness");
    for path in ["/api/session", "/", "/api/projects"] {
        let r = h.get(path);
        assert_eq!(r.header("x-content-type-options"), Some("nosniff"), "{path}");
        assert_eq!(r.header("cross-origin-opener-policy"), Some("same-origin"), "{path}");
        assert_eq!(r.header("cross-origin-embedder-policy"), Some("require-corp"), "{path}");
        assert!(r.header("content-security-policy").is_some_and(|c| c.contains("default-src 'self'")), "{path}");
        assert!(r.header("access-control-allow-origin").is_none(), "{path}");
    }
}
