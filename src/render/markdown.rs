//! Streaming markdown pipeline (ticket 05 Q5/Q12).
//!
//! [`render_node`] renders one chat node into unstyled-then-styled `Line`s,
//! re-parsing the node's accumulated text on every call — cheap for bounded
//! rows; caching happens at the [`crate::render::row_cache::RowCache`] level
//! (idle nodes cached, dirty nodes re-parsed).
//!
//! Markdown surface: CommonMark + tables + strikethrough via pulldown-cmark,
//! syntect-highlighted code fences, `[image]` placeholders (ratatui-image
//! lands in a later lane). Semantic styling only — no hardcoded colors:
//! dim/bold/italic/crossed-out/reversed modifiers; colors come from the theme
//! registry lane later. Code fences are the one sanctioned color source
//! (syntect's fixed default theme).

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

use crate::store::event_data::ContentBlock;
use crate::store::node::{AssistantBlock, ChatNode, NodeData, UserNodeKind};

/// Inline code uses reversed video for background emphasis (no colors).
const CODE_MODIFIER: Modifier = Modifier::REVERSED;
/// Maximum width of a tool-arguments preview on the call line.
const ARGS_PREVIEW_MAX: usize = 100;

/// Render one chat node to (unwrapped) display lines. `collapsed` is the
/// node's fold state (Q11): collapsed tool nodes render a one-line summary.
pub fn render_node(node: &ChatNode, collapsed: bool) -> Vec<Line<'static>> {
    match &node.data {
        NodeData::User { kind, content, .. } => render_user_node(*kind, content),
        NodeData::Assistant {
            blocks,
            interrupted,
            ..
        } => {
            let mut lines = render_assistant_blocks(blocks);
            if *interrupted {
                lines.push(Line::styled(
                    "[interrupted]",
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }
            lines
        }
        NodeData::Tool { call, result, .. } => {
            render_tool_node(call.as_ref(), result.as_deref(), collapsed)
        }
        NodeData::Compaction {
            shadowed_item_count,
            ..
        } => {
            let count = shadowed_item_count.unwrap_or(0);
            vec![Line::raw(format!("[compacted {count} messages]"))]
        }
        NodeData::TurnError { code, .. } => {
            let code = code.as_deref().unwrap_or("unknown");
            vec![Line::raw(format!("[turn error: {code}]"))]
        }
        NodeData::TurnMaxTokens { .. } => vec![Line::raw("[max tokens]")],
        NodeData::Unknown { r#type, .. } => vec![Line::raw(format!("[unknown: {type}]"))],
    }
}

/// Render the markdown `text` with `base_style` into display lines.
pub fn render_markdown(text: &str, base_style: Style) -> Vec<Line<'static>> {
    let mut options = pulldown_cmark::Options::empty();
    // ENABLE_HEADINGS / ENABLE_BOLD_ITALIC were removed in pulldown-cmark
    // 0.12+ (headings and bold/italic are always enabled); tables and
    // strikethrough are the opt-in surface (ticket 05 Q12).
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    let parser = pulldown_cmark::Parser::new_ext(text, options);
    let mut sink = Sink::new(base_style);
    for event in parser {
        sink.push_event(event);
    }
    sink.finish()
}

// ---------------------------------------------------------------------------
// node renderers
// ---------------------------------------------------------------------------

fn render_user_node(kind: UserNodeKind, content: &[ContentBlock]) -> Vec<Line<'static>> {
    // The Steering distinction is a store TODO (v1 renders by source kind);
    // user and context rows render their content identically.
    let _ = kind;
    render_content_blocks(content)
}

fn render_assistant_blocks(blocks: &[AssistantBlock]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for block in blocks {
        match block {
            AssistantBlock::Text { text } => lines.extend(render_markdown(text, Style::default())),
            AssistantBlock::Reasoning { text } => lines.extend(render_markdown(
                text,
                Style::default().add_modifier(Modifier::DIM),
            )),
            AssistantBlock::ToolCall { name, args_raw, .. } => {
                lines.push(tool_call_line(name, args_raw));
            }
        }
    }
    lines
}

fn render_content_blocks(content: &[ContentBlock]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text { text } => lines.extend(render_markdown(text, Style::default())),
            ContentBlock::Reasoning { text } => lines.extend(render_markdown(
                text,
                Style::default().add_modifier(Modifier::DIM),
            )),
            ContentBlock::Image { attachment } => {
                let name = attachment.name.as_deref().unwrap_or("image");
                lines.push(Line::raw(format!("[image: {name}]")));
            }
            ContentBlock::ToolCall { name, .. } => {
                lines.push(Line::raw(format!("[tool] {name}")));
            }
            ContentBlock::ToolResult { is_error, .. } => {
                let suffix = if *is_error == Some(true) {
                    " failed"
                } else {
                    ""
                };
                lines.push(Line::raw(format!("[tool-result]{suffix}")));
            }
            ContentBlock::Raw(value) => {
                let block_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                lines.push(Line::raw(format!("[block: {block_type}]")));
            }
        }
    }
    lines
}

