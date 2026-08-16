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
//!
//! OSC 8 hyperlinks (#2): the installed ratatui 0.30.2 has no
//! `Span::hyperlink` API (no HYPERLINK modifier, no style field — verified
//! in the ratatui-core 0.1.2 sources it re-exports), so link spans carry a
//! zero-width prefix — `ZWSP url ZWSP` ([`LINK_PREFIX`]) — that the chat
//! view strips at draw time and turns into raw OSC 8 sequences in the cell
//! symbols ([`crate::render::chat_view`]). Zero width keeps the wrap math
//! exact, and `Buffer::set_stringn` drops zero-width graphemes, so the
//! prefix is invisible even where a line is drawn without the injection.

use std::collections::HashMap;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::i18n::{Locale, tr, trf};
use crate::render::image::{ImageCache, ImageRow};
use crate::store::event_data::ContentBlock;
use crate::store::node::{AssistantBlock, ChatNode, NodeData, NodeKey};
use crate::theme::Theme;

/// Zero-width marker wrapping a link's URL inside its span content:
/// `ZWSP url ZWSP text`. See the module docs (OSC 8 hyperlinks, #2).
pub const LINK_PREFIX: char = '\u{200B}';

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
    /// #31: the line index (into `lines`, pre-wrap) of a detected
    /// skill-list block's header row — the click-toggle hit target. `None`
    /// when the message has no skill block.
    pub skill_header: Option<usize>,
}

/// Render one chat node to (unwrapped) display lines. `collapsed` is the
/// node's fold state (Q11): collapsed tool nodes render a one-line summary.
/// `ctx` is the render-context bag (#32): theme tokens, locale, image
/// cache, wrap width, and the #31 skill-fold map (resolved per node).
///
/// Placeholder tier: image blocks always render their `[image: name]`
/// caption (no image cache consulted). Byte-identical to the pre-image
/// pipeline output.
pub fn render_node(
    node: &ChatNode,
    collapsed: bool,
    ctx: &RenderContext<'_>,
) -> Vec<Line<'static>> {
    render_node_full(node, collapsed, ctx).lines
}

/// The render-context bag threaded through the markdown pipeline (#32):
/// the theme tokens, locale, image cache (the inline fit budget), the
/// wrap width, and the #31 skill-fold map. Built once per draw pass in
/// the app shell and passed down instead of a growing parameter list —
/// adding a render input touches the struct, not every render signature.
#[derive(Clone, Copy)]
pub struct RenderContext<'a> {
    /// The wrap width (the inline image fit budget).
    pub width: u16,
    pub theme: &'a Theme,
    pub locale: Locale,
    pub images: &'a ImageCache,
    /// #31: per-message skill-block fold state (absent = folded, the
    /// default; the row cache's render signature includes it, so a
    /// toggle re-renders that message's rows).
    pub skill_folds: &'a HashMap<NodeKey, bool>,
}

impl RenderContext<'_> {
    /// The node's skill-block fold state (absent = folded, the #31
    /// default).
    pub fn skill_fold(&self, key: &NodeKey) -> bool {
        self.skill_folds.get(key).copied().unwrap_or(true)
    }
}

