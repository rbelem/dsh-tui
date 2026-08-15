//! Mouse support tests (issue #12): sidebar click-to-select + group-header
//! toggle, wheel scroll (3 lines/event, clamped), composer click-to-cursor,
//! `v` + drag + release → OSC 52 copy + status flash, selection clamping to
//! the chat rect, popup-open gating, and paste-into-composer.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use serde_json::json;

use dsh_tui::app::{Action, App, AppEvent, EventChannel, Focus};
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId, SessionSummary};

// ---------------------------------------------------------------------------
// fixture helpers (mirrors tests/ui_surfaces.rs)
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

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn down(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row))
}

fn drag(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(mouse(MouseEventKind::Drag(MouseButton::Left), column, row))
}

fn up(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(mouse(MouseEventKind::Up(MouseButton::Left), column, row))
}

fn wheel_down(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(mouse(MouseEventKind::ScrollDown, column, row))
}

fn wheel_up(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, column, row))
}

/// The inert draw-force key: F(1) (a printable would type into the
/// composer; see the mock-harness skill).
fn draw_force() -> AppEvent {
    AppEvent::Key(key(KeyCode::F(1)))
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

// Layout constants at 120x30 (verified against run.rs draw):
// sidebar cols 0..21 (inner x=2, rows from y=2), gap col 22, right pane
// cols 23..119; chat rows 0..26 (content y=1), composer rows 27..28
// (content x=25), status row 29.

// ---------------------------------------------------------------------------
// acceptance 1: sidebar click-to-select + group-header toggle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sidebar_click_selects_the_row() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false), summary("s2", false)];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Composer;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(), // draws: hit-test areas stored
            down(2, 3),   // flat list: row 3 = s2
            up(2, 3),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert_eq!(
        app.active_session,
        Some(SessionId("s2".into())),
        "click selected s2"
    );
    assert_eq!(app.sidebar.selected, 1, "selection follows the click");
    // The click never steals the composer's focus.
    assert_eq!(app.focus, Focus::Composer);
}

#[tokio::test]
async fn sidebar_click_on_the_active_row_is_a_noop() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false), summary("s2", false)];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Composer;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            down(2, 2), // row 2 = s1 (the active row)
            up(2, 2),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert_eq!(
        app.active_session,
        Some(SessionId("s1".into())),
        "active row click is a no-op"
    );
    assert_eq!(app.focus, Focus::Composer, "focus untouched");
}

#[tokio::test]
async fn sidebar_click_toggles_the_archived_group_header() {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false), summary("s2", false)];
    app.active_session = Some(SessionId("s1".into()));
    // s2 is archived: groups = [ungrouped, archived]; the archived header
    // is the last group and the only collapsible one in the v1 model.
    app.archived_session_ids = vec![SessionId("s2".into())];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            down(2, 4), // row 4 = the "▸ archived (1)" header
            up(2, 4),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert!(app.archived_expanded, "header click expanded the group");
}

// ---------------------------------------------------------------------------
// acceptance 2: wheel scroll — 3 lines/event, clamped at the bounds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wheel_burst_scrolls_30_lines_in_one_draw() {
    let mut app = App::default();
    app.focus = Focus::Chat; // F(1)/q are chat keys
    app.active_session = Some(SessionId("s1".into()));
    for seq in 1..=30 {
        app.store
            .ingest(frame(
                "s1",
                ev(
                    seq,
                    "user/message",
                    user_msg(&format!("m{seq}"), &format!("text-{seq}")),
                ),
            ))
            .expect("ingest");
    }
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = vec![draw_force()]; // draw: areas + cache sync (boots
    // follow-locked at offset 33)
    for _ in 0..11 {
        events.push(wheel_up(60, 10)); // 33 → 0 (the top bound)
    }
    for _ in 0..10 {
        events.push(wheel_down(60, 10)); // over the chat: 0 → 30
    }
    events.push(draw_force()); // a final draw reflects the scrolled state
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;

    // 30 messages + 29 #11 inter-message blanks = 59 lines; the chat pane
    // is 27 rows (content 26) → bottom lock at 33. A 10-event burst = 30.
    assert_eq!(app.view.offset, 30, "10 wheel events × 3 lines");
    assert!(!app.view.follow, "wheel disables follow");

    // One redraw per tick: the burst coalesces (the final F(1) forces the
    // one draw the buffer captures).
    let view = format!("{}", term.backend());
    assert!(
        view.contains("text-16") && !view.contains("text-1 "),
        "viewport shows the scrolled window: {view}"
    );
}

