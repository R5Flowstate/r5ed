//! The editor relay: drives the user's open tab from a CLI or an agent.
//!
//! The page holds `WS /api/editor` open; `POST /api/editor/{verb}` wraps the
//! caller's params with a request id, sends the envelope down the most recent
//! socket, and returns the page's reply. Request ids keep concurrent calls
//! from crossing. The host never interprets verbs or replies: it only moves
//! them, so nothing here can evaluate anything in either direction.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::api::AppState;
use crate::error::ApiError;

/// How long a verb call waits for the page before giving up.
pub const EDITOR_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// A request id is a uuid; the page echoes it back on its reply.
fn request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[derive(Clone, Debug)]
pub enum LinkError {
    NoEditor,
    Gone,
    Timeout,
    BadReply(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEditor => write!(f, "no editor is connected"),
            Self::Gone => write!(f, "the editor went away mid-call"),
            Self::Timeout => write!(f, "the editor did not answer within 60 s"),
            Self::BadReply(m) => write!(f, "the editor answered outside its envelope: {m}"),
        }
    }
}

impl std::error::Error for LinkError {}

#[derive(Debug, Deserialize)]
struct PageReply {
    id: String,
    #[serde(flatten)]
    rest: serde_json::Value,
}

struct Inner {
    socket: Option<mpsc::UnboundedSender<String>>,
    pending: HashMap<String, oneshot::Sender<serde_json::Value>>,
}

/// The relay itself. Clone shares one relay; the socket a page holds is the
/// most recently connected one, which is the tab the user is looking at.
#[derive(Clone)]
pub struct EditorLink {
    inner: Arc<Mutex<Inner>>,
}

impl EditorLink {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner { socket: None, pending: HashMap::new() })),
        }
    }

    /// True when a page currently holds the socket.
    pub fn has_editor(&self) -> bool {
        self.inner.lock().map(|i| i.socket.is_some()).unwrap_or(false)
    }

    /// Registers the page's socket, replacing whatever was there before.
    pub fn set_socket(&self, tx: mpsc::UnboundedSender<String>) {
        if let Ok(mut i) = self.inner.lock() {
            i.socket = Some(tx);
        }
    }

    /// Drops the page's socket, e.g. when it disconnected.
    pub fn clear_socket(&self) {
        if let Ok(mut i) = self.inner.lock() {
            i.socket = None;
        }
    }

    /// Sends one verb call and waits for the reply carrying its id.
    pub async fn call(&self, verb: &str, params: serde_json::Value) -> Result<serde_json::Value, LinkError> {
        let id = request_id();
        let envelope = serde_json::json!({ "id": id, "verb": verb, "params": params });
        let text = serde_json::to_string(&envelope).map_err(|e| LinkError::BadReply(e.to_string()))?;
        let rx = {
            let mut guard = self.inner.lock().map_err(|_| LinkError::Gone)?;
            let tx = guard.socket.clone().ok_or(LinkError::NoEditor)?;
            let (tx_reply, rx) = oneshot::channel();
            guard.pending.insert(id.clone(), tx_reply);
            if tx.send(text).is_err() {
                guard.pending.remove(&id);
                guard.socket = None;
                return Err(LinkError::Gone);
            }
            rx
        };
        match tokio::time::timeout(EDITOR_CALL_TIMEOUT, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(LinkError::Gone),
            Err(_) => {
                if let Ok(mut guard) = self.inner.lock() {
                    guard.pending.remove(&id);
                }
                Err(LinkError::Timeout)
            }
        }
    }

    /// Routes one socket frame to the call waiting for its id. Anything
    /// without a known id is not a reply and is dropped.
    pub fn on_message(&self, text: &str) {
        let reply: PageReply = match serde_json::from_str(text) {
            Ok(r) => r,
            Err(_) => return,
        };
        let sender = self.inner.lock().map(|mut i| i.pending.remove(&reply.id)).unwrap_or(None);
        if let Some(tx) = sender {
            let mut body = reply.rest;
            if !body.is_object() {
                body = serde_json::json!({});
            }
            let _ = tx.send(body);
        }
    }
}

