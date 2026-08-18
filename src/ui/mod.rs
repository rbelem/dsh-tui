//! UI surfaces: the sidebar (session list), the composer (multi-line input
//! with seeded `/` and `@` popups), and the shared neutral style tokens.
//!
//! Everything draws into a ratatui `Buffer` and stays terminal-agnostic;
//! the app shell owns layout and focus. Style discipline: modifiers only,
//! no raw colors — the theme lane maps [`style`] tokens later.

pub mod catalog;
pub mod composer;
pub mod context_menu;
pub mod hero;
pub mod image_viewer;
pub mod launcher;
pub mod new_session;
pub mod onboarding;
pub mod queue;
pub mod search;
pub mod settings;
pub mod sidebar;
pub mod style;
pub mod takeover;
pub mod view_options;

pub use composer::{Composer, ComposerView, PopupKind, SeedPopup};
pub use context_menu::{
    ContextMenuAction, ContextMenuItem, ContextMenuPopup, ContextMenuState, ContextMenuTarget,
};
pub use hero::HeroView;
pub use image_viewer::{ImageViewer, ImageViewerView};
pub use launcher::{LauncherAction, LauncherEntry, LauncherPopup};
pub use new_session::{NewSessionEntry, NewSessionPopup};
pub use onboarding::{OnboardingState, OnboardingStep, OnboardingView};
pub use queue::{QueuePopup, QueueStrip};
pub use search::SidebarSearchPopup;
pub use settings::{SettingsState, SettingsView};
pub use sidebar::{SidebarGroup, SidebarState, SidebarView, build_groups, sidebar_width};
pub use takeover::{ApprovalTakeover, ApprovalView, Mode, QuestionTakeover, QuestionView};
