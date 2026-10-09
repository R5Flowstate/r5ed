//! The TCP client for the server's agent socket.
//!
//! The protocol is one request per line and one reply line per request:
//! `AUTH <token>` first, then `SCRIPT <code>`, `EVAL <id> <ctx> <code>` or
//! `CMD <id> <line>`. The connection is per call, so a half-open socket can
//! never wedge the editor.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::error::{ApiError, Result};
use crate::live::protocol::Target;

/// One request's timeout. The socket is loopback, so a stuck server is the only
/// way to hit this.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// The server's hello line, which every connection opens with.
const HELLO: &str = "AGENTLINK ";
/// The longest line the server will read; anything past it is discarded whole.
const LINE_MAX: usize = 65535;
/// The request-id grammar the server enforces before it looks at anything else.
fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub struct Link {
    target: Target,
    timeout: Duration,
}

impl Link {
    pub fn new(target: Target) -> Self {
        Self { target, timeout: IO_TIMEOUT }
    }

    pub fn with_timeout(target: Target, timeout: Duration) -> Self {
        Self { target, timeout }
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    async fn connect(&self) -> Result<TcpStream> {
        let stream = tokio::time::timeout(
            self.timeout,
            TcpStream::connect(("127.0.0.1", self.target.port)),
        )
        .await
        .map_err(|_| ApiError::Unavailable("the agent link did not answer".into()))?
        .map_err(|e| ApiError::Unavailable(format!("the agent link did not answer: {e}")))?;
        let _ = stream.set_nodelay(true);
        Ok(stream)
    }

    /// Opens a connection, authenticates, and hands the stream plus a reader
    /// to the caller. The hello line is consumed here.
    pub async fn session(&self) -> Result<Session> {
        let token = self.target.token()?;
        let stream = self.connect().await?;
        let (r, w) = stream.into_split();
        let mut r = BufReader::new(r);
        let mut w = w;
        let mut hello = String::new();
        read_line(&mut r, &mut hello, self.timeout).await?;
        if !hello.starts_with(HELLO) {
            return Err(ApiError::Unavailable("the agent link sent an unknown greeting".into()));
        }
        let greeting = hello.trim().to_string();
        w.write_all(format!("AUTH {token}\n").as_bytes())
            .await
            .map_err(|e| ApiError::Unavailable(format!("the agent link refused the connection: {e}")))?;
        w.flush().await.map_err(|e| ApiError::Unavailable(e.to_string()))?;
        let mut reply = String::new();
        read_line(&mut r, &mut reply, self.timeout).await?;
        if reply.trim() != "OK" {
            return Err(ApiError::Forbidden);
        }
        Ok(Session { reader: r, writer: w, timeout: self.timeout, greeting })
    }
}

pub struct Session {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    timeout: Duration,
    greeting: String,
}

impl Session {
    /// The greeting the server sent, e.g. `AGENTLINK 2 server`.
    pub fn greeting(&self) -> &str {
        &self.greeting
    }

    /// Sends one line and returns the reply line, both newline terminated.
    pub async fn request(&mut self, line: &str) -> Result<String> {
        if line.contains(['\n', '\r', '\0']) {
            return Err(ApiError::bad("an agent link line cannot contain a newline or a NUL"));
        }
        if line.len() > LINE_MAX {
            return Err(ApiError::bad(format!("an agent link line is longer than {LINE_MAX} bytes")));
        }
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| ApiError::Unavailable(format!("the agent link closed: {e}")))?;
        self.writer.flush().await.map_err(|e| ApiError::Unavailable(e.to_string()))?;
        let mut out = String::new();
        read_line(&mut self.reader, &mut out, self.timeout).await?;
        Ok(out.trim_end_matches(['\r', '\n']).to_string())
    }

    /// `SCRIPT <code>`, which the server runs on its next frame.
    pub async fn script(&mut self, code: &str) -> Result<()> {
        match self.request(&format!("SCRIPT {code}")).await {
            Ok(ref r) if r == "OK" => Ok(()),
            Ok(ref r) if r == "BUSY" => Err(ApiError::conflict("the agent link queue is full")),
            Ok(_) => Err(ApiError::Unavailable("the agent link rejected the script".into())),
            Err(e) => Err(e),
        }
    }

    /// `EVAL <id> <ctx> <code>`, which answers with `RESULT <id> ok <json>`.
    pub async fn eval(&mut self, id: &str, ctx: &str, code: &str) -> Result<String> {
        if !id_ok(id) {
            return Err(ApiError::bad("an agent link request id must be 1-32 characters of A-Z a-z 0-9 _ -"));
        }
        if !matches!(ctx, "server" | "client" | "ui") {
            return Err(ApiError::bad("eval context must be server, client or ui"));
        }
        if code.contains(['\n', '\r', '\0']) {
            return Err(ApiError::bad("eval code must be a single line"));
        }
        let reply = self.request(&format!("EVAL {id} {ctx} {code}")).await?;
        let mut it = reply.splitn(3, ' ');
        let kind = it.next().unwrap_or("");
        let rid = it.next().unwrap_or("");
        let body = it.next().unwrap_or("");
        if kind != "RESULT" || rid != id {
            return Err(ApiError::Unavailable(format!("the agent link answered {reply:?}")));
        }
        match body.split_once(' ') {
            Some(("ok", rest)) => Ok(rest.to_string()),
            _ => Err(ApiError::Unavailable(format!("the agent link reported {body:?}"))),
        }
    }
}

async fn read_line<R>(r: &mut BufReader<R>, out: &mut String, timeout: Duration) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = tokio::time::timeout(timeout, r.read_line(&mut line))
        .await
        .map_err(|_| ApiError::Unavailable("the agent link did not answer in time".into()))?
        .map_err(|e| ApiError::Unavailable(format!("the agent link closed: {e}")))?;
    if n == 0 {
        return Err(ApiError::Unavailable("the agent link closed the connection".into()));
    }
    *out = line;
    Ok(())
}
