//! UI surface tests: sidebar (with sessions, empty, collapsed below 60
//! columns), composer typing, the seeded `/` popup, submit dispatch to the
//! mock gateway, sidebar session switching, and focus cycling. Keyless:
//! injected events + `TestBackend` only.

mod common;
use common::{MockAction, MockGateway};

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::app::{Action, App, AppEvent, EventChannel, Focus};
use dsh_tui::client::WireClient;
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId, SessionSummary};

// ---------------------------------------------------------------------------
// fixture helpers
// ---------------------------------------------------------------------------

fn summary(id: &str, running: bool) -> SessionSummary {
    SessionSummary {
        session_id: SessionId(id.into()),
        updated_at: 1.0,
        running,
        blank: false,
        parent_session_id: None,
        origin: None,
        cwd: None,
        agent_preset: None,
        projections: None,
    }
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn type_text(events: &mut Vec<AppEvent>, text: &str) {
    for c in text.chars() {
        events.push(AppEvent::Key(key(KeyCode::Char(c))));
    }
}

/// Feed buffered events into a fresh channel and run the loop to completion
/// (the quit event breaks it).
async fn run_with(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut channel = EventChannel::new();
    for event in events {
        channel.tx.send(event).expect("event channel");
    }
    app.run(term, &mut channel)
        .await
        .expect("run must not fail");
}

/// Draw the current state (Esc forces an immediate draw) and quit. In the
/// composer `Esc` only returns focus to the chat, so quit via Ctrl+C.
async fn draw_and_quit(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut events = events;
    events.push(AppEvent::Key(key(KeyCode::Esc)));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('c'))));
    run_with(app, term, events).await;
}

// ---------------------------------------------------------------------------
// sidebar
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sidebar_with_sessions_120x30() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false), summary("s2", true)];
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(
        &mut app,
        &mut term,
        vec![AppEvent::Frame(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "chat body")),
        ))],
    )
    .await;

    let view = format!("{}", term.backend());
    assert!(view.contains("Sessions"), "header: {view}");
    assert!(view.contains("● s1"), "active marker: {view}");
    assert!(view.contains("s2 · running"), "running suffix: {view}");
    assert!(
        view.contains("chat body"),
        "chat renders beside the sidebar"
    );
}

#[tokio::test]
async fn sidebar_empty_60x15() {
    let mut app = App::default();
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(&mut app, &mut term, vec![]).await;

    let view = format!("{}", term.backend());
    assert!(view.contains("no sessions yet"), "empty state: {view}");
    assert!(view.contains("they'll appear here"), "hint: {view}");
}

#[tokio::test]
async fn sidebar_collapses_below_60_columns() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false)];
    let backend = TestBackend::new(50, 15);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(&mut app, &mut term, vec![]).await;

    let view = format!("{}", term.backend());
    assert!(
        !view.contains("Sessions"),
        "sidebar hidden at 50 cols: {view}"
    );
}

// ---------------------------------------------------------------------------
// composer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn composer_typed_text_120x30() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = vec![AppEvent::Key(key(KeyCode::Tab))]; // chat → composer
    type_text(&mut events, "hello world");
    draw_and_quit(&mut app, &mut term, events).await;

    let view = format!("{}", term.backend());
    assert!(view.contains("hello world"), "typed text: {view}");
    assert!(view.contains("focus: chat"), "Esc returned focus: {view}");
}

#[tokio::test]
async fn composer_placeholder_when_empty() {
    let mut app = App::default();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(&mut app, &mut term, vec![]).await;

    let view = format!("{}", term.backend());
    assert!(view.contains("type a message"), "placeholder: {view}");
}

#[tokio::test]
async fn slash_seed_popup_opens_120x30() {
    let mut app = App::default();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = vec![AppEvent::Key(key(KeyCode::Tab))];
    type_text(&mut events, "/");
    // No trailing Esc: it would dismiss the popup before the last draw.
    // Ctrl+C quits without redrawing, so the buffer keeps the popup draw.
    events.push(AppEvent::Key(ctrl(KeyCode::Char('c'))));
    run_with(&mut app, &mut term, events).await;

    let view = format!("{}", term.backend());
    assert!(view.contains("commands"), "popup title: {view}");
    assert!(view.contains("/compact"), "seeded item: {view}");
    assert!(view.contains("/help"), "seeded item: {view}");
}

