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

use crate::render::markdown::render_node;
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
    /// markdown, code fences, tables).
    pub lines: Vec<Line<'static>>,
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
    pub fn sync(
        &mut self,
        store: &SessionStore,
        session_id: &SessionId,
        width: u16,
        theme: &Theme,
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
                    reordered.push(CachedRow {
                        node_key: node.key.clone(),
                        anchor_seq: node.anchor_seq,
                        lines: wrap_lines(render_node(node, collapsed, theme), width),
                        signature,
                    });
                    changed = true;
                }
            }
        }
        // Rows whose nodes vanished were never re-collected — dropped.
        self.rows = reordered;
        changed || self.rows.len() != old_len || !self.dirty.is_empty()
    }

    /// Re-render exactly the dirty nodes (markdown re-parse per chunk, Q5)
    /// and clear the dirty set.
    pub fn render_dirty(
        &mut self,
        store: &SessionStore,
        session_id: &SessionId,
        width: u16,
        theme: &Theme,
    ) {
        for key in self.dirty.drain() {
            let Some(state) = store.session(session_id) else {
                continue;
            };
            let Some(node) = state.nodes.iter().find(|node| node.key == key) else {
                continue;
            };
            let collapsed = store.fold_state(session_id, &key).collapsed;
            let lines = wrap_lines(render_node(node, collapsed, theme), width);
            if let Some(row) = self.rows.iter_mut().find(|row| row.node_key == key) {
                row.lines = lines;
            }
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

    /// Find and remove the cached row for `key` (sync's reordering helper).
    fn take_row(&mut self, key: &str) -> Option<CachedRow> {
        let position = self.rows.iter().position(|row| row.node_key == key)?;
        Some(self.rows.remove(position))
    }
}

/// Wrap each line at `width`, unicode-width-aware (ratatui 0.30 removed
/// `Text::wrap`; the reflow machinery lives inside widget internals, so the
/// cache hand-rolls the split). A single grapheme wider than the width is
/// kept whole (the buffer truncates it visually).
fn wrap_lines(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let width = width as usize;
    let mut wrapped = Vec::new();
    for line in lines {
        wrapped.extend(wrap_line(line, width));
    }
    wrapped
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
