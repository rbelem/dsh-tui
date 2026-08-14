//! The sidebar: the session list from the gateway, one row per session.
//!
//! Layout: a full-height left pane separated from the chat by a single
//! vertical rule (no box). The first line is the "Sessions" header; rows
//! follow. The active session carries a bold `●` marker at all times; the
//! selection row is reversed, but only while the sidebar has focus (an
//! unfocused sidebar shows no selection — there is nothing to operate on).
//! Running sessions get a dim `· running` suffix.
//!
//! The list is the attach flow's `session.list` snapshot plus live
//! host-stream updates (`App::handle_host_frame`). Workspace grouping is a
//! later lane.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use crate::i18n::{Locale, tr};
use crate::ui::style;
use crate::wire::session::{SessionId, SessionSummary};

/// Sidebar pane width; the pane is hidden below 60 total columns.
pub const SIDEBAR_WIDTH: u16 = 22;

/// Sidebar width for a terminal `total` columns wide (collapse gracefully).
pub fn sidebar_width(total: u16) -> u16 {
    if total < 60 { 0 } else { SIDEBAR_WIDTH }
}

/// Sidebar interaction state (the list itself lives on `App::sessions`).
#[derive(Debug, Default)]
pub struct SidebarState {
    /// Selected row index into `App::sessions`.
    pub selected: usize,
}

impl SidebarState {
    /// Move the selection by `delta` rows (clamped to the list).
    pub fn move_by(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        let last = len.saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    pub fn first(&mut self) {
        self.selected = 0;
    }

    pub fn last(&mut self, len: usize) {
        self.selected = len.saturating_sub(1);
    }

    /// Keep the selection inside the list (sessions are a static snapshot in
    /// v1, but the live-update lane will shrink it).
    pub fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
    }
}

/// The sidebar widget: header + one row per session.
pub struct SidebarView<'a> {
    pub sessions: &'a [SessionSummary],
    pub active: Option<&'a SessionId>,
    pub selected: usize,
    pub focused: bool,
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl Widget for SidebarView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::new()
            .borders(Borders::RIGHT)
            .border_style(if self.focused {
                style::border_focused(self.theme)
            } else {
                style::border(self.theme)
            });
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        buf.set_line(
            inner.x,
            inner.y,
            &Line::styled(tr(self.locale, "sidebar.header"), style::header(self.theme)),
            inner.width,
        );

        if self.sessions.is_empty() {
            let y = inner.y + 2;
            if y < inner.bottom() {
                buf.set_line(
                    inner.x,
                    y,
                    &Line::raw(tr(self.locale, "sidebar.empty")),
                    inner.width,
                );
            }
            if y + 1 < inner.bottom() {
                buf.set_line(
                    inner.x,
                    y + 1,
                    &Line::styled(
                        tr(self.locale, "sidebar.empty_hint"),
                        style::hint(self.theme),
                    ),
                    inner.width,
                );
            }
            return;
        }

        let visible = inner.height.saturating_sub(1) as usize;
        // Keep the selected row visible (scrolls so it sits at the edge).
        let start = self.selected.saturating_sub(visible.saturating_sub(1));
        for (i, summary) in self.sessions.iter().enumerate().skip(start) {
            let row = i - start + 1;
            if row as u16 >= inner.height {
                break;
            }
            let y = inner.y + row as u16;
            if self.focused && i == self.selected {
                buf.set_style(
                    Rect::new(inner.x, y, inner.width, 1),
                    style::selection(self.theme),
                );
            }
            let is_active = self.active == Some(&summary.session_id);
            let marker = if is_active { "● " } else { "  " };
            let mut spans = vec![Span::styled(marker, style::active(self.theme))];
            spans.push(Span::raw(label(summary)));
            if summary.running {
                spans.push(Span::styled(
                    tr(self.locale, "sidebar.running"),
                    style::hint(self.theme),
                ));
            }
            buf.set_line(inner.x, y, &Line::from(spans), inner.width);
        }
    }
}

/// Row label: the session title projection when present, else the id.
fn label(summary: &SessionSummary) -> String {
    let title = summary
        .projections
        .as_ref()
        .and_then(|block| block.values.get("title"))
        .and_then(|value| {
            // The title projection rides both shapes on the wire: a bare
            // string, or the rename snapshot `{title, seq}`.
            value
                .as_str()
                .or_else(|| value.get("title").and_then(|v| v.as_str()))
        });
    match title {
        Some(title) if !title.trim().is_empty() => title.to_string(),
        _ => summary.session_id.0.clone(),
    }
}
