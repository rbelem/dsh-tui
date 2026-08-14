//! The full-screen image viewer (PARITY.md Images row): a takeover-style
//! mode opened with `v` from the chat (the chat has no per-row focus, so `v`
//! opens on the first image row at/after the viewport top, else the last
//! image in the session). `n`/`p` cycle the session's image blocks in
//! display order (wrap-around), `t` toggles fit-to-screen / actual-size,
//! `Esc`/`q` close. Zoom/pan are out of scope.
//!
//! Rendering: with decoded bytes in the [`ImageCache`] the image draws LARGE
//! — fit mode is `Resize::Fit` into the body; actual mode renders the
//! natural pixel size centered and clipped at the screen edge (no scrolling
//! in v1). Without bytes (v1: always — the `session.attachment` fetch is
//! unwired) the viewer shows the centered placeholder: the `[image: name]`
//! caption, the attachment's dimensions/meta line, and a dim notice. The
//! placeholder path is fully interactive (n/p/t/Esc all work).

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, StatefulWidget, Widget, Wrap};

use crate::i18n::{Locale, tr, trf};
use crate::render::image::{ASSUMED_FONT_SIZE, ImageCache, ImageProtocol};
use crate::ui::style;
use crate::wire::session::{ImageAttachmentRef, ImageMediaType, SessionId};

/// The viewer mode state: the session's image blocks in display order plus
/// the cursor and the fit toggle.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageViewer {
    pub session_id: SessionId,
    /// Every image block of the session in display order (the `n`/`p` cycle
    /// list — attachment refs, not bytes; the cache may not have them).
    pub images: Vec<ImageAttachmentRef>,
    /// The focused image (wraps on both ends).
    pub index: usize,
    /// true = fit to the screen; false = actual pixel size (centered,
    /// clipped at the screen edge).
    pub fit: bool,
}

impl ImageViewer {
    pub fn new(session_id: SessionId, images: Vec<ImageAttachmentRef>, index: usize) -> Self {
        ImageViewer {
            session_id,
            images,
            index,
            fit: true,
        }
    }

    /// The focused image (the viewer never opens on an empty list).
    pub fn current(&self) -> &ImageAttachmentRef {
        &self.images[self.index.min(self.images.len().saturating_sub(1))]
    }

    /// `n`: next image, wrapping past the end to the first.
    pub fn next(&mut self) {
        if !self.images.is_empty() {
            self.index = (self.index + 1) % self.images.len();
        }
    }

    /// `p`: previous image, wrapping before the first to the last.
    pub fn prev(&mut self) {
        if !self.images.is_empty() {
            self.index = (self.index + self.images.len() - 1) % self.images.len();
        }
    }

    /// `t`: toggle fit / actual size.
    pub fn toggle_fit(&mut self) {
        self.fit = !self.fit;
    }
}

/// The wire media type as its MIME string (the schema's four raster types).
fn media_type_str(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::ImagePng => "image/png",
        ImageMediaType::ImageJpeg => "image/jpeg",
        ImageMediaType::ImageWebp => "image/webp",
        ImageMediaType::ImageGif => "image/gif",
    }
}

/// Compact byte count ("640 B", "45 KB", "3 MB").
fn byte_size(bytes: i64) -> String {
    if bytes >= 1_000_000 {
        format!("{} MB", bytes / 1_000_000)
    } else if bytes >= 1_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{bytes} B")
    }
}