#[tokio::test]
async fn wheel_clamps_at_the_bottom_bound() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.active_session = Some(SessionId("s1".into()));
    for seq in 1..=30 {
        app.store
            .ingest(frame(
                "s1",
                ev(
                    seq,
                    "user/message",
                    user_msg(&format!("m{seq}"), &format!("text-{seq}")),
                ),
            ))
            .expect("ingest");
    }
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = vec![draw_force()];
    for _ in 0..200 {
        events.push(wheel_down(60, 10)); // far past the end
    }
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;

    assert_eq!(
        app.view.offset, 33,
        "wheel never passes total - chat_height (59 - 26)"
    );
    // And back up: the top bound is 0.
    let mut events = vec![draw_force()];
    for _ in 0..200 {
        events.push(wheel_up(60, 10));
    }
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;
    assert_eq!(app.view.offset, 0, "wheel never goes above the top");
}

#[tokio::test]
async fn wheel_over_the_sidebar_scrolls_the_list_when_overflowing() {
    let mut app = App::default();
    app.sessions = (0..40).map(|i| summary(&format!("s{i}"), false)).collect();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = vec![draw_force()];
    events.push(wheel_down(2, 10)); // over the sidebar
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;

    // 40 flat rows + header/blank/footer: the window is 27 rows, so the
    // list overflows; the selection-driven window moved by 3.
    assert_eq!(app.sidebar.selected, 3, "sidebar wheel scrolled 3 rows");

    // A short list does not scroll.
    let mut app = App::default();
    app.sessions = vec![summary("s1", false), summary("s2", false)];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = vec![draw_force()];
    events.push(wheel_down(2, 10));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;
    assert_eq!(app.sidebar.selected, 0, "no overflow → no scroll");
}

#[tokio::test]
async fn wheel_over_status_or_hero_is_a_noop() {
    let mut app = App::default(); // no session: the hero fills the chat
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            wheel_down(60, 10), // over the hero
            wheel_down(60, 29), // over the status row
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.view.offset, 0, "hero/status wheel is a no-op");
}

// ---------------------------------------------------------------------------
// acceptance 3: composer click-to-cursor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn composer_click_focuses_and_places_the_caret() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.composer.set_text("hello world");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            // Composer content x = 25; a click at col 28 → char col 3.
            down(28, 28),
            up(28, 28),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert_eq!(app.focus, Focus::Composer, "click focused the composer");
    assert_eq!(
        app.composer.caret_position(),
        (0, 3),
        "caret at the clicked cell"
    );
}

#[tokio::test]
async fn composer_click_clamps_to_the_line_end() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.composer.set_text("hello world");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            down(100, 28), // far past the text
            up(100, 28),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(
        app.composer.caret_position(),
        (0, 11),
        "click past the text clamps to the line end"
    );
}

// ---------------------------------------------------------------------------
// acceptance 4: `v` + drag + release → OSC 52 copy + status flash
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v_toggle_shows_the_select_hint() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.active_session = Some(SessionId("s1".into()));
    app.store
        .ingest(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "hello world")),
        ))
        .expect("ingest");
    assert_eq!(app.handle_key(key(KeyCode::Char('v'))), Some(Action::None));
    assert!(app.select_mode);
    assert_eq!(
        app.hint.as_deref(),
        Some("v select · esc cancel"),
        "status hint while armed"
    );
    // Esc cancels (and clears the hint).
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.select_mode);
    assert_eq!(app.hint, None);
}

#[tokio::test]
async fn drag_selects_and_release_copies_via_osc52() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.active_session = Some(SessionId("s1".into()));
    app.store
        .ingest(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "hello world")),
        ))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('v'))), // arm selection mode
            draw_force(),                           // draw (areas + hint)
            down(26, 1),                            // content (0, 1): "e"
            drag(30, 1),                            // content (0, 5): " "
            draw_force(),                           // overlay visible now
            up(30, 1),                              // copy + exit
            draw_force(),                           // flash visible now
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    // "hello world"[1..5] = "ello" → 4 chars.
    let flash = app
        .copied_flash
        .as_ref()
        .expect("copied flash after release");
    assert_eq!(flash.0, "copied · 4 chars", "flash text");
    assert!(!app.select_mode, "release exits select mode");
    assert_eq!(app.hint, None, "hint cleared after copy");
    assert_eq!(app.selection, None, "selection cleared after copy");

    // The final buffer shows the flash in the status row and no REVERSED
    // cells (the selection is gone).
    let view = format!("{}", term.backend());
    assert!(view.contains("copied · 4 chars"), "flash rendered: {view}");
    for y in 0..30u16 {
        for x in 0..120u16 {
            if let Some(cell) = term.backend().buffer().cell((x, y))
                && cell.modifier.contains(Modifier::REVERSED)
            {
                panic!("REVERSED survives the copy at ({x},{y})");
            }
        }
    }
}

