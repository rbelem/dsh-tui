//! Trajectory ledger render tests (#44): the retained event window folds
//! into TOOL/ASSISTANT ledger rows with turn/step grouping, the header
//! counters, the arrow result summaries, and the eviction gap marker —
//! rendered via `TestBackend` exactly like the chat snapshots.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::i18n::Locale;
use dsh_tui::render::TrajectoryView;
use dsh_tui::store::SessionStore;
use dsh_tui::theme::Theme;
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId};

// ---------------------------------------------------------------------------
// fixture helpers (same construction patterns as tests/render_snapshots.rs)
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

fn tool_result_message(id: &str, call_id: &str, text: &str) -> serde_json::Value {
    json!({
        "id": id,
        "content": [{
            "type": "tool-result",
            "toolCallId": call_id,
            "content": [{"type": "text", "text": text}]
        }],
        "source": {"kind": "tool", "callId": call_id}
    })
}

/// One turn with a streamed assistant reply and a bash tool round-trip.
fn one_turn_events() -> Vec<SessionEvent> {
    vec![
        ev(1, "turn/start", json!({"turn": 1})),
        ev(
            2,
            "request/context",
            json!({"provider": "deepseek", "model": "deepseek-chat", "contextWindow": 65536}),
        ),
        ev(3, "step/start", json!({"turn": 1, "step": 1})),
        ev(
            4,
            "tool/call",
            json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": "{\"command\": \"ls -la\"}"}),
        ),
        ev(
            5,
            "tool/result",
            json!({"turn": 1, "step": 1, "message": tool_result_message("tr1", "c1", "total 4\ndrwxr-xr-x"), "error": null, "meta": null}),
        ),
        ev(
            6,
            "assistant/message",
            json!({"turn": 1, "step": 1, "message": {"id": "m2", "content": [{"type": "text", "text": "Done!"}], "source": {"kind": "model"}}, "usage": {"inputTokens": 150, "outputTokens": 80, "cacheReadTokens": 0, "cacheWriteTokens": 0}}),
        ),
        ev(7, "turn/end", json!({"turn": 1, "reason": "completed"})),
    ]
}

fn render_trajectory(store: &SessionStore, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        f.render_widget(
            TrajectoryView {
                store,
                session_id: &SessionId("s1".into()),
                offset: 0,
                theme: &Theme::default(),
                locale: Locale::En,
            },
            f.area(),
        )
    })
    .unwrap();
    format!("{}", term.backend())
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn ledger_shows_tool_result_assistant_and_grouping() {
    let mut store = SessionStore::new();
    ingest_all(&mut store, "s1", one_turn_events());
    let view = render_trajectory(&store, 80, 24);

    // The header: title + duration + counters.
    assert!(view.contains("trajectory"), "title: {view}");
    assert!(
        view.contains("Duration / Actual time"),
        "duration header: {view}"
    );
    assert!(view.contains("Turns 1"), "turn counter: {view}");
    assert!(view.contains("Calls 1"), "calls counter: {view}");
    assert!(view.contains("Input 150"), "input counter: {view}");
    assert!(
        view.contains("Model deepseek-chat"),
        "model counter: {view}"
    );
    assert!(view.contains("Tools 1"), "tools counter: {view}");

    // Turn/step grouping headers.
    let turn = view.find("turn 1").expect("turn header");
    let step = view.find("step 1").expect("step header");
    assert!(turn < step, "turn before step: {view}");

    // The tool row: `TOOL bash {"command": "ls -la"}` then the `→` result.
    let tool = view.find("TOOL bash").expect("tool row");
    let args = view.find("ls -la").expect("tool args");
    let result = view.find("→ total 4").expect("arrow result");
    let assistant = view.find("ASSISTANT Done!").expect("assistant row");
    let pager = view
        .find("Load earlier history")
        .expect("load-earlier pager");
    assert!(
        tool < args && args < result && result < assistant && assistant < pager,
        "ledger order: tool, result, assistant, pager\n{view}"
    );
}

#[test]
fn evicted_window_shows_the_gap_marker() {
    // A 2-event cap evicts the head: the truncated window shows the gap
    // row at the ledger's top.
    let mut store = SessionStore::with_max_buffered_events(2);
    ingest_all(&mut store, "s1", one_turn_events());
    let view = render_trajectory(&store, 80, 24);
    assert!(
        view.contains("earlier events evicted"),
        "gap marker: {view}"
    );
    assert!(view.contains("Load earlier history"), "pager: {view}");
}

#[test]
fn zh_locale_renders() {
    let mut store = SessionStore::new();
    ingest_all(&mut store, "s1", one_turn_events());
    let backend = TestBackend::new(80, 24);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        f.render_widget(
            TrajectoryView {
                store: &store,
                session_id: &SessionId("s1".into()),
                offset: 0,
                theme: &Theme::default(),
                locale: Locale::Zh,
            },
            f.area(),
        )
    })
    .unwrap();
    let view = format!("{}", term.backend());
    assert!(view.contains("轨迹"), "zh title: {view}");
    assert!(view.contains("回合 1"), "zh turn header: {view}");
    assert!(view.contains("时长 / 实际时间"), "zh duration: {view}");
    assert!(view.contains("加载更早的历史"), "zh pager: {view}");
}

