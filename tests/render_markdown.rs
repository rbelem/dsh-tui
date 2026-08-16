//! Coverage-push render tests: the markdown surface (headings, quotes,
//! lists, code, tables, links, breaks), the node renderers for every
//! NodeData/ContentBlock variant (interrupted, turn-error, max-tokens,
//! raw, tool errors, truncation), the fenced-code path, the
//! row-cache signature hash arms, and the image viewer's loaded-image and
//! meta arms. Direct construction where the types are public; the store
//! path where the row cache needs real nodes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Modifier, Style};
use serde_json::json;

use dsh_tui::i18n::Locale;
use dsh_tui::render::markdown::{render_node, render_node_full};
use dsh_tui::render::{ImageCache, ImageProtocol, RowCache, render_markdown};
use dsh_tui::store::event_data::ContentBlock;
use dsh_tui::store::node::{AssistantBlock, ChatNode, ChatNodeKind, NodeData};
use dsh_tui::store::{SessionStore, SessionStore as Store};
use dsh_tui::theme::Theme;
use dsh_tui::ui::image_viewer::ImageViewer;
use dsh_tui::ui::takeover::Mode;
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{
    ImageAttachmentRef, ImageMediaType, SessionEvent, SessionId, SessionSummary,
};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn ev(seq: i64, r#type: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        r#type: r#type.into(),
        seq,
        time: seq as f64,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn frame(session: &str, event: SessionEvent) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: None,
    }
}

fn user_msg(id: &str, text: &str) -> serde_json::Value {
    json!({"id": id, "role": "user", "content": [{"type": "text", "text": text}], "source": {"kind": "user"}})
}

/// #32: the render-context bag for a test render. Folds always empty —
/// skill-fold behavior is covered by the dedicated markdown tests below.
fn render_ctx<'a>(
    width: u16,
    theme: &'a Theme,
    locale: Locale,
    images: &'a ImageCache,
    folds: &'a std::collections::HashMap<dsh_tui::store::node::NodeKey, bool>,
) -> dsh_tui::render::markdown::RenderContext<'a> {
    dsh_tui::render::markdown::RenderContext {
        width,
        theme,
        locale,
        images,
        skill_folds: folds,
    }
}

fn chunk(turn: i64, step: i64, data: serde_json::Value) -> serde_json::Value {
    json!({"turn": turn, "step": step, "chunk": data})
}

fn node(key: &str, anchor_seq: i64, data: NodeData) -> ChatNode {
    ChatNode {
        key: key.into(),
        kind: ChatNodeKind::Unknown, // kind is derived; renderers read `data`
        anchor_seq,
        data,
    }
}

fn text_node(key: &str, text: &str) -> ChatNode {
    node(
        key,
        1,
        NodeData::User {
            kind: dsh_tui::store::node::UserNodeKind::User,
            message_id: key.into(),
            content: vec![ContentBlock::Text { text: text.into() }],
            source: json!({"kind": "user"}),
        },
    )
}

// ---------------------------------------------------------------------------
// 1. markdown surface
// ---------------------------------------------------------------------------

