//! UI surfaces: the sidebar (session list), the composer (multi-line input
//! with seeded `/` and `@` popups), and the shared neutral style tokens.
//!
//! Everything draws into a ratatui `Buffer` and stays terminal-agnostic;
//! the app shell owns layout and focus. Style discipline: modifiers only,
//! no raw colors — the theme lane maps [`style`] tokens later.

pub mod catalog;
pub mod composer;
pub mod launcher;
pub mod queue;
pub mod settings;
pub mod sidebar;
pub mod style;
pub mod takeover;

pub use composer::{Composer, ComposerView, PopupKind, SeedPopup};
pub use launcher::{LauncherAction, LauncherEntry, LauncherPopup};
pub use queue::{QueuePopup, QueueStrip};
pub use settings::{SettingsState, SettingsView};
pub use sidebar::{SidebarState, SidebarView, sidebar_width};
pub use takeover::{ApprovalTakeover, ApprovalView, Mode, QuestionTakeover, QuestionView};