async fn editor_socket(State(s): State<AppState>, ws: WebSocketUpgrade) -> Response {
    let link = s.editor.clone();
    ws.on_upgrade(move |socket| async move {
        use axum::extract::ws::Message as Ws;
        use futures_util::SinkExt;
        use futures_util::StreamExt;
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        link.set_socket(tx);
        // Replies and verb calls travel one way each on this socket; a second
        // task owns the sink while this one reads the replies.
        let (mut sink, mut stream) = socket.split();
        let pump = tokio::spawn(async move {
            while let Some(text) = rx.recv().await {
                if sink.send(Ws::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        while let Some(Ok(msg)) = stream.next().await {
            let text = match msg {
                Ws::Text(t) => t.to_string(),
                Ws::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                Ws::Close(_) => break,
                Ws::Ping(_) | Ws::Pong(_) => continue,
            };
            link.on_message(&text);
        }
        link.clear_socket();
        pump.abort();
    })
}

async fn editor_verb(
    State(s): State<AppState>,
    AxumPath(verb): AxumPath<String>,
    body: Bytes,
) -> Response {
    if verb.is_empty() || verb.len() > 64 || !verb.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-') {
        return ApiError::bad("unknown editor verb").into_response();
    }
    let params = if body.is_empty() {
        serde_json::Value::Object(Default::default())
    } else {
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) => v,
            Err(_) => return ApiError::bad("the verb body must be JSON").into_response(),
        }
    };
    match s.editor.call(&verb, params).await {
        Ok(reply) => axum::Json(reply).into_response(),
        Err(LinkError::NoEditor | LinkError::Gone) => {
            (StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({ "error": "no editor is connected" }))).into_response()
        }
        Err(LinkError::Timeout) => {
            (StatusCode::GATEWAY_TIMEOUT, axum::Json(serde_json::json!({ "error": "the editor did not answer within 60 s" }))).into_response()
        }
        Err(e) => ApiError::internal(e.to_string()).into_response(),
    }
}

/// The relay routes. The caller merges them under the same admission
/// middleware as every other route.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/editor", get(editor_socket))
        .route("/api/editor/{verb}", post(editor_verb))
}

