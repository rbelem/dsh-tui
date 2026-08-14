//! dsh-tui: terminal UI client for the deepseek-harness gateway.
//!
//! The wire protocol models live in [`wire`]; the crate is currently a stub
//! binary around them.

pub mod app;
pub mod client;
pub mod render;
pub mod store;
pub mod wire;

pub use wire::*;