/// The viewer body (full-screen replace, like the takeovers).
pub struct ImageViewerView<'a> {
    pub viewer: &'a ImageViewer,
    /// Decoded images (empty in v1 — the placeholder path renders).
    pub images: &'a mut ImageCache,
    /// The detected protocol tier (drives the placeholder notice).
    pub protocol: ImageProtocol,
    /// Transient notice (toast/hint), rendered dim with the hints.
    pub notice: Option<&'a str>,
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl Widget for ImageViewerView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let viewer = self.viewer;
        let attachment = viewer.current();
        let name = attachment
            .name
            .as_deref()
            .unwrap_or_else(|| tr(self.locale, "marker.image_default"));
        let title = trf(
            self.locale,
            "viewer.title",
            &[
                &(viewer.index + 1).to_string(),
                &viewer.images.len().to_string(),
                name,
            ],
        );
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title(title);
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // The bottom hint line is always visible; the body is the rest.
        let body = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
        let hint_y = inner.y + inner.height - 1;
        let hints = Line::from(vec![
            Span::styled("n", style::active(self.theme)),
            Span::raw(tr(self.locale, "viewer.action_next")),
            Span::styled("p", style::active(self.theme)),
            Span::raw(tr(self.locale, "viewer.action_prev")),
            Span::styled("t", style::active(self.theme)),
            Span::raw(tr(self.locale, "viewer.action_fit")),
            Span::styled("esc", style::active(self.theme)),
            Span::raw(tr(self.locale, "viewer.action_close")),
        ]);
        buf.set_line(inner.x, hint_y, &hints, inner.width);

        match self.images.get_mut(&attachment.attachment_id) {
            Some(loaded) if body.height > 0 => {
                let rect = if viewer.fit {
                    body
                } else {
                    // Actual pixel size, centered, clipped at the screen edge.
                    let natural = ratatui_image::Resize::Fit(None).size_for(
                        &loaded.source,
                        ASSUMED_FONT_SIZE,
                        ratatui::layout::Size {
                            width: u16::MAX,
                            height: u16::MAX,
                        },
                    );
                    let width = natural.width.min(body.width);
                    let height = natural.height.min(body.height);
                    Rect::new(
                        body.x + body.width.saturating_sub(width) / 2,
                        body.y + body.height.saturating_sub(height) / 2,
                        width,
                        height,
                    )
                };
                let resize = if viewer.fit {
                    ratatui_image::Resize::Fit(None)
                } else {
                    // Crop keeps native pixels when the image exceeds the
                    // rect (clip at the edge; no zoom/pan in v1).
                    ratatui_image::Resize::Crop(None)
                };
                ratatui_image::StatefulImage::new().resize(resize).render(
                    rect,
                    buf,
                    &mut loaded.protocol,
                );
            }
            _ => self.render_placeholder(body, buf, attachment, name),
        }

        if let Some(notice) = self.notice
            && hint_y > 0
        {
            buf.set_line(
                inner.x,
                hint_y - 1,
                &Line::styled(notice, style::hint(self.theme)),
                inner.width,
            );
        }
    }
}

impl ImageViewerView<'_> {
    /// The no-bytes body: the `[image]` caption, the attachment meta line,
    /// and the dim why-not notice — centered as a block.
    fn render_placeholder(
        &self,
        body: Rect,
        buf: &mut Buffer,
        attachment: &ImageAttachmentRef,
        name: &str,
    ) {
        let why = if self.protocol == ImageProtocol::None {
            tr(self.locale, "viewer.no_protocol")
        } else {
            tr(self.locale, "viewer.no_bytes")
        };
        let caption = trf(self.locale, "marker.image", &[name]);
        let meta = trf(
            self.locale,
            "viewer.meta",
            &[
                &attachment.width.to_string(),
                &attachment.height.to_string(),
                media_type_str(attachment.media_type),
                &byte_size(attachment.bytes),
            ],
        );
        let lines = vec![
            Line::styled(caption, style::active(self.theme)),
            Line::styled(meta, Style::default().fg(self.theme.muted)),
            Line::styled(why, style::hint(self.theme)),
        ];
        let block_height = lines.len() as u16;
        let y = body.y + body.height.saturating_sub(block_height) / 2;
        let centered = Rect::new(body.x, y, body.width, block_height.min(body.height));
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .render(centered, buf);
    }
}