fn render_tool_node(
    call: Option<&crate::store::node::RunningToolCall>,
    result: Option<&crate::store::node::ToolResultNode>,
    collapsed: bool,
) -> Vec<Line<'static>> {
    let name = call
        .map(|c| c.name.as_str())
        .or_else(|| result.and_then(|r| r.call.as_ref().map(|c| c.name.as_str())))
        .unwrap_or("tool");
    let args_raw = call.map(|c| c.args_raw.as_str()).unwrap_or_default();

    if collapsed {
        // One-line summary (lifecycle icon + title, Q11).
        let suffix = if result.is_some_and(|r| r.is_error) {
            " failed"
        } else {
            ""
        };
        return vec![Line::raw(format!("[tool] {name}{suffix}"))];
    }

    let mut lines = vec![tool_call_line(name, args_raw)];
    if let Some(result) = result {
        lines.extend(render_content_blocks(&result.content));
        if result.is_error {
            let code = result
                .error
                .as_ref()
                .map(|e| e.code.as_str())
                .unwrap_or("failed");
            lines.push(Line::raw(format!("[tool-result] failed: {code}")));
        }
    }
    lines
}

fn tool_call_line(name: &str, args_raw: &str) -> Line<'static> {
    let mut spans = vec![Span::raw(format!("[tool] {name}"))];
    if !args_raw.is_empty() {
        let preview = truncate_width(args_raw, ARGS_PREVIEW_MAX);
        spans.push(Span::styled(
            format!(" {preview}"),
            Style::default().add_modifier(Modifier::DIM),
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
    lines: Vec<Line<'static>>,
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
    fn new(base: Style) -> Self {
        Sink {
            base,
            lines: Vec::new(),
            current: Vec::new(),
            modifiers: Vec::new(),
            quote_depth: 0,
            lists: Vec::new(),
            in_code: None,
            in_cell: false,
            table: None,
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        self.lines
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
                self.push_span(Span::styled(
                    code.to_string(),
                    self.style().add_modifier(CODE_MODIFIER),
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
                self.modifiers.push(Modifier::BOLD);
            }
            pulldown_cmark::Tag::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth += 1;
            }
            pulldown_cmark::Tag::CodeBlock(kind) => {
                self.flush_line();
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
            pulldown_cmark::Tag::Link { .. } | pulldown_cmark::Tag::Image { .. } => {}
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
                    self.lines
                        .extend(highlight_code(&code.text, code.language.as_deref()));
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
            pulldown_cmark::TagEnd::Link | pulldown_cmark::TagEnd::Image => {}
            _ => {}
        }
    }

    fn push_span(&mut self, span: Span<'static>) {
        self.current.push(span);
    }

    fn style(&self) -> Style {
        let mut style = self.base;
        for modifier in &self.modifiers {
            style = style.add_modifier(*modifier);
        }
        style
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
// code fences (syntect)
// ---------------------------------------------------------------------------

/// Syntax set and theme set are process-wide and expensive; load once.
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    THEMES.get_or_init(ThemeSet::load_defaults)
}

/// Highlight one code block; one line per source line, `│ `-prefixed.
fn highlight_code(code: &str, language: Option<&str>) -> Vec<Line<'static>> {
    let syntax = language
        .and_then(|lang| {
            syntaxes()
                .find_syntax_by_name(lang)
                .or_else(|| syntaxes().find_syntax_by_extension(lang))
        })
        .unwrap_or_else(|| syntaxes().find_syntax_plain_text());
    let theme = themes().themes.get("base16-ocean.dark").unwrap_or_else(|| {
        themes()
            .themes
            .values()
            .next()
            .expect("default themes non-empty")
    });
    let mut highlighter = HighlightLines::new(syntax, theme);
    code.split('\n')
        .map(|line| {
            let ranges = highlighter
                .highlight_line(line, syntaxes())
                .unwrap_or_default();
            let mut spans = vec![Span::raw("│ ")];
            for (syntect_style, text) in ranges {
                spans.push(Span::styled(
                    text.to_string(),
                    syntect_style_to_ratatui(&syntect_style),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

/// Map a syntect style to a ratatui style. This is the one sanctioned color
/// source in the renderer (the fixed default theme).
fn syntect_style_to_ratatui(style: &syntect::highlighting::Style) -> Style {
    let mut ratatui_style = Style::default();
    if style.foreground.a != 0 {
        let fg = style.foreground;
        ratatui_style = ratatui_style.fg(Color::Rgb(fg.r, fg.g, fg.b));
    }
    if style.font_style.contains(FontStyle::BOLD) {
        ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED);
    }
    ratatui_style
}
