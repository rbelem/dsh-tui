//! App-shell integration tests: attach flow, event loop, coalesced draw,
//! keymap, follow, resize. Keyless: the shared mock gateway, injected events,
//! and `TestBackend` only — no real terminal.

mod common;
use common::{MockAction, MockGateway};

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use tokio::sync::mpsc;

use dsh_tui::app::{Action, App, AppEvent, attach, spawn_frame_bridge};
use dsh_tui::client::WireClient;
use dsh_tui::store::SessionStore;
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId};

// ---------------------------------------------------------------------------
// fixture helpers
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

fn user_msg(id: &str, text: &str) -> serde_json::Value {
    json!({"id": id, "role": "user", "content": [{"type": "text", "text": text}], "source": {"kind": "user"}})
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn mux_subscribed(session: &str, last_seq: i64) -> String {
    format!(
        r#"{{"type":"server-request","rpcId":"ws","method":"events.mux","payload":{{"type":"session/subscribed","sessionId":"{session}","lastSeq":{last_seq}}}}}"#
    )
}

fn mux_chunk(session: &str, seq: i64, chunk: serde_json::Value) -> String {
    let payload = json!({
        "type": "session/event",
        "sessionId": session,
        "event": {
            "type": "assistant/chunk",
            "seq": seq,
            "time": seq as f64,
            "data": {"turn": 2, "step": 1, "chunk": chunk},
        },
    });
    serde_json::to_string(&json!({
        "type": "server-request",
        "rpcId": "ws",
        "method": "events.mux",
        "payload": payload,
    }))
    .expect("serialize mux frame")
}

fn mux_assistant_message(session: &str, seq: i64, text: &str) -> String {
    let payload = json!({
        "type": "session/event",
        "sessionId": session,
        "event": {
            "type": "assistant/message",
            "seq": seq,
            "time": seq as f64,
            "data": {
                "turn": 2,
                "step": 1,
                "message": {
                    "id": "m3",
                    "role": "assistant",
                    "content": [{"type": "text", "text": text}],
                    "source": {"kind": "model", "provider": "p", "model": "m"},
                },
            },
        },
    });
    serde_json::to_string(&json!({
        "type": "server-request",
        "rpcId": "ws",
        "method": "events.mux",
        "payload": payload,
    }))
    .expect("serialize mux frame")
}

/// Feed buffered events into a fresh channel and run the loop to completion
/// (the quit key breaks it).
async fn run_with(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    for event in events {
        tx.send(event).expect("event channel");
    }
    app.run(term, &mut rx).await.expect("run must not fail");
}

// ---------------------------------------------------------------------------
// 1. attach → history → mux → draw (end to end)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attach_history_draw_end_to_end() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[
                {"sessionId":"s1","updatedAt":200.0,"running":false,"blank":false},
                {"sessionId":"s2","updatedAt":100.0,"running":false,"blank":true}
            ]}}}"#,
        ),
    )
    .await;
    mock.set_handler(
        "session.history",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"events":[
                {"event":{"type":"user/message","seq":1,"time":1.0,"data":{"id":"m1","role":"user","content":[{"type":"text","text":"from history\n\n```rust\nfn main() {}\n```"}],"source":{"kind":"user"}}}},
                {"event":{"type":"assistant/message","seq":2,"time":2.0,"data":{"turn":1,"step":1,"message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"from history assistant"}],"source":{"kind":"model","provider":"p","model":"m"}}}}}
            ],"hasMore":false}}}"#,
        ),
    )
    .await;
    mock.set_ws_frames(
        "/api/events.mux",
        vec![
            mux_subscribed("s1", 2),
            mux_chunk(
                "s1",
                3,
                json!({"type": "block-start", "index": 0, "blockType": "text"}),
            ),
            mux_chunk(
                "s1",
                4,
                json!({"type": "text-delta", "index": 0, "text": "streamed hello"}),
            ),
            mux_assistant_message("s1", 5, "streamed hello"),
        ],
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut store = SessionStore::new();
    let opened = attach(&client, &mut store).await.expect("attach");
    assert_eq!(opened, Some(SessionId("s1".into())));

    let mut app = App::default();
    app.store = store;
    app.active_session = opened;

    let (tx, mut rx) = mpsc::unbounded_channel();
    spawn_frame_bridge(client.mux_stream(), tx.clone());
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut rx).await;
        (result, term)
    });
    // Let the mock's scripted frames arrive and be drawn.
    tokio::time::sleep(Duration::from_millis(400)).await;
    tx.send(AppEvent::Key(key(KeyCode::Char('q'))))
        .expect("quit key");
    let (result, term) = run_task.await.expect("run task");
    result.expect("run");

    let view = format!("{}", term.backend());
    assert!(
        view.contains("from history"),
        "user text from the history page"
    );
    assert!(view.contains("│ fn main()"), "code fence from history");
    assert!(
        view.contains("streamed hello"),
        "streamed assistant text via mux"
    );
    assert!(view.contains("session s1"), "status line shows the session");

    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 2. coalescing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn coalescing_draws_once_per_batch() {
    let mut app = App::default();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = Vec::new();
    for seq in 1..=3 {
        events.push(AppEvent::Frame(frame(
            "s1",
            ev(seq, "user/message", user_msg(&format!("m{seq}"), "hi")),
        )));
    }
    events.push(AppEvent::Tick);
    events.push(AppEvent::Key(key(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;

    // One draw for the whole batch: the first frame draws, the rest coalesce
    // until the (not-yet-elapsed) tick; the quit key does not draw.
    assert_eq!(app.draws, 1, "rapid frame batch coalesces into one draw");
}

// ---------------------------------------------------------------------------
// 3. keymap table
// ---------------------------------------------------------------------------

#[test]
fn keymap_table() {
    let cases: Vec<(KeyEvent, Option<Action>)> = vec![
        (key(KeyCode::Char('q')), Some(Action::Quit)),
        (ctrl(KeyCode::Char('c')), Some(Action::Quit)),
        (key(KeyCode::Char('j')), Some(Action::Scroll(1))),
        (key(KeyCode::Down), Some(Action::Scroll(1))),
        (key(KeyCode::Char('k')), Some(Action::Scroll(-1))),
        (key(KeyCode::Up), Some(Action::Scroll(-1))),
        (key(KeyCode::Char('g')), Some(Action::Scroll(i64::MIN))),
        (key(KeyCode::Home), Some(Action::Scroll(i64::MIN))),
        (key(KeyCode::Char('G')), Some(Action::Scroll(i64::MAX))),
        (key(KeyCode::End), Some(Action::Scroll(i64::MAX))),
        (ctrl(KeyCode::Char('d')), Some(Action::Scroll(12))),
        (ctrl(KeyCode::Char('u')), Some(Action::Scroll(-12))),
        (key(KeyCode::Esc), Some(Action::None)),
        (key(KeyCode::Char('x')), None),
    ];
    for (event, expected) in cases {
        let mut app = App::default();
        assert_eq!(app.handle_key(event), expected, "key {event:?}");
    }
}

// ---------------------------------------------------------------------------
// 4. follow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn follow_sticks_to_bottom_until_manual_scroll() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(100, 5);
    let mut term = Terminal::new(backend).unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut rx).await;
        (result, app)
    });
    for seq in 1..=5 {
        tx.send(AppEvent::Frame(frame(
            "s1",
            ev(seq, "user/message", user_msg(&format!("m{seq}"), "hi")),
        )))
        .expect("frame");
    }
    // Let a tick draw the accumulated state: follow clamps to the bottom
    // (5 rows, chat height 4 → offset 1).
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(AppEvent::Key(key(KeyCode::Char('j')))).expect("j");
    tokio::time::sleep(Duration::from_millis(20)).await;
    tx.send(AppEvent::Key(key(KeyCode::Char('q')))).expect("q");
    let (result, app) = run_task.await.expect("run task");
    result.expect("run");

    assert!(!app.view.follow, "manual scroll disables follow");
    // Followed bottom was offset 1; j moves one row past it.
    assert_eq!(app.view.offset, 2);
}