#[test]
fn markdown_surface_covers_every_sink_branch() {
    let theme = Theme::default();
    let md = concat!(
        "# Heading\n", // heading
        "\n",
        "> quoted line\n", // blockquote
        "\n",
        "3. three\n", // ordered list with an explicit start
        "4. four\n",
        "\n",
        "- bullet\n", // unordered list
        "\n",
        "*emph* **strong** `code`\n", // emphasis/strong/inline code
        "\n",
        "soft\nbreak\n",   // soft break
        "hard  \nbreak\n", // hard break
        "\n",
        "---\n", // rule
        "\n",
        "<div>raw html</div>\n", // html (skipped)
        "\n",
        "- [ ] todo\n", // task list marker (skipped)
        "\n",
        "[link](https://x) ![img](y)\n", // link + image (skipped)
        "\n",
        "    indented code\n", // indented code block (unfenced)
        "\n",
        "| head | short |\n|---|---|\n| long-cell | x |\n| missing |\n", // table
    );
    let (lines, _code, _skill) = render_markdown(
        md,
        Style::default().fg(theme.text),
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        true,
    );
    let rendered: Vec<String> = lines.iter().map(ToString::to_string).collect();

    // Heading is bold; quote lines carry the │ prefix.
    assert!(
        rendered.iter().any(|l| l.contains("Heading")),
        "{rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .any(|l| l.contains("│ ") && l.contains("quoted line")),
        "quote prefix: {rendered:?}"
    );
    // Ordered list numbers come from the explicit start.
    assert!(
        rendered.iter().any(|l| l.starts_with("3. ")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|l| l.starts_with("4. ")),
        "{rendered:?}"
    );
    // Unordered bullets.
    assert!(
        rendered.iter().any(|l| l.contains("• bullet")),
        "{rendered:?}"
    );
    // Emphasis/strong/code modifiers.
    let emph = lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "emph"));
    assert!(emph.is_some_and(|l| {
        l.spans
            .iter()
            .any(|s| s.style.has_modifier(Modifier::ITALIC))
    }));
    let strong = lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "strong"));
    assert!(strong.is_some_and(|l| l.spans.iter().any(|s| s.style.has_modifier(Modifier::BOLD))));
    let code = lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "code"));
    assert!(
        code.is_some_and(|l| l.spans.iter().any(|s| s.style.fg == Some(theme.code))),
        "#11: inline code is the theme's code token (REVERSED is gone)"
    );
    // Soft break joins with a space; hard break splits lines.
    assert!(
        rendered.iter().any(|l| l.starts_with("soft break")),
        "{rendered:?}"
    );
    assert!(rendered.iter().any(|l| l == "break"), "{rendered:?}");
    assert!(rendered.iter().any(|l| l == "break"), "{rendered:?}");
    // Rule.
    assert!(rendered.iter().any(|l| l.contains("────")), "{rendered:?}");
    // Indented code renders as a plain-code fence line.
    assert!(
        rendered.iter().any(|l| l.contains("indented code")),
        "{rendered:?}"
    );
    // Table with padding and a missing trailing cell.
    assert!(
        rendered.iter().any(|l| l.contains("| head")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|l| l.contains("long-cell")),
        "{rendered:?}"
    );
}

#[test]
fn links_carry_the_osc8_prefix_and_empty_urls_stay_plain() {
    let theme = Theme::default();
    let (lines, _, _) = render_markdown(
        "see [docs](https://example.com) and [plain]()",
        Style::default().fg(theme.text),
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        true,
    );
    // #2: the link span carries the `ZWSP url ZWSP` OSC 8 prefix — zero
    // width, so wrap math and layout are untouched.
    let link = lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content.as_ref().ends_with("docs")));
    let link_span = link
        .expect("link span")
        .spans
        .iter()
        .find(|s| s.content.as_ref().ends_with("docs"))
        .expect("span");
    assert_eq!(
        link_span.content.as_ref(),
        "\u{200B}https://example.com\u{200B}docs"
    );
    assert_eq!(link_span.style.fg, Some(theme.accent));
    // An empty URL keeps the accent styling but adds no prefix.
    let plain = lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "plain"));
    let plain_span = plain
        .expect("empty-url link")
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "plain")
        .expect("span");
    assert_eq!(plain_span.content.as_ref(), "plain");
    assert_eq!(plain_span.style.fg, Some(theme.accent));
}

#[test]
fn fenced_code_uses_the_code_token_with_panel_fill_range() {
    let theme = Theme::default();
    // An unknown language: the block renders exactly as before #5 — the
    // single all-`code` span (the known-language path is covered by the
    // highlighting golden tests below).
    let (lines, code_ranges, _skill) = render_markdown(
        "before\n```notalang\nfn main() { println!(\"hi\"); }\n```\nafter",
        Style::default().fg(theme.text),
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        true,
    );
    let rendered: Vec<String> = lines.iter().map(ToString::to_string).collect();
    // #11: no box, no `│` indent — the code lines are bare.
    assert!(
        rendered.iter().any(|l| l.contains("fn main()")),
        "code body rendered: {rendered:?}"
    );
    assert!(
        !rendered.iter().any(|l| l.starts_with("│ ")),
        "no indent prefix: {rendered:?}"
    );
    // The body is the theme's code token; the row cache gets the range.
    assert_eq!(
        code_ranges,
        vec![(2, 3)],
        "code lines: index of `fn main...` + the 1-blank-row spacing"
    );
    let code_span = lines[2]
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "fn main() { println!(\"hi\"); }")
        .expect("code span");
    assert_eq!(code_span.style.fg, Some(theme.code), "code token color");
    // 1 blank row around the block.
    assert!(rendered[1].trim().is_empty(), "blank before: {rendered:?}");
    assert!(rendered[3].trim().is_empty(), "blank after: {rendered:?}");
    assert_eq!(rendered[4], "after", "content follows: {rendered:?}");
}

