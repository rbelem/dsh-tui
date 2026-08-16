//! The sidebar search popup (`/` in the sidebar): a small bordered overlay
//! over the sidebar listing the live `session.search` results, mirroring
//! the new-session picker (ui::new_session) — `▸`-marked selection,
//! bordered overlay, centered draw. The query line on top shows the typed
//! text (or the placeholder while empty); result rows show each item's
//! snippet; the footer carries the key hint, a "searching…" row while a
//! search is in flight, or a "no matches" hint for an empty result set.
//! Enter switches to the highlighted session, Esc closes (the app restores
//! the full grouped list).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::i18n::{Locale, tr};
use crate::theme::Theme;
use crate::ui::style;
use crate::wire::session::SessionSearchItem;

/// The search popup: the query text, the result rows, the selection, and
/// the in-flight flag (the app guards one `session.search` POST at a time).
pub struct SidebarSearchPopup<'a> {
    pub query: &'a str,
    pub results: &'a [SessionSearchItem],
    pub selected: usize,
    /// A `session.search` POST is in flight (the footer shows "searching…").
    pub sending: bool,
    pub theme: &'a Theme,
    pub locale: Locale,
}

impl SidebarSearchPopup<'_> {
    /// Outer size (border included): the query line + one row per result
    /// + the footer row, capped at the room available (mirrors
    ///   `NewSessionPopup::size`, plus the extra query line).
    pub fn size(&self, available: u16, room: u16) -> (u16, u16) {
        let text = self
            .results
            .iter()
            .map(|item| item.snippet.len())
            .max()
            .unwrap_or(0)
            .max(self.query.len());
        let width = (text + 8) as u16;
        let height = (self.results.len() as u16 + 4).min(room);
        // #19: never wider than the terminal (the min is a floor).
        (width.max(28).min(available), height)
    }
}

impl Widget for SidebarSearchPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title_style(
                Style::new()
                    .add_modifier(Modifier::BOLD)
                    .fg(self.theme.accent),
            )
            .title(tr(self.locale, "search.title"));
        let inner = block.inner(area);
        block.render(area, buf);
        // #11 popup treatment: panel_bg fill after Clear, inside the border.
        buf.set_style(inner, style::panel_fill(self.theme));
        let mut y = inner.y;

        // The query line: the typed text, or the placeholder while empty.
        let query_line = if self.query.is_empty() {
            Line::styled(
                format!(" {}", tr(self.locale, "search.placeholder")),
                style::hint(self.theme),
            )
        } else {
            Line::raw(format!(" {}", self.query))
        };
        buf.set_line(inner.x, y, &query_line, inner.width);
        y += 1;

        for (i, item) in self.results.iter().enumerate() {
            if y >= inner.bottom().saturating_sub(1) {
                break;
            }
            if i == self.selected {
                buf.set_style(
                    Rect::new(inner.x, y, inner.width, 1),
                    style::selection(self.theme),
                );
            }
            let marker = if i == self.selected { "▸ " } else { "  " };
            // The snippet is the match context; a blank one degrades to
            // the session id so the row never renders empty.
            let label = if item.snippet.trim().is_empty() {
                item.session_id.0.as_str()
            } else {
                item.snippet.as_str()
            };
            buf.set_line(
                inner.x,
                y,
                &Line::from(vec![
                    Span::styled(marker, style::active(self.theme)),
                    Span::raw(label),
                ]),
                inner.width,
            );
            y += 1;
        }

        // The footer: "searching…" while in flight, the empty-results hint
        // for a finished search with no rows, else the key hint.
        if y < inner.bottom() {
            let key = if self.sending {
                "search.sending"
            } else if !self.query.is_empty() && self.results.is_empty() {
                "search.empty"
            } else {
                "search.hint"
            };
            buf.set_line(
                inner.x,
                y,
                &Line::styled(
                    format!(" {}", tr(self.locale, key)),
                    style::hint(self.theme),
                ),
                inner.width,
            );
        }
    }
}
