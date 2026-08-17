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

/// A chat-focus app with two sessions and s1 active (the selection tests'
/// fixture; s1 receives the streamed messages).
fn app_with_session() -> App {
    let mut app = App::default();
    app.sessions = vec![summary("s1", false), summary("s2", false)];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Chat; // 'v'/'q' chat keys (boot is Composer)
    app
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
    for _ in 0..12 {
        events.push(wheel_up(60, 10)); // 35 → 0 (the top bound)
    }
    for _ in 0..10 {
        events.push(wheel_down(60, 10)); // over the chat: 0 → 30
    }
    events.push(draw_force()); // a final draw reflects the scrolled state
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;

    // 30 messages + 29 #11 inter-message blanks = 59 lines; the chat pane
    // is 25 rows (content 24 — header 1 + two status rows) → bottom lock
    // at 35. A 10-event burst = 30.
    // (12 wheel-ups from 35 hit the top bound at 0.)
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
        app.view.offset, 35,
        "wheel never passes total - chat_height (59 - 24)"
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
            // The composer sits at rows 26-27 now (header + two status
            // rows) — a click at row 26 is its first content row.
            down(28, 26),
            up(28, 26),
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
            drag(26, 28), // the composer row: clamped to content row 23
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    let (_, current) = app.selection.expect("selection active");
    assert_eq!(current.row, 23, "clamped to the last chat content row");
    // The overlay never renders outside the chat rect: buffer rows below
    // the chat area carry no REVERSED cells.
    for y in 26..30u16 {
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
        .cell((26, 2))
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

// ---------------------------------------------------------------------------
// #21/#22/#23: text-anchored selection, the width cache, word select
// ---------------------------------------------------------------------------

/// #21: wheel during an active selection keeps the anchors on the TEXT —
/// the dragged position maps through the scroll delta (extending over the
/// underlying text), and mouse-up copies the text-anchored range even
/// when the viewport moved past it.
#[tokio::test]
async fn wheel_extends_the_selection_over_the_text() {
    let mut app = app_with_session();
    for seq in 1..=40 {
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
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('v'))),
            draw_force(),
            // The boot draw follow-clamps to the bottom (offset 53); the
            // test starts from the top so the anchors are deterministic.
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)), // 55 → 0
            down(25, 1),                                              // content (0,0) → abs line 0
            drag(25, 4),                                              // content (3,0) → abs line 3
            draw_force(),
            wheel_down(60, 10), // scroll 3 lines: offset 0 → 3
            wheel_down(60, 10), // offset 3 → 6
            wheel_down(60, 10), // offset 6 → 9
            wheel_down(60, 10), // offset 9 → 12
            wheel_down(60, 10), // offset 12 → 15
            draw_force(),
            // The drag AFTER the wheel maps through the new offset: the
            // same screen cell now anchors at abs line 18 (the wheel
            // extended the selection over the underlying text).
            drag(25, 4),
            up(25, 4),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    // The anchors stayed text-fixed through the wheel burst (rows are
    // absolute); the copy covers the anchored range 0..17 — 18 entries
    // (text-1..text-9 with 9 interleaved blanks; the end row is a blank
    // line, so its column-0 slice is empty) = 9×6 + 9 + 17 separators.
    let flash = app.copied_flash.as_ref().expect("flash after release");
    assert_eq!(
        flash.0, "copied · 71 chars",
        "the text-anchored range copied (not the screen highlight)"
    );
}

