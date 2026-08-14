//! The virtualized chat widget (ticket 05 Q4).
//!
//! Draws only the visible window of the cached row array: the caller keeps
//! the viewport `offset` (scroll position); the widget clamps it and renders
//! rows into the buffer until the area is filled. Multi-line nodes spill
//! across their cached lines naturally; a row taller than the remaining area
//! is simply cut (v1 choice — no ellipsis marker).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use crate::render::row_cache::RowCache;
use crate::store::SessionStore;
use crate::wire::session::SessionId;

/// Virtualized chat list over the cached rows of one session.
pub struct ChatView<'a> {
    pub store: &'a SessionStore,
    pub session_id: &'a SessionId,
    /// Viewport offset into the cached row array (scroll position).
    pub offset: usize,
    pub row_cache: &'a mut RowCache,
}

impl Widget for ChatView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rows = self.row_cache.lines();
        if rows.is_empty() || area.height == 0 {
            return;
        }
        let height = area.height as usize;
        // Clamp the offset so the viewport stays inside the row array.
        let offset = self.offset.min(rows.len().saturating_sub(height));
        let mut y = area.top();
        for row in &rows[offset..] {
            for line in &row.lines {
                if y >= area.bottom() {
                    return;
                }
                buf.set_line(area.x, y, line, area.width);
                y += 1;
            }
        }
    }
}
