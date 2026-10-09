//! The HTTP and WebSocket surface.
//!
//! Every route under `/api` goes through the admission middleware, and the
//! WebSocket carries the token in its query string because a handshake cannot
//! set headers. Handlers take typed extractors and return typed bodies; the
//! only raw bytes that cross the wire are the file and asset payloads, and each
//! of those has its own size cap.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

use crate::assets::{Assets, Capabilities, Session};
use crate::config::Config;
use crate::editor_link::EditorLink;
use crate::error::{ApiError, Result};
use crate::jobs::Manager;
use crate::live::agent::Link;
use crate::live::protocol::Target;
use crate::secure::{self, Guard};
use crate::token::Token;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Everything a handler needs. One of these is built at startup and cloned into
/// every router clone.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub assets: Assets,
    pub jobs: Manager,
    pub guard: Guard,
    pub editor: EditorLink,
}

impl AppState {
    pub fn new(config: Config, assets: Assets, token: Token) -> Self {
        let env = crate::jobs::Env {
            game_root: config.game_root.clone(),
            projects_root: config.projects_root.clone(),
            cache_root: config.cache_root.clone(),
        };
        Self {
            config: Arc::new(config),
            assets,
            jobs: Manager::new(env),
            guard: Guard::new(token, 0),
            editor: EditorLink::new(),
        }
    }

    /// The guard carries the port the server is actually listening on, which is
    /// only known once the socket is bound.
    pub fn with_port(self, port: u16) -> Self {
        let token = (*self.guard.token).clone();
        Self { guard: Guard::new(token, port), ..self }
    }

    fn link(&self) -> Link {
        Link::new(Target::new(&self.config.game_root))
    }
}

#[derive(Clone, Debug, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    kind: Option<String>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
struct FileQuery {
    #[serde(default)]
    path: String,
}

/// The texture route names an asset by guid rather than by path.
#[derive(Clone, Debug, Deserialize)]
struct GuidQuery {
    #[serde(default)]
    guid: String,
}

#[derive(Clone, Debug, Deserialize)]
struct NewProject {
    name: String,
}

#[derive(Clone, Debug, Serialize)]
struct ProjectList(Vec<crate::projects::ProjectSummary>);

#[derive(Clone, Debug, Serialize)]
struct NameOnly {
    name: String,
}

#[derive(Clone, Debug, Serialize)]
struct SearchResult {
    total: usize,
    items: Vec<rpak::AssetHit>,
}

#[derive(Clone, Debug, Serialize)]
struct JobAccepted {
    id: String,
}

#[derive(Clone, Debug, Deserialize)]
struct JobQuery {
    #[serde(default)]
    since: u64,
    #[serde(default)]
    limit: Option<usize>,
}

fn project_dir(state: &AppState, name: &str) -> Result<std::path::PathBuf> {
    crate::projects::root_dir(state.config.projects_root.as_path(), name)
}

async fn session(State(s): State<AppState>) -> Response {
    let caps = Capabilities {
        assets: true,
        compile: s.config.tools.compile.as_ref().is_some_and(|p| p.is_file()),
        bake: s.config.tools.compile.as_ref().is_some_and(|p| p.is_file()),
        navmesh: s.config.tools.recast.as_ref().is_some_and(|p| p.is_file()),
        package: s.config.tools.repak.as_ref().is_some_and(|p| p.is_file())
            && s.config.tools.revpk.as_ref().is_some_and(|p| p.is_file()),
        deploy: true,
        live: s.config.game_root.join("platform").is_dir(),
        import_tf2: s.config.tools.compile.as_ref().is_some_and(|p| p.is_file()),
        import_map: s.config.tools.rsx.as_ref().is_some_and(|p| p.is_file()),
        minimap: s.config.tools.compile.as_ref().is_some_and(|p| p.is_file()),
        loadscreen: s.config.tools.compile.as_ref().is_some_and(|p| p.is_file()),
    };
    Json(Session {
        version: VERSION.to_string(),
        game_root: s.config.game_root.to_string_lossy().into_owned(),
        projects_root: s.config.projects_root.to_string_lossy().into_owned(),
        capabilities: caps,
    })
    .into_response()
}

