//! Cached per-row rendered lines + dirty set (ticket 05 Q4).
//!
//! The cache holds one [`CachedRow`] per chat node in store display order
//! (rows are contiguous per node, already wrapped to the current width). The
//! dirty set keys nodes whose accumulated text changed since the last render;
//! the draw loop re-renders only those (Q5: re-parse the accumulated text,
//! cache when idle) and virtualization slices the cached array.
//!
//! Change detection: the store rebuilds the node list wholesale on mutation,
//! so [`RowCache::sync`] compares each node against its cached row via a cheap
//! rendered signature (kind + anchor + flags + accumulated text lengths + fold
//! state) instead of full-equality clones. Length-only text hashing is the
//! deliberate v1 signal — a same-length mid-stream text edit would be missed,
//! which is acceptable for append-only chat content.

use std::collections::HashSet;

use ratatui::text::Line;

use crate::i18n::Locale;
use crate::render::image::{ImageCache, ImageRow};
use crate::render::markdown::render_node_full;
use crate::store::SessionStore;
use crate::store::node::{ChatNode, NodeKey};
use crate::theme::Theme;
use crate::wire::session::SessionId;

/// One display-order cached row: the rendered (and wrapped) lines of one node.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedRow {
    pub node_key: NodeKey,
    pub anchor_seq: i64,
    /// Rendered lines for this node — may be more than one (wrapped/multi-line
    /// markdown, code fences, tables). An inline image's filler lines are
    /// blank here; the widget paints over them at draw time. Every row but
    /// the last carries a trailing blank (the #11 between-messages spacing,
    /// maintained by [`RowCache::ensure_inter_message_blank`]).
    pub lines: Vec<Line<'static>>,
    /// Inline image segments (indices are post-wrap, into `lines`).
    pub images: Vec<ImageRow>,
    /// Half-open line ranges (post-wrap, into `lines`) that are fenced
    /// code — the widget paints them with the `code_fill` background at
    /// full content width.
    pub code_ranges: Vec<(usize, usize)>,
    /// The fill color for the code ranges (the theme's `panel_bg` at render
    /// time; rows are re-rendered on theme change, so it stays current).
    pub code_fill: ratatui::style::Color,
    /// Rendered-relevant state at render time (change detection).
    signature: u64,
}

/// The per-session row cache.
#[derive(Debug, Default)]
pub struct RowCache {
    rows: Vec<CachedRow>,
    dirty: HashSet<NodeKey>,
    /// Width the cached rows were wrapped at (resize → full re-render, Q10).
    width: u16,
}

impl RowCache {
    pub fn new() -> Self {
        RowCache::default()
    }

    /// Reconcile with the store's current node list for `session_id` at
    /// `width`:
    /// - new node → render via the markdown pipeline, insert at its display
    ///   position (nothing marked dirty — a fresh render is current);
    /// - cached node whose rendered signature changed → mark its key dirty;
    /// - node gone from the store → drop its cached rows (nothing marked);
    /// - width change → drop everything and re-render (Q10).
    ///
    /// Returns whether anything changed (the app shell uses this to decide
    /// whether to redraw).
    ///
    /// `images` is the decoded-image cache: an image block with cached bytes
    /// gets filler lines + an [`ImageRow`] segment; otherwise the bare
    /// caption placeholder (an empty cache renders exactly the placeholder
    /// tier).
    pub fn sync(
        &mut self,
        store: &SessionStore,
        session_id: &SessionId,
        width: u16,
        theme: &Theme,
        locale: Locale,
        images: &ImageCache,
    ) -> bool {
        let mut changed = false;
        let Some(state) = store.session(session_id) else {
            if !self.rows.is_empty() {
                self.rows.clear();
                changed = true;
            }
            self.dirty.clear();
            return changed;
        };
        if self.width != width {
            self.rows.clear();
            self.dirty.clear();
            self.width = width;
            changed = true;
        }

        let old_len = self.rows.len();
        let mut reordered = Vec::with_capacity(state.nodes.len());
        for node in &state.nodes {
            let collapsed = store.fold_state(session_id, &node.key).collapsed;
            let signature = rendered_signature(node, collapsed);
            match self.take_row(&node.key) {
                Some(mut row) => {
                    if row.signature != signature {
                        row.signature = signature;
                        self.dirty.insert(node.key.clone());
                        changed = true;
                    }
                    reordered.push(row);
                }
                None => {
                    let rendered = render_row(node, collapsed, width, theme, locale, images);
                    reordered.push(CachedRow {
                        node_key: node.key.clone(),
                        anchor_seq: node.anchor_seq,
                        lines: rendered.lines,
                        images: rendered.images,
                        code_ranges: rendered.code_ranges,
                        code_fill: rendered.code_fill,
                        signature,
                    });
                    changed = true;
                }
            }
        }
        // Rows whose nodes vanished were never re-collected — dropped.
        self.rows = reordered;
        // #11: 1 blank row between messages — every row but the last.
        Self::ensure_inter_message_blank(&mut self.rows);
        changed || self.rows.len() != old_len || !self.dirty.is_empty()
    }