#[tokio::test]
async fn drag_without_v_is_a_noop() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.active_session = Some(SessionId("s1".into()));
    app.store
        .ingest(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "hello world")),
        ))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            down(26, 1),
            drag(60, 10),
            up(60, 10),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.selection, None, "no selection without v");
    assert_eq!(app.copied_flash, None, "no copy without v");
}

// ---------------------------------------------------------------------------
// acceptance 5: selection clamps to the chat rect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn drag_into_the_composer_clamps_at_the_chat_boundary() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.active_session = Some(SessionId("s1".into()));
    for seq in 1..=3 {
        app.store
            .ingest(frame(
                "s1",
                ev(seq, "user/message", user_msg(&format!("m{seq}"), "hi")),
            ))
            .expect("ingest");
    }
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('v'))),
            draw_force(),
            down(26, 1),
            drag(26, 28), // the composer row: clamped to content row 25
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    let (_, current) = app.selection.expect("selection active");
    assert_eq!(current.row, 25, "clamped to the last chat content row");
    // The overlay never renders outside the chat rect: buffer rows below
    // the chat area carry no REVERSED cells.
    for y in 27..30u16 {
        for x in 0..120u16 {
            if let Some(cell) = term.backend().buffer().cell((x, y))
                && cell.modifier.contains(Modifier::REVERSED)
            {
                panic!("selection rendered below the chat at ({x},{y})");
            }
        }
    }
    // ...and the rows inside the chat that ARE selected carry it. The
    // anchor is content (0, 1), so row 0's highlight is col 1 only.
    let selected = term
        .backend()
        .buffer()
        .cell((26, 1))
        .expect("cell in the selection");
    assert!(
        selected.modifier.contains(Modifier::REVERSED),
        "the range is highlighted"
    );
}

// ---------------------------------------------------------------------------
// acceptance 6: popup-open gating + paste
// ---------------------------------------------------------------------------

#[tokio::test]
async fn popup_open_routes_mouse_and_paste_to_noop() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.active_session = Some(SessionId("s1".into()));
    app.sessions = vec![summary("s1", false), summary("s2", false)];
    app.store
        .ingest(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "hello world")),
        ))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('t'))), // theme picker open
            draw_force(),
            down(2, 3),         // sidebar click: no-op
            wheel_down(60, 10), // chat wheel: no-op
            down(26, 1),        // chat anchor: no-op
            up(26, 1),
            AppEvent::Paste("zzz".into()), // paste: no-op (popup open)
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert!(app.theme_picker.open);
    assert_eq!(
        app.active_session,
        Some(SessionId("s1".into())),
        "sidebar click gated by the popup"
    );
    assert_eq!(app.view.offset, 0, "wheel gated by the popup");
    assert_eq!(app.selection, None, "selection gated by the popup");
    assert_eq!(app.composer.buffer(), "", "paste gated by the popup");
}

#[tokio::test]
async fn paste_inserts_only_when_the_composer_is_focused() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    assert_eq!(
        app.handle_paste("ignored".into()),
        Action::None,
        "chat focus: paste dropped"
    );
    assert!(app.composer.is_empty());

    app.focus = Focus::Composer;
    app.handle_paste("hello ".into());
    app.handle_paste("world".into());
    assert_eq!(app.composer.buffer(), "hello world");
    assert_eq!(app.composer.caret_position(), (0, 11));
}

// ---------------------------------------------------------------------------
// acceptance 7: right-click is a no-op (and the keyboard path is intact)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn right_click_is_a_noop() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.active_session = Some(SessionId("s1".into()));
    app.sessions = vec![summary("s1", false)];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Right), 2, 3)),
            AppEvent::Mouse(mouse(MouseEventKind::Up(MouseButton::Right), 2, 3)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(
        app.active_session,
        Some(SessionId("s1".into())),
        "right-click does not select"
    );
}

/// Moved events (every pointer motion while capture is on) must not
/// schedule draws — there is no hover chrome, so repainting per motion
/// would flood the 16ms redraw budget.
#[tokio::test]
async fn moved_events_do_not_schedule_draws() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(), // one draw (also stores the hit-test areas)
            AppEvent::Mouse(mouse(MouseEventKind::Moved, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::Moved, 61, 11)),
            AppEvent::Mouse(mouse(MouseEventKind::Moved, 62, 12)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.draws, 1, "Moved events do not schedule draws");
}