// ---------------------------------------------------------------------------
// #5: syntax highlighting in fenced code
// ---------------------------------------------------------------------------

/// A concrete palette theme with distinct colors per token, so highlighted
/// spans are distinguishable from each other and from the fallback.
fn highlight_theme() -> Theme {
    Theme::from_toml_str(
        r##"
name = "highlight-golden"
accent = "#111111"
muted = "#222222"
error = "#333333"
warning = "#444444"
success = "#555555"
code = "#666666"
bg = "#777777"
text = "#888888"
panel_bg = "#999999"
border = "#aaaaaa"
"##,
    )
    .expect("valid theme")
}

/// The fenced-body lines of a rendered markdown string (pre-wrap indices).
fn fenced_lines(md: &str, theme: &Theme) -> Vec<Vec<ratatui::text::Span<'static>>> {
    let (lines, code_ranges, _skill) = render_markdown(
        md,
        Style::default().fg(theme.text),
        &render_ctx(
            200,
            theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        true,
    );
    assert_eq!(code_ranges.len(), 1, "one fence expected");
    let (start, end) = code_ranges[0];
    lines[start..end]
        .iter()
        .map(|line| line.spans.clone())
        .collect()
}

#[test]
fn fenced_code_highlights_known_languages_via_theme_tokens() {
    let theme = highlight_theme();
    // rust: keyword-ish storage types bold-text, function names code,
    // strings success, operators accent, numbers warning.
    let rust = fenced_lines(
        "```rust\nfn main() { let n = 42; s!(\"hi\"); }\n```",
        &theme,
    );
    let rust_spans = rust[0].clone();
    let span = |needle: &str| {
        rust_spans
            .iter()
            .find(|s| s.content.as_ref() == needle)
            .unwrap_or_else(|| panic!("missing rust span {needle:?}: {rust_spans:?}"))
    };
    assert_eq!(span("fn").style.fg, Some(theme.text), "storage.type → text");
    assert!(
        span("fn").style.add_modifier.contains(Modifier::BOLD),
        "storage.type → bold"
    );
    assert_eq!(span("main").style.fg, Some(theme.code), "function → code");
    assert_eq!(span("42").style.fg, Some(theme.warning), "number → warning");
    assert_eq!(span("hi").style.fg, Some(theme.success), "string → success");
    // bash: support.function → code, comment → muted, string → success.
    let bash = fenced_lines("```bash\necho \"hi\" # done\n```", &theme);
    let bash_spans = bash[0].clone();
    let span = |needle: &str| {
        bash_spans
            .iter()
            .find(|s| s.content.as_ref() == needle)
            .unwrap_or_else(|| panic!("missing bash span {needle:?}: {bash_spans:?}"))
    };
    assert_eq!(span("echo").style.fg, Some(theme.code), "builtin → code");
    assert_eq!(span("#").style.fg, Some(theme.muted), "comment → muted");
    assert_eq!(span("hi").style.fg, Some(theme.success), "string → success");
    // json: keys → success, numbers/booleans → warning.
    let json = fenced_lines("```json\n{\"a\": 1, \"b\": true}\n```", &theme);
    let json_spans = json[0].clone();
    let span = |needle: &str| {
        json_spans
            .iter()
            .find(|s| s.content.as_ref() == needle)
            .unwrap_or_else(|| panic!("missing json span {needle:?}: {json_spans:?}"))
    };
    assert_eq!(span("a").style.fg, Some(theme.success), "key → string");
    assert_eq!(span("1").style.fg, Some(theme.warning), "number → warning");
    assert_eq!(span("true").style.fg, Some(theme.warning), "bool → warning");
}

#[test]
fn fenced_code_unknown_language_stays_byte_identical_to_plain_code() {
    let theme = highlight_theme();
    let md = "```xyzlang\nfn main() { println!(\"hi\"); }\n```";
    let fences = fenced_lines(md, &theme);
    assert_eq!(fences.len(), 1);
    // One span per line, exactly the source line, in the code token —
    // the pre-#5 rendering, byte for byte.
    let expected = "fn main() { println!(\"hi\"); }";
    assert_eq!(fences[0].len(), 1, "single span, not highlighted");
    assert_eq!(fences[0][0].content.as_ref(), expected);
    assert_eq!(fences[0][0].style.fg, Some(theme.code));
    assert_eq!(fences[0][0].style.add_modifier, Modifier::empty());
}