/// #21: after scrolling, the overlay clips to the viewport — the anchored
/// range above the window highlights nothing, and the drag position still
/// maps through the offset.
#[tokio::test]
async fn text_anchored_overlay_clips_to_the_viewport() {
    let mut app = app_with_session();
    for seq in 1..=40 {
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
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('v'))),
            draw_force(),
            // The boot draw follow-clamps to the bottom; scroll back to 0
            // (18 × 3 = 54 ≥ 53) so the anchors are deterministic.
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)),
            AppEvent::Mouse(mouse(MouseEventKind::ScrollUp, 60, 10)), // 55 → 0
            down(25, 1),                                              // abs line 0
            drag(25, 4),                                              // abs line 3
            draw_force(),
            wheel_down(60, 10), // offset → 3
            wheel_down(60, 10), // offset → 6
            wheel_down(60, 10), // offset → 9
            wheel_down(60, 10), // offset → 12
            wheel_down(60, 10), // offset → 15
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    // The anchors are unchanged (text-fixed) — rows 0..2 — but the
    // viewport now starts at line 15, so nothing highlights.
    assert_eq!(
        app.selection,
        Some((
            dsh_tui::app::CellPos { row: 0, col: 0 },
            dsh_tui::app::CellPos { row: 2, col: 0 },
        )),
        "anchors text-fixed through the wheel"
    );
    for y in 0..30u16 {
        for x in 0..120u16 {
            if let Some(cell) = term.backend().buffer().cell((x, y))
                && cell.modifier.contains(Modifier::REVERSED)
            {
                panic!("anchored range above the viewport must not highlight ({x},{y})");
            }
        }
    }
}

/// #21: a down-click on the chat margins (the 2/2 padding + top blank row)
/// in select mode anchors deterministically at the clamped edge — a drag
/// can always start (the old behavior left the mode armed with a dead
/// drag).
#[tokio::test]
async fn margin_click_anchors_deterministically() {
    let mut app = app_with_session();
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
            // The left margin (content.x = 25): the anchor clamps to col 0.
            down(24, 1),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(
        app.selection,
        Some((
            dsh_tui::app::CellPos { row: 0, col: 0 },
            dsh_tui::app::CellPos { row: 0, col: 0 },
        )),
        "margin click anchors at the clamped edge"
    );

    // The drag works from the margin start (extends to abs line 2).
    let mut app = app_with_session();
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
            down(24, 1), // margin anchor
            drag(27, 4), // content (0,2), col 2 → abs line 2
            up(27, 4),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let flash = app.copied_flash.as_ref().expect("flash");
    // Lines 0..2: "hi", "", "hi" → "hi\n\nhi".
    assert_eq!(flash.0, "copied · 6 chars", "margin-started drag copies");
}

/// #23: double-click selects the word under the cursor — selection intent
/// is implicit (no `v` needed); mouse-up copies the word.
#[tokio::test]
async fn double_click_selects_the_word() {
    let mut app = app_with_session();
    app.store
        .ingest(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "hello world foo")),
        ))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            // "world" spans cells 6..11 of line 0 (content x = 25).
            down(32, 1), // cell 7, inside "world"
            down(32, 1), // the double-click
            up(32, 1),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    // The up copies and exits the mode (like any drag); the flash proves
    // the double-click selected the word.
    assert!(!app.select_mode, "mode exits after the copy");
    let flash = app.copied_flash.as_ref().expect("flash");
    assert_eq!(flash.0, "copied · 5 chars", "the word copied");
}

/// #23: the word boundaries are CJK-safe — a double-click inside a CJK
/// run selects the whole run (wide chars never split).
#[tokio::test]
async fn double_click_selects_a_cjk_run() {
    let mut app = app_with_session();
    app.store
        .ingest(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "你好 世界")),
        ))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            // "世界" spans cells 5..9 (wide chars); click on 世 (cell 5).
            down(30, 1),
            down(30, 1),
            up(30, 1),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let flash = app.copied_flash.as_ref().expect("flash");
    assert_eq!(flash.0, "copied · 2 chars", "the CJK run copied");
}

