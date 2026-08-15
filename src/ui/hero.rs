//! The hero: the chat panel's empty state (no session selected).
//!
//! Calm and terminal-native — no ASCII art. A title line, a subtitle, and
//! two key-hint lines (accent key, muted label), vertically centered in the
//! chat area. It renders only while `App.active_session` is `None`; with a
//! session selected the normal chat renders (no regression to the surface).
//! Everything is theme-driven (no raw colors).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::i18n::{Locale, tr};
use crate::ui::style;

/// The empty-chat hero: title, subtitle, key hints.
pub struct HeroView<'a> {
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl Widget for HeroView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Four content rows (title, subtitle, gap, two hint lines); center
        // the block vertically, pad two columns from the left edge.
        const ROWS: u16 = 6;
        if area.height < ROWS || area.width < 8 {
            return;
        }
        let top = area.y + (area.height - ROWS) / 2;
        let x = area.x + 2;
        let width = area.width.saturating_sub(2);

        // #11 two-tone wordmark: `dsh` in bold accent, `-tui` in bold text.
        buf.set_line(
            x,
            top,
            &Line::from(vec![
                Span::styled(
                    "dsh",
                    Style::new()
                        .add_modifier(Modifier::BOLD)
                        .fg(self.theme.accent),
                ),
                Span::styled(
                    "-tui",
                    Style::new()
                        .add_modifier(Modifier::BOLD)
                        .fg(self.theme.text),
                ),
            ]),
            width,
        );
        buf.set_line(
            x,
            top + 1,
            &Line::styled(tr(self.locale, "hero.subtitle"), style::hint(self.theme)),
            width,
        );
        // Row 2 is the gap.
        let hint = |key: &'static str, label: &'static str| {
            Line::from(vec![
                Span::styled(key, style::active(self.theme)),
                Span::styled(format!(" — {label}"), style::hint(self.theme)),
            ])
        };
        buf.set_line(
            x,
            top + 3,
            &hint("n", tr(self.locale, "hero.new_hint")),
            width,
        );
        buf.set_line(
            x,
            top + 4,
            &hint("tab", tr(self.locale, "hero.sidebar_hint")),
            width,
        );
    }
}