#[test]
fn fenced_code_without_language_stays_byte_identical_to_plain_code() {
    let theme = highlight_theme();
    let fences = fenced_lines("```\nplain text\n```", &theme);
    assert_eq!(fences.len(), 1);
    assert_eq!(fences[0].len(), 1, "single span, not highlighted");
    assert_eq!(fences[0][0].content.as_ref(), "plain text");
    assert_eq!(fences[0][0].style.fg, Some(theme.code));
}

#[test]
fn render_node_wrapper_matches_the_full_render() {
    let theme = Theme::default();
    let chat = text_node("m1", "hello");
    let wrapped = render_node(
        &chat,
        false,
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
    );
    let full = render_node_full(
        &chat,
        false,
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
    );
    assert_eq!(wrapped, full.lines, "render_node is the default-cache full");
}

// ---------------------------------------------------------------------------
// 2. node-data variants
// ---------------------------------------------------------------------------

#[test]
fn node_data_variants_render_their_markers() {
    let theme = Theme::default();
    let image_cache = ImageCache::default();
    let render = |n: &ChatNode| {
        render_node_full(
            n,
            false,
            &render_ctx(
                80,
                &theme,
                Locale::En,
                &image_cache,
                &std::collections::HashMap::new(),
            ),
        )
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
    };

    // Interrupted assistant appends the marker.
    let interrupted = node(
        "1:1",
        1,
        NodeData::Assistant {
            turn: 1,
            step: 1,
            blocks: vec![AssistantBlock::Text {
                text: "partial".into(),
            }],
            usage: None,
            finalized: false,
            interrupted: true,
        },
    );
    assert!(
        render(&interrupted)
            .iter()
            .any(|l| l.contains("[interrupted]"))
    );

    // Turn error + max tokens + unknown + compaction.
    let turn_error = node(
        "e1",
        1,
        NodeData::TurnError {
            turn: 1,
            message: "boom".into(),
            code: Some("bad-input".into()),
        },
    );
    assert!(
        render(&turn_error)
            .iter()
            .any(|l| l.contains("[turn error: bad-input]"))
    );
    let max_tokens = node("e2", 1, NodeData::TurnMaxTokens { turn: 1 });
    assert!(
        render(&max_tokens)
            .iter()
            .any(|l| l.contains("[max tokens]"))
    );
    let unknown = node(
        "u1",
        1,
        NodeData::Unknown {
            r#type: "plugin/x".into(),
            data: json!({}),
        },
    );
    assert!(
        render(&unknown)
            .iter()
            .any(|l| l.contains("[unknown: plugin/x]"))
    );
    let compaction = node(
        "c1",
        1,
        NodeData::Compaction {
            summary: None,
            summary_event_seq: None,
            shadowed_item_count: Some(3),
            shadowed_token_count: Some(100),
        },
    );
    assert!(
        render(&compaction)
            .iter()
            .any(|l| l.contains("[compacted 3 messages]"))
    );

    // Assistant ToolCall block renders the tool line.
    let tool_call_block = node(
        "1:1",
        1,
        NodeData::Assistant {
            turn: 1,
            step: 1,
            blocks: vec![AssistantBlock::ToolCall {
                call_id: "c1".into(),
                name: "bash".into(),
                args_raw: "ls".into(),
            }],
            usage: None,
            finalized: true,
            interrupted: false,
        },
    );
    assert!(render(&tool_call_block).iter().any(|l| l.contains("bash")));

    // User content with every block kind.
    let content_blocks = node(
        "m1",
        1,
        NodeData::User {
            kind: dsh_tui::store::node::UserNodeKind::User,
            message_id: "m1".into(),
            content: vec![
                ContentBlock::Reasoning {
                    text: "think".into(),
                },
                ContentBlock::ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: "ls".into(),
                },
                ContentBlock::ToolResult {
                    tool_call_id: "c1".into(),
                    content: vec![],
                    is_error: Some(true),
                },
                ContentBlock::Raw(json!({"type": "custom-block"})),
            ],
            source: json!({"kind": "user"}),
        },
    );
    let lines = render(&content_blocks);
    assert!(lines.iter().any(|l| l.contains("[tool] bash")), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("[tool-result] failed")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("[block: custom-block]")),
        "{lines:?}"
    );
}

