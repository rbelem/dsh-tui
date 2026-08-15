//! Streaming markdown pipeline (ticket 05 Q5/Q12).
//!
//! [`render_node`] renders one chat node into unstyled-then-styled `Line`s,
//! re-parsing the node's accumulated text on every call — cheap for bounded
//! rows; caching happens at the [`crate::render::row_cache::RowCache`] level
//! (idle nodes cached, dirty nodes re-parsed).
//!
//! Markdown surface (#11): CommonMark + tables + strikethrough via
//! pulldown-cmark, `[image]` placeholder captions that upgrade to inline
//! images when the [`crate::render::image`] pipeline has decoded bytes for
//! the attachment ([`render_node_full`] reports the filler segments; v1 has
//! no fetch path, so captions are what renders). Semantic styling only —
//! no hardcoded colors: dim/bold/italic/crossed-out modifiers; colors come
//! from the theme tokens. Code (inline + fenced blocks) is the theme's
//! `code` token; fenced blocks additionally report their line range so the
//! row cache can paint the `panel_bg` fill at full content width (no box,
//! no indent). One blank row around code blocks and before headings; links
//! are the accent token.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::i18n::{Locale, tr, trf};
use crate::render::image::{ImageCache, ImageRow};
use crate::store::event_data::ContentBlock;
use crate::store::node::{AssistantBlock, ChatNode, NodeData};
use crate::theme::Theme;

/// Maximum width of a tool-arguments preview on the call line.
const ARGS_PREVIEW_MAX: usize = 100;

/// [`render_node_full`] output: the display lines plus the inline-image
/// segments and the code-block line ranges. Segment/range line indices are
/// PRE-wrap indices into `lines`; the row cache re-bases them after wrapping
/// (filler lines never split, so the re-base is exact).
pub struct NodeRender {
    pub lines: Vec<Line<'static>>,
    pub images: Vec<ImageRow>,
    /// Half-open line ranges (into `lines`, pre-wrap) that are fenced code:
    /// the row cache paints them with the theme's `panel_bg` fill at full
    /// content width.
    pub code_ranges: Vec<(usize, usize)>,
}

/// Render one chat node to (unwrapped) display lines. `collapsed` is the
/// node's fold state (Q11): collapsed tool nodes render a one-line summary.
/// `theme` supplies the semantic colors (text/muted/error/warning/code);
/// `locale` localizes the row markers.
///
/// Placeholder tier: image blocks always render their `[image: name]`
/// caption (no image cache consulted). Byte-identical to the pre-image
/// pipeline output.
pub fn render_node(
    node: &ChatNode,
    collapsed: bool,
    theme: &Theme,
    locale: Locale,
) -> Vec<Line<'static>> {
    render_node_full(node, collapsed, theme, locale, &ImageCache::default(), 0).lines
}

/// The inline-image context threaded through the node renderers: the byte
/// cache plus the wrap width (the inline fit budget).
struct InlinePlan<'a> {
    cache: &'a ImageCache,
    width: u16,
}

