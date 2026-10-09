//! Shared scaffolding for the host's tests: a server bound to an ephemeral
//! loopback port, a token, and a client that speaks the API the way the page
//! does.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use r5ed_host::api::{self, AppState};
use r5ed_host::assets::Assets;
use r5ed_host::config::Config;
use r5ed_host::token::Token;
use rpak::{Library, Rsx};

pub struct Harness {
    pub addr: SocketAddr,
    pub token: String,
    pub dir: PathBuf,
    pub config: Config,
    pub jobs: r5ed_host::jobs::Manager,
    pub client: reqwest_like::Client,
}

/// A tiny blocking HTTP client, so the tests need no async runtime of their own
/// beyond the one the server already runs on.
pub mod reqwest_like {
    use std::io::{BufReader, Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    pub struct Client {
        pub addr: std::net::SocketAddr,
    }

    pub struct Response {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Response {
        pub fn text(&self) -> String {
            String::from_utf8_lossy(&self.body).into_owned()
        }

        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    impl Client {
        pub fn new(addr: std::net::SocketAddr) -> Self {
            Self { addr }
        }

        /// One request, one response, connection closed. Deliberately literal
        /// so a test can send a header a normal client would not allow, and so
        /// it can send the same header twice.
        pub fn request(
            &self,
            method: &str,
            path: &str,
            headers: &[(&str, String)],
            body: Option<&[u8]>,
        ) -> std::io::Result<Response> {
            let mut s = TcpStream::connect(self.addr)?;
            s.set_read_timeout(Some(Duration::from_secs(30)))?;
            s.set_write_timeout(Some(Duration::from_secs(30)))?;
            let has_host = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host"));
            let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
            if !has_host {
                req.push_str(&format!("Host: {}\r\n", self.addr));
            }
            if body.is_some() && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                req.push_str("Content-Type: application/json\r\n");
            }
            for (k, v) in headers {
                req.push_str(&format!("{k}: {v}\r\n"));
            }
            req.push_str(&format!(
                "Content-Length: {}\r\n\r\n",
                body.map_or(0, <[u8]>::len)
            ));
            s.write_all(req.as_bytes())?;
            if let Some(b) = body {
                s.write_all(b)?;
            }
            s.flush()?;
            let mut raw = Vec::new();
            BufReader::new(s).read_to_end(&mut raw)?;
            parse(&raw).ok_or_else(|| std::io::Error::other("unparseable response"))
        }
    }

    /// Splits a raw reply. The body stays as bytes, so a glb or a dds can be
    /// compared against the file it came from.
    fn parse(raw: &[u8]) -> Option<Response> {
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let body = raw[split + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let status: u16 = lines.next()?.split(' ').nth(1)?.parse().ok()?;
        let mut headers = Vec::new();
        for l in lines {
            if let Some((k, v)) = l.split_once(": ") {
                headers.push((k.to_string(), v.to_string()));
            }
        }
        Some(Response { status, headers, body })
    }
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("r5ed-host-harness-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch dir");
    d
}

/// The compiler CLI, which a job plan needs before it will build one.
pub fn fake_tool() -> PathBuf {
    let exe = std::env::current_exe().expect("test exe");
    // The test binary lives in `deps`; the helper binaries live beside that.
    // The `deps` copy of a bin target is its own unit-test harness, not the
    // program, so that directory is skipped.
    let mut dir = exe.parent().map(Path::to_path_buf);
    for _ in 0..3 {
        let Some(d) = dir else { break };
        if d.file_name().is_some_and(|n| n != "deps") {
            for name in ["r5ed-faketool.exe", "r5ed-faketool"] {
                let p = d.join(name);
                if p.is_file() {
                    return p;
                }
            }
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    panic!("r5ed-faketool was not built next to the tests; look in {}", exe.display());
}

pub struct HarnessBuilder {
    tag: String,
    with_tools: bool,
    with_library: bool,
}

impl HarnessBuilder {
    pub fn new(tag: &str) -> Self {
        Self { tag: tag.to_string(), with_tools: false, with_library: false }
    }

    pub fn tools(mut self) -> Self {
        self.with_tools = true;
        self
    }

    /// Uses the real install and asset tool, which the gate's environment
    /// provides. A test that asks for this skips when either is absent.
    pub fn real_library(mut self) -> Self {
        self.with_library = true;
        self
    }

    pub async fn start(self) -> Option<Harness> {
        let dir = scratch(&self.tag);
        let mut config = Config {
            game_root: dir.join("game"),
            projects_root: dir.join("projects"),
            cache_root: dir.join("cache"),
            port: 0,
            tools: Default::default(),
        };
        std::fs::create_dir_all(&config.game_root).expect("game root");
        std::fs::create_dir_all(&config.projects_root).expect("projects root");
        std::fs::create_dir_all(&config.cache_root).expect("cache root");
        if self.with_tools {
            config.tools.compile = Some(fake_tool());
            config.tools.rsx = Some(fake_tool());
            config.tools.revpk = Some(fake_tool());
            config.tools.recast = Some(fake_tool());
        }
        let library = if self.with_library {
            let game_root = std::env::var("R5ED_GAME_ROOT").ok().map(PathBuf::from);
            let rsx = std::env::var("R5ED_RSX").ok().map(PathBuf::from);
            let (Some(gr), Some(exe)) = (game_root, rsx) else {
                eprintln!("no game root or asset tool configured; skipping");
                return None;
            };
            if !gr.is_dir() || !exe.is_file() {
                eprintln!("the configured game root or asset tool is missing; skipping");
                return None;
            }
            match Library::open(&gr, &config.cache_root, Rsx::new(exe)) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("could not open the asset library: {e}; skipping");
                    return None;
                }
            }
        } else {
            empty_library(&dir)
        };
        let assets = Assets::new(library, config.game_root.clone(), config.projects_root.clone());
        let token = Token::generate();
        let state = AppState::new(config.clone(), assets, token.clone());
        let jobs = state.jobs.clone();
        let (listener, addr) = api::bind(0).await.expect("bind");
        let app = api::router(state, addr.port());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        Some(Harness {
            addr,
            token: token.as_str().to_string(),
            dir,
            config,
            jobs,
            client: reqwest_like::Client::new(addr),
        })
    }
}

/// A library with no paks: the index is empty but every route still works.
fn empty_library(dir: &PathBuf) -> Library {
    let paks = dir.join("empty-paks");
    std::fs::create_dir_all(paks.join("game").join("paks").join("Win64")).expect("empty pak dir");
    std::fs::create_dir_all(paks.join("cache")).expect("empty cache dir");
    Library::open(&paks.join("game"), &paks.join("cache"), Rsx::new(std::env::current_exe().expect("exe")))
        .expect("empty library")
}

impl Harness {
    /// The roots a plan or a deploy resolves against, the same ones the server
    /// uses.
    pub fn env(&self) -> r5ed_host::jobs::Env {
        r5ed_host::jobs::Env {
            game_root: self.config.game_root.clone(),
            projects_root: self.config.projects_root.clone(),
            cache_root: self.config.cache_root.clone(),
        }
    }

    pub fn auth(&self) -> Vec<(&'static str, String)> {
        vec![("X-R5ED-Token", self.token.clone()), ("Host", format!("127.0.0.1:{}", self.addr.port()))]
    }

    /// Authenticated plus one extra header, for the tests that vary one thing.
    pub fn with(&self, name: &'static str, value: String) -> Vec<(&'static str, String)> {
        let mut v = self.auth();
        v.push((name, value));
        v
    }

    pub fn get(&self, path: &str) -> reqwest_like::Response {
        self.client.request("GET", path, &self.auth(), None).expect("request")
    }

    pub fn post(&self, path: &str, body: &[u8]) -> reqwest_like::Response {
        self.client.request("POST", path, &self.auth(), Some(body)).expect("request")
    }

    pub fn put(&self, path: &str, body: &[u8]) -> reqwest_like::Response {
        self.client.request("PUT", path, &self.auth(), Some(body)).expect("request")
    }

    /// Waits for a job to leave the running state.
    pub async fn wait_job(&self, id: &str) -> r5ed_host::jobs::JobView {
        for _ in 0..600 {
            let v = self
                .client
                .request("GET", &format!("/api/jobs/{id}"), &self.auth(), None)
                .expect("job request");
            if v.status == 200 {
                if let Ok(j) = serde_json::from_str::<r5ed_host::jobs::JobView>(&v.text()) {
                    if !matches!(j.state, r5ed_host::jobs::JobState::Queued | r5ed_host::jobs::JobState::Running) {
                        return j;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("job {id} never finished");
    }
}