#[test]
fn tool_node_error_paths_render() {
    let theme = Theme::default();
    let image_cache = ImageCache::default();
    let render = |n: &ChatNode, collapsed: bool| {
        render_node_full(
            n,
            collapsed,
            &render_ctx(
                80,
                &theme,
                Locale::En,
                &image_cache,
                &std::collections::HashMap::new(),
            ),
        )
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
    };

    let long_args = "x".repeat(200);
    let error_tool = node(
        "t1",
        1,
        NodeData::Tool {
            call: Some(dsh_tui::store::node::RunningToolCall {
                call_id: "c1".into(),
                name: "bash".into(),
                args_raw: long_args.clone(),
                turn: 1,
                step: 1,
                time: 1.0,
                call_view: None,
            }),
            result: Some(Box::new(dsh_tui::store::node::ToolResultNode {
                call_id: "c1".into(),
                call: Some(dsh_tui::store::node::ToolCallBackfill {
                    name: "bash".into(),
                    args_raw: long_args.clone(),
                }),
                call_time: Some(1.0),
                content: vec![],
                is_error: true,
                error: Some(dsh_tui::store::event_data::ToolErrorIdentity {
                    name: "bash".into(),
                    code: "exit-1".into(),
                }),
                meta: None,
                call_view: None,
                result_view: None,
            })),
            interrupted: false,
        },
    );

    // Expanded: the tool-call line truncates the long args; the error
    // result appends the failed marker with the error code.
    let expanded = render(&error_tool, false);
    assert!(
        expanded
            .iter()
            .any(|l| l.contains("...") && l.contains("bash")),
        "truncated args: {expanded:?}"
    );
    assert!(
        expanded
            .iter()
            .any(|l| l.contains("[tool-result] failed: exit-1")),
        "{expanded:?}"
    );

    // Collapsed: a one-line summary, error-styled with the failed suffix.
    let collapsed = render(&error_tool, true);
    assert_eq!(collapsed.len(), 1, "one-line summary: {collapsed:?}");
    assert!(
        collapsed[0].contains("failed"),
        "failed suffix: {collapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. row-cache signature arms through a real store
// ---------------------------------------------------------------------------

fn store_with_all_node_kinds() -> SessionStore {
    let mut store = Store::new();
    let s = "s1";
    let mut ingest = |seq: i64, r#type: &str, data: serde_json::Value| {
        store.ingest(frame(s, ev(seq, r#type, data))).unwrap();
    };
    ingest(1, "user/message", user_msg("m1", "hello"));
    ingest(2, "step/start", json!({"turn": 1, "step": 1}));
    ingest(
        3,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "block-start", "index": 0, "blockType": "reasoning"}),
        ),
    );
    ingest(
        4,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "reasoning-delta", "index": 0, "text": "think"}),
        ),
    );
    ingest(
        5,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "block-end", "index": 0, "block": {"type": "reasoning", "text": "think"}}),
        ),
    );
    ingest(
        6,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "block-start", "index": 1, "blockType": "tool-call"}),
        ),
    );
    ingest(
        7,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "block-end", "index": 1, "block": {"type": "tool-call", "id": "c1", "name": "bash", "arguments": "ls"}}),
        ),
    );
    ingest(
        8,
        "assistant/message",
        json!({
            "turn": 1, "step": 1,
            "message": {"id": "m2", "role": "assistant", "content": [
                {"type": "reasoning", "text": "think"},
                {"type": "tool-call", "id": "c1", "name": "bash", "arguments": "ls"},
            ], "source": {"kind": "model", "provider": "p", "model": "m"}},
        }),
    );
    ingest(
        9,
        "tool/result",
        json!({
            "turn": 1, "step": 1,
            "message": {
                "id": "r1", "role": "user",
                "content": [{"type": "tool-result", "toolCallId": "c1", "content": [{"type": "text", "text": "out"}], "isError": true}],
                "source": {"kind": "tool", "callId": "c1"},
            },
            "error": {"name": "bash", "code": "exit-1"},
        }),
    );
    ingest(
        10,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "error", "error": {"code": "bad-input", "message": "nope"}}}),
    );
    ingest(11, "step/start", json!({"turn": 2, "step": 1}));
    ingest(
        12,
        "turn/end",
        json!({"turn": 2, "reason": {"kind": "max-tokens"}}),
    );
    ingest(
        13,
        "compaction",
        json!({"compactionId": "cmp-1", "summary": "s", "shadowedItemCount": 2, "shadowedTokenCount": 50, "seq": 13}),
    );
    ingest(14, "user/message", user_msg("m3", "another"));
    store
}

