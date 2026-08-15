//! Renderer snapshot + unit tests (ticket 08: 120×30 + 60×15).
//!
//! A scripted session is built in a `SessionStore` (user message with
//! markdown incl. a code fence + table, assistant streaming chunks incl.
//! reasoning, a tool round-trip, a compaction checkpoint), then rendered via
//! `TestBackend` + `ChatView` and insta-snapshotted (ratatui's `buffer_view`
//! cell dump — deterministic, no timestamps).

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::i18n::Locale;
use dsh_tui::render::{ChatView, ImageCache, RowCache, render_markdown};
use dsh_tui::store::SessionStore;
use dsh_tui::store::node::{FoldState, NodeData};
use dsh_tui::theme::Theme;
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId};

// ---------------------------------------------------------------------------
// fixture helpers (same construction patterns as tests/store_fold.rs)
// ---------------------------------------------------------------------------

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

fn ev_surface(
    seq: i64,
    r#type: &str,
    data: serde_json::Value,
    surface_op: serde_json::Value,
) -> SessionEvent {
    let mut event = ev(seq, r#type, data);
    event.surface_op = Some(surface_op);
    event
}

fn frame(session: &str, event: SessionEvent) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: None,
    }
}

fn ingest_all(store: &mut SessionStore, session: &str, events: Vec<SessionEvent>) {
    for event in events {
        store
            .ingest(frame(session, event))
            .expect("ingest must succeed");
    }
}

fn user_msg(id: &str, text: &str, source: serde_json::Value) -> serde_json::Value {
    json!({"id": id, "role": "user", "content": [{"type": "text", "text": text}], "source": source})
}

fn chunk(turn: i64, step: i64, chunk: serde_json::Value) -> serde_json::Value {
    json!({"turn": turn, "step": step, "chunk": chunk})
}

fn user_source() -> serde_json::Value {
    json!({"kind": "user"})
}

const S: &str = "s1";

/// The full scripted scenario: user markdown → streamed assistant (text +
/// reasoning) → tool round-trip → compaction checkpoint.
fn build_full_store() -> SessionStore {
    let mut store = SessionStore::new();
    ingest_all(
        &mut store,
        S,
        vec![
            ev(
                1,
                "user/message",
                user_msg(
                    "m1",
                    "Check this out:\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n~~stale~~ fresh",
                    user_source(),
                ),
            ),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 0, "blockType": "text"}),
                ),
            ),
            ev(
                4,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "Hello "}),
                ),
            ),
            ev(
                5,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "world"}),
                ),
            ),
            ev(
                6,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 1, "blockType": "reasoning"}),
                ),
            ),
            ev(
                7,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "reasoning-delta", "index": 1, "text": "thinking"}),
                ),
            ),
            ev(
                8,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "read_file", "arguments": r#"{"path":"/etc"}"#}),
            ),
            ev(
                9,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "m2", "role": "assistant",
                        "content": [{"type": "text", "text": "Hello world"}, {"type": "reasoning", "text": "thinking"}],
                        "source": {"kind": "model", "provider": "p", "model": "m"},
                    },
                    "usage": {"inputTokens": 10, "outputTokens": 5},
                }),
            ),
            ev(
                10,
                "tool/result",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "r1", "role": "user",
                        "content": [{"type": "tool-result", "toolCallId": "c1", "content": [{"type": "text", "text": "the /etc directory"}], "isError": false}],
                        "source": {"kind": "tool", "callId": "c1"},
                    },
                }),
            ),
            ev(11, "step/end", json!({"turn": 1, "step": 1})),
            ev(
                12,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
            ev(
                13,
                "compaction/start",
                json!({"compactionId": "comp-1", "turn": null}),
            ),
            ev(
                14,
                "compaction/summary",
                json!({
                    "compactionId": "comp-1",
                    "summary": [{"type": "text", "text": "summarized content"}],
                    "shadowedRange": {"start": 1, "end": 1},
                    "shadowedSeqs": [1],
                    "shadowedTokenCount": 120,
                    "provider": "p",
                    "model": "m",
                }),
            ),
            ev_surface(
                15,
                "user/message",
                user_msg(
                    "m3",
                    "summarized content",
                    json!({"kind": "plugin", "plugin": "compact", "compactionId": "comp-1"}),
                ),
                json!({"op": "replace", "start": 1, "end": 1}),
            ),
            ev(
                16,
                "compaction/end",
                json!({"compactionId": "comp-1", "turn": null}),
            ),
        ],
    );
    store
}