/// #23: a drag after the double-click extends the word selection WORD-WISE
/// — the moving edge snaps to word boundaries (past mid-word → the next
/// word edge; before mid-word → the word start); the anchor edge stays
/// word-fixed.
#[tokio::test]
async fn drag_extends_from_the_word_selection() {
    let mut app = app_with_session();
    app.store
        .ingest(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "hello world foo")),
        ))
        .expect("ingest");
    app.store
        .ingest(frame("s1", ev(2, "user/message", user_msg("m2", "bar"))))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            down(32, 1), // "world" (line 0, cells 6..11)
            down(32, 1), // double-click
            // "bar" is cells 0..3 of line 2; col 2 is past its mid-point
            // → the moving edge snaps to the word end (col 3).
            drag(27, 4),
            up(27, 4),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let flash = app.copied_flash.as_ref().expect("flash");
    // The start row runs to its line end from the word anchor:
    // "world foo" + blank + the snapped word "bar" → 9 + 1 + 1 + 3.
    assert_eq!(
        flash.0, "copied · 14 chars",
        "past-mid-word drag snaps to the word end"
    );

    // Before mid-word (col 1 of "bar"): the edge snaps back to the word
    // start (col 0) — that row contributes nothing.
    let mut app = app_with_session();
    app.store
        .ingest(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "hello world foo")),
        ))
        .expect("ingest");
    app.store
        .ingest(frame("s1", ev(2, "user/message", user_msg("m2", "bar"))))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            down(32, 1),
            down(32, 1),
            drag(26, 4), // col 1 of "bar" → snaps to the word start
            up(26, 4),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let flash = app.copied_flash.as_ref().expect("flash");
    assert_eq!(
        flash.0, "copied · 11 chars",
        "before-mid-word drag snaps to the word start"
    );
}

/// #22: the selection overlay's line-widths are cached per render — the
/// key tracks the scroll offset and the row cache's render generation, so
/// a live selection doesn't rescan the transcript every draw.
#[tokio::test]
async fn selection_widths_are_cached_per_render() {
    let mut app = app_with_session();
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
            drag(28, 3),
            draw_force(), // the overlay render populates the cache
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let (offset, generation, widths) = app
        .selection_widths_cache
        .clone()
        .expect("cache populated by the selection draw");
    assert_eq!(offset, app.view.offset, "cached at the current offset");
    assert_eq!(generation, app.row_cache.generation(), "generation keyed");
    assert_eq!(widths.len(), 5, "one width per flat line");

    // A later draw with the same state reuses the cached key.
    let before = app.selection_widths_cache.clone();
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    assert_eq!(app.selection_widths_cache, before, "key unchanged");

    // Scrolling moves the key's offset (the widths recompute for the new
    // window).
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('k'))), // scroll up 1
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(
        app.selection_widths_cache.as_ref().expect("cache").0,
        app.view.offset,
        "the key follows the scroll offset"
    );
}

/// #22 (reviewer follow-up): streaming growth under a live selection — a
/// same-count width change (the last line grows without wrapping) must
/// bust the width cache, so the highlight tracks the growing line.
#[tokio::test]
async fn streaming_growth_reclamps_the_highlight() {
    let mut app = app_with_session();
    app.store
        .ingest(frame("s1", ev(1, "user/message", user_msg("m1", "hello"))))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('v'))),
            draw_force(),
            // Select cols 4..20 of line 0; the 5-wide line clamps the
            // highlight to [4,5).
            down(29, 1),
            drag(45, 1),
            draw_force(),
            // Stream the same message id with longer text (a new seq
            // replaces the node — the flat count stays 1, but the render
            // generation bumps).
            AppEvent::Frame(frame(
                "s1",
                ev(2, "user/message", user_msg("m1", "hello world foo bar")),
            )),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    // The highlight now spans [4,19) of the 19-wide line — a stale
    // (offset, flat-count) key would still clamp at 5 and leave col 15
    // unhighlighted.
    let highlighted = term
        .backend()
        .buffer()
        .cell((25 + 15, 2))
        .expect("cell at col 15")
        .modifier
        .contains(Modifier::REVERSED);
    assert!(highlighted, "the highlight tracks the streamed line width");
}

// ---------------------------------------------------------------------------
// #31: skill-list block fold — header click toggles
// ---------------------------------------------------------------------------