/// [`render_node`] with the image pipeline wired: an image block whose
/// attachment has decoded bytes in `ctx.images` renders its caption
/// followed by `rows` blank filler lines (the widget draws over them at
/// draw time); anything else keeps the bare caption placeholder.
/// `ctx.width` is the wrap width (the inline fit budget).
pub fn render_node_full(node: &ChatNode, collapsed: bool, ctx: &RenderContext<'_>) -> NodeRender {
    let theme = ctx.theme;
    let locale = ctx.locale;
    // #31: the skill fold is per-node state — resolved here from the map
    // (absent = folded, the ticket default).
    let skill_fold = ctx.skill_fold(&node.key);
    let notice = |text: String| Line::styled(text, Style::default().fg(theme.muted));
    let mut image_rows = Vec::new();
    let mut code_ranges = Vec::new();
    let mut skill_header = None;
    let lines = match &node.data {
        NodeData::User { content, .. } => {
            // The user-message tint (#38): the theme's `user_bg` behind the
            // flat text — `None` keeps the plain fg(text) look. Style-layer
            // only; the markdown structure is identical either way.
            let user_style = Style::default()
                .fg(theme.text)
                .bg(theme.user_bg.unwrap_or_default());
            let (lines, ranges, skill) =
                render_content_blocks(content, user_style, ctx, &mut image_rows, skill_fold);
            code_ranges = ranges;
            skill_header = skill;
            lines
        }
        NodeData::Assistant {
            blocks,
            interrupted,
            ..
        } => {
            let mut lines = Vec::new();
            for block in blocks {
                let (block_lines, block_ranges, block_skill) =
                    render_assistant_block(block, ctx, skill_fold);
                code_ranges.extend(
                    block_ranges
                        .into_iter()
                        .map(|(start, end)| (start + lines.len(), end + lines.len())),
                );
                // #31: the first skill block in the message wins the fold
                // header (later ones render as plain headings).
                if skill_header.is_none() {
                    skill_header = block_skill.map(|line| line + lines.len());
                }
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
            let (lines, ranges, skill) = render_tool_node(
                call.as_ref(),
                result.as_deref(),
                collapsed,
                ctx,
                &mut image_rows,
                skill_fold,
            );
            code_ranges = ranges;
            skill_header = skill;
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
        skill_header,
    }
}

/// Render the markdown `text` with `base_style` into display lines. `ctx`
/// supplies the token colors (text/muted/code/accent) and locale. Returns
/// the lines, the half-open line ranges (into the returned vec) that are
/// fenced code (the `panel_bg` fill targets), and — #31 — the line index
/// of a detected skill-list block's header row (`None` when the message
/// has none).
///
/// A `##`/`###` heading whose text is exactly `skills` / `available
/// skills` (case-insensitive, optional trailing count like `(12)`)
/// followed by an unordered bullet list opens a skill block: folded
/// (`skill_fold`), it contributes exactly one `▸ N skills` header row;
/// expanded, the header (`▾`) plus one row per item (name bold, a
/// muted ` — description` suffix). Non-matching messages render exactly as
/// before (byte-identical).
pub fn render_markdown(
    text: &str,
    base_style: Style,
    ctx: &RenderContext<'_>,
    skill_fold: bool,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>, Option<usize>) {
    let mut options = pulldown_cmark::Options::empty();
    // ENABLE_HEADINGS / ENABLE_BOLD_ITALIC were removed in pulldown-cmark
    // 0.12+ (headings and bold/italic are always enabled); tables and
    // strikethrough are the opt-in surface (ticket 05 Q12).
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    let parser = pulldown_cmark::Parser::new_ext(text, options);
    let events: Vec<pulldown_cmark::Event<'_>> = parser.collect();
    // #31: the pre-pass finds the skill block's event range + items (both
    // passes parse the same text with the same options, so the event
    // indices line up exactly).
    let skill_block = detect_skill_block(&events);
    let mut sink = Sink::new(base_style, ctx, skill_fold);
    let mut skip_range = None;
    if let Some((start, end, _)) = &skill_block {
        skip_range = Some((*start, *end));
    }
    for (index, event) in events.into_iter().enumerate() {
        if let Some((start, end)) = skip_range {
            // #31: the header renders AT the block boundary — content
            // preceding the heading (an intro paragraph) has already been
            // flushed, so a mid-message block's header lands after it.
            if index == start {
                if let Some((_, _, items)) = &skill_block {
                    sink.begin_skill_block(items);
                }
                continue;
            }
            // Everything after the heading through the list's end is the
            // block (indices before `start` render normally).
            if index > start && index <= end {
                continue;
            }
        }
        sink.push_event(event);
    }
    sink.finish();
    (sink.lines, sink.code_ranges, sink.skill_header)
}

// ---------------------------------------------------------------------------
// node renderers
// ---------------------------------------------------------------------------

/// Render one assistant block into lines; the returned code ranges index
/// into the returned vec. (Assistant blocks have no image content, so the
/// image side of the context is not consulted here.)
fn render_assistant_block(
    block: &AssistantBlock,
    ctx: &RenderContext<'_>,
    skill_fold: bool,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>, Option<usize>) {
    match block {
        AssistantBlock::Text { text } => {
            render_markdown(text, Style::default().fg(ctx.theme.text), ctx, skill_fold)
        }
        AssistantBlock::Reasoning { text } => render_markdown(
            text,
            Style::default()
                .fg(ctx.theme.muted)
                .add_modifier(Modifier::DIM),
            ctx,
            skill_fold,
        ),
        AssistantBlock::ToolCall { name, .. } => (
            vec![tool_call_line(name, ctx.theme, ctx.locale)],
            Vec::new(),
            None,
        ),
    }
}

/// Render content blocks into lines; the returned code ranges index into
/// the returned vec, and the skill-header line index (of the FIRST skill
/// block, if any) is relative to the returned lines. `text_style` styles
/// text blocks (the user-message tint path passes the bg-carrying style).
fn render_content_blocks(
    content: &[ContentBlock],
    text_style: Style,
    ctx: &RenderContext<'_>,
    image_rows: &mut Vec<ImageRow>,
    skill_fold: bool,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>, Option<usize>) {
    let mut lines = Vec::new();
    let mut code_ranges = Vec::new();
    let mut skill_header = None;
    for block in content {
        let (block_lines, block_ranges, block_skill) = match block {
            ContentBlock::Text { text } => render_markdown(text, text_style, ctx, skill_fold),
            ContentBlock::Reasoning { text } => render_markdown(
                text,
                Style::default()
                    .fg(ctx.theme.muted)
                    .add_modifier(Modifier::DIM),
                ctx,
                skill_fold,
            ),
            ContentBlock::Image { attachment } => {
                let name = attachment
                    .name
                    .as_deref()
                    .unwrap_or_else(|| tr(ctx.locale, "marker.image_default"));
                // The caption is the placeholder tier AND the inline image's
                // label — always emitted, byte-identical either way.
                let mut caption_lines = vec![Line::styled(
                    trf(ctx.locale, "marker.image", &[name]),
                    Style::default().fg(ctx.theme.muted),
                )];
                // Inline tier: decoded bytes in the cache → reserve `rows`
                // blank filler lines below the caption; the draw loop paints
                // the image widget over them. v1's cache is always empty
                // (no session.attachment fetch — see render::image docs).
                if let Some(loaded) = ctx.images.get(&attachment.attachment_id) {
                    let rows = ImageCache::inline_rows(&loaded.source, ctx.width);
                    image_rows.push(ImageRow {
                        line_index: lines.len() + caption_lines.len(),
                        attachment_id: attachment.attachment_id.clone(),
                        rows,
                    });
                    caption_lines.extend((0..rows).map(|_| Line::raw("")));
                }
                (caption_lines, Vec::new(), None)
            }
            ContentBlock::ToolCall { name, .. } => (
                vec![Line::styled(
                    format!("{} {name}", tr(ctx.locale, "marker.tool")),
                    Style::default().fg(ctx.theme.text),
                )],
                Vec::new(),
                None,
            ),
            ContentBlock::ToolResult { is_error, .. } => {
                let suffix = if *is_error == Some(true) {
                    tr(ctx.locale, "marker.failed_suffix")
                } else {
                    ""
                };
                (
                    vec![Line::styled(
                        format!("{}{suffix}", tr(ctx.locale, "marker.tool_result")),
                        Style::default().fg(ctx.theme.muted),
                    )],
                    Vec::new(),
                    None,
                )
            }
            ContentBlock::Raw(value) => {
                let block_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                (
                    vec![Line::styled(
                        trf(ctx.locale, "marker.block", &[block_type]),
                        Style::default().fg(ctx.theme.muted),
                    )],
                    Vec::new(),
                    None,
                )
            }
        };
        code_ranges.extend(
            block_ranges
                .into_iter()
                .map(|(start, end)| (start + lines.len(), end + lines.len())),
        );
        // #31: the first skill block in the message wins the fold header.
        if skill_header.is_none() {
            skill_header = block_skill.map(|line| line + lines.len());
        }
        lines.extend(block_lines);
    }
    (lines, code_ranges, skill_header)
}

fn render_tool_node(
    call: Option<&crate::store::node::RunningToolCall>,
    result: Option<&crate::store::node::ToolResultNode>,
    collapsed: bool,
    ctx: &RenderContext<'_>,
    image_rows: &mut Vec<ImageRow>,
    skill_fold: bool,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>, Option<usize>) {
    let theme = ctx.theme;
    let locale = ctx.locale;
    let name = call
        .map(|c| c.name.as_str())
        .or_else(|| result.and_then(|r| r.call.as_ref().map(|c| c.name.as_str())))
        .unwrap_or("tool");

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
            None,
        );
    }

    let mut lines = vec![tool_call_line(name, theme, locale)];
    let mut code_ranges = Vec::new();
    let mut skill_header = None;
    if let Some(result) = result {
        // The tool OUTPUT is literal: a code block (panel-bg fill, syntax
        // highlight for known tool languages) — never markdown, so bash
        // output with `*`, backticks, or [brackets] stays byte-identical.
        // Exotic results (image/reasoning blocks) keep the markdown path.
        let all_text = !result.content.is_empty()
            && result
                .content
                .iter()
                .all(|b| matches!(b, ContentBlock::Text { .. }));
        if all_text {
            let mut text = String::new();
            for block in &result.content {
                if let ContentBlock::Text { text: part } = block {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(part);
                }
            }
            let start = lines.len();
            lines.extend(code_block_lines(&text, tool_language(name), theme));
            let end = lines.len();
            if end > start {
                code_ranges.push((start, end));
                // #11: 1 blank row after the block.
                lines.push(Line::raw(""));
            }
        } else if !result.content.is_empty() {
            let (result_lines, result_ranges, result_skill) = render_content_blocks(
                &result.content,
                Style::default().fg(theme.text),
                ctx,
                image_rows,
                skill_fold,
            );
            code_ranges.extend(
                result_ranges
                    .into_iter()
                    .map(|(start, end)| (start + lines.len(), end + lines.len())),
            );
            skill_header = result_skill.map(|line| line + lines.len());
            lines.extend(result_lines);
        }
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
    (lines, code_ranges, skill_header)
}

/// The syntect language for a tool's output. Known tools map to a grammar
/// (bash → shell); everything else renders plain — the `code` token with
/// the panel-bg fill, exactly like an unfenced block.
fn tool_language(name: &str) -> Option<&'static str> {
    match name {
        "bash" | "sh" | "shell" => Some("bash"),
        _ => None,
    }
}