    /// #11 spacing: exactly one blank row between messages — attached to
    /// every row except the last. A row ending in a code block already
    /// carries its trailing blank (markdown-level), so it is not doubled.
    /// Idempotent: safe to run over the whole array on every pass.
    fn ensure_inter_message_blank(rows: &mut [CachedRow]) {
        let last = rows.len().saturating_sub(1);
        for (i, row) in rows.iter_mut().enumerate() {
            if i == last {
                continue;
            }
            let ends_blank = row
                .lines
                .last()
                .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()));
            if !ends_blank {
                row.lines.push(Line::raw(""));
            }
        }
    }

    /// Re-render exactly the dirty nodes (markdown re-parse per chunk, Q5)
    /// and clear the dirty set.
    pub fn render_dirty(
        &mut self,
        store: &SessionStore,
        session_id: &SessionId,
        width: u16,
        theme: &Theme,
        locale: Locale,
        images: &ImageCache,
    ) {
        for key in self.dirty.drain() {
            let Some(state) = store.session(session_id) else {
                continue;
            };
            let Some(node) = state.nodes.iter().find(|node| node.key == key) else {
                continue;
            };
            let collapsed = store.fold_state(session_id, &key).collapsed;
            let rendered = render_row(node, collapsed, width, theme, locale, images);
            if let Some(row) = self.rows.iter_mut().find(|row| row.node_key == key) {
                row.lines = rendered.lines;
                row.images = rendered.images;
                row.code_ranges = rendered.code_ranges;
                row.code_fill = rendered.code_fill;
            }
            // Dirty re-render dropped the inter-message blank; restore it.
            Self::ensure_inter_message_blank(&mut self.rows);
        }
    }

    /// Drop every cached row (terminal resize, Q10). The next [`RowCache::sync`]
    /// re-renders everything at the new width.
    pub fn invalidate_all(&mut self) {
        self.rows.clear();
        self.dirty.clear();
    }

    /// The dirty node keys (read-only view).
    pub fn dirty(&self) -> &HashSet<NodeKey> {
        &self.dirty
    }

    /// Take the dirty set (the app shell may want to own it).
    pub fn take_dirty(&mut self) -> HashSet<NodeKey> {
        std::mem::take(&mut self.dirty)
    }

    /// The full cached row array in display order (virtualization slices this).
    pub fn lines(&self) -> &[CachedRow] {
        &self.rows
    }

    /// Map a LINE-space viewport offset (the app scrolls by rendered lines)
    /// to the (row index, line-within-row) start. The offset is clamped to
    /// the last line: an offset past the end yields the final line of the
    /// final row, so follow-mode (`offset = total - height`) always lands at
    /// the true bottom.
    pub fn line_to_row(&self, offset: usize) -> (usize, usize) {
        let mut remaining = offset;
        for (index, row) in self.rows.iter().enumerate() {
            if remaining < row.lines.len() {
                return (index, remaining);
            }
            remaining -= row.lines.len();
        }
        match self.rows.last() {
            Some(row) => (self.rows.len() - 1, row.lines.len().saturating_sub(1)),
            None => (0, 0),
        }
    }

    /// Find and remove the cached row for `key` (sync's reordering helper).
    fn take_row(&mut self, key: &str) -> Option<CachedRow> {
        let position = self.rows.iter().position(|row| row.node_key == key)?;
        Some(self.rows.remove(position))
    }
}

