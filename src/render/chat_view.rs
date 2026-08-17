//! The virtualized chat widget (ticket 05 Q4).
//!
//! Draws only the visible window of the cached row array: the caller keeps
//! the viewport `offset` (scroll position); the widget clamps it and renders
//! rows into the buffer until the area is filled. Multi-line nodes spill
//! across their cached lines naturally; a row taller than the remaining area
//! is simply cut (v1 choice — no ellipsis marker).
//!
//! #11 surface: the chat content sits inside a 2/2 horizontal margin with 1
//! blank top row (the caller's cache wraps at the same content width —
//! [`content_width`]); fenced code ranges paint the theme's `panel_bg` fill
//! at full content width; a scrollbar hugs the right edge (vertically inset
//! 1, inside the content margin) when the conversation overflows.
//!
//! Inline images: a row's [`crate::render::image::ImageRow`] segments paint
//! the ratatui-image widget over their blank filler lines (after the text
//! pass, clipped to the visible window). Rows without cached bytes have no
//! segments and draw their `[image]` caption placeholder unchanged.
//!
//! #39 live tool overlay: a running tool node's header row (the
//! [`CachedRow::tool_header`] line) gets a per-tick spinner + elapsed
//! indicator written straight into the buffer after its text — the cached
//! lines stay static, so the animation costs no re-render (the caller
//! supplies the frame + per-node elapsed via [`ChatView::live`]).

use std::collections::HashMap;
use std::num::NonZeroU16;
use std::time::{Duration, Instant};

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Scrollbar, ScrollbarState, StatefulWidget, Widget};

use crate::render::image::ImageCache;
use crate::render::markdown::LINK_PREFIX;
use crate::render::row_cache::RowCache;
use crate::store::SessionStore;
use crate::wire::session::SessionId;

/// The chat's 2/2 horizontal content margin (must match `ChatView`'s inner
/// inset — the caller wraps the row cache at this width).
pub fn content_width(area_width: u16) -> u16 {
    area_width.saturating_sub(4)
}

/// The busy-spinner braille frames, cycled per tick on the status line AND
/// on running tool headers (#39). All 1 cell wide — no layout shift;
/// glyphs, not strings — i18n-safe.
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Format a busy duration codex-style: `(2m 03s)` past a minute (seconds
/// zero-padded), `(9s)` under it. `pub` for integration-test observability
/// (like [`SPINNER_FRAMES`]).
pub fn format_elapsed(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    if total < 60 {
        format!("({total}s)")
    } else {
        format!("({}m {:02}s)", total / 60, total % 60)
    }
}

/// #37: a tool duration — `1.2s` under a minute (one decimal),
/// `2m 03s` past it (the web header's `31m33s` shape).
pub fn format_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let total = secs as u64;
        format!("{}m {:02}s", total / 60, total % 60)
    }
}

/// #37: a wall-clock started stamp — the epoch seconds rendered as UTC
/// `HH:MM:SS`. UTC is deliberate: no tz database ships in this crate, and
/// the fixed rendering is deterministic + testable (a time-of-day, not a
/// timezone claim).
pub fn format_timestamp(epoch_secs: f64) -> String {
    let secs = epoch_secs.max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

/// #38: compact token counts, web-header style — `999`, `138K`, `49.2M`.
/// Sub-K values are exact; compact values drop a trailing `.0` (`138K`,
/// never `138.0K`).
pub fn format_tokens(count: i64) -> String {
    let n = count.max(0) as f64;
    let compact = |value: f64, suffix: &str| {
        let text = format!("{value:.1}");
        let text = text.strip_suffix(".0").unwrap_or(&text);
        format!("{text}{suffix}")
    };
    if n >= 1_000_000.0 {
        compact(n / 1_000_000.0, "M")
    } else if n >= 1_000.0 {
        compact(n / 1_000.0, "K")
    } else {
        count.to_string()
    }
}

/// #38: absolute token counts with thousands separators — `141,021`
/// (the web composer meter's exact form).
pub fn format_tokens_abs(count: i64) -> String {
    let digits = count.max(0).to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// #39: a summed duration in the stats bar's compact web shape —
/// `31m33s` / `10m45s` past a minute, `9s` under it (no decimals, no
/// spaces — the header's exact form).
pub fn format_duration_compact(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    if total < 60 {
        format!("{total}s")
    } else {
        format!("{}m{:02}s", total / 60, total % 60)
    }
}

/// #39: the per-draw live chat state for running tool headers — the
/// spinner frame index, the indicator styles (built by the caller from
/// the active theme), plus, per running tool node key, the instant the
/// caller first observed it running (elapsed is measured at draw time).
/// `None` on `ChatView` draws with no live overlay.
pub struct LiveChatState<'a> {
    pub frame: usize,
    pub running: &'a HashMap<String, Instant>,
    /// The spinner glyph's style (accent).
    pub spinner_style: Style,
    /// The elapsed text's style (hint).
    pub elapsed_style: Style,
}

/// Virtualized chat list over the cached rows of one session.
pub struct ChatView<'a> {
    pub store: &'a SessionStore,
    pub session_id: &'a SessionId,
    /// Viewport offset into the cached row array (scroll position).
    pub offset: usize,
    pub row_cache: &'a mut RowCache,
    /// Decoded images for the inline segments (empty in v1 — module docs).
    pub images: &'a mut ImageCache,
    /// #39: the live running/elapsed overlay for tool headers (`None`
    /// when idle — no per-tick chrome).
    pub live: Option<LiveChatState<'a>>,
}