fn sid() -> SessionId {
    SessionId(S.into())
}

/// sync + render_dirty + draw into a TestBackend; return the buffer_view dump.
fn render_snapshot(
    store: &SessionStore,
    cache: &mut RowCache,
    width: u16,
    height: u16,
    offset: usize,
) -> String {
    cache.sync(
        store,
        &sid(),
        width,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
    );
    cache.render_dirty(
        store,
        &sid(),
        width,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
    );
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|frame| {
            frame.render_widget(
                ChatView {
                    store,
                    session_id: &sid(),
                    offset,
                    row_cache: cache,
                    images: &mut ImageCache::default(),
                },
                frame.area(),
            );
        })
        .expect("draw");
    format!("{}", terminal.backend())
}

// ---------------------------------------------------------------------------
// snapshots
// ---------------------------------------------------------------------------

#[test]
fn full_scenario_renders_at_120x30() {
    let store = build_full_store();
    let mut cache = RowCache::new();
    insta::assert_snapshot!(
        "chat-120x30",
        render_snapshot(&store, &mut cache, 120, 30, 0)
    );
}

#[test]
fn full_scenario_renders_at_60x15() {
    let store = build_full_store();
    let mut cache = RowCache::new();
    insta::assert_snapshot!("chat-60x15", render_snapshot(&store, &mut cache, 60, 15, 0));
}

#[test]
fn streaming_chunk_marks_dirty_and_rerenders() {
    let mut store = SessionStore::new();
    // Phase A: streamed text without a finalize yet.
    ingest_all(
        &mut store,
        S,
        vec![
            ev(1, "user/message", user_msg("m1", "hello", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 0, "blockType": "text"}),
                ),
            ),
            ev(
                4,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "Hel"}),
                ),
            ),
        ],
    );
    let mut cache = RowCache::new();
    assert!(
        cache.sync(
            &store,
            &sid(),
            120,
            &Theme::default(),
            Locale::En,
            &ImageCache::default()
        ),
        "fresh sync renders everything"
    );
    assert!(
        !cache.sync(
            &store,
            &sid(),
            120,
            &Theme::default(),
            Locale::En,
            &ImageCache::default()
        ),
        "idle sync changes nothing"
    );
    assert!(cache.dirty().is_empty());

    // Phase B: more chunks + finalize.
    ingest_all(
        &mut store,
        S,
        vec![
            ev(
                5,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "lo"}),
                ),
            ),
            ev(
                6,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {"id": "m2", "role": "assistant", "content": [{"type": "text", "text": "Hello world"}], "source": {"kind": "model", "provider": "p", "model": "m"}},
                }),
            ),
            ev(
                7,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ],
    );
    assert!(
        cache.sync(
            &store,
            &sid(),
            120,
            &Theme::default(),
            Locale::En,
            &ImageCache::default()
        ),
        "changed node detected"
    );
    assert!(
        cache.dirty().contains("1:1"),
        "streaming chunk marks the node dirty"
    );
    let snapshot = render_snapshot(&store, &mut cache, 120, 30, 0);
    assert!(cache.dirty().is_empty(), "render_dirty clears the set");
    assert!(
        snapshot.contains("Hello world"),
        "dirty re-parse renders the delta"
    );
    insta::assert_snapshot!("chat-streaming-final", snapshot);
}

