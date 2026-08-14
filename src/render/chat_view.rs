//! The virtualized chat widget (ticket 05 Q4).
//!
//! Draws only the visible window of the cached row array: the caller keeps
//! the viewport `offset` (scroll position); the widget clamps it and renders
//! rows into the buffer until the area is filled. Multi-line nodes spill
//! across their cached lines naturally; a row taller than the remaining area
//! is simply cut (v1 choice — no ellipsis marker).
//!
//! Inline images: a row's [`crate::render::image::ImageRow`] segments paint
//! the ratatui-image widget over their blank filler lines (after the text
//! pass, clipped to the visible window). Rows without cached bytes have no
//! segments and draw their `[image]` caption placeholder unchanged.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{StatefulWidget, Widget};

use crate::render::image::ImageCache;
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
    /// Decoded images for the inline segments (empty in v1 — module docs).
    pub images: &'a mut ImageCache,
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
            let row_top = y;
            for line in &row.lines {
                if y >= area.bottom() {
                    break;
                }
                buf.set_line(area.x, y, line, area.width);
                y += 1;
            }
            // Image segments paint over their filler lines (text pass first).
            for segment in &row.images {
                let seg_y = row_top + segment.line_index as u16;
                if seg_y >= area.bottom() {
                    continue;
                }
                let visible = segment.rows.min(area.bottom() - seg_y);
                let rect = Rect::new(area.x, seg_y, area.width, visible);
                if let Some(loaded) = self.images.get_mut(&segment.attachment_id) {
                    ratatui_image::StatefulImage::new().render(rect, buf, &mut loaded.protocol);
                }
            }
            if y >= area.bottom() {
                return;
            }
        }
    }
}