impl Widget for ChatView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rows = self.row_cache.lines();
        if rows.is_empty() || area.height < 2 || area.width == 0 {
            return;
        }
        // The 2/2 horizontal content margin (#11); the first content row is
        // a blank spacer (the 1-top inset).
        let inner = area.inner(Margin {
            horizontal: 2,
            vertical: 0,
        });
        let content_top = inner.top() + 1;
        let total: usize = rows.iter().map(|row| row.lines.len()).sum();
        // The offset is LINE-space (the app scrolls by rendered lines); map
        // it to a (row, line-within-row) start and clamp to the last line.
        let (start_row, start_line) = self.row_cache.line_to_row(self.offset);
        let mut y = content_top;
        for (row_index, row) in rows.iter().enumerate().skip(start_row) {
            let row_top = y;
            let skip = if row_index == start_row {
                start_line
            } else {
                0
            };
            // #11: fenced code rows paint the panel_bg fill at full content
            // width (with the Reset default theme the fill is a no-op).
            for &(start, end) in &row.code_ranges {
                for line_index in start.max(skip)..end {
                    let fy = row_top + line_index as u16;
                    if fy >= area.bottom() {
                        break;
                    }
                    buf.set_style(
                        Rect::new(inner.x, fy, inner.width, 1),
                        Style::new().bg(row.code_fill),
                    );
                }
            }
            for line in row.lines.iter().skip(skip) {
                if y >= area.bottom() {
                    break;
                }
                draw_line_with_links(buf, inner.x, y, line, inner.width);
                // #39: the running tool's header carries the live
                // spinner + elapsed indicator (buffer-only — the cached
                // lines stay static, so the per-tick animation costs no
                // re-render).
                let line_index = y - row_top;
                if usize::from(line_index) == row.tool_header.unwrap_or(usize::MAX)
                    && let Some(live) = self.live.as_ref()
                    && let Some(since) = live.running.get(&row.node_key)
                {
                    draw_live_indicator(buf, inner.x, y, inner.width, line, live, since.elapsed());
                }
                y += 1;
            }
            // Image segments paint over their filler lines (text pass first).
            for segment in &row.images {
                let seg_y = row_top + segment.line_index as u16;
                if seg_y >= area.bottom() {
                    continue;
                }
                let visible = segment.rows.min(area.bottom() - seg_y);
                let rect = Rect::new(inner.x, seg_y, inner.width, visible);
                if let Some(loaded) = self.images.get_mut(&segment.attachment_id) {
                    ratatui_image::StatefulImage::new().render(rect, buf, &mut loaded.protocol);
                }
            }
            if y >= area.bottom() {
                break;
            }
        }
        // #11: the scrollbar hugs the pane's right edge (inside the content
        // margin), vertically inset 1 so it clears the blank top row and the
        // bottom edge. Only when the conversation overflows.
        let viewport = area.height.saturating_sub(1) as usize;
        if total > viewport {
            let mut state = ScrollbarState::new(total).position(self.offset);
            Scrollbar::default()
                .orientation(ratatui::widgets::ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .render(
                    area.inner(Margin {
                        vertical: 1,
                        horizontal: 0,
                    }),
                    buf,
                    &mut state,
                );
        }
    }
}