#[test]
fn row_cache_signs_and_renders_every_node_kind() {
    let mut store = store_with_all_node_kinds();
    let sid = SessionId("s1".into());
    let mut cache = RowCache::new();
    let theme = Theme::default();
    let images = ImageCache::default();
    let folds = std::collections::HashMap::new();
    let ctx = |width: u16| render_ctx(width, &theme, Locale::En, &images, &folds);
    assert!(cache.sync(&store, &sid, &ctx(100)), "first sync renders");
    assert!(!cache.lines().is_empty(), "rows cached");
    // Idle second sync: nothing dirty.
    assert!(!cache.sync(&store, &sid, &ctx(100)));

    // A new event dirties its row; render_dirty re-renders it.
    store
        .ingest(frame("s1", ev(15, "user/message", user_msg("m4", "dirty"))))
        .expect("ingest");
    assert!(cache.sync(&store, &sid, &ctx(100)));
    cache.render_dirty(&store, &sid, &ctx(100));

    // Width change re-wraps.
    assert!(cache.sync(&store, &sid, &ctx(40)));

    // A session with no store state clears the rows.
    assert!(cache.sync(&store, &SessionId("gone".into()), &ctx(100)));
    assert!(
        cache.lines().is_empty(),
        "rows cleared for a missing session"
    );
}