#[tokio::test]
async fn popup_enter_accepts_and_esc_closes() {
    let mut app = App::default();
    app.focus = Focus::Composer;
    app.composer.insert_char('/');
    // Down + Enter inserts the second seeded item.
    assert_eq!(
        app.handle_key(key(KeyCode::Down)),
        Some(Action::Input),
        "popup nav"
    );
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Action::Input),
        "popup accept"
    );
    assert_eq!(app.composer.buffer(), "/clear ");
    assert_eq!(app.composer.popup(), None, "accept closes the popup");

    // Reopen and dismiss with Esc; the buffer is untouched.
    app.composer.take();
    app.composer.insert_char('@');
    assert!(app.composer.popup().is_some());
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Some(Action::None));
    assert_eq!(app.composer.buffer(), "@");
    assert_eq!(app.composer.popup(), None, "dismissed until the next edit");
}

#[tokio::test]
async fn shift_enter_inserts_newline() {
    let mut app = App::default();
    app.focus = Focus::Composer;
    for c in "one".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    for c in "two".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.composer.buffer(), "one\ntwo");
    assert_eq!(app.composer.line_count(), 2);
}

// ---------------------------------------------------------------------------
// submit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn composer_submit_posts_session_prompt() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.prompt",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"accepted":true}}}"#,
        ),
    )
    .await;

    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.sessions = vec![summary("s1", false)];
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = vec![AppEvent::Key(key(KeyCode::Tab))];
    type_text(&mut events, "hello gateway");
    events.push(AppEvent::Key(key(KeyCode::Enter)));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('c'))));
    run_with(&mut app, &mut term, events).await;

    // The prompt RPC is dispatched from a spawned task — give it a moment.
    let mut prompt = None;
    for _ in 0..50 {
        let requests = mock.requests().await;
        prompt = requests.into_iter().find(|r| r.method == "session.prompt");
        if prompt.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let prompt = prompt.expect("session.prompt reached the gateway");
    let body: serde_json::Value = serde_json::from_str(&prompt.body).expect("json body");
    assert_eq!(body["payload"]["sessionId"], "s1");
    assert_eq!(body["payload"]["mode"], "queue");
    assert_eq!(
        body["payload"]["content"],
        json!([{"type": "text", "text": "hello gateway"}]),
        "one text part, queue mode"
    );
    assert!(app.composer.is_empty(), "buffer cleared after submit");
    mock.stop().await;
}

#[tokio::test]
async fn enter_is_blocked_while_the_turn_is_running() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", true)]; // running flag from the list
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Composer;
    for c in "wait".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Action::None),
        "Enter is a no-op while running"
    );
    assert_eq!(app.composer.buffer(), "wait", "buffer kept");
    assert_eq!(
        app.hint.as_deref(),
        Some("turn running — wait for it to finish")
    );

    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(&mut app, &mut term, vec![]).await;
    let view = format!("{}", term.backend());
    assert!(view.contains("running"), "status shows running: {view}");
    assert!(
        view.contains("wait for it to finish"),
        "status shows the hint: {view}"
    );
}

// ---------------------------------------------------------------------------
// sidebar switching + focus cycling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sidebar_enter_switches_the_active_session() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false), summary("s2", false)];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Sidebar;
    assert_eq!(
        app.handle_key(key(KeyCode::Char('j'))),
        Some(Action::Select),
        "move to s2"
    );
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Action::SwitchSession(SessionId("s2".into())))
    );
    assert_eq!(app.active_session, Some(SessionId("s2".into())));

    // The chat renders the newly active session's nodes, not the old one's.
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(frame(
                "s1",
                ev(1, "user/message", user_msg("m1", "s1 says this")),
            )),
            AppEvent::Frame(frame(
                "s2",
                ev(1, "user/message", user_msg("m2", "s2 says that")),
            )),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("s2 says that"), "new session renders: {view}");
    assert!(!view.contains("s1 says this"), "old session gone: {view}");
    assert!(view.contains("session s2"), "status follows: {view}");
    assert!(view.contains("● s2"), "active marker moved: {view}");
}

#[tokio::test]
async fn tab_cycles_focus_through_all_surfaces() {
    let mut app = App::default();
    assert_eq!(app.focus, Focus::Chat);
    let expected = [
        (Focus::Composer, "composer"),
        (Focus::Sidebar, "sidebar"),
        (Focus::Chat, "chat"),
    ];
    for (focus, label) in expected {
        assert_eq!(
            app.handle_key(key(KeyCode::Tab)),
            Some(Action::Focus(focus)),
            "Tab → {label}"
        );
        assert_eq!(app.focus, focus);
    }
}
