//! The localhost editor host: project IO, the asset library, jobs, deploy and
//! the live link to a running dedicated server.
//!
//! Everything that touches the game install or runs an external tool happens
//! here and nowhere else, so this crate is the project's whole attack surface.
//! It binds the loopback interface only, authenticates every request with a
//! per-launch token, and validates every path and number it is handed.

pub mod api;
pub mod assets;
pub mod browse;
pub mod config;
pub mod deploy;
pub mod editor_link;
pub mod error;
pub mod jobs;
pub mod live;
pub mod paths;
pub mod plan;
pub mod projects;
pub mod reference;
pub mod secure;
pub mod token;
pub mod web;

pub use api::router;
pub use config::Config;
pub use error::{ApiError, Result};
pub use token::Token;