#[tokio::test]
async fn chat_view_renders_all_node_kinds_without_panicking() {
    let store = store_with_all_node_kinds();
    let sid = SessionId("s1".into());
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut app = dsh_tui::app::App::default();
    app.store = store;
    app.active_session = Some(sid.clone());
    app.sessions = vec![SessionSummary {
        session_id: sid,
        updated_at: 1.0,
        running: false,
        blank: false,
        parent_session_id: None,
        origin: None,
        cwd: None,
        agent_preset: None,
        projections: None,
    }];
    let mut channel = dsh_tui::app::EventChannel::new();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('x'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .unwrap();
    app.run(&mut term, &mut channel).await.unwrap();
    let view = format!("{}", term.backend());
    assert!(view.contains("[tool-result] failed: exit-1"), "{view}");
}

// ---------------------------------------------------------------------------
// 4. image viewer loaded-image + meta arms
// ---------------------------------------------------------------------------

fn attachment(media: ImageMediaType, bytes: i64) -> ImageAttachmentRef {
    ImageAttachmentRef {
        attachment_id: dsh_tui::wire::session::AttachmentId(format!("att-{media:?}")),
        name: Some("pic".into()),
        media_type: media,
        bytes,
        width: 10,
        height: 10,
    }
}

#[tokio::test]
async fn image_viewer_renders_loaded_images_and_notices() {
    // A real halfblocks picker decodes a tiny png (1×1).
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        .expect("valid 1x1 png base64");
    let png = png.as_slice();
    let picker = dsh_tui::render::picker_for(ImageProtocol::Halfblocks).expect("picker");
    let attachments = vec![
        attachment(ImageMediaType::ImagePng, 2_000_000),
        attachment(ImageMediaType::ImageJpeg, 2_000),
        attachment(ImageMediaType::ImageWebp, 200),
        attachment(ImageMediaType::ImageGif, 999),
    ];
    // The FIRST attachment has decoded bytes (loaded-image arms); the rest
    // fall to the placeholder (the no-bytes arm).
    let mut images = ImageCache::default();
    images
        .insert(&picker, attachments[0].attachment_id.clone(), png)
        .expect("decode");

    let viewer = ImageViewer::new(SessionId("s1".into()), attachments, 0);

    let mut app = dsh_tui::app::App::default();
    app.mode = Mode::Image(viewer);
    app.image_cache = images;
    app.toast = Some(("a notice".into(), std::time::Instant::now()));
    let backend = TestBackend::new(80, 24);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = dsh_tui::app::EventChannel::new();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('x'))))
        .unwrap();
    // Cycle through every media type + the fit toggle + back.
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('n'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('n'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('n'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('t'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('t'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('p'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .unwrap();
    app.run(&mut term, &mut channel).await.unwrap();

    let view = format!("{}", term.backend());
    // n n n → the gif, then p → the webp: its meta line renders.
    assert!(view.contains("image/webp"), "media type: {view}");
    assert!(view.contains("200 B"), "byte size: {view}");
    assert!(view.contains("a notice"), "notice line: {view}");
    assert!(view.contains(" image "), "viewer title: {view}");
}

#[tokio::test]
async fn image_viewer_meta_covers_kb_and_mb_sizes() {
    let viewer = ImageViewer::new(
        SessionId("s1".into()),
        vec![attachment(ImageMediaType::ImageJpeg, 2_000)],
        0,
    );
    let mut app = dsh_tui::app::App::default();
    app.mode = Mode::Image(viewer);
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = dsh_tui::app::EventChannel::new();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('x'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .unwrap();
    app.run(&mut term, &mut channel).await.unwrap();
    let view = format!("{}", term.backend());
    assert!(view.contains("2 KB"), "KB meta: {view}");

    let viewer = ImageViewer::new(
        SessionId("s1".into()),
        vec![attachment(ImageMediaType::ImagePng, 2_000_000)],
        0,
    );
    let mut app = dsh_tui::app::App::default();
    app.mode = Mode::Image(viewer);
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = dsh_tui::app::EventChannel::new();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('x'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .unwrap();
    app.run(&mut term, &mut channel).await.unwrap();
    let view = format!("{}", term.backend());
    assert!(view.contains("2 MB"), "MB meta: {view}");
}

#[tokio::test]
async fn image_viewer_placeholder_shows_protocol_notice() {
    let viewer = ImageViewer::new(
        SessionId("s1".into()),
        vec![attachment(ImageMediaType::ImagePng, 100)],
        0,
    );
    let mut app = dsh_tui::app::App::default();
    app.mode = Mode::Image(viewer);
    // No graphics tier: the "no protocol" notice renders.
    let backend = TestBackend::new(40, 10);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = dsh_tui::app::EventChannel::new();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(key(KeyCode::Char('x'))))
        .unwrap();
    channel
        .tx
        .send(dsh_tui::app::AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .unwrap();
    app.run(&mut term, &mut channel).await.unwrap();
    let view = format!("{}", term.backend());
    assert!(view.contains("no graphics protocol detected"), "{view}");
}

#[test]
fn markdown_tool_result_false_and_missing_error_code() {
    let theme = Theme::default();
    let image_cache = ImageCache::default();
    let render = |n: &ChatNode, collapsed: bool| {
        render_node_full(
            n,
            collapsed,
            &render_ctx(
                80,
                &theme,
                Locale::En,
                &image_cache,
                &std::collections::HashMap::new(),
            ),
        )
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
    };
    // A successful tool result: no failed suffix.
    let ok_tool = node(
        "t1",
        1,
        NodeData::Tool {
            call: Some(dsh_tui::store::node::RunningToolCall {
                call_id: "c1".into(),
                name: "bash".into(),
                args_raw: "ls".into(),
                turn: 1,
                step: 1,
                time: 1.0,
                call_view: None,
            }),
            result: Some(Box::new(dsh_tui::store::node::ToolResultNode {
                call_id: "c1".into(),
                call: None,
                call_time: None,
                content: vec![],
                is_error: false,
                error: None,
                meta: None,
                call_view: None,
                result_view: None,
            })),
            interrupted: false,
        },
    );
    let expanded = render(&ok_tool, false);
    assert!(
        !expanded.iter().any(|l| l.contains("failed")),
        "no failure marker: {expanded:?}"
    );

    // An error result WITHOUT an error identity: the code degrades.
    let no_code = node(
        "t2",
        1,
        NodeData::Tool {
            call: None,
            result: Some(Box::new(dsh_tui::store::node::ToolResultNode {
                call_id: "c2".into(),
                call: None,
                call_time: None,
                content: vec![],
                is_error: true,
                error: None,
                meta: None,
                call_view: None,
                result_view: None,
            })),
            interrupted: false,
        },
    );
    let expanded = render(&no_code, false);
    assert!(
        expanded.iter().any(|l| l.contains("failed")),
        "failed marker: {expanded:?}"
    );
    let collapsed = render(&no_code, true);
    assert!(
        collapsed.iter().any(|l| l.contains("failed")),
        "collapsed failed marker: {collapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// #31: skill-list block detection + fold rendering
// ---------------------------------------------------------------------------

const SKILLS_MD: &str =
    "## Skills\n- bash — run shell commands\n- git — version control\n  - nested detail";

#[test]
fn skill_block_folds_to_one_header_row() {
    let theme = Theme::default();
    let (lines, _code, skill_header) = render_markdown(
        SKILLS_MD,
        Style::default().fg(theme.text),
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        true, // folded (the default)
    );
    let rendered: Vec<String> = lines.iter().map(ToString::to_string).collect();
    // The message starts with the heading: the top-blank guard skips the
    // leading blank, so exactly the one header row renders.
    assert_eq!(rendered.len(), 1, "one header row: {rendered:?}");
    assert_eq!(rendered[0], "▸ 2 skills", "folded header: {rendered:?}");
    assert_eq!(skill_header, Some(0), "the header line is the hit target");
    // The glyph is accent+bold; the count text is plain text.
    let header = &lines[0];
    assert!(
        header.spans[0].style.fg == Some(theme.accent),
        "accent glyph"
    );
    assert!(
        header.spans[0].style.add_modifier.contains(Modifier::BOLD),
        "bold glyph"
    );
}

#[test]
fn skill_block_expands_to_header_and_items() {
    let theme = Theme::default();
    let (lines, _code, skill_header) = render_markdown(
        SKILLS_MD,
        Style::default().fg(theme.text),
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        false, // expanded
    );
    let rendered: Vec<String> = lines.iter().map(ToString::to_string).collect();
    assert_eq!(rendered[0], "▾ 2 skills", "expanded header: {rendered:?}");
    assert_eq!(rendered[1], "bash — run shell commands", "item name+desc");
    assert_eq!(
        rendered[2], "git — version control nested detail",
        "the nested bullet appended to its parent item"
    );
    assert_eq!(skill_header, Some(0), "the header is still the hit target");
}

#[test]
fn skill_heading_variants_and_cjk_parse() {
    let theme = Theme::default();
    let md = "### available skills (12)\n- psutil — 系统进程\n- 磁盘占用 检查：df -h";
    let (lines, _code, skill_header) = render_markdown(
        md,
        Style::default().fg(theme.text),
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        false,
    );
    assert_eq!(skill_header, Some(0), "### + trailing count matches");
    let rendered: Vec<String> = lines.iter().map(ToString::to_string).collect();
    assert_eq!(rendered[0], "▾ 2 skills");
    assert_eq!(rendered[1], "psutil — 系统进程", "CJK description");
    // `：` is a tolerated separator: name "磁盘占用 检查", desc "df -h".
    assert_eq!(rendered[2], "磁盘占用 检查 — df -h");
}

#[test]
fn non_skill_messages_render_byte_identical() {
    let theme = Theme::default();
    // A skills heading WITHOUT a following list, and a list under a
    // different heading: no fold, the normal rendering path.
    for md in [
        "## Skills\nno list follows",
        "## Skills Overview\n- bash — run shell",
        "## Other\n- bash — run shell",
        "## skills\n1. ordered\n2. list",
    ] {
        let (lines, _code, skill_header) = render_markdown(
            md,
            Style::default().fg(theme.text),
            &render_ctx(
                80,
                &theme,
                Locale::En,
                &ImageCache::default(),
                &std::collections::HashMap::new(),
            ),
            true,
        );
        assert_eq!(skill_header, None, "no fold for: {md}");
        assert!(!lines.iter().any(|l| l.to_string().contains("▸")), "{md}");
    }
}

#[test]
fn skill_header_is_localized() {
    let theme = Theme::default();
    let (lines, _code, _skill) = render_markdown(
        SKILLS_MD,
        Style::default().fg(theme.text),
        &render_ctx(
            80,
            &theme,
            Locale::Zh,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        true,
    );
    assert_eq!(
        lines[0].to_string(),
        "▸ 2 技能",
        "zh header via the same trf path"
    );
}

/// #31 review: a mid-message skill block — the header lands AFTER the
/// intro's rows (it renders at the block boundary, not at the top).
#[test]
fn mid_message_skill_block_follows_the_intro() {
    let theme = Theme::default();
    let md = "Here's what I can do:\n\n## Skills\n- a — b\n- c — d";
    let (lines, _code, skill_header) = render_markdown(
        md,
        Style::default().fg(theme.text),
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        true,
    );
    let rendered: Vec<String> = lines.iter().map(ToString::to_string).collect();
    assert_eq!(
        rendered,
        vec!["Here's what I can do:", "", "▸ 2 skills"],
        "intro rows, then the folded header"
    );
    assert_eq!(skill_header, Some(2), "the header index follows the intro");

    let (lines, _code, _skill) = render_markdown(
        md,
        Style::default().fg(theme.text),
        &render_ctx(
            80,
            &theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
        false,
    );
    let rendered: Vec<String> = lines.iter().map(ToString::to_string).collect();
    assert_eq!(
        rendered,
        vec!["Here's what I can do:", "", "▾ 2 skills", "a — b", "c — d"],
        "expanded items in order after the intro"
    );
}