/// [`render_node`] with the image pipeline wired: an image block whose
/// attachment has decoded bytes in `images` renders its caption followed by
/// `rows` blank filler lines (the widget draws over them at draw time);
/// anything else keeps the bare caption placeholder. `width` is the wrap
/// width (the inline fit budget).
pub fn render_node_full(
    node: &ChatNode,
    collapsed: bool,
    theme: &Theme,
    locale: Locale,
    images: &ImageCache,
    width: u16,
) -> NodeRender {
    let notice = |text: String| Line::styled(text, Style::default().fg(theme.muted));
    let plan = InlinePlan {
        cache: images,
        width,
    };
    let mut image_rows = Vec::new();
    let mut code_ranges = Vec::new();
    let lines = match &node.data {
        NodeData::User { content, .. } => {
            let (lines, ranges) =
                render_content_blocks(content, theme, locale, &plan, &mut image_rows);
            code_ranges = ranges;
            lines
        }
        NodeData::Assistant {
            blocks,
            interrupted,
            ..
        } => {
            let mut lines = Vec::new();
            for block in blocks {
                let (block_lines, block_ranges) = render_assistant_block(block, theme, locale);
                code_ranges.extend(
                    block_ranges
                        .into_iter()
                        .map(|(start, end)| (start + lines.len(), end + lines.len())),
                );
                lines.extend(block_lines);
            }
            if *interrupted {
                lines.push(Line::styled(
                    tr(locale, "marker.interrupted"),
                    Style::default().fg(theme.warning),
                ));
            }
            lines
        }
        NodeData::Tool { call, result, .. } => {
            let (lines, ranges) = render_tool_node(
                call.as_ref(),
                result.as_deref(),
                collapsed,
                theme,
                locale,
                &plan,
                &mut image_rows,
            );
            code_ranges = ranges;
            lines
        }
        NodeData::Compaction {
            shadowed_item_count,
            ..
        } => {
            let count = shadowed_item_count.unwrap_or(0);
            vec![notice(trf(
                locale,
                "marker.compacted",
                &[&count.to_string()],
            ))]
        }
        NodeData::TurnError { code, .. } => {
            let code = code.as_deref().unwrap_or("unknown");
            vec![Line::styled(
                trf(locale, "marker.turn_error", &[code]),
                Style::default().fg(theme.error),
            )]
        }
        NodeData::TurnMaxTokens { .. } => {
            vec![Line::styled(
                tr(locale, "marker.max_tokens"),
                Style::default().fg(theme.warning),
            )]
        }
        NodeData::Unknown { r#type, .. } => vec![notice(trf(locale, "marker.unknown", &[r#type]))],
    };
    NodeRender {
        lines,
        images: image_rows,
        code_ranges,
    }
}

/// Render the markdown `text` with `base_style` into display lines. `theme`
/// supplies the token colors (text/muted/code/accent). Returns the lines
/// plus the half-open line ranges (into the returned vec) that are fenced
/// code — the `panel_bg` fill targets.
pub fn render_markdown(
    text: &str,
    base_style: Style,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let mut options = pulldown_cmark::Options::empty();
    // ENABLE_HEADINGS / ENABLE_BOLD_ITALIC were removed in pulldown-cmark
    // 0.12+ (headings and bold/italic are always enabled); tables and
    // strikethrough are the opt-in surface (ticket 05 Q12).
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    let parser = pulldown_cmark::Parser::new_ext(text, options);
    let mut sink = Sink::new(base_style, theme);
    for event in parser {
        sink.push_event(event);
    }
    sink.finish();
    (sink.lines, sink.code_ranges)
}

// ---------------------------------------------------------------------------
// node renderers
// ---------------------------------------------------------------------------

/// Render one assistant block into lines; the returned code ranges index
/// into the returned vec. (Assistant blocks have no image content, so the
/// image plan is not threaded here.)
fn render_assistant_block(
    block: &AssistantBlock,
    theme: &Theme,
    locale: Locale,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    match block {
        AssistantBlock::Text { text } => {
            render_markdown(text, Style::default().fg(theme.text), theme)
        }
        AssistantBlock::Reasoning { text } => render_markdown(
            text,
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
            theme,
        ),
        AssistantBlock::ToolCall { name, args_raw, .. } => (
            vec![tool_call_line(name, args_raw, theme, locale)],
            Vec::new(),
        ),
    }
}

/// Render content blocks into lines; the returned code ranges index into
/// the returned vec.
fn render_content_blocks(
    content: &[ContentBlock],
    theme: &Theme,
    locale: Locale,
    plan: &InlinePlan<'_>,
    image_rows: &mut Vec<ImageRow>,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let mut lines = Vec::new();
    let mut code_ranges = Vec::new();
    for block in content {
        let (block_lines, block_ranges) = match block {
            ContentBlock::Text { text } => {
                render_markdown(text, Style::default().fg(theme.text), theme)
            }
            ContentBlock::Reasoning { text } => render_markdown(
                text,
                Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
                theme,
            ),
            ContentBlock::Image { attachment } => {
                let name = attachment
                    .name
                    .as_deref()
                    .unwrap_or_else(|| tr(locale, "marker.image_default"));
                // The caption is the placeholder tier AND the inline image's
                // label — always emitted, byte-identical either way.
                let mut caption_lines = vec![Line::styled(
                    trf(locale, "marker.image", &[name]),
                    Style::default().fg(theme.muted),
                )];
                // Inline tier: decoded bytes in the cache → reserve `rows`
                // blank filler lines below the caption; the draw loop paints
                // the image widget over them. v1's cache is always empty
                // (no session.attachment fetch — see render::image docs).
                if let Some(loaded) = plan.cache.get(&attachment.attachment_id) {
                    let rows = ImageCache::inline_rows(&loaded.source, plan.width);
                    image_rows.push(ImageRow {
                        line_index: lines.len() + caption_lines.len(),
                        attachment_id: attachment.attachment_id.clone(),
                        rows,
                    });
                    caption_lines.extend((0..rows).map(|_| Line::raw("")));
                }
                (caption_lines, Vec::new())
            }
            ContentBlock::ToolCall { name, .. } => (
                vec![Line::styled(
                    format!("{} {name}", tr(locale, "marker.tool")),
                    Style::default().fg(theme.text),
                )],
                Vec::new(),
            ),
            ContentBlock::ToolResult { is_error, .. } => {
                let suffix = if *is_error == Some(true) {
                    tr(locale, "marker.failed_suffix")
                } else {
                    ""
                };
                (
                    vec![Line::styled(
                        format!("{}{suffix}", tr(locale, "marker.tool_result")),
                        Style::default().fg(theme.muted),
                    )],
                    Vec::new(),
                )
            }
            ContentBlock::Raw(value) => {
                let block_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                (
                    vec![Line::styled(
                        trf(locale, "marker.block", &[block_type]),
                        Style::default().fg(theme.muted),
                    )],
                    Vec::new(),
                )
            }
        };
        code_ranges.extend(
            block_ranges
                .into_iter()
                .map(|(start, end)| (start + lines.len(), end + lines.len())),
        );
        lines.extend(block_lines);
    }
    (lines, code_ranges)
}

fn render_tool_node(
    call: Option<&crate::store::node::RunningToolCall>,
    result: Option<&crate::store::node::ToolResultNode>,
    collapsed: bool,
    theme: &Theme,
    locale: Locale,
    plan: &InlinePlan<'_>,
    image_rows: &mut Vec<ImageRow>,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let name = call
        .map(|c| c.name.as_str())
        .or_else(|| result.and_then(|r| r.call.as_ref().map(|c| c.name.as_str())))
        .unwrap_or("tool");
    let args_raw = call.map(|c| c.args_raw.as_str()).unwrap_or_default();

    if collapsed {
        // One-line summary (lifecycle icon + title, Q11).
        let failed = result.is_some_and(|r| r.is_error);
        let suffix = if failed {
            tr(locale, "marker.failed_suffix")
        } else {
            ""
        };
        let style = if failed {
            Style::default().fg(theme.error)
        } else {
            Style::default().fg(theme.text)
        };
        return (
            vec![Line::styled(
                format!("{} {name}{suffix}", tr(locale, "marker.tool")),
                style,
            )],
            Vec::new(),
        );
    }

    let mut lines = vec![tool_call_line(name, args_raw, theme, locale)];
    let mut code_ranges = Vec::new();
    if let Some(result) = result {
        let (result_lines, result_ranges) =
            render_content_blocks(&result.content, theme, locale, plan, image_rows);
        code_ranges.extend(
            result_ranges
                .into_iter()
                .map(|(start, end)| (start + lines.len(), end + lines.len())),
        );
        lines.extend(result_lines);
        if result.is_error {
            let code = result
                .error
                .as_ref()
                .map(|e| e.code.as_str())
                .unwrap_or("failed");
            lines.push(Line::styled(
                trf(locale, "marker.tool_result_failed", &[code]),
                Style::default().fg(theme.error),
            ));
        }
    }
    (lines, code_ranges)
}

fn tool_call_line(name: &str, args_raw: &str, theme: &Theme, locale: Locale) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{} {name}", tr(locale, "marker.tool")),
        Style::default().fg(theme.text),
    )];
    if !args_raw.is_empty() {
        let preview = truncate_width(args_raw, ARGS_PREVIEW_MAX);
        spans.push(Span::styled(
            format!(" {preview}"),
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

/// Unicode-width-aware truncation with an ASCII ellipsis.
fn truncate_width(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w + 3 > max {
            break;
        }
        out.push(ch);
        width += w;
    }
    format!("{out}...")
}

// ---------------------------------------------------------------------------
// markdown sink
// ---------------------------------------------------------------------------

/// One level of list nesting.
struct ListState {
    ordered: bool,
    counter: u64,
}

/// Accumulating fenced/indented code block.
struct CodeBlockState {
    language: Option<String>,
    text: String,
}

/// Accumulating table (cells are span vectors).
#[derive(Default)]
struct TableState {
    header: Vec<Vec<Span<'static>>>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    current_row: Vec<Vec<Span<'static>>>,
}

/// Streaming event sink: pulldown-cmark events → styled lines.
struct Sink {
    base: Style,
    theme: Theme,
    lines: Vec<Line<'static>>,
    /// Half-open line ranges (into `lines`) that are fenced code — the
    /// `panel_bg` fill targets.
    code_ranges: Vec<(usize, usize)>,
    /// Inside a link: the accent color applies (span-level, so nested
    /// emphasis still works).
    link: bool,
    /// The line being assembled (blockquote/list markers prepended on flush).
    current: Vec<Span<'static>>,
    /// Open emphasis/strong/strikethrough/heading modifiers.
    modifiers: Vec<Modifier>,
    quote_depth: usize,
    lists: Vec<ListState>,
    in_code: Option<CodeBlockState>,
    in_cell: bool,
    table: Option<TableState>,
}

impl Sink {
    fn new(base: Style, theme: &Theme) -> Self {
        Sink {
            base,
            theme: theme.clone(),
            lines: Vec::new(),
            code_ranges: Vec::new(),
            link: false,
            current: Vec::new(),
            modifiers: Vec::new(),
            quote_depth: 0,
            lists: Vec::new(),
            in_code: None,
            in_cell: false,
            table: None,
        }
    }

    fn finish(&mut self) {
        self.flush_line();
    }

    fn push_event(&mut self, event: pulldown_cmark::Event<'_>) {
        match event {
            pulldown_cmark::Event::Start(tag) => self.start(tag),
            pulldown_cmark::Event::End(tag_end) => self.end(tag_end),
            pulldown_cmark::Event::Text(text) => {
                if let Some(code) = &mut self.in_code {
                    code.text.push_str(&text);
                } else {
                    self.push_span(Span::styled(text.to_string(), self.style()));
                }
            }
            pulldown_cmark::Event::Code(code) => {
                // #11: inline code is the theme's `code` token (violet),
                // not reversed video.
                self.push_span(Span::styled(
                    code.to_string(),
                    self.style().fg(self.theme.code),
                ));
            }
            pulldown_cmark::Event::SoftBreak => self.push_span(Span::raw(" ")),
            pulldown_cmark::Event::HardBreak => self.flush_line(),
            pulldown_cmark::Event::Rule => {
                self.flush_line();
                self.push_span(Span::raw("────────"));
                self.flush_line();
            }
            // Raw HTML and footnotes have no v1 surface; skipped.
            pulldown_cmark::Event::Html(_)
            | pulldown_cmark::Event::InlineHtml(_)
            | pulldown_cmark::Event::FootnoteReference(_)
            | pulldown_cmark::Event::InlineMath(_)
            | pulldown_cmark::Event::DisplayMath(_)
            | pulldown_cmark::Event::TaskListMarker(_) => {}
        }
    }

    fn start(&mut self, tag: pulldown_cmark::Tag<'_>) {
        match tag {
            pulldown_cmark::Tag::Paragraph => {}
            pulldown_cmark::Tag::Heading { .. } => {
                self.flush_line();
                // #11: 1 blank row before headings (skipped at the very top
                // of a message, where the blank would dangle).
                self.blank_row();
                self.modifiers.push(Modifier::BOLD);
            }
            pulldown_cmark::Tag::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth += 1;
            }
            pulldown_cmark::Tag::CodeBlock(kind) => {
                self.flush_line();
                // #11: 1 blank row before the block (the trailing one is
                // emitted at TagEnd::CodeBlock).
                self.blank_row();
                let language = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().map(str::to_string)
                    }
                    pulldown_cmark::CodeBlockKind::Indented => None,
                };
                self.in_code = Some(CodeBlockState {
                    language,
                    text: String::new(),
                });
            }
            pulldown_cmark::Tag::List(ordered) => {
                let start = ordered.unwrap_or(1);
                self.lists.push(ListState {
                    ordered: ordered.is_some(),
                    counter: start,
                });
            }
            pulldown_cmark::Tag::Item => {
                let marker = match self.lists.last() {
                    Some(list) if list.ordered => {
                        let number = list.counter;
                        format!("{number}. ")
                    }
                    _ => "• ".to_string(),
                };
                if self.lists.last().is_some_and(|list| list.ordered) {
                    self.lists.last_mut().expect("checked above").counter += 1;
                }
                self.push_span(Span::raw(marker));
            }
            pulldown_cmark::Tag::Emphasis => self.modifiers.push(Modifier::ITALIC),
            pulldown_cmark::Tag::Strong => self.modifiers.push(Modifier::BOLD),
            pulldown_cmark::Tag::Strikethrough => self.modifiers.push(Modifier::CROSSED_OUT),
            pulldown_cmark::Tag::Table(_) => {
                self.flush_line();
                self.table = Some(TableState::default());
            }
            pulldown_cmark::Tag::TableHead | pulldown_cmark::Tag::TableRow => {}
            pulldown_cmark::Tag::TableCell => {
                self.in_cell = true;
                self.current.clear();
            }
            pulldown_cmark::Tag::Link { .. } => {
                // #11: links are the accent token.
                self.link = true;
            }
            pulldown_cmark::Tag::Image { .. } => {}
            _ => {}
        }
    }

    fn end(&mut self, tag_end: pulldown_cmark::TagEnd) {
        match tag_end {
            pulldown_cmark::TagEnd::Paragraph => self.flush_line(),
            pulldown_cmark::TagEnd::Heading(_) => {
                self.modifiers.pop();
                self.flush_line();
            }
            pulldown_cmark::TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            pulldown_cmark::TagEnd::CodeBlock => {
                if let Some(code) = self.in_code.take() {
                    let start = self.lines.len();
                    self.lines.extend(code_block_lines(
                        &code.text,
                        code.language.as_deref(),
                        &self.theme,
                    ));
                    let end = self.lines.len();
                    if end > start {
                        self.code_ranges.push((start, end));
                    }
                    // #11: 1 blank row after the block.
                    self.lines.push(Line::raw(""));
                }
            }
            pulldown_cmark::TagEnd::List(_) => {
                self.lists.pop();
                self.flush_line();
            }
            pulldown_cmark::TagEnd::Item => self.flush_line(),
            pulldown_cmark::TagEnd::Emphasis
            | pulldown_cmark::TagEnd::Strong
            | pulldown_cmark::TagEnd::Strikethrough => {
                self.modifiers.pop();
            }
            pulldown_cmark::TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.render_table(table);
                }
            }
            pulldown_cmark::TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.header = std::mem::take(&mut table.current_row);
                }
            }
            pulldown_cmark::TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    table.rows.push(std::mem::take(&mut table.current_row));
                }
            }
            pulldown_cmark::TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    table.current_row.push(std::mem::take(&mut self.current));
                }
                self.in_cell = false;
            }
            pulldown_cmark::TagEnd::Link => {
                self.link = false;
            }
            pulldown_cmark::TagEnd::Image => {}
            _ => {}
        }
    }

    fn push_span(&mut self, span: Span<'static>) {
        self.current.push(span);
    }

    fn style(&self) -> Style {
        let mut style = self.base;
        if self.link {
            style = style.fg(self.theme.accent);
        }
        for modifier in &self.modifiers {
            style = style.add_modifier(*modifier);
        }
        style
    }

    /// Push one blank row — but not at the very top of the message (a
    /// dangling leading blank) and never two in a row.
    fn blank_row(&mut self) {
        let has_content = !self.lines.is_empty() || !self.current.is_empty();
        let last_is_blank = self
            .lines
            .last()
            .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()));
        if has_content && !last_is_blank {
            self.lines.push(Line::raw(""));
        }
    }

    fn flush_line(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let mut spans = Vec::with_capacity(self.current.len() + self.quote_depth);
        for _ in 0..self.quote_depth {
            spans.push(Span::raw("│ "));
        }
        spans.append(&mut self.current);
        self.lines.push(Line::from(spans));
    }

    /// Simple `|`-joined table rows with a `─` separator (no alignment math
    /// beyond per-column padding).
    fn render_table(&mut self, table: TableState) {
        let mut rows = Vec::new();
        if !table.header.is_empty() {
            rows.push(table.header);
        }
        rows.extend(table.rows);
        if rows.is_empty() {
            return;
        }
        let columns = rows.iter().map(|row| row.len()).max().unwrap_or(0);
        let mut widths = vec![0usize; columns];
        for row in &rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(cell_width(cell));
            }
        }
        for (index, row) in rows.iter().enumerate() {
            let mut spans = vec![Span::raw("| ")];
            for (column, column_width) in widths.iter().enumerate() {
                if column > 0 {
                    spans.push(Span::raw(" | "));
                }
                match row.get(column) {
                    Some(cell) => {
                        let pad = column_width.saturating_sub(cell_width(cell));
                        spans.extend(cell.iter().cloned());
                        if pad > 0 {
                            spans.push(Span::raw(" ".repeat(pad)));
                        }
                    }
                    None => spans.push(Span::raw(" ".repeat(*column_width))),
                }
            }
            spans.push(Span::raw(" |"));
            self.lines.push(Line::from(spans));
            if index == 0 {
                let mut separator = String::from("|");
                for width in &widths {
                    separator.push_str(&"─".repeat(width + 2));
                    separator.push('|');
                }
                self.lines.push(Line::raw(separator));
            }
        }
    }
}

fn cell_width(cell: &[Span<'_>]) -> usize {
    cell.iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

// ---------------------------------------------------------------------------
// code fences (#11)
// ---------------------------------------------------------------------------

/// One line per source line, all in the theme's `code` token (violet) — no
/// box, no `│` prefix, no indent: the block body carries a `panel_bg` fill
/// painted by the row cache at full content width. Per-language syntax
/// highlighting was dropped with #11 (the single violet token is the design
/// contract; `language` is kept for the signature). The style rides on the
/// SPAN, not the line: the row cache's wrap pass only carries span styles.
/// The final newline's empty split artifact is dropped.
fn code_block_lines(code: &str, _language: Option<&str>, app_theme: &Theme) -> Vec<Line<'static>> {
    let mut source: Vec<&str> = code.split('\n').collect();
    if source.last().is_some_and(|line| line.is_empty()) {
        source.pop();
    }
    source
        .into_iter()
        .map(|line| {
            Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(app_theme.code),
            ))
        })
        .collect()
}
