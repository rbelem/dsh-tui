//! Trajectory view-mode toggle tests (#44): `T` (shift+t, rebindable via
//! `[keymap] trajectory-toggle`) switches the chat area between the chat
//! transcript and the trajectory ledger, renders the ledger content, and is
//! inert while the composer has focus (the composer keeps typing `T`).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::app::{Action, App, AppEvent, EventChannel, Focus, ViewMode};
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
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

/// An app with one scripted session (user prompt + tool round-trip).
fn trajectory_app() -> App {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    app.store
        .ingest(frame(
            "s1",
            ev(
                1,
                "user/message",
                json!({"id": "m1", "role": "user", "content": [{"type": "text", "text": "list files"}], "source": {"kind": "user"}}),
            ),
        ))
        .expect("ingest user");
    app.store
        .ingest(frame(
            "s1",
            ev(
                2,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": "{\"command\": \"ls\"}"}),
            ),
        ))
        .expect("ingest tool");
    app.store
        .ingest(frame(
            "s1",
            ev(
                3,
                "tool/result",
                json!({"turn": 1, "step": 1, "message": {"id": "tr1", "content": [{"type": "tool-result", "toolCallId": "c1", "content": [{"type": "text", "text": "a.txt"}]}], "source": {"kind": "tool", "callId": "c1"}}, "error": null, "meta": null}),
            ),
        ))
        .expect("ingest result");
    app
}

/// Feed buffered events into a fresh channel and run the loop to completion.
async fn run_with(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut channel = EventChannel::new();
    for event in events {
        channel.tx.send(event).expect("event channel");
    }
    app.run(term, &mut channel)
        .await
        .expect("run must not fail");
}

/// The `T` toggle flips the chat area to the trajectory ledger and back.
#[tokio::test]
async fn toggle_switches_between_chat_and_trajectory() {
    let mut app = trajectory_app();
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();

    // Toggle to the trajectory ledger, draw, assert the ledger content.
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(shift(KeyCode::Char('T'))),
            AppEvent::Key(key(KeyCode::F(1))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.view_mode, ViewMode::Trajectory, "toggled to trajectory");
    let view = format!("{}", term.backend());
    assert!(view.contains("TOOL bash"), "tool row: {view}");
    assert!(view.contains("→ a.txt"), "result: {view}");
    assert!(
        view.contains("Load earlier history"),
        "trajectory pager: {view}"
    );

    // Toggle back: the chat transcript renders the user prompt.
    let mut app = app;
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(shift(KeyCode::Char('T'))),
            AppEvent::Key(key(KeyCode::F(1))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.view_mode, ViewMode::Chat, "toggled back to chat");
    let view = format!("{}", term.backend());
    assert!(view.contains("list files"), "chat transcript back: {view}");
}

/// Trajectory mode without an active session renders the empty-chat hero
/// (the same fallback the chat view uses).
#[tokio::test]
async fn trajectory_without_session_shows_the_hero() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(shift(KeyCode::Char('T'))),
            AppEvent::Key(key(KeyCode::F(1))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.view_mode, ViewMode::Trajectory);
    let view = format!("{}", term.backend());
    assert!(
        view.contains("dsh-tui"),
        "hero renders without a session: {view}"
    );
}

/// Trajectory-mode scrolling (`j`/`g`) clamps against the LEDGER's row
/// count, not the chat's cached lines, and `G` re-follows the tail.
#[tokio::test]
async fn trajectory_scroll_clamps_to_ledger_rows() {
    let mut app = trajectory_app();
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();

    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(shift(KeyCode::Char('T'))),
            // g (top) then j — the clamps run in trajectory row space.
            AppEvent::Key(key(KeyCode::Char('g'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(shift(KeyCode::Char('T'))),
            AppEvent::Key(key(KeyCode::Char('G'))),
            AppEvent::Key(shift(KeyCode::Char('T'))),
            AppEvent::Key(shift(KeyCode::Char('T'))),
            AppEvent::Key(key(KeyCode::F(1))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.view_mode, ViewMode::Chat);
    let view = format!("{}", term.backend());
    assert!(view.contains("list files"), "back to the chat tail: {view}");
}

/// The toggle is inert while the composer has focus (typing `T` stays
/// typing); it works from the sidebar focus too.
#[test]
fn toggle_is_composer_gated_and_sidebar_usable() {
    let mut app = App::default();
    app.focus = Focus::Composer;
    // A plain lowercase `t` never toggles (it is not a `T` chord), and the
    // matching key stays a typed char in the composer.
    assert_eq!(
        app.handle_key(shift(KeyCode::Char('T'))),
        Some(Action::Input),
        "the composer keeps typing: the toggle never fires"
    );
    assert_eq!(app.view_mode, ViewMode::Chat, "unchanged in composer");

    // Sidebar focus toggles (per the web's any-surface parity).
    app.focus = Focus::Sidebar;
    assert_eq!(
        app.handle_key(shift(KeyCode::Char('T'))),
        Some(Action::None)
    );
    assert_eq!(app.view_mode, ViewMode::Trajectory);
    // And back.
    app.handle_key(shift(KeyCode::Char('T')));
    assert_eq!(app.view_mode, ViewMode::Chat);
}
