//! Neutral style tokens for the UI surfaces (sidebar, composer, seed popup,
//! status line). Modifiers only — no raw colors, matching the renderer's
//! discipline. The theme lane maps these tokens onto a real palette later.

use ratatui::style::{Modifier, Style};

/// Pane divider of an unfocused pane.
pub const BORDER: Style = Style::new().add_modifier(Modifier::DIM);
/// Pane divider of the focused pane.
pub const BORDER_FOCUSED: Style = Style::new();
/// Focused selection row (sidebar list, seed popup item).
pub const SELECTION: Style = Style::new().add_modifier(Modifier::REVERSED);
/// The active session's marker in the sidebar.
pub const ACTIVE: Style = Style::new().add_modifier(Modifier::BOLD);
/// Secondary text: hints, placeholders, popup descriptions.
pub const HINT: Style = Style::new().add_modifier(Modifier::DIM);
/// Section headers (the sidebar's "Sessions").
pub const HEADER: Style = Style::new().add_modifier(Modifier::BOLD);