async fn list_projects(State(s): State<AppState>) -> Response {
    match crate::projects::list(&s.config.projects_root) {
        Ok(v) => Json(ProjectList(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn create_project(State(s): State<AppState>, Json(body): Json<NewProject>) -> Response {
    match crate::projects::create(&s.config.projects_root, &body.name) {
        Ok(_) => Json(NameOnly { name: body.name }).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn list_files(State(s): State<AppState>, AxumPath(name): AxumPath<String>) -> Response {
    let r = project_dir(&s, &name).and_then(|d| crate::projects::files(&d));
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn get_file(
    State(s): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<FileQuery>,
) -> Response {
    let r = project_dir(&s, &name).and_then(|d| crate::projects::read_file(&d, &q.path));
    match r {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn put_file(
    State(s): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<FileQuery>,
    body: Bytes,
) -> Response {
    let r = project_dir(&s, &name).and_then(|d| crate::projects::write_file(&d, &q.path, &body));
    match r {
        Ok(()) => secure::empty(StatusCode::NO_CONTENT),
        Err(e) => e.into_response(),
    }
}

/// GUIDs are 64-bit and a JSON number past 2^53 loses precision in a browser,
/// so every guid (and every texture slot guid) goes out as a `0x` hex string.
fn hex_guids(v: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    fn hex(v: &Value) -> Value {
        match v.as_u64() {
            Some(n) => Value::String(format!("0x{n:016x}")),
            None => v.clone(),
        }
    }
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let v = match k.as_str() {
                        "guid" | "shader_set" => hex(&v),
                        "textures" => match v {
                            Value::Object(t) => Value::Object(t.into_iter().map(|(sk, sv)| (sk, hex(&sv))).collect()),
                            other => other,
                        },
                        _ => hex_guids(v),
                    };
                    (k, v)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(hex_guids).collect()),
        other => other,
    }
}

fn json_hex<T: serde::Serialize>(v: &T) -> Response {
    match serde_json::to_value(v) {
        Ok(v) => Json(hex_guids(v)).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn search_assets(State(s): State<AppState>, Query(q): Query<SearchQuery>) -> Response {
    match s.assets.search(&q.q, q.kind.as_deref(), q.offset.unwrap_or(0), q.limit.unwrap_or(50)) {
        Ok((total, items)) => json_hex(&SearchResult { total, items }),
        Err(e) => e.into_response(),
    }
}

async fn rebuild_assets(State(s): State<AppState>) -> Response {
    match s.assets.rebuild() {
        Ok(n) => Json(serde_json::json!({ "assets": n })).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Runs a library call that may shell out to the asset tool off the async
/// workers, so one slow export does not hold up every other request.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| ApiError::Internal(e.to_string()))?
}

async fn asset_model(State(s): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let assets = s.assets.clone();
    match blocking(move || assets.model_glb(&q.path)).await {
        Ok(b) => ([(header::CONTENT_TYPE, "model/gltf-binary")], b).into_response(),
        Err(e) => e.into_response(),
    }
}

/// How long one prefetch may run.
const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// The most model paths one prefetch takes; a batch this size exports well
/// inside the request timeout.
const PREFETCH_MAX: usize = 4096;

#[derive(Deserialize)]
struct PrefetchBody {
    paths: Vec<String>,
}

async fn prefetch_models(State(s): State<AppState>, Json(body): Json<PrefetchBody>) -> Response {
    if body.paths.len() > PREFETCH_MAX {
        return ApiError::bad(format!("at most {PREFETCH_MAX} models per prefetch")).into_response();
    }
    let assets = s.assets.clone();
    match blocking(move || assets.prefetch_models(&body.paths)).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct MaterialPrefetchBody {
    names: Vec<String>,
    suffixes: Vec<String>,
}

/// The most suffixes a material prefetch tries per name.
const SUFFIXES_MAX: usize = 32;

async fn prefetch_materials(State(s): State<AppState>, Json(body): Json<MaterialPrefetchBody>) -> Response {
    if body.names.len() > PREFETCH_MAX || body.suffixes.len() > SUFFIXES_MAX {
        return ApiError::bad(format!("at most {PREFETCH_MAX} materials and {SUFFIXES_MAX} suffixes per prefetch")).into_response();
    }
    let assets = s.assets.clone();
    match blocking(move || assets.prefetch_materials(&body.names, &body.suffixes)).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn asset_model_info(State(s): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    match s.assets.model_info(&q.path) {
        Ok(i) => json_hex(&i),
        Err(e) => e.into_response(),
    }
}

async fn asset_material(State(s): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let assets = s.assets.clone();
    match blocking(move || assets.material_info(&q.path)).await {
        Ok(i) => json_hex(&i),
        Err(e) => e.into_response(),
    }
}

async fn asset_thumbnail(State(s): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let assets = s.assets.clone();
    match blocking(move || assets.model_thumbnail(&q.path)).await {
        Ok(b) => ([(header::CONTENT_TYPE, "image/png")], b).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn asset_texture(State(s): State<AppState>, Query(q): Query<GuidQuery>) -> Response {
    if q.guid.is_empty() {
        return ApiError::bad("a texture is named by guid").into_response();
    }
    let assets = s.assets.clone();
    match blocking(move || assets.texture_dds(&q.guid)).await {
        Ok(b) => ([(header::CONTENT_TYPE, "image/vnd.ms.dds")], b).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn surface_props(State(s): State<AppState>) -> Response {
    match s.assets.surface_props() {
        Ok(m) => Json(m).into_response(),
        Err(e) => e.into_response(),
    }
}

/// The rpak browser: one folder's subfolders and map client paks. With no
/// folder given it opens on the install's `paks/Win64`.
async fn browse(State(s): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let dir = if q.path.is_empty() {
        let win64 = s.assets.game_root().join("paks").join("Win64");
        if win64.is_dir() { win64 } else { s.assets.game_root().to_path_buf() }
    } else {
        std::path::PathBuf::from(&q.path)
    };
    match crate::browse::list(&dir) {
        Ok(l) => Json(l).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn list_maps(State(s): State<AppState>) -> Response {
    Json(s.assets.maps()).into_response()
}

/// The reference layer of one imported map: its chunks, its entities and the
/// extent of its geometry.
async fn map_reference(State(s): State<AppState>, AxumPath(stem): AxumPath<String>) -> Response {
    let stem = match crate::plan::map_stem(&stem) {
        Ok(v) => v.to_string(),
        Err(e) => return e.into_response(),
    };
    match crate::reference::build(&s.config.cache_root, &stem) {
        Ok((doc, _)) => Json(doc).into_response(),
        Err(e) => e.into_response(),
    }
}

/// One chunk of a reference layer, as a glb.
async fn map_chunk(State(s): State<AppState>, AxumPath((stem, n)): AxumPath<(String, u32)>) -> Response {
    let stem = match crate::plan::map_stem(&stem) {
        Ok(v) => v.to_string(),
        Err(e) => return e.into_response(),
    };
    if n as usize >= crate::reference::MAX_CHUNKS {
        return ApiError::not_found(format!("chunk {n} is out of range")).into_response();
    }
    match crate::reference::chunk(&s.config.cache_root, &stem, n as usize) {
        Ok(b) => ([(header::CONTENT_TYPE, "model/gltf-binary")], b).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn create_job(State(s): State<AppState>, body: Bytes) -> Response {
    let req = match crate::plan::parse_request(&body) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let plan = match crate::plan::build(&s.config, s.jobs.env(), &req, &|stem| s.assets.map_pak(stem)) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let job = s.jobs.submit(plan, req.kind, req.project);
    Json(JobAccepted { id: job.id.clone() }).into_response()
}

async fn get_job(State(s): State<AppState>, AxumPath(id): AxumPath<String>, Query(q): Query<JobQuery>) -> Response {
    match s.jobs.view(&id, q.since, q.limit.unwrap_or(400)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn cancel_job(State(s): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    match s.jobs.cancel(&id) {
        Ok(()) => Json(serde_json::json!({})).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn list_jobs(State(s): State<AppState>) -> Response {
    let out: Vec<serde_json::Value> =
        s.jobs.recent().iter().take(64).map(|j| j.view(0, 1)).collect::<Vec<_>>().into_iter().map(|v| {
            serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
        }).collect();
    Json(out).into_response()
}

/// Built into the host so it answers wherever it was started from.
const EMBEDDED_SCHEMA: &str = include_str!("../../../schema/entities.json");

async fn entity_schema(State(s): State<AppState>) -> Response {
    // A regenerated schema in the working directory wins over the built-in copy.
    let candidates = [
        std::path::PathBuf::from("schema/entities.json"),
        std::path::PathBuf::from("../schema/entities.json"),
    ];
    let _ = s;
    let text = candidates
        .iter()
        .find_map(|c| std::fs::read_to_string(c).ok())
        .unwrap_or_else(|| EMBEDDED_SCHEMA.to_string());
    ([(header::CONTENT_TYPE, "application/json")], text).into_response()
}

/// The live socket. The token rides in the query string because a WebSocket
/// handshake cannot carry a custom header, and the admission middleware has
/// already compared it by the time this runs.
async fn live_socket(State(s): State<AppState>, ws: axum::extract::ws::WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |mut socket| async move {
        use axum::extract::ws::Message as Ws;
        use futures_util::StreamExt;
        while let Some(Ok(msg)) = socket.next().await {
            let text = match msg {
                Ws::Text(t) => t.as_bytes().to_vec(),
                Ws::Binary(b) => b.to_vec(),
                Ws::Close(_) => break,
                Ws::Ping(_) | Ws::Pong(_) => continue,
            };
            let reply = match crate::live::handle(&s.link(), &text).await {
                Ok(body) if body.is_empty() => Ws::Text("{\"ok\":true}".to_string().into()),
                Ok(body) => Ws::Text(format!("{{\"ok\":true,\"result\":{body}}}").into()),
                Err(e) => Ws::Text(format!("{{\"ok\":false,\"error\":{:?}}}", e.to_string()).into()),
            };
            if socket.send(reply).await.is_err() {
                break;
            }
        }
    })
}

async fn fallback(State(s): State<AppState>, uri: Uri, headers: HeaderMap) -> Response {
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let mut res = crate::web::serve(&uri, range);
    secure::set_security_headers(&mut res);
    // The page itself carries the session: only to this host's own names, so a
    // rebound DNS name never receives it.
    let page = uri.path() == "/" || uri.path() == "/index.html";
    if page && secure::check_host(&headers, s.guard.port).is_ok() {
        if let Ok(v) = header::HeaderValue::from_str(&secure::token_cookie(&s.guard.token)) {
            res.headers_mut().insert(header::SET_COOKIE, v);
        }
    }
    res
}

/// Builds the whole router. The port is needed by the `Host` and `Origin`
/// checks, so it is not known until the socket is bound.
pub fn router(state: AppState, port: u16) -> Router {
    let state = state.with_port(port);
    let guard = state.guard.clone();
    let api = Router::new()
        .route("/api/session", get(session))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{name}/files", get(list_files))
        .route("/api/projects/{name}/file", get(get_file).put(put_file))
        .route("/api/assets/search", get(search_assets))
        .route("/api/assets/rebuild", post(rebuild_assets))
        .route("/api/assets/model", get(asset_model))
        .route("/api/assets/model_info", get(asset_model_info))
        .route("/api/assets/material", get(asset_material))
        .route("/api/assets/thumbnail", get(asset_thumbnail))
        .route("/api/assets/texture", get(asset_texture))
        .route("/api/assets/surface_props", get(surface_props))
        .route("/api/maps", get(list_maps))
        .route("/api/browse", get(browse))
        .route("/api/maps/{stem}/reference", get(map_reference))
        .route("/api/maps/{stem}/chunk/{n}", get(map_chunk))
        .route("/api/schema", get(entity_schema))
        .route("/api/jobs", get(list_jobs).post(create_job))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/live", get(live_socket))
        .layer(axum::middleware::from_fn_with_state(guard, secure::admit))
        .layer(RequestBodyLimitLayer::new(secure::MAX_CHUNK_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            std::time::Duration::from_secs(120),
        ))
        .with_state(state.clone());
    // A prefetch exports a map's worth of assets in one go; loading a shared
    // pak alone takes seconds, so these get a longer bound than a request.
    let exports = Router::new()
        .route("/api/assets/models/prefetch", post(prefetch_models))
        .route("/api/assets/materials/prefetch", post(prefetch_materials))
        .layer(axum::middleware::from_fn_with_state(state.guard.clone(), secure::admit))
        .layer(RequestBodyLimitLayer::new(secure::MAX_CHUNK_BYTES))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, EXPORT_TIMEOUT))
        .with_state(state.clone());

    Router::new()
        .merge(api)
        .merge(exports)
        .merge(
            crate::editor_link::routes().layer(axum::middleware::from_fn_with_state(
                state.guard.clone(),
                crate::secure::admit,
            )),
        )
        .fallback(fallback)
        .with_state(state)
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::SERVER,
            HeaderValue::from_static("r5ed"),
        ))
}

/// The one address this server will ever bind. A wildcard address would put the
/// editor and its token on the network.
pub fn bind_addr(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// Binds loopback only, and reports the port it actually got.
pub async fn bind(port: u16) -> std::io::Result<(tokio::net::TcpListener, SocketAddr)> {
    let listener = tokio::net::TcpListener::bind(bind_addr(port)).await?;
    let local = listener.local_addr()?;
    Ok((listener, local))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bind_address_is_loopback_and_nothing_else() {
        let a = bind_addr(7797);
        assert!(a.ip().is_loopback());
        assert_eq!(a.port(), 7797);
        assert_eq!(a.ip().to_string(), "127.0.0.1");
        assert!(!a.ip().is_unspecified());
    }
}