/// The wrapped render of one node ([`render_row`]'s output).
struct WrappedRender {
    lines: Vec<Line<'static>>,
    images: Vec<ImageRow>,
    code_ranges: Vec<(usize, usize)>,
    code_fill: ratatui::style::Color,
}

/// Render one node, wrap at `width`, and re-base the image segments' and
/// code ranges' pre-wrap indices onto the wrapped line array (filler lines
/// never split, so each marked input line maps to exactly one output start).
/// The #11 1-blank-row spacing between messages is maintained by
/// [`RowCache::ensure_inter_message_blank`], not here (a trailing blank on
/// the LAST row would break the follow-mode bottom clamp).
fn render_row(
    node: &ChatNode,
    collapsed: bool,
    width: u16,
    theme: &Theme,
    locale: Locale,
    images: &ImageCache,
) -> WrappedRender {
    let render = render_node_full(node, collapsed, theme, locale, images, width);
    let marks: Vec<usize> = render.images.iter().map(|seg| seg.line_index).collect();
    let (lines, rebased_images, code_ranges) =
        wrap_lines_marked(render.lines, width, &marks, &render.code_ranges);
    let images = render
        .images
        .into_iter()
        .zip(rebased_images)
        .map(|(mut seg, base)| {
            seg.line_index = base;
            seg
        })
        .collect();
    WrappedRender {
        lines,
        images,
        code_ranges,
        code_fill: theme.panel_bg,
    }
}

/// Wrap each line at `width`, unicode-width-aware (ratatui 0.30 removed
/// `Text::wrap`; the reflow machinery lives inside widget internals, so the
/// cache hand-rolls the split). A single grapheme wider than the width is
/// kept whole (the buffer truncates it visually). `image_marks` are input
/// line indices whose OUTPUT start indices are returned (in mark order).
/// `code_ranges` are half-open input line ranges, returned re-based to
/// output line ranges (code input lines are contiguous, so their output
/// lines form a contiguous run).
fn wrap_lines_marked(
    lines: Vec<Line<'static>>,
    width: u16,
    image_marks: &[usize],
    code_ranges: &[(usize, usize)],
) -> (Vec<Line<'static>>, Vec<usize>, Vec<(usize, usize)>) {
    if width == 0 {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let width = width as usize;
    let mut wrapped = Vec::new();
    let mut rebased = Vec::with_capacity(image_marks.len());
    let mut next_mark = image_marks.iter().peekable();
    // Output line count before each input line — maps input ranges to
    // output ranges.
    let mut out_before = Vec::with_capacity(lines.len() + 1);
    for (index, line) in lines.into_iter().enumerate() {
        out_before.push(wrapped.len());
        if next_mark.peek() == Some(&&index) {
            rebased.push(wrapped.len());
            next_mark.next();
        }
        wrapped.extend(wrap_line(line, width));
    }
    out_before.push(wrapped.len());
    let code_ranges = code_ranges
        .iter()
        .map(|(start, end)| (out_before[*start], out_before[*end]))
        .collect();
    (wrapped, rebased, code_ranges)
}

fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut result: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<ratatui::text::Span<'static>> = Vec::new();
    let mut current_width = 0usize;
    for span in line.spans {
        let mut pending = String::new();
        for ch in span.content.chars() {
            let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            // Flush when the next grapheme would overflow; a grapheme wider
            // than the whole width still lands (current_width == 0) and the
            // buffer truncates it visually.
            if current_width + ch_width > width && current_width > 0 {
                if !pending.is_empty() {
                    current.push(ratatui::text::Span::styled(
                        std::mem::take(&mut pending),
                        span.style,
                    ));
                }
                result.push(Line::from(std::mem::take(&mut current)));
                current_width = 0;
            }
            pending.push(ch);
            current_width += ch_width;
        }
        if !pending.is_empty() {
            current.push(ratatui::text::Span::styled(pending, span.style));
        }
    }
    if !current.is_empty() || result.is_empty() {
        result.push(Line::from(current));
    }
    result
}

