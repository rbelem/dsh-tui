//! Queue strip + live sidebar tests: the queue dock above the composer, the
//! view-only queue popup, and host-stream session liveness. Keyless:
//! injected events + `TestBackend`; one end-to-end host-bridge test through
//! the mock gateway.

mod common;
use common::MockGateway;

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{App, AppEvent, EventChannel, spawn_host_bridge};
use dsh_tui::client::WireClient;
use dsh_tui::wire::events::{
    HostFrame, MessageRole, MuxFrame, QueueItem, QueueMessage, QueueMessageSource, QueuePlacement,
};
use dsh_tui::wire::session::{ContentBlock, MessageId, SessionId, SessionSummary};

// ---------------------------------------------------------------------------
// fixture helpers
// ---------------------------------------------------------------------------

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn alt(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::ALT)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn summary(id: &str) -> SessionSummary {
    SessionSummary {
        session_id: SessionId(id.into()),
        updated_at: 1.0,
        running: false,
        blank: false,
        parent_session_id: None,
        origin: None,
        cwd: None,
        agent_preset: None,
        projections: None,
    }
}

fn queue_item(id: &str, placement: QueuePlacement, text: &str) -> QueueItem {
    QueueItem {
        id: MessageId(id.into()),
        placement,
        message: QueueMessage {
            id: MessageId(id.into()),
            role: MessageRole::User,
            content: vec![ContentBlock {
                r#type: "text".into(),
                extra: serde_json::Map::from_iter([(
                    "text".to_string(),
                    serde_json::Value::String(text.into()),
                )]),
            }],
            source: QueueMessageSource {
                kind: "user".into(),
            },
        },
    }
}

fn queue_frame(session: &str, items: Vec<QueueItem>) -> MuxFrame {
    MuxFrame::SessionQueue {
        session_id: SessionId(session.into()),
        items,
    }
}

async fn run_with(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut channel = EventChannel::new();
    for event in events {
        channel.tx.send(event).expect("event channel");
    }
    app.run(term, &mut channel)
        .await
        .expect("run must not fail");
}

// ---------------------------------------------------------------------------
// queue strip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queue_strip_shows_count_and_preview_then_hides() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(queue_frame(
                "s1",
                vec![
                    queue_item("m1", QueuePlacement::Queued, "fix the tests"),
                    queue_item("m2", QueuePlacement::Steering, "steer me"),
                ],
            )),
            // Esc forces an immediate draw; the frame's own draw coalesces.
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    let view = format!("{}", term.backend());
    assert!(
        view.contains("2 queued · 1 steering · fix the tests"),
        "strip: {view}"
    );

    // The queue empties: the strip disappears.
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(queue_frame("s1", vec![])),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(!view.contains("queued ·"), "strip gone: {view}");
}

// ---------------------------------------------------------------------------
// queue popup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queue_popup_lists_items_and_closes_60x15() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let mut items = vec![
        queue_item("m1", QueuePlacement::Queued, "fix the tests"),
        queue_item("m2", QueuePlacement::Steering, "row 2"),
        queue_item("m3", QueuePlacement::Context, "row 3"),
    ];
    for i in 4..=10 {
        items.push(queue_item(
            &format!("m{i}"),
            QueuePlacement::Queued,
            &format!("row {i}"),
        ));
    }
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(queue_frame("s1", items)),
            AppEvent::Key(alt(KeyCode::Char('q'))),
            AppEvent::Key(key(KeyCode::Down)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert!(app.queue_popup_open, "Alt+q opened the popup");
    assert_eq!(app.queue_scroll, 1, "Down scrolled");
    let view = format!("{}", term.backend());
    assert!(view.contains("queue"), "popup title: {view}");
    assert!(
        view.contains("[steering] user · row 2"),
        "scrolled row: {view}"
    );
    assert!(
        view.contains("[context] user · row 3"),
        "scrolled row: {view}"
    );
    assert!(view.contains("row 9"), "last visible row: {view}");
    assert!(!view.contains("row 10"), "beyond the window: {view}");
    // 60 columns: the strip clips after the placement counts (38-wide
    // column); the full preview is asserted in the 120×30 strip test.
    assert!(
        view.contains("10 queued · 1 steering · 1 context"),
        "strip: {view}"
    );

    // Esc closes; the strip stays.
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.queue_popup_open, "Esc closed the popup");
    let view = format!("{}", term.backend());
    assert!(!view.contains("[steering]"), "popup closed: {view}");
    assert!(view.contains("10 queued"), "strip stays: {view}");
}