#[test]
fn long_rows_truncate_with_ellipsis() {
    let mut store = SessionStore::new();
    ingest_all(
        &mut store,
        "s1",
        vec![
            ev(
                1,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": "{\"command\": \"echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}"}),
            ),
            ev(
                2,
                "tool/result",
                json!({"turn": 1, "step": 1, "message": tool_result_message("tr1", "c1", &"y".repeat(200)), "error": null, "meta": null}),
            ),
            ev(
                3,
                "assistant/message",
                json!({"turn": 1, "step": 1, "message": {"id": "m2", "content": [{"type": "text", "text": "Done!"}], "source": {"kind": "model"}}, "usage": {"inputTokens": 1, "outputTokens": 1}}),
            ),
        ],
    );
    let view = render_trajectory(&store, 40, 24);
    assert!(view.contains("…"), "long rows truncate: {view}");
}

#[test]
fn overflowing_ledger_stops_at_the_bottom() {
    // Enough rows to overflow a tiny area: the render stops at the pane's
    // bottom edge without panicking.
    let mut store = SessionStore::new();
    let mut events = Vec::new();
    for i in 1..=40 {
        events.push(ev(
            i,
            "tool/call",
            json!({"turn": 1, "step": 1, "callId": format!("c{i}"), "name": "bash", "arguments": "{}"}),
        ));
    }
    ingest_all(&mut store, "s1", events);
    let view = render_trajectory(&store, 40, 6);
    assert!(view.contains("TOOL bash"), "tool rows render: {view}");
}

#[test]
fn empty_events_render_pager_only() {
    // An OPENED-but-empty session (the store knows it, the window has no
    // events): the duration is 0 and only the pager row follows the header.
    let mut store = SessionStore::new();
    store.open_session(SessionId("s1".into()));
    let view = render_trajectory(&store, 40, 10);
    assert!(
        view.contains("Load earlier history"),
        "empty store still shows the pager: {view}"
    );
    assert!(!view.contains("TOOL"), "no tool rows: {view}");
    assert!(!view.contains("0.0s"), "no spurious duration: {view}");
}

#[test]
fn offset_clamps_and_guard_returns_early() {
    let mut store = SessionStore::new();
    ingest_all(&mut store, "s1", one_turn_events());
    // An offset past the ledger's end clamps to the last visible row —
    // the tail (pager) stays on screen.
    let backend = TestBackend::new(40, 10);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        f.render_widget(
            TrajectoryView {
                store: &store,
                session_id: &SessionId("s1".into()),
                offset: 10_000,
                theme: &Theme::default(),
                locale: Locale::En,
            },
            f.area(),
        )
    })
    .unwrap();
    let view = format!("{}", term.backend());
    assert!(
        view.contains("Load earlier history"),
        "clamped tail: {view}"
    );

    // Too small to hold the header: the widget draws nothing.
    let backend = TestBackend::new(40, 3);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        f.render_widget(
            TrajectoryView {
                store: &store,
                session_id: &SessionId("s1".into()),
                offset: 0,
                theme: &Theme::default(),
                locale: Locale::En,
            },
            f.area(),
        )
    })
    .unwrap();
    let view = format!("{}", term.backend());
    assert!(!view.contains("trajectory"), "height guard: {view}");
}

#[test]
fn duration_formats_minutes_for_long_windows() {
    let mut store = SessionStore::new();
    let mut events = one_turn_events();
    // Stretch the last event's wall clock past a minute.
    events.last_mut().expect("events").time = 130.0;
    ingest_all(&mut store, "s1", events);
    let view = render_trajectory(&store, 60, 24);
    assert!(view.contains("2m 09s"), "minute duration: {view}");
}

#[test]
fn missing_model_drops_the_counter_segment() {
    let mut store = SessionStore::new();
    ingest_all(
        &mut store,
        "s1",
        vec![ev(1, "turn/start", json!({"turn": 1}))],
    );
    let view = render_trajectory(&store, 60, 10);
    assert!(!view.contains("Model"), "no model segment: {view}");
    assert!(view.contains("Turns 1"), "turns still shown: {view}");
}

#[test]
fn tool_without_args_and_missing_content_stay_graceful() {
    let mut store = SessionStore::new();
    ingest_all(
        &mut store,
        "s1",
        vec![
            // A tool call with empty args, and a result whose message has
            // no tool-result block (empty snippet), plus an assistant
            // message with only a raw block (no text).
            ev(
                1,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": "  "}),
            ),
            ev(
                2,
                "tool/result",
                json!({"turn": 1, "step": 1, "message": {"id": "tr1", "content": [{"type": "text", "text": "plain"}], "source": {"kind": "tool", "callId": "c1"}}, "error": null, "meta": null}),
            ),
            ev(
                3,
                "assistant/message",
                json!({"turn": 1, "step": 1, "message": {"id": "m2", "content": [{"type": "plugin-block", "x": 1}], "source": {"kind": "model"}}, "usage": null}),
            ),
        ],
    );
    let view = render_trajectory(&store, 60, 24);
    assert!(view.contains("TOOL bash"), "name-only tool row: {view}");
    assert!(
        view.contains("ASSISTANT"),
        "assistant row still renders: {view}"
    );
    // Both empty snippets render the row frames without crashing.
    assert!(view.contains("→"), "result row frame: {view}");
}
