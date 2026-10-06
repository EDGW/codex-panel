//! Application modules; billing destinations are independent of terminal and Codex plumbing.
pub mod app;
pub mod conversion;
pub mod cost;
pub mod dest;
pub mod exchange;

mod billing;
mod bridge;
mod codex;
pub mod config;
mod credentials;
mod daemon;
mod panel;
mod runtime;
mod session;
mod subprocess;
mod tmux;
mod url;

pub(crate) type AppResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