#[tokio::test]
async fn alt_q_with_empty_queue_only_hints() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(alt(KeyCode::Char('q'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.queue_popup_open, "no popup without items");
    let view = format!("{}", term.backend());
    assert!(view.contains("queue is empty"), "hint: {view}");
}

// ---------------------------------------------------------------------------
// live sidebar (host stream)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn host_session_added_lands_on_top() {
    let mut app = App::default();
    app.sessions = vec![summary("s1")];
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::HostFrame(HostFrame::HostSessionAdded {
                session_id: SessionId("s2".into()),
                blank: false,
                parent_session_id: None,
                origin: None,
                cwd: Some("/tmp/work".into()),
                agent_preset: None,
            }),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert_eq!(
        app.sessions[0].session_id,
        SessionId("s2".into()),
        "the new session lands at the top"
    );
    assert_eq!(app.sessions[0].cwd.as_deref(), Some("/tmp/work"));
    let view = format!("{}", term.backend());
    assert!(view.contains("s2"), "row visible: {view}");
    assert!(
        view.find("s2").expect("s2 row") < view.find("s1").expect("s1 row"),
        "s2 above s1: {view}"
    );
}

#[tokio::test]
async fn host_session_status_marks_running() {
    let mut app = App::default();
    app.sessions = vec![summary("s1")];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::HostFrame(HostFrame::HostSessionStatus {
                session_id: SessionId("s1".into()),
                running: true,
            }),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert!(app.sessions[0].running);
    let view = format!("{}", term.backend());
    assert!(view.contains("s1 · running"), "running marker: {view}");
}

#[tokio::test]
async fn host_session_removed_clears_the_active_chat_60x15() {
    let mut app = App::default();
    app.sessions = vec![summary("s1"), summary("s2")];
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::HostFrame(HostFrame::HostSessionRemoved {
                session_id: SessionId("s2".into()),
            }),
            AppEvent::HostFrame(HostFrame::HostSessionRemoved {
                session_id: SessionId("s1".into()),
            }),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert!(app.sessions.is_empty(), "both rows removed");
    assert_eq!(app.active_session, None, "active removal clears, no switch");
    let view = format!("{}", term.backend());
    assert!(view.contains("no session"), "empty state: {view}");
    assert!(
        view.contains("no sessions yet"),
        "sidebar empty state: {view}"
    );
}

// ---------------------------------------------------------------------------
// host bridge, end to end through the mock gateway
// ---------------------------------------------------------------------------

#[tokio::test]
async fn host_bridge_feeds_the_sidebar() {
    let mock = MockGateway::start().await;
    mock.set_ws_frames(
        "/api/events.host",
        vec![
            r#"{"type":"server-request","rpcId":"h-1","method":"events.host","payload":{"type":"host/session-added","sessionId":"s9","blank":false}}"#.to_string(),
            r#"{"type":"server-request","rpcId":"h-2","method":"events.host","payload":{"type":"host/session-status","sessionId":"s9","running":true}}"#.to_string(),
        ],
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut app = App::default();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = EventChannel::new();
    spawn_host_bridge(client.host_stream(), channel.tx.clone());
    let tx = channel.tx.clone();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut channel).await;
        (result, app)
    });
    // Let the scripted host frames arrive and be handled.
    tokio::time::sleep(Duration::from_millis(300)).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    let (result, app) = run_task.await.expect("run task");
    result.expect("run");

    let row = app
        .sessions
        .iter()
        .find(|summary| summary.session_id == SessionId("s9".into()))
        .expect("s9 added via the host bridge");
    assert!(row.running, "status frame applied");
    mock.stop().await;
}