fn tool_call_line(name: &str, theme: &Theme, locale: Locale) -> Line<'static> {
    Line::styled(
        format!("{} {name}", tr(locale, "marker.tool")),
        Style::default().fg(theme.text),
    )
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

// ---------------------------------------------------------------------------
// skill-list blocks (#31)
// ---------------------------------------------------------------------------

/// One parsed skill-list item: the leading name token + the description
/// (nested bullets append to it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillItem {
    pub name: String,
    pub description: String,
}

/// Detect a skill-list block in the parsed events: a `##`/`###` heading
/// whose text is exactly `skills` / `available skills` (case-insensitive,
/// optional trailing count like `(12)`) immediately followed by an
/// unordered bullet list. Returns the heading's event index, the list's
/// end event index, and the parsed items. Nested (indented) bullets are
/// description continuations of the preceding item. The event stream is
/// identical to the render pass's, so the indices line up.
fn detect_skill_block(
    events: &[pulldown_cmark::Event<'_>],
) -> Option<(usize, usize, Vec<SkillItem>)> {
    let mut index = 0;
    while index < events.len() {
        if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::Heading {
            level: pulldown_cmark::HeadingLevel::H2 | pulldown_cmark::HeadingLevel::H3,
            ..
        }) = &events[index]
        {
            // Collect the heading text (until End(Heading)).
            let mut heading = String::new();
            let mut j = index + 1;
            while j < events.len() {
                match &events[j] {
                    pulldown_cmark::Event::Text(text) => heading.push_str(text),
                    pulldown_cmark::Event::SoftBreak => heading.push(' '),
                    pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Heading(_)) => break,
                    _ => {
                        heading.clear();
                        break;
                    }
                }
                j += 1;
            }
            if !is_skills_heading(&heading) {
                index += 1;
                continue;
            }
            // The list must follow immediately (unordered only).
            if let Some(pulldown_cmark::Event::Start(pulldown_cmark::Tag::List(None))) =
                events.get(j + 1)
            {
                // Walk the list: depth counts nested lists; a top-level
                // item's text is its name+description, a nested item's
                // text appends to the previous top-level item.
                let mut items: Vec<SkillItem> = Vec::new();
                let mut depth = 0usize;
                let mut top_text = String::new();
                let mut sub_text = String::new();
                let mut in_sub = false;
                let mut k = j + 1;
                while k < events.len() {
                    match &events[k] {
                        pulldown_cmark::Event::Start(pulldown_cmark::Tag::List(_)) => {
                            depth += 1;
                            in_sub = depth >= 2;
                        }
                        pulldown_cmark::Event::End(pulldown_cmark::TagEnd::List(_)) => {
                            depth -= 1;
                            in_sub = depth >= 2;
                            if depth == 0 {
                                break;
                            }
                        }
                        pulldown_cmark::Event::Text(text) => {
                            if in_sub {
                                sub_text.push_str(text);
                            } else {
                                top_text.push_str(text);
                            }
                        }
                        pulldown_cmark::Event::Code(text) => {
                            if in_sub {
                                sub_text.push_str(text);
                            } else {
                                top_text.push_str(text);
                            }
                        }
                        pulldown_cmark::Event::SoftBreak => {
                            if in_sub {
                                sub_text.push(' ');
                            } else {
                                top_text.push(' ');
                            }
                        }
                        pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Item) => {
                            if in_sub {
                                // Nested continuation: the parent item's
                                // own text arrives first (markdown order),
                                // so flush it, then append the
                                // continuation to it.
                                if !top_text.trim().is_empty() {
                                    if let Some((name, description)) =
                                        parse_skill_item(top_text.trim())
                                    {
                                        items.push(SkillItem { name, description });
                                    }
                                    top_text.clear();
                                }
                                let text = sub_text.trim().to_string();
                                sub_text.clear();
                                if !text.is_empty()
                                    && let Some(last) = items.last_mut()
                                {
                                    if last.description.is_empty() {
                                        last.description = text;
                                    } else {
                                        last.description.push(' ');
                                        last.description.push_str(&text);
                                    }
                                }
                            } else {
                                let text = top_text.trim().to_string();
                                top_text.clear();
                                if let Some((name, description)) = parse_skill_item(&text) {
                                    items.push(SkillItem { name, description });
                                }
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                if !items.is_empty() {
                    return Some((index, k, items));
                }
            }
            index = j + 1;
        } else {
            index += 1;
        }
    }
    None
}

/// The heading matches the skill-list pattern: trimmed, lower-cased, an
/// optional trailing `(count)` stripped, exactly `skills` or
/// `available skills`.
fn is_skills_heading(text: &str) -> bool {
    let mut heading = text.trim().to_lowercase();
    if let Some(open) = heading.rfind('(')
        && heading.ends_with(')')
        && heading[open + 1..heading.len() - 1]
            .chars()
            .all(|c| c.is_ascii_digit())
    {
        heading.truncate(open);
    }
    let heading = heading.trim();
    heading == "skills" || heading == "available skills"
}

/// Split one item into (name, description): the first ` — ` / ` - ` /
/// `：` / `: ` separator splits; no separator → the whole item is the
/// name. Empty items (blank bullets) are dropped.
fn parse_skill_item(text: &str) -> Option<(String, String)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    for separator in [" — ", " - ", "：", ": "] {
        if let Some(position) = text.find(separator) {
            let name = text[..position].trim();
            if name.is_empty() {
                return None;
            }
            let description = text[position + separator.len()..].trim().to_string();
            return Some((name.to_string(), description));
        }
    }
    Some((text.to_string(), String::new()))
}

/// Streaming event sink: pulldown-cmark events → styled lines.
struct Sink {
    base: Style,
    theme: Theme,
    locale: Locale,
    /// #31: the fold flag for a detected skill block (true = folded).
    skill_fold: bool,
    /// #31: the output line index of the skill block's header row, set by
    /// [`Sink::begin_skill_block`].
    skill_header: Option<usize>,
    lines: Vec<Line<'static>>,
    /// Half-open line ranges (into `lines`) that are fenced code — the
    /// `panel_bg` fill targets.
    code_ranges: Vec<(usize, usize)>,
    /// Inside a link: the destination URL (accent color applies; non-empty
    /// URLs additionally wrap pushed spans in the OSC 8 prefix — see the
    /// module docs, #2).
    link: Option<String>,
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
    fn new(base: Style, ctx: &RenderContext<'_>, skill_fold: bool) -> Self {
        Sink {
            base,
            theme: ctx.theme.clone(),
            locale: ctx.locale,
            skill_fold,
            skill_header: None,
            lines: Vec::new(),
            code_ranges: Vec::new(),
            link: None,
            current: Vec::new(),
            modifiers: Vec::new(),
            quote_depth: 0,
            lists: Vec::new(),
            in_code: None,
            in_cell: false,
            table: None,
        }
    }

    /// #31: render the skill block's header row (and, expanded, the item
    /// rows) in place of the heading + list the events would have
    /// produced. The header carries the same heading-like spacing (one
    /// blank row before, guarded against the top of the message).
    fn begin_skill_block(&mut self, items: &[SkillItem]) {
        self.blank_row();
        self.skill_header = Some(self.lines.len());
        let glyph = if self.skill_fold { "▸" } else { "▾" };
        let mut spans = vec![Span::styled(
            format!("{glyph} "),
            Style::new()
                .add_modifier(Modifier::BOLD)
                .fg(self.theme.accent),
        )];
        spans.push(Span::styled(
            trf(self.locale, "skill.count", &[&items.len().to_string()]),
            Style::default().fg(self.theme.text),
        ));
        self.lines.push(Line::from(spans));
        if !self.skill_fold {
            for item in items {
                let mut item_spans = vec![Span::styled(
                    item.name.clone(),
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(self.theme.text),
                )];
                if !item.description.is_empty() {
                    item_spans.push(Span::styled(
                        format!(" — {}", item.description),
                        Style::default().fg(self.theme.muted),
                    ));
                }
                self.lines.push(Line::from(item_spans));
            }
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
            pulldown_cmark::Tag::Link { dest_url, .. } => {
                // #11: links are the accent token; #2: the URL rides the
                // span content as the OSC 8 prefix (see [`LINK_PREFIX`]).
                self.link = Some(dest_url.to_string());
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
                self.link = None;
            }
            pulldown_cmark::TagEnd::Image => {}
            _ => {}
        }
    }

    fn push_span(&mut self, span: Span<'static>) {
        // #2: inside a link, wrap the span in the OSC 8 prefix so the chat
        // view can emit the hyperlink around it. Empty URLs keep the plain
        // accent styling (no prefix — rendering unchanged).
        let span = match self.link.as_deref() {
            Some(url) if !url.is_empty() => Span::styled(
                format!("{LINK_PREFIX}{url}{LINK_PREFIX}{}", span.content),
                span.style,
            ),
            _ => span,
        };
        self.current.push(span);
    }

    fn style(&self) -> Style {
        let mut style = self.base;
        if self.link.is_some() {
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

/// One line per source line. Known languages (#5) highlight through the
/// scope classifier (`src/render/highlight.rs`) into the theme tokens,
/// keeping the `panel_bg` fill painted by the row cache; unknown or absent
/// languages keep the single all-`code` span (violet, the #11 design
/// contract) — byte-identical to the pre-#5 rendering. The style rides on
/// the SPAN, not the line: the row cache's wrap pass only carries span
/// styles. The final newline's empty split artifact is dropped.
fn code_block_lines(code: &str, language: Option<&str>, app_theme: &Theme) -> Vec<Line<'static>> {
    if let Some(lines) =
        language.and_then(|lang| crate::render::highlight::highlight_code(code, lang, app_theme))
    {
        return lines;
    }
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