#[test]
fn collapsed_tool_renders_one_line_summary() {
    let store = build_full_store();
    let mut cache = RowCache::new();
    // Expanded default first (tool rows expand by default, Q11).
    let expanded = render_snapshot(&store, &mut cache, 120, 30, 0);
    assert!(
        expanded.contains("the /etc directory"),
        "expanded tool shows its result"
    );

    let mut store = build_full_store();
    let mut cache = RowCache::new();
    store.set_fold(&sid(), "c1", FoldState::collapsed());
    let snapshot = render_snapshot(&store, &mut cache, 120, 30, 0);
    assert!(
        snapshot.contains("[tool] read_file"),
        "collapsed tool keeps the title line"
    );
    assert!(
        !snapshot.contains("the /etc directory"),
        "collapsed tool hides the result"
    );
    insta::assert_snapshot!("chat-tool-collapsed", snapshot);
}

#[test]
fn offset_scrolling_renders_from_viewport() {
    let store = build_full_store();
    let mut cache = RowCache::new();
    insta::assert_snapshot!(
        "chat-offset-2",
        render_snapshot(&store, &mut cache, 120, 30, 2)
    );
}

/// Regression (ticket 08 Q5 live smoke): the viewport offset is LINE-space,
/// so an offset past the end (follow-mode: `total_lines - height`) must
/// clamp to the conversation's last line — never to the top. The store here
/// is ~18 rendered lines tall; a huge offset must show the tail.
#[test]
fn offset_past_end_clamps_to_bottom_not_top() {
    let store = build_full_store();
    let mut cache = RowCache::new();
    let snapshot = render_snapshot(&store, &mut cache, 120, 30, 1000);
    // The last node's last line is the compaction summary — the tail must
    // be visible, while the head must be gone.
    assert!(
        snapshot.contains("summarized content"),
        "tail missing: {snapshot}"
    );
    assert!(
        !snapshot.contains("Check this out"),
        "head still visible: {snapshot}"
    );
}

// ---------------------------------------------------------------------------
// row cache unit tests
// ---------------------------------------------------------------------------

#[test]
fn row_cache_signature_change_detection() {
    let mut store = SessionStore::new();
    ingest_all(
        &mut store,
        S,
        vec![
            ev(1, "user/message", user_msg("m1", "hello", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "Hel"}),
                ),
            ),
        ],
    );
    let mut cache = RowCache::new();
    assert!(cache.sync(
        &store,
        &sid(),
        120,
        &Theme::default(),
        Locale::En,
        &ImageCache::default()
    ));
    assert!(
        !cache.sync(
            &store,
            &sid(),
            120,
            &Theme::default(),
            Locale::En,
            &ImageCache::default()
        ),
        "no change → false"
    );
    assert!(cache.dirty().is_empty());

    // A new chunk changes the accumulated text → signature flips.
    store
        .ingest(frame(
            S,
            ev(
                4,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "lo"}),
                ),
            ),
        ))
        .unwrap();
    assert!(
        cache.sync(
            &store,
            &sid(),
            120,
            &Theme::default(),
            Locale::En,
            &ImageCache::default()
        ),
        "changed node → true"
    );
    assert_eq!(cache.dirty().len(), 1);
    assert!(cache.dirty().contains("1:1"));

    // render_dirty consumes the set; the next sync is clean.
    cache.render_dirty(
        &store,
        &sid(),
        120,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
    );
    assert!(cache.dirty().is_empty());
    assert!(!cache.sync(
        &store,
        &sid(),
        120,
        &Theme::default(),
        Locale::En,
        &ImageCache::default()
    ));

    // Fold state is part of the signature: collapsing marks the node dirty.
    store.set_fold(&sid(), "1:1", FoldState::collapsed());
    assert!(cache.sync(
        &store,
        &sid(),
        120,
        &Theme::default(),
        Locale::En,
        &ImageCache::default()
    ));
    assert!(cache.dirty().contains("1:1"));
}