/// FNV-1a hash over the rendered-relevant fields of a node (kind, anchor,
/// fold state, flags, and accumulated text lengths).
fn rendered_signature(node: &ChatNode, collapsed: bool) -> u64 {
    use crate::store::node::NodeData;

    let mut hash = 0xcbf29ce484222325u64;
    hash_u64(&mut hash, node.kind as u8 as u64);
    hash_u64(&mut hash, node.anchor_seq as u64);
    hash_u64(&mut hash, collapsed as u64);
    match &node.data {
        NodeData::User { content, .. } => {
            for block in content {
                hash_content_block(&mut hash, block);
            }
        }
        NodeData::Assistant {
            blocks,
            finalized,
            interrupted,
            ..
        } => {
            hash_u64(&mut hash, *finalized as u64);
            hash_u64(&mut hash, *interrupted as u64);
            for block in blocks {
                match block {
                    crate::store::node::AssistantBlock::Text { text }
                    | crate::store::node::AssistantBlock::Reasoning { text } => {
                        hash_u64(&mut hash, text.len() as u64);
                    }
                    crate::store::node::AssistantBlock::ToolCall { name, args_raw, .. } => {
                        hash_u64(&mut hash, name.len() as u64);
                        hash_u64(&mut hash, args_raw.len() as u64);
                    }
                }
            }
        }
        NodeData::Tool { call, result, .. } => {
            if let Some(call) = call {
                hash_u64(&mut hash, call.name.len() as u64);
                hash_u64(&mut hash, call.args_raw.len() as u64);
            }
            if let Some(result) = result {
                hash_u64(&mut hash, result.is_error as u64);
                if let Some(error) = &result.error {
                    hash_u64(&mut hash, error.code.len() as u64);
                }
                for block in &result.content {
                    hash_content_block(&mut hash, block);
                }
            }
        }
        NodeData::Compaction {
            summary,
            shadowed_item_count,
            shadowed_token_count,
            ..
        } => {
            hash_u64(&mut hash, summary.as_ref().map_or(0, String::len) as u64);
            hash_u64(&mut hash, shadowed_item_count.unwrap_or(0) as u64);
            hash_u64(&mut hash, shadowed_token_count.unwrap_or(0) as u64);
        }
        NodeData::TurnError { message, code, .. } => {
            hash_u64(&mut hash, message.len() as u64);
            hash_u64(&mut hash, code.as_ref().map_or(0, String::len) as u64);
        }
        NodeData::TurnMaxTokens { .. } => {}
        NodeData::Unknown { r#type, data } => {
            hash_u64(&mut hash, r#type.len() as u64);
            hash_u64(&mut hash, data.to_string().len() as u64);
        }
    }
    hash
}

fn hash_content_block(hash: &mut u64, block: &crate::store::event_data::ContentBlock) {
    use crate::store::event_data::ContentBlock;

    match block {
        ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
            hash_u64(hash, text.len() as u64);
        }
        ContentBlock::Image { attachment } => {
            hash_u64(hash, attachment.name.as_ref().map_or(0, String::len) as u64);
        }
        ContentBlock::ToolCall {
            name, arguments, ..
        } => {
            hash_u64(hash, name.len() as u64);
            hash_u64(hash, arguments.len() as u64);
        }
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            hash_u64(hash, is_error.unwrap_or(false) as u64);
            for inner in content {
                hash_content_block(hash, inner);
            }
        }
        ContentBlock::Raw(value) => {
            hash_u64(hash, value.to_string().len() as u64);
        }
    }
}

fn hash_u64(hash: &mut u64, value: u64) {
    *hash ^= value;
    *hash = hash.wrapping_mul(0x100000001b3);
}
