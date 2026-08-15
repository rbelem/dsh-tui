//! The new-session picker (`n` in the chat or sidebar): a small centered
//! overlay listing the workspaces (durable order) plus a "no workspace"
//! entry, mirroring the launcher's structure. `j`/`k`/arrows move, `Enter`
//! creates with the highlighted entry, `Esc` closes. While a create is in
//! flight the hint line becomes a "creating…" row and Enter is inert.
//!
//! There is deliberately no title field: `session.create` takes
//! `workspaceId`/`cwd`/`sessionId`/`agentPreset` only (sessions.schema.ts
//! :106-110) — a fresh session is titled later via the sidebar's rename
//! (`r`). The commit sends `workspace_id` only; the "no workspace" entry
//! creates with all-None (the host's default cwd).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::i18n::{Locale, tr};
use crate::theme::Theme;
use crate::ui::style;

/// One picker row: the workspace id to create under (`None` = the "no
/// workspace" entry) plus its display label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSessionEntry {
    pub workspace_id: Option<crate::wire::session::WorkspaceId>,
    pub label: String,
}

/// The picker popup: rows of workspaces + the no-workspace entry.
pub struct NewSessionPopup<'a> {
    pub entries: &'a [NewSessionEntry],
    pub selected: usize,
    /// A create is in flight (the hint line becomes "creating…").
    pub sending: bool,
    pub theme: &'a Theme,
    pub locale: Locale,
}

impl NewSessionPopup<'_> {
    /// Outer size (border included): one row per entry + the hint row,
    /// capped at the room available.
    pub fn size(&self, available: u16, room: u16) -> (u16, u16) {
        let text = self
            .entries
            .iter()
            .map(|entry| entry.label.len())
            .max()
            .unwrap_or(0)
            .max(tr(self.locale, "create.hint").len());
        let width = (text + 8) as u16;
        let height = (self.entries.len() as u16 + 3).min(room);
        (width.clamp(28, available.max(28)), height)
    }
}

impl Widget for NewSessionPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title_style(
                Style::new()
                    .add_modifier(Modifier::BOLD)
                    .fg(self.theme.accent),
            )
            .title(tr(self.locale, "create.title"));
        let inner = block.inner(area);
        block.render(area, buf);
        // #11 popup treatment: panel_bg fill after Clear, inside the border.
        buf.set_style(inner, style::panel_fill(self.theme));
        let mut y = inner.y;

        for (i, entry) in self.entries.iter().enumerate() {
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
            buf.set_line(
                inner.x,
                y,
                &Line::from(vec![
                    Span::styled(marker, style::active(self.theme)),
                    Span::raw(entry.label.as_str()),
                ]),
                inner.width,
            );
            y += 1;
        }

        // The footer: "creating…" while in flight, else the key hint.
        if y < inner.bottom() {
            let key = if self.sending {
                "create.sending"
            } else {
                "create.hint"
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
