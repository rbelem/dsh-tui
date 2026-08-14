//! Semantic style tokens for the UI surfaces (sidebar, composer, seed popup,
//! status line, takeovers), driven by the active [`Theme`]. Each token keeps
//! its modifier character and gains the theme color; with the terminal-
//! following default theme every color is `Reset`, so the modifiers-only
//! look is the fallback.

use ratatui::style::{Modifier, Style};

use crate::theme::Theme;

/// Pane divider of an unfocused pane.
pub fn border(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::DIM).fg(theme.muted)
}
/// Pane divider of the focused pane.
pub fn border_focused(theme: &Theme) -> Style {
    Style::new().fg(theme.accent)
}
/// Focused selection row (sidebar list, seed popup item).
pub fn selection(theme: &Theme) -> Style {
    Style::new()
        .add_modifier(Modifier::REVERSED)
        .fg(theme.bg)
        .bg(theme.accent)
}
/// The active session's marker in the sidebar.
pub fn active(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::BOLD).fg(theme.accent)
}
/// Secondary text: hints, placeholders, popup descriptions.
pub fn hint(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::DIM).fg(theme.muted)
}
/// Section headers (the sidebar's "Sessions").
pub fn header(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::BOLD).fg(theme.text)
}
/// Cautionary accents (the queue strip's steering count, popup tags).
pub fn warning(theme: &Theme) -> Style {
    Style::new().fg(theme.warning)
}
