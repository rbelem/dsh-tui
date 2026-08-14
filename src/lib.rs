//! dsh-tui: terminal UI client for the deepseek-harness gateway.
//!
//! The wire protocol models live in [`wire`]; the crate is currently a stub
//! binary around them.

pub mod app;
pub mod client;
pub mod i18n;
pub mod render;
pub mod store;
pub mod theme;
pub mod ui;
pub mod wire;

pub use wire::*;
