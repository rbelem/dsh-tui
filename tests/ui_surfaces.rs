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
    // Ctrl+Q is the always-quit binding (Q15: Ctrl+C now cancels a running
    // turn instead of quitting).
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
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
    // The hint is 20 chars; the 22-col pane's 2/2 padding leaves 18 content
    // columns, so at the minimum sidebar width it truncates (#11).
    assert!(view.contains("they'll appear"), "hint: {view}");
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
// #11 acceptance: pane gap, selection stripe, status alignment
// ---------------------------------------------------------------------------

/// Acceptance 8: at 80 columns the sidebar and the chat are separated by a
/// 1-cell gap column showing the main bg — no rule character.
#[tokio::test]
async fn sidebar_chat_gap_column_at_80x24() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false)];
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(80, 24);
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

    let buffer = term.backend().buffer();
    // Sidebar = cols 0..22, gap = col 22, chat starts at 23. The gap cell
    // must be a plain space (bg contrast), never a border glyph.
    let gap = buffer.cell((22, 2)).expect("gap cell at the header row");
    assert_eq!(gap.symbol(), " ", "gap shows bg, not a rule: {gap:?}");
    let header = buffer.cell((2, 0)).expect("sidebar header start");
    assert_eq!(
        header.symbol(),
        "S",
        "2/2 padding: header at col 2: {header:?}"
    );
    let edge = buffer.cell((21, 2)).expect("sidebar edge cell");
    assert_eq!(
        edge.symbol(),
        " ",
        "sidebar edge is padding, not a border glyph: {edge:?}"
    );
    let chat = buffer.cell((23, 2)).expect("chat cell");
    assert_eq!(chat.symbol(), " ", "chat starts after the gap: {chat:?}");
}

/// Acceptance 9: below 80 columns the gap drops to 0 — the chat starts
/// immediately after the 22-col sidebar, exactly as before #11.
#[tokio::test]
async fn sidebar_chat_gap_drops_below_80() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false)];
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(79, 24);
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
    let buffer = term.backend().buffer();
    let chat_edge = buffer.cell((22, 2)).expect("chat starts at col 22");
    assert_eq!(chat_edge.symbol(), " ", "no gap column: {chat_edge:?}");
}

/// Acceptance 4: the sidebar's selected row is identifiable by glyph +
/// weight — the accent `▎` stripe — never color alone.
#[tokio::test]
async fn sidebar_selected_row_carries_the_stripe() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false), summary("s2", false)];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Sidebar;
    let backend = TestBackend::new(80, 24);
    let mut term = Terminal::new(backend).unwrap();
    // No Esc: it would drop focus back to the chat before the final draw
    // (the selection only renders while the sidebar has focus). F(1) forces
    // a draw with the sidebar focused; Ctrl+Q quits without redrawing.
    let mut events = vec![AppEvent::Key(key(KeyCode::F(1)))];
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;

    let view = format!("{}", term.backend());
    assert!(view.contains("▎"), "selection stripe: {view}");
    assert!(view.contains("▎● s1"), "stripe + active marker: {view}");
    assert!(
        view.contains("   s2"),
        "unselected row keeps the spacer column: {view}"
    );
}

/// Acceptance 10: the status indicator stays right-aligned (and the left
/// cluster absorbs the truncation) down to width 40.
#[tokio::test]
async fn status_indicator_right_aligned_at_width_40() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false)];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Chat;
    let backend = TestBackend::new(40, 15);
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
    // Height 15: chat 12 + composer 2 + status 1 — the status is row 14.
    // buffer_view wraps each row in quotes; strip them before trimming.
    let status = view
        .lines()
        .nth(14)
        .expect("status row")
        .trim_matches('"')
        .trim_end();
    assert!(
        status.ends_with('●'),
        "idle indicator right-aligned at 40 cols: {status:?}"
    );
    assert!(view.contains("session s1"), "left context: {view}");
}

/// Acceptance 5: the status indicators carry their semantic colors — the
/// braille spinner in accent (running), `●` success (idle), `✕` error.
#[tokio::test]
async fn status_indicators_carry_semantic_colors() {
    let dark = dsh_tui::theme::Theme::from_toml_str(include_str!("../themes/dsh-dark.toml"))
        .expect("dsh-dark");
    // Drive one app instance to a draw and read its status-row indicator.
    async fn indicator_cell(app: &mut App) -> (String, ratatui::style::Color) {
        let backend = TestBackend::new(100, 15);
        let mut term = Terminal::new(backend).unwrap();
        // F(1) forces the draw; Ctrl+Q quits without redrawing.
        run_with(
            app,
            &mut term,
            vec![
                AppEvent::Key(key(KeyCode::F(1))),
                AppEvent::Key(ctrl(KeyCode::Char('q'))),
            ],
        )
        .await;
        let buffer = term.backend().buffer();
        let y = 14u16; // status row (chat 12 + composer 2 + status 1)
        for x in 0..100u16 {
            if let Some(cell) = buffer.cell((x, y))
                && matches!(cell.symbol(), "●" | "⠋" | "✕" | "△")
            {
                return (cell.symbol().to_string(), cell.fg);
            }
        }
        panic!("no indicator glyph on the status row");
    }

    // Idle: ● in success.
    let mut app = App::default();
    app.theme = dark.clone();
    app.sessions = vec![summary("s1", false)];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Chat;
    let (glyph, fg) = indicator_cell(&mut app).await;
    assert_eq!(glyph, "●");
    assert_eq!(fg, dark.success, "idle indicator = success");

    // Running: ⠋ in accent.
    let mut app = App::default();
    app.theme = dark.clone();
    app.sessions = vec![summary("s1", true)];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Chat;
    let (glyph, fg) = indicator_cell(&mut app).await;
    assert_eq!(glyph, "⠋");
    assert_eq!(fg, dark.accent, "running indicator = accent");

    // Error: ✕ in error (beats the running spinner).
    let mut app = App::default();
    app.theme = dark;
    app.sessions = vec![summary("s1", true)];
    app.active_session = Some(SessionId("s1".into()));
    app.last_error = Some("boom".into());
    app.focus = Focus::Chat;
    let (glyph, fg) = indicator_cell(&mut app).await;
    assert_eq!(glyph, "✕");
    assert_eq!(fg, app.theme.error, "error indicator = error");
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
    let mut events = Vec::new(); // the app boots in the composer — no Tab
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
    let mut events = Vec::new(); // the app boots in the composer — no Tab
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
    // Down + Enter inserts the second mirrored command (/compact — the
    // catalog order is /help, /compact, /clear, ...).
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
    assert_eq!(app.composer.buffer(), "/compact ");
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
    let mut events = Vec::new(); // the app boots in the composer — no Tab
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
    assert_eq!(app.focus, Focus::Composer, "boot focuses the input area");
    let expected = [
        (Focus::Sidebar, "sidebar"),
        (Focus::Chat, "chat"),
        (Focus::Composer, "composer"),
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