/// #31: a `## Skills` message folds to one header row by default; a click
/// on the header row expands it (header + item rows), a second click
/// collapses it. A header click never starts a selection.
#[tokio::test]
async fn skill_header_click_toggles_the_fold() {
    let mut app = app_with_session();
    app.store
        .ingest(frame(
            "s1",
            ev(
                1,
                "user/message",
                user_msg(
                    "m1",
                    "## Skills\n- bash — run shell commands\n- git — version control",
                ),
            ),
        ))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;

    // Collapsed by default: the message renders as exactly one header row.
    let view = format!("{}", term.backend());
    assert!(view.contains("▸ 2 skills"), "folded header: {view}");
    assert!(
        !view.contains("bash — run shell commands"),
        "items hidden while folded: {view}"
    );

    // Click the header row (content row 0 = buffer row 1) → expands.
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            down(26, 1),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("▾ 2 skills"), "expanded header: {view}");
    assert!(
        view.contains("bash — run shell commands"),
        "items visible while expanded: {view}"
    );
    assert_eq!(app.selection, None, "a header click never selects");

    // Click again → collapses.
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            down(26, 1),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("▸ 2 skills"), "collapsed again: {view}");
    assert!(
        !view.contains("bash — run shell commands"),
        "items hidden again: {view}"
    );
}

/// #31 review: the cached skill_header is the POST-WRAP line — at a
/// narrow width where the intro wraps, the click on the rendered header
/// row toggles, and one row above (an intro wrap line) does NOT.
#[tokio::test]
async fn wrapped_skill_header_click_lands_on_the_header_row() {
    let mut app = app_with_session();
    app.store
        .ingest(frame(
            "s1",
            ev(
                1,
                "user/message",
                user_msg(
                    "m1",
                    "Here is a fairly long introductory paragraph that wraps                      at narrow widths.\n\n## Skills\n- bash — run shell commands\n- git — version control",
                ),
            ),
        ))
        .expect("ingest");
    let backend = TestBackend::new(40, 20);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;

    // The cached header index is post-wrap: the intro wrapped above it.
    let header_line = app.row_cache.lines()[0].skill_header.expect("skill header");
    assert!(
        header_line > 1,
        "the intro wrapped above the header (line {header_line})"
    );

    // Click the rendered header row (content row = header_line, buffer
    // y = 1 + row) → expands.
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            down(26, 2 + header_line as u16),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("▾ 2 skills"), "header click expanded: {view}");
    assert!(
        view.contains("bash — run shell commands"),
        "items visible: {view}"
    );

    // One row above the header is an intro wrap line — clicking it does
    // not toggle (and without `v` it starts no selection either).
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            down(26, 1 + header_line as u16), // the row above the header
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(
        view.contains("▾ 2 skills"),
        "the row above the header does not toggle: {view}"
    );
    assert_eq!(app.selection, None, "no selection from the miss");
}

// ---------------------------------------------------------------------------
// #39: tool node fold — header click toggles
// ---------------------------------------------------------------------------

/// #39: a settled tool node folds to a one-line summary by default; a
/// click on its header row expands it (header + literal output), a second
/// click collapses it. A header click never starts a selection.
#[tokio::test]
async fn tool_header_click_toggles_the_fold() {
    let mut app = app_with_session();
    app.store
        .ingest(frame(
            "s1",
            ev(
                1,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": r#"{"cmd":"ls"}"#}),
            ),
        ))
        .expect("ingest");
    app.store
        .ingest(frame(
            "s1",
            ev(
                2,
                "tool/result",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "r1", "role": "user",
                        "content": [{"type": "tool-result", "toolCallId": "c1", "content": [{"type": "text", "text": "file.txt\nfile2.txt"}], "isError": false}],
                        "source": {"kind": "tool", "callId": "c1"},
                    },
                }),
            ),
        ))
        .expect("ingest");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;

    // Folded by default: the tool renders as exactly one header row.
    let view = format!("{}", term.backend());
    assert!(view.contains("▸ [tool] bash"), "folded header: {view}");
    assert!(
        !view.contains("file.txt"),
        "output hidden while folded: {view}"
    );

    // Click the header row (content row 0 = buffer row 1) → expands.
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            down(26, 1),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("▾ [tool] bash"), "expanded header: {view}");
    assert!(
        view.contains("file.txt"),
        "output visible while expanded: {view}"
    );
    assert_eq!(app.selection, None, "a header click never selects");

    // Click again → collapses.
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            down(26, 1),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("▸ [tool] bash"), "collapsed again: {view}");
    assert!(!view.contains("file.txt"), "output hidden again: {view}");
}
