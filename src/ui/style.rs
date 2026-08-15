//! Semantic style tokens for the UI surfaces (sidebar, composer, seed popup,
//! status line, takeovers), driven by the active [`Theme`]. Each token keeps
//! its modifier character and gains the theme color; with the terminal-
//! following default theme every color is `Reset`, so the modifiers-only
//! look is the fallback.

use ratatui::style::{Modifier, Style};

use crate::theme::Theme;

/// Pane divider of an unfocused pane: the #11 `border` token (a subtle rule
/// color, tuned independently of `muted`).
pub fn border(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::DIM).fg(theme.border)
}
/// Pane divider of the focused pane.
pub fn border_focused(theme: &Theme) -> Style {
    Style::new().fg(theme.accent)
}
/// Selected row (#11): bold — the state rides on glyph shape (`▎` stripe,
/// `▸`/`●` markers) and weight, never on color alone. REVERSED is gone.
pub fn selection(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::BOLD).fg(theme.text)
}
/// The accent selection stripe (`▎`) prepended to a selected row.
pub fn selection_stripe(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::BOLD).fg(theme.accent)
}
/// The panel background fill (`panel_bg` token): sidebar, composer strip,
/// popup interiors. With the Reset default theme the fill is a no-op, so
/// non-truecolor terminals skip bg fills entirely.
pub fn panel_fill(theme: &Theme) -> Style {
    Style::new().bg(theme.panel_bg)
}
/// The active session's marker in the sidebar.
pub fn active(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::BOLD).fg(theme.accent)
}
/// Secondary text: hints, placeholders, popup descriptions.
pub fn hint(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::DIM).fg(theme.muted)
}
/// Section headers (the settings nav, popup group headers).
pub fn header(theme: &Theme) -> Style {
    Style::new().add_modifier(Modifier::BOLD).fg(theme.text)
}
/// Cautionary accents (the queue strip's steering count, popup tags).
pub fn warning(theme: &Theme) -> Style {
    Style::new().fg(theme.warning)
}