/// Where the running host leaves `{port, token}` for the CLI.
pub fn session_path() -> std::path::PathBuf {
    crate::config::app_dir().join("session.json")
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionFile {
    port: u16,
    token: String,
}

/// Writes the session file so `r5ed-compile ctl` can find this host. The
/// permissions are user-only on a best effort basis: a umask or an inherited
/// ACL may widen them, but the token stays out of any shared directory.
pub fn write_session(port: u16, token: &crate::token::Token) -> std::io::Result<()> {
    let path = session_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_string(&SessionFile { port, token: token.as_str().to_string() })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        opts.open(&path).and_then(|mut f| {
            use std::io::Write;
            f.write_all(body.as_bytes())
        })?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, body)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_call_meets_its_reply_by_id() {
        let link = EditorLink::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        link.set_socket(tx);
        let worker = tokio::spawn({
            let link = link.clone();
            async move { link.call("state", serde_json::json!({})).await }
        });
        let envelope: serde_json::Value = serde_json::from_str(&rx.recv().await.expect("the call sends")).unwrap();
        assert_eq!(envelope["verb"], "state");
        let id = envelope["id"].as_str().expect("the call carries an id").to_string();
        link.on_message(&serde_json::json!({ "id": id, "mode": "object" }).to_string());
        let reply = worker.await.unwrap().expect("the reply arrives");
        assert_eq!(reply["mode"], "object");
    }

    #[tokio::test]
    async fn concurrent_calls_do_not_cross() {
        let link = EditorLink::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        link.set_socket(tx);
        let first = tokio::spawn({
            let link = link.clone();
            async move { link.call("state", serde_json::json!({})).await }
        });
        let second = tokio::spawn({
            let link = link.clone();
            async move { link.call("view", serde_json::json!({})).await }
        });
        let mut ids = Vec::new();
        for _ in 0..2 {
            let envelope: serde_json::Value = serde_json::from_str(&rx.recv().await.expect("both calls send")).unwrap();
            ids.push(envelope["id"].as_str().expect("an id").to_string());
        }
        assert_ne!(ids[0], ids[1]);
        link.on_message(&serde_json::json!({ "id": ids[1], "second": true }).to_string());
        link.on_message(&serde_json::json!({ "id": ids[0], "first": true }).to_string());
        assert_eq!(first.await.unwrap().unwrap()["first"], true);
        assert_eq!(second.await.unwrap().unwrap()["second"], true);
    }

    #[tokio::test]
    async fn frames_without_a_known_id_are_dropped() {
        let link = EditorLink::new();
        link.on_message("not json");
        link.on_message(&serde_json::json!({ "id": "nobody-waits", "x": 1 }).to_string());
        assert!(!link.has_editor());
    }

    #[tokio::test]
    async fn a_call_with_no_editor_fails_at_once() {
        let link = EditorLink::new();
        let err = link.call("state", serde_json::json!({})).await.expect_err("no socket");
        assert!(matches!(err, LinkError::NoEditor));
    }

    #[tokio::test]
    async fn replacing_the_socket_keeps_waiting_calls_by_id() {
        let link = EditorLink::new();
        let (old_tx, _old_rx) = mpsc::unbounded_channel::<String>();
        link.set_socket(old_tx);
        assert!(link.has_editor());
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        link.set_socket(tx);
        drop(_old_rx);
        let worker = tokio::spawn({
            let link = link.clone();
            async move { link.call("state", serde_json::json!({})).await }
        });
        let envelope: serde_json::Value = serde_json::from_str(&rx.recv().await.expect("the new socket carries the call")).unwrap();
        let id = envelope["id"].as_str().expect("an id").to_string();
        link.clear_socket();
        assert!(!link.has_editor());
        // A reconnecting page answers by id, not by socket, so the call still lands.
        link.on_message(&serde_json::json!({ "id": id, "mode": "object" }).to_string());
        let reply = worker.await.unwrap().expect("the reply arrives");
        assert_eq!(reply["mode"], "object");
        let err = link.call("state", serde_json::json!({})).await.expect_err("no socket now");
        assert!(matches!(err, LinkError::NoEditor));
    }

    #[test]
    fn verbs_are_a_closed_grammar() {
        for bad in ["", "a/b", "a b", "state;rm", &"x".repeat(65)] {
            assert!(bad.is_empty() || bad.len() > 64 || !bad.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-'), "{bad:?}");
        }
    }

    /// The merged relay routes refuse a caller without the token, through the
    /// same admission middleware as every other route. Served on a full app
    /// router over an empty game root, so the wiring under test is the real one.
    /// Multi-threaded: the test thread blocks on raw TCP while the server runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_relay_answers_unauthorised_without_a_token() {
        use std::io::{Read, Write};
        let base = std::env::temp_dir().join(format!("r5ed-editor-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let game = base.join("game");
        let cache = base.join("cache");
        let projects = base.join("projects");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&projects).unwrap();
        let library = rpak::Library::open(&game, &cache, rpak::Rsx::new("rsx.exe")).unwrap();
        let assets = crate::assets::Assets::new(library, game.clone(), projects.clone());
        let config = crate::config::Config {
            game_root: game,
            projects_root: projects,
            cache_root: cache,
            port: 0,
            tools: Default::default(),
        };
        let token = crate::token::Token::generate();
        let state = AppState::new(config, assets, token);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = crate::api::router(state, port);
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service()).await.unwrap();
        });
        let post = |path: &str| {
            let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            write!(stream, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
            let mut out = Vec::new();
            stream.read_to_end(&mut out).unwrap();
            String::from_utf8_lossy(&out).into_owned()
        };
        let denied = post("/api/editor/state");
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
        let denied_ws = {
            let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            write!(
                stream,
                "GET /api/editor HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut out = Vec::new();
            stream.read_to_end(&mut out).unwrap();
            String::from_utf8_lossy(&out).into_owned()
        };
        assert!(denied_ws.starts_with("HTTP/1.1 401"), "{denied_ws}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
