//! The live link to a running dedicated server.

pub mod agent;
pub mod protocol;

use crate::error::Result;
use crate::live::agent::Link;
use crate::live::protocol::{script, Checked, Message};

pub use crate::live::protocol::Target;

/// Sends one validated message to the server.
pub async fn send(link: &Link, m: &Checked) -> Result<String> {
    let code = script(m);
    let mut s = link.session().await?;
    match m {
        Checked::Status => s.eval("r5ed1", "server", &code).await,
        _ => {
            s.script(&code).await?;
            Ok(String::new())
        }
    }
}

/// Decodes and validates one frame from the page, then sends it.
pub async fn handle(link: &Link, frame: &[u8]) -> Result<String> {
    let m: Message = protocol::parse_frame(frame)?;
    let checked = protocol::check(&m)?;
    send(link, &checked).await
}

/// The target for a game root, using the default agent port.
pub fn target_for(game_root: &std::path::Path) -> Target {
    Target::new(game_root)
}