/// OSC 8 hyperlink open/close sequences. Emitted around every markdown
/// link's cells (see [`draw_line_with_links`]); terminals without OSC 8
/// support ignore the sequences and render the text unchanged — no
/// terminal detection needed.
fn osc8_open(url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\")
}

fn osc8_close() -> &'static str {
    "\x1b]8;;\x1b\\"
}

/// Split a span content into (visible text, link URL): a leading
/// `ZWSP url ZWSP` prefix (set by the markdown sink, #2) marks the span as
/// a markdown link; anything else is plain text.
fn strip_link_prefix(content: &str) -> (&str, Option<&str>) {
    let Some(rest) = content.strip_prefix(LINK_PREFIX) else {
        return (content, None);
    };
    let Some((url, text)) = rest.split_once(LINK_PREFIX) else {
        return (content, None);
    };
    (text, Some(url))
}

/// Draw one (pre-wrapped) line, injecting the OSC 8 hyperlink sequences
/// around markdown link spans. `Buffer::set_stringn` strips control
/// characters from span content, so the sequences cannot ride the content
/// — instead the prefix is removed and the sequences are written straight
/// into the first/last cell symbols, which the terminal backend prints
/// verbatim. Ratatui 0.30.2 has no `Span::hyperlink` API, hence the raw
/// sequences (see the markdown module docs, #2).
fn draw_line_with_links(buf: &mut Buffer, x: u16, y: u16, line: &Line<'static>, width: u16) {
    let mut x = x;
    let mut remaining = width;
    for span in &line.spans {
        if remaining == 0 {
            break;
        }
        let (text, url) = strip_link_prefix(&span.content);
        let x0 = x;
        let (x_end, _) = buf.set_span(
            x,
            y,
            &Span::styled(text, line.style.patch(span.style)),
            remaining,
        );
        let drawn = x_end.saturating_sub(x0);
        if drawn > 0
            && let Some(url) = url
        {
            // The open sequence precedes the first visible cell, the close
            // follows the last — the hyperlink region is exactly the span.
            // Both cells are forced to width 1: the diff derives a cell's
            // width from its symbol, and the embedded sequences would
            // otherwise make the cell look multi-width and swallow the
            // following cells as trailing columns.
            let first = buf[(x0, y)].symbol().to_string();
            buf[(x0, y)]
                .set_symbol(&format!("{}{}", osc8_open(url), first))
                .set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
            let last = buf[(x_end - 1, y)].symbol().to_string();
            buf[(x_end - 1, y)]
                .set_symbol(&format!("{last}{}", osc8_close()))
                .set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
        }
        x = x_end;
        remaining = remaining.saturating_sub(drawn);
    }
}

/// #39: write the live running indicator — ` ⠋ (12s)` — into the buffer
/// immediately after a running tool header's drawn text (spinner in the
/// accent style, elapsed in the hint style, both truncated to the line's
/// remaining width). The header's drawn width is recomputed from its spans
/// so the indicator lands right after the text even when the header was
/// wrapped (a narrow terminal truncates the tail — the "subtle" ask).
fn draw_live_indicator(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    header: &Line<'static>,
    live: &LiveChatState<'_>,
    elapsed: Duration,
) {
    let drawn = line_visible_width(header);
    let mut cursor = x + drawn;
    if cursor >= x + width {
        return;
    }
    let spinner = SPINNER_FRAMES[live.frame % SPINNER_FRAMES.len()];
    let remaining = (x + width) - cursor;
    let (next, _) = buf.set_stringn(
        cursor,
        y,
        format!(" {spinner}"),
        usize::from(remaining),
        live.spinner_style,
    );
    cursor = next;
    if cursor < x + width {
        let remaining = (x + width) - cursor;
        buf.set_stringn(
            cursor,
            y,
            format_elapsed(elapsed),
            usize::from(remaining),
            live.elapsed_style,
        );
    }
}

/// The rendered width of a (pre-wrapped) line: the sum of its spans'
/// visible unicode widths, with markdown-link prefixes stripped (the OSC 8
/// sequences ride the symbols and occupy no cells). The line was wrapped
/// at the content width, so the result never exceeds it.
fn line_visible_width(line: &Line<'static>) -> u16 {
    use unicode_width::UnicodeWidthStr;
    line.spans
        .iter()
        .map(|span| {
            let (text, _) = strip_link_prefix(&span.content);
            UnicodeWidthStr::width(text) as u16
        })
        .sum()
}