// ---------------------------------------------------------------------------
// 5. no-session attach
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_session_attach_renders_empty_chat() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[]}}}"#,
        ),
    )
    .await;
    let client = WireClient::attach(mock.port()).unwrap();
    let mut store = SessionStore::new();
    let opened = attach(&client, &mut store).await.expect("attach");
    assert_eq!(opened, None, "no sessions on the gateway");

    let mut app = App::default();
    app.store = store;
    app.last_error = Some("gateway has no sessions".into());
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    // A frame still arrives (any session); the empty chat must not panic.
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(frame("s1", ev(1, "user/message", user_msg("m1", "hi")))),
            AppEvent::Tick,
            AppEvent::Key(key(KeyCode::Char('q'))),
        ],
    )
    .await;

    let view = format!("{}", term.backend());
    assert!(view.contains("no session"), "status line: {view}");
    assert!(view.contains("no sessions"), "hint in status line: {view}");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 6. resize
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resize_invalidates_and_rerenders() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let long_text = "This is a fairly long paragraph line that will definitely wrap at narrower widths like sixty columns or so. And here is another sentence to make the paragraph long enough that the wrap counts differ clearly between widths.";
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let wide_lines_before = {
        run_with(
            &mut app,
            &mut term,
            vec![
                AppEvent::Frame(frame(
                    "s1",
                    ev(1, "user/message", user_msg("m1", long_text)),
                )),
                AppEvent::Resize(60, 15),
                AppEvent::Key(key(KeyCode::Char('q'))),
            ],
        )
        .await;
        app.row_cache.lines()[0].lines.len()
    };

    let view = format!("{}", term.backend());
    assert!(
        view.contains("long paragraph"),
        "content survives the resize"
    );

    // Re-wrap at the new width (backend resized between runs).
    term.backend_mut().resize(60, 15);
    app.running = true;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut rx).await;
        (result, app)
    });
    // Duplicate seq: ignored by the store, still triggers a draw.
    tx.send(AppEvent::Frame(frame(
        "s1",
        ev(1, "user/message", user_msg("m1", long_text)),
    )))
    .expect("frame");
    // Let a tick draw the re-wrapped state.
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(AppEvent::Key(key(KeyCode::Char('q')))).expect("q");
    let (result, app) = run_task.await.expect("run task");
    result.expect("run");
    let wide = wide_lines_before;
    let narrow = app.row_cache.lines()[0].lines.len();
    assert!(
        narrow > wide,
        "rows re-wrap at 60 columns (wide={wide}, narrow={narrow})"
    );
}
