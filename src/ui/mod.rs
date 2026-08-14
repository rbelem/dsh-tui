//! UI surfaces: the sidebar (session list), the composer (multi-line input
//! with seeded `/` and `@` popups), and the shared neutral style tokens.
//!
//! Everything draws into a ratatui `Buffer` and stays terminal-agnostic;
//! the app shell owns layout and focus. Style discipline: modifiers only,
//! no raw colors — the theme lane maps [`style`] tokens later.

pub mod composer;
pub mod sidebar;
pub mod style;

pub use composer::{Composer, ComposerView, PopupKind, SeedItem, SeedPopup};
pub use sidebar::{SidebarState, SidebarView, sidebar_width};