#[test]
fn row_cache_width_change_invalidates_all() {
    let mut store = SessionStore::new();
    ingest_all(
        &mut store,
        S,
        vec![
            ev(
                1,
                "user/message",
                user_msg(
                    "m1",
                    "This is a fairly long paragraph line that will wrap at narrower widths like forty columns or so.",
                    user_source(),
                ),
            ),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "Hi"}),
                ),
            ),
        ],
    );
    let mut cache = RowCache::new();
    cache.sync(
        &store,
        &sid(),
        120,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
    );
    let rows_at_120 = cache.lines().len();
    let first_lines_at_120 = cache.lines()[0].lines.len();

    // Narrower width: everything re-renders, the long paragraph wraps.
    assert!(
        cache.sync(
            &store,
            &sid(),
            40,
            &Theme::default(),
            Locale::En,
            &ImageCache::default()
        ),
        "width change → re-render"
    );
    assert_eq!(cache.lines().len(), rows_at_120, "same node count");
    assert!(
        cache.lines()[0].lines.len() > first_lines_at_120,
        "narrow width wraps the long paragraph"
    );
    // Idle at the new width.
    assert!(!cache.sync(
        &store,
        &sid(),
        40,
        &Theme::default(),
        Locale::En,
        &ImageCache::default()
    ));
}

// ---------------------------------------------------------------------------
// markdown unit tests
// ---------------------------------------------------------------------------

#[test]
fn markdown_code_fence_highlights_with_syntect() {
    let lines = render_markdown(
        "before\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\nafter",
        Default::default(),
        &Theme::default(),
    );
    let fence_lines: Vec<String> = lines.iter().map(ToString::to_string).collect();
    assert!(
        fence_lines.iter().any(|line| line.starts_with("│ ")),
        "code fence lines carry the │ prefix: {fence_lines:?}"
    );
    assert!(fence_lines.iter().any(|line| line.contains("fn main()")));
    assert!(fence_lines.iter().any(|line| line == "before"));
}

#[test]
fn markdown_table_renders_joined_rows() {
    let lines = render_markdown(
        "| a | b |\n|---|---|\n| 1 | 2 |",
        Default::default(),
        &Theme::default(),
    );
    let rendered: Vec<String> = lines.iter().map(ToString::to_string).collect();
    assert!(rendered.contains(&"| a | b |".to_string()), "{rendered:?}");
    assert!(rendered.contains(&"| 1 | 2 |".to_string()), "{rendered:?}");
    assert!(
        rendered
            .iter()
            .any(|line| line.starts_with('|') && line.contains('─')),
        "{rendered:?}"
    );
}

#[test]
fn markdown_strikethrough_sets_modifier() {
    use ratatui::style::Modifier;
    let lines = render_markdown("~~gone~~ here", Default::default(), &Theme::default());
    assert_eq!(lines.len(), 1);
    let crossed = lines[0]
        .spans
        .iter()
        .filter(|span| span.content.as_ref() == "gone")
        .collect::<Vec<_>>();
    assert_eq!(crossed.len(), 1);
    assert!(
        crossed[0].style.has_modifier(Modifier::CROSSED_OUT),
        "struck text is crossed out"
    );
}

#[test]
fn markdown_cjk_renders_and_wraps_by_width() {
    // CJK text renders without panic and wraps at narrow widths.
    let text = "日本語のテキストが続きますここで折り返されるはずです";
    let lines = render_markdown(text, Default::default(), &Theme::default());
    assert_eq!(lines.len(), 1);
    let mut store = SessionStore::new();
    store
        .ingest(frame(
            S,
            ev(1, "user/message", user_msg("m1", text, user_source())),
        ))
        .unwrap();
    let mut cache = RowCache::new();
    cache.sync(
        &store,
        &sid(),
        8,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
    );
    assert!(
        cache.lines()[0].lines.len() >= 2,
        "CJK text wraps at width 8"
    );
    // Each wrapped line fits the width (a wide grapheme may overflow by one).
    for line in &cache.lines()[0].lines {
        assert!(line.width() <= 8, "wrapped line width {}", line.width());
    }
}

#[test]
fn full_scenario_folds_five_nodes() {
    // The store's node list (display order) drives the pipeline.
    let store = build_full_store();
    let state = store.session(&sid()).expect("session");
    assert_eq!(
        state.nodes.len(),
        5,
        "user + tool + assistant + compaction + context user"
    );
    assert!(matches!(&state.nodes[3].data, NodeData::Compaction { .. }));
}
