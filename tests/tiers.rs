//! Responsive-tier pins (issue #19): the drawer tier (60–79), the status
//! variants (60 / 39), the too-small screen (<32 with live restore), and
//! the popup width clamps.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::app::{Action, App, AppEvent, EventChannel, Focus};
use dsh_tui::theme::Theme;
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId, SessionSummary};

// ---------------------------------------------------------------------------
// fixture helpers (mirrors tests/ui_surfaces.rs)
// ---------------------------------------------------------------------------

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

fn mouse_down(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn drag(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn mouse_up(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// The inert draw-force key: F(1).
fn draw_force() -> AppEvent {
    AppEvent::Key(key(KeyCode::F(1)))
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

fn app_with_session() -> App {
    let mut app = App::default();
    app.sessions = vec![summary("s1"), summary("s2")];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Chat; // 's'/'q' chat keys (boot is Composer)
    app
}

// ---------------------------------------------------------------------------
// acceptance 1: the drawer tier at 70 cols
// ---------------------------------------------------------------------------

#[tokio::test]
async fn drawer_tier_at_70_cols() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),                           // draw: the tier + areas latch (70 < 80)
            AppEvent::Key(key(KeyCode::Char('s'))), // open the drawer
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert!(app.drawer_open, "s opened the drawer");
    assert_eq!(app.focus, Focus::Sidebar, "opening moves focus into it");
    let view = format!("{}", term.backend());
    assert!(view.contains("Sessions"), "drawer header: {view}");
    assert!(view.contains("● s1"), "drawer rows: {view}");
    // The drawer is min(width, 30) cols: at 70 it is 30 wide — its top
    // border row starts at col 0 and the `≡` affordance doubles as the
    // corner while open.
    assert!(
        view.lines().next().is_some_and(|line| line.contains('─')),
        "drawer box renders: {view}"
    );
    // No layout shift: the chat/composer/status rects are untouched by
    // the drawer (asserted via the app state the hit-testing reads).
    assert_eq!(app.chat_area.width, 70, "chat fills the terminal");
    assert_eq!(app.composer_area.x, 0, "composer starts at col 0");
}

#[tokio::test]
async fn drawer_closes_via_esc_click_outside_and_session_select() {
    // Esc closes and restores the prior focus.
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "Esc closed the drawer");
    assert_eq!(app.focus, Focus::Chat, "prior focus restored");

    // A session click selects AND closes.
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            draw_force(),
            // The drawer inner starts at (1,1); rows from y=3 (header,
            // blank): row 4 = s2 (flat list).
            mouse_down(5, 4),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "session click closed the drawer");
    assert_eq!(
        app.active_session,
        Some(SessionId("s2".into())),
        "session click selected the row"
    );

    // Click-outside closes.
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            draw_force(),
            mouse_down(50, 10), // the chat, right of the 30-col drawer
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "click-outside closed the drawer");
}

// ---------------------------------------------------------------------------
// acceptance 2: status variants at 60 and 39
// ---------------------------------------------------------------------------

/// The trimmed status row (the last buffer row, minus the buffer_view
/// quotes).
fn status_row(term: &Terminal<TestBackend>) -> String {
    let view = format!("{}", term.backend());
    view.lines()
        .last()
        .expect("status row")
        .trim_matches('"')
        .trim()
        .to_string()
}

#[tokio::test]
async fn status_at_60_is_session_id_only() {
    // #29: insta-pinned — the session-id-only left cluster + the full
    // indicator cluster at 60 cols.
    let mut app = app_with_session();
    app.store
        .ingest(frame("s1", ev(1, "user/message", user_msg("m1", "hi"))))
        .expect("ingest");
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    insta::assert_snapshot!("status-60", format!("{}", term.backend()));
}

#[tokio::test]
async fn status_at_39_is_indicators_only() {
    // #29: insta-pinned — at 39 cols the left cluster is hidden; only the
    // right indicator cluster renders.
    let mut app = app_with_session();
    app.store
        .ingest(frame("s1", ev(1, "user/message", user_msg("m1", "hi"))))
        .expect("ingest");
    let backend = TestBackend::new(39, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    insta::assert_snapshot!("status-39", format!("{}", term.backend()));
}

/// #38/#39: at ≥80 the full left cluster gains the context meter and the
/// session stats bar (turns · steps | in · out | cache) — driven by the
/// store's aggregated usage + the last `request/context` window; both hide
/// when the session has no data.
#[tokio::test]
async fn status_wide_shows_context_meter_and_stats_bar() {
    let mut app = app_with_session();
    app.store
        .ingest(frame(
            "s1",
            ev(
                1,
                "request/context",
                json!({"provider": "p", "model": "m", "contextWindow": 50}),
            ),
        ))
        .expect("ingest");
    app.store
        .ingest(frame(
            "s1",
            ev(
                2,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "a1", "role": "assistant",
                        "content": [{"type": "text", "text": "hi"}],
                        "source": {"kind": "model", "provider": "p", "model": "m"},
                    },
                    "usage": {"inputTokens": 10, "outputTokens": 3, "cacheReadTokens": 5},
                }),
            ),
        ))
        .expect("ingest");
    let backend = TestBackend::new(140, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    let row = status_row(&term);
    // used = input + cache_read = 15 of the 50-token window → 30%.
    assert!(row.contains("ctx 30%"), "meter: {row}");
    // The full stats cluster fits at 140 cols (sidebar 22 + gap 1).
    assert!(
        row.contains("1 turns · 1 steps | in 10 · out 3 | cache 33%"),
        "stats bar: {row}"
    );
}

// ---------------------------------------------------------------------------
// acceptance 3: the too-small screen at 31 with live restore
// ---------------------------------------------------------------------------

#[tokio::test]
async fn too_small_screen_at_31_with_live_restore() {
    let dark = Theme::from_toml_str(include_str!("../themes/dsh-dark.toml")).expect("dsh-dark");
    let mut app = app_with_session();
    app.theme = dark.clone();
    let backend = TestBackend::new(31, 10);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("terminal too small"), "title: {view}");
    assert!(view.contains("widen or rotate to continue"), "hint: {view}");
    // Themed tokens: the wordmark is accent + text at 31×10 (centered:
    // y = 0 + (10-3)/2 = 3; x = 0 + (31-27)/2 = 2).
    let accent_cell = term.backend().buffer().cell((2, 3)).expect("dsh cell");
    assert_eq!(accent_cell.symbol(), "d", "wordmark: {accent_cell:?}");
    assert_eq!(accent_cell.fg, dark.accent, "dsh in accent");
    let text_cell = term.backend().buffer().cell((5, 3)).expect("-tui cell");
    assert_eq!(text_cell.fg, dark.text, "-tui in text");

    // Only q works below 32: j is a no-op (no scroll, no panic).
    assert_eq!(app.handle_key(key(KeyCode::Char('j'))), Some(Action::None));

    // Live restore: resize the backend back to a chat-tier width and run
    // the same app again — the prior screen returns without a restart.
    term.backend_mut().resize(80, 24);
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(
        view.contains("● s1"),
        "the sidebar is back after the resize: {view}"
    );
    assert!(view.contains("type a message"), "composer back: {view}");
}

// ---------------------------------------------------------------------------
// acceptance 4: no popup exceeds the terminal width
// ---------------------------------------------------------------------------

#[test]
fn popup_widths_clamp_to_the_terminal() {
    // The theme picker at a 20-col terminal clamps to 20 (was 28+).
    let themes = dsh_tui::theme::ThemeRegistry::bundled();
    let picker = dsh_tui::theme::ThemePopup {
        themes: &themes.themes,
        selected: 0,
        current: &Theme::default(),
        locale: dsh_tui::i18n::Locale::En,
    };
    let (width, _) = picker.size(20);
    assert_eq!(width, 20, "theme picker ≤ terminal width");

    // The other popups cap at the available width too.
    let launcher = dsh_tui::ui::launcher::LauncherPopup {
        entries: &[],
        selected: 0,
        search: &"query".repeat(10),
        loading: false,
        theme: &Theme::default(),
        locale: dsh_tui::i18n::Locale::En,
    };
    assert_eq!(launcher.size(20, 20).0, 20, "launcher ≤ terminal width");

    let search = dsh_tui::ui::search::SidebarSearchPopup {
        query: &"q".repeat(10),
        results: &[],
        selected: 0,
        sending: false,
        theme: &Theme::default(),
        locale: dsh_tui::i18n::Locale::En,
    };
    assert_eq!(search.size(20, 20).0, 20, "search ≤ terminal width");

    let new_session = dsh_tui::ui::new_session::NewSessionPopup {
        entries: &[],
        selected: 0,
        sending: false,
        theme: &Theme::default(),
        locale: dsh_tui::i18n::Locale::En,
    };
    assert_eq!(new_session.size(20, 20).0, 20, "new-session ≤ terminal");

    let queue = dsh_tui::ui::queue::QueuePopup {
        items: &[],
        scroll: 0,
        theme: &Theme::default(),
        locale: dsh_tui::i18n::Locale::En,
        editor: None,
    };
    assert_eq!(queue.size(20, 20).0, 20, "queue ≤ terminal width");
    // The queue popup still fills the available width in the happy range.
    assert_eq!(queue.size(80, 20).0, 64, "queue caps at its max width");
}

// ---------------------------------------------------------------------------
// acceptance 5: ≥80 behavior is byte-identical — spot pins
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wide_tier_keeps_the_permanent_sidebar() {
    let mut app = app_with_session();
    let backend = TestBackend::new(80, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("Sessions"), "permanent sidebar: {view}");
    assert!(view.contains("● s1"), "sidebar rows: {view}");
    // `s` is inert at ≥80 (the permanent sidebar exists; no drawer).
    app.running = true;
    app.focus = Focus::Chat;
    assert_eq!(
        app.handle_key(key(KeyCode::Char('s'))),
        None,
        "s is a no-op at ≥80"
    );
    assert!(!app.drawer_open);
    assert_eq!(app.hint, None, "#30: no drawer hint at ≥80");
    // The affordance does not render.
    assert!(!view.contains('≡'), "no affordance at ≥80: {view}");
}

// ---------------------------------------------------------------------------
// corner coverage: truncation, affordance click, drawer keys, selection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compact_status_truncates_a_long_session_id() {
    let long_id =
        "a-very-long-session-identifier-that-overflows-the-status-area-entirely-1234567890";
    let mut app = app_with_session();
    app.sessions[0].session_id = SessionId(long_id.into());
    app.active_session = Some(SessionId(long_id.into()));
    let backend = TestBackend::new(70, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    let status = status_row(&term);
    assert!(
        status.starts_with("session a-very-long-") && status.contains('…'),
        "#19: the id truncates with …: {status:?}"
    );
    assert!(status.ends_with('●'), "indicator intact: {status:?}");
}

#[tokio::test]
async fn affordance_click_toggles_the_drawer() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            mouse_down(0, 0), // the ≡ at the chat's top-left
            draw_force(),
            mouse_down(0, 0),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "the affordance toggles open then closed");
}

#[tokio::test]
async fn drawer_header_click_does_not_select() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            draw_force(),
            mouse_down(5, 1), // the "Sessions" header row
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(
        app.active_session,
        Some(SessionId("s1".into())),
        "header clicks never select"
    );
    // Any click inside the drawer dismisses it (click-to-dismiss); only a
    // session click changes the selection.
    assert!(!app.drawer_open, "inside-click dismisses the drawer");
}

#[tokio::test]
async fn drawer_navigation_keys_clamp_to_the_list() {
    let mut app = app_with_session();
    app.sessions = (0..40).map(|i| summary(&format!("s{i}"))).collect();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('k'))),
            AppEvent::Key(key(KeyCode::Char('g'))),
            AppEvent::Key(key(KeyCode::Char('G'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.sidebar.selected, 39, "j/j/k/g/G navigate and clamp");
}

#[tokio::test]
async fn s_types_in_the_composer_at_narrow_widths() {
    let mut app = App::default(); // boot focus is the composer
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        !app.drawer_open,
        "s in the composer is typing, not the drawer"
    );
    assert_eq!(app.composer.buffer(), "s", "the key was typed");
}

#[tokio::test]
async fn empty_text_selection_copies_nothing() {
    let mut app = app_with_session();
    app.store
        .ingest(frame("s1", ev(1, "user/message", user_msg("m1", "hi"))))
        .expect("ingest");
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('v'))),
            draw_force(),
            // A zero-width selection at "hi"[1..1] → no text to copy.
            mouse_down(26, 1),
            mouse_up(26, 1),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.select_mode, "the mode still exits");
    assert_eq!(app.copied_flash, None, "nothing copied");
}

#[tokio::test]
async fn multi_row_selection_copies_joined_lines() {
    let mut app = app_with_session();
    for seq in 1..=3 {
        app.store
            .ingest(frame(
                "s1",
                ev(seq, "user/message", user_msg(&format!("m{seq}"), "hi")),
            ))
            .expect("ingest");
    }
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('v'))),
            draw_force(),
            // Anchor (0,1) → current (2,3): "i" + blank + "hi".
            mouse_down(26, 1),
            drag(28, 3),
            mouse_up(28, 3),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let flash = app.copied_flash.as_ref().expect("flash");
    assert_eq!(flash.0, "copied · 5 chars", "multi-row extraction");
}

#[tokio::test]
async fn too_small_q_still_quits() {
    let mut app = app_with_session();
    let backend = TestBackend::new(31, 10);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![draw_force(), AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    assert_eq!(
        app.handle_key(key(KeyCode::Char('q'))),
        Some(Action::Quit),
        "plain q quits on the too-small screen"
    );
}

#[tokio::test]
async fn v_toggled_off_clears_the_hint() {
    let mut app = app_with_session();
    app.focus = Focus::Chat;
    assert_eq!(app.handle_key(key(KeyCode::Char('v'))), Some(Action::None));
    assert!(app.select_mode);
    assert_eq!(app.hint.as_deref(), Some("v select · esc cancel"));
    // A second v disarms and clears the hint.
    assert_eq!(app.handle_key(key(KeyCode::Char('v'))), Some(Action::None));
    assert!(!app.select_mode);
    assert_eq!(app.hint, None);
    assert_eq!(app.selection, None);
}

#[tokio::test]
async fn empty_drawer_clicks_are_inert() {
    // An empty session list: the drawer has no rows — a click in the row
    // area is a no-op (no panic, nothing selected).
    let mut app = App::default();
    app.focus = Focus::Chat;
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            draw_force(),
            mouse_down(5, 4), // a row slot below the empty state
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.sessions.is_empty());
    assert_eq!(app.active_session, None, "no session to select");
}

#[tokio::test]
async fn paste_is_dropped_outside_chat_mode() {
    let mut app = app_with_session();
    app.focus = Focus::Composer;
    app.mode = dsh_tui::ui::takeover::Mode::Settings(dsh_tui::ui::settings::SettingsState::new());
    assert_eq!(app.handle_paste("nope".into()), Action::None);
    assert!(app.composer.is_empty(), "paste gated outside chat mode");
}

#[tokio::test]
async fn s_while_open_closes_the_drawer() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            AppEvent::Key(key(KeyCode::Char('s'))), // toggle closed
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "s toggles the drawer closed");
    assert_eq!(app.focus, Focus::Chat, "prior focus restored");
}

#[tokio::test]
async fn selection_past_the_tail_copies_what_exists() {
    let mut app = app_with_session();
    for seq in 1..=3 {
        app.store
            .ingest(frame(
                "s1",
                ev(seq, "user/message", user_msg(&format!("m{seq}"), "hi")),
            ))
            .expect("ingest");
    }
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('v'))),
            draw_force(),
            // Drag far past the 6 cached lines (rows 0..5): the tail rows
            // contribute nothing.
            mouse_down(26, 1),
            drag(28, 11),
            mouse_up(28, 11),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let flash = app.copied_flash.as_ref().expect("flash");
    assert_eq!(flash.0, "copied · 9 chars", "past-tail rows are skipped");
}

#[tokio::test]
async fn workspace_header_click_does_not_toggle_archived() {
    let mut app = app_with_session();
    app.workspaces = vec![dsh_tui::wire::workspace::WorkspaceView {
        workspace_id: dsh_tui::wire::session::WorkspaceId("wA".into()),
        path: "/tmp/wA".into(),
        title: "alpha".into(),
        session_ids: vec![SessionId("s1".into()), SessionId("s2".into())],
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            // The workspace header row (row 2 of the permanent sidebar).
            mouse_down(2, 2),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        !app.archived_expanded,
        "a workspace header click only toggles the archived group"
    );
    assert_eq!(
        app.active_session,
        Some(SessionId("s1".into())),
        "header clicks never select"
    );
}

#[tokio::test]
async fn drawer_enter_selects_and_closes() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            AppEvent::Key(key(KeyCode::Char('j'))), // s2
            AppEvent::Key(key(KeyCode::Enter)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "Enter closed the drawer");
    assert_eq!(
        app.active_session,
        Some(SessionId("s2".into())),
        "Enter selected the drawer row"
    );
    assert_eq!(app.focus, Focus::Chat, "prior focus restored");
}

#[tokio::test]
async fn backward_drag_normalizes_the_range() {
    let mut app = app_with_session();
    for seq in 1..=3 {
        app.store
            .ingest(frame(
                "s1",
                ev(seq, "user/message", user_msg(&format!("m{seq}"), "hi")),
            ))
            .expect("ingest");
    }
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('v'))),
            draw_force(),
            // Drag upward: anchor (2,3) → current (0,1) — the range
            // normalizes to the same text as the forward drag.
            mouse_down(28, 3),
            drag(26, 1),
            mouse_up(26, 1),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let flash = app.copied_flash.as_ref().expect("flash");
    assert_eq!(flash.0, "copied · 5 chars", "backward range normalized");
}

#[tokio::test]
async fn expired_copied_flash_is_cleared() {
    let mut app = app_with_session();
    app.copied_flash = Some((
        "copied · 1 chars".into(),
        std::time::Instant::now() - std::time::Duration::from_secs(10),
    ));
    app.expire_copied_flash();
    assert_eq!(app.copied_flash, None, "aged flash expires");
}

fn wheel_down(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// #26: every tier-CHANGING resize closes the drawer — the <32
/// round-trip included (70 → 31 → 70 leaves no stale drawer state).
#[tokio::test]
async fn drawer_closes_on_the_too_small_round_trip() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))), // open the drawer
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.drawer_open, "drawer open at 70");

    // Shrink below 32: a tier-changing resize closes the drawer.
    term.backend_mut().resize(31, 10);
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Resize(31, 10),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "70 → 31 closed the drawer");
    assert_eq!(app.focus, Focus::Chat, "focus restored");

    // Grow back into the drawer tier: no stale state reappears.
    term.backend_mut().resize(70, 24);
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Resize(70, 24),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "31 → 70 stays closed");
    // The drawer is still reachable: `s` opens it fresh.
    app.focus = Focus::Chat;
    assert_eq!(app.handle_key(key(KeyCode::Char('s'))), Some(Action::None));
    assert!(app.drawer_open, "s reopens the drawer after the round-trip");
}

/// #26: a same-tier resize keeps the drawer open (no boundary crossed).
#[tokio::test]
async fn same_tier_resize_keeps_the_drawer() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))), // open the drawer
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.drawer_open);

    // 70 → 60: still the drawer tier — the drawer survives the resize.
    term.backend_mut().resize(60, 24);
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Resize(60, 24),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.drawer_open, "same-tier resize keeps the drawer");
    assert_eq!(app.focus, Focus::Sidebar, "drawer still owns focus");
}

/// #15: the running spinner animates on ticks (the frame counter advances
/// per tick and the status glyph is a pure function of it — the
/// wall-clock repaint cadence is the existing DRAW_INTERVAL budget, so the
/// deterministic pins are the counter and the idle quiescence: idle ticks
/// schedule no repaints).
#[tokio::test]
async fn spinner_animates_while_running_and_stays_quiet_when_idle() {
    let mut app = app_with_session();
    app.sessions[0].running = true;
    let backend = TestBackend::new(100, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Tick,
            AppEvent::Tick,
            AppEvent::Tick,
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    // The frame advanced on each tick (10-frame cycle).
    assert_eq!(app.spinner_frame, 3, "frame advances per tick");
    // The glyph the status line shows is a pure function of the frame.
    let frame = dsh_tui::app::run::SPINNER_FRAMES[app.spinner_frame % 10];
    assert!(!frame.is_empty(), "a spinner frame is selected");

    // Idle (no running session): ticks advance the counter but schedule
    // no repaints — draws stay at exactly the one F(1) forced.
    let mut app = app_with_session();
    let backend = TestBackend::new(100, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Tick,
            AppEvent::Tick,
            AppEvent::Tick,
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.spinner_frame, 3, "counter advances regardless");
    assert_eq!(app.draws, 1, "idle ticks schedule no repaint");
}

/// #30: the drawer discoverability hint — appears on the FIRST open of
/// the run (in the compact status left cluster), clears on close, and
/// never nags on subsequent opens.
#[tokio::test]
async fn drawer_hint_shows_once_and_clears_on_close() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))), // first open
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(
        view.contains("s sessions · esc close"),
        "first open shows the hint: {view}"
    );

    // Close clears the hint.
    app.focus = Focus::Chat;
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.drawer_open);
    assert_eq!(app.hint, None, "hint cleared on close");

    // A second open does not re-show it (once per run).
    app.focus = Focus::Chat;
    assert_eq!(app.handle_key(key(KeyCode::Char('s'))), Some(Action::None));
    assert!(app.drawer_open);
    assert_eq!(app.hint, None, "hint is one-time");
}

/// #30: the drawer hint is i18n'd (zh).
#[tokio::test]
async fn drawer_hint_is_localized() {
    let mut app = app_with_session();
    app.locale = dsh_tui::i18n::Locale::Zh;
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("s 会话 · esc 关闭"), "zh hint: {view}");
}

/// #30: the once-per-run hint flag must not burn invisibly below 40 cols
/// (where the status left cluster is hidden) — a first open at 35 leaves
/// the flag unset, so a later open at 70 still shows the hint.
#[tokio::test]
async fn drawer_hint_flag_survives_a_below_40_first_open() {
    let mut app = app_with_session();
    // First open at 35 cols: the hint cannot render (no left cluster).
    let backend = TestBackend::new(35, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.drawer_open, "drawer opens at 35");
    assert!(!app.drawer_hint_shown, "the flag must not burn below 40");
    assert_eq!(app.hint, None, "no hint set below 40");

    // Close explicitly, then reopen at 70: the hint still shows (the flag
    // was never set by the below-40 open).
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.drawer_open);
    term.backend_mut().resize(70, 24);
    app.running = true;
    app.focus = Focus::Chat;
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))), // reopen at 70
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.drawer_open, "drawer reopened at 70");
    assert!(app.drawer_hint_shown, "flag set at 70");
    let view = format!("{}", term.backend());
    assert!(
        view.contains("s sessions · esc close"),
        "hint renders at 70: {view}"
    );
}

/// #30: closing the drawer restores an armed select-mode hint (the drawer
/// hint overwrote it while open; `v` mode stays active).
#[tokio::test]
async fn drawer_close_restores_the_select_hint() {
    let mut app = app_with_session();
    app.focus = Focus::Chat;
    // Arm selection mode: the `v select · esc cancel` hint shows.
    assert_eq!(app.handle_key(key(KeyCode::Char('v'))), Some(Action::None));
    assert!(app.select_mode);
    assert_eq!(app.hint.as_deref(), Some("v select · esc cancel"));

    // Open the drawer (the drawer hint replaces the select hint) …
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))),
            AppEvent::Key(key(KeyCode::Esc)), // … and close it
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "drawer closed");
    assert!(app.select_mode, "select mode is still armed");
    assert_eq!(
        app.hint.as_deref(),
        Some("v select · esc cancel"),
        "the select hint is restored"
    );
}

/// Wheel over the OPEN drawer scrolls the drawer's list (the drawer inner
/// rect sits inside `chat_area`; the wheel path must check it first).
#[tokio::test]
async fn wheel_over_the_open_drawer_scrolls_the_drawer() {
    let mut app = app_with_session();
    app.sessions = (0..40).map(|i| summary(&format!("s{i}"))).collect();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))), // open the drawer
            draw_force(),
            wheel_down(5, 10), // inside the drawer
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(app.sidebar.selected, 3, "drawer list scrolled by 3");
    assert_eq!(app.view.offset, 0, "the chat underneath did NOT scroll");
}

/// The drawer never leaks across the tier boundary: an open drawer at 70
/// closes on the resize to ≥80 and the keys work normally after.
#[tokio::test]
async fn drawer_closes_on_resize_to_the_wide_tier() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))), // open the drawer
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.drawer_open, "drawer open at 70");

    // Resize to the wide tier: the drawer closes and focus is restored.
    term.backend_mut().resize(80, 24);
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Resize(80, 24),
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.drawer_open, "resize ≥80 closed the drawer");
    assert_eq!(app.focus, Focus::Chat, "prior focus restored");
    // Keys are live again: `v` arms selection instead of being swallowed.
    assert_eq!(app.handle_key(key(KeyCode::Char('v'))), Some(Action::None));
    assert!(app.select_mode, "chat keys work after the resize");
}

/// Below 32 the too-small screen gates mouse and paste too — the stale
/// wide-draw rects must not select, and the invisible composer must not
/// receive paste.
#[tokio::test]
async fn too_small_gates_mouse_and_paste() {
    let mut app = app_with_session();
    let backend = TestBackend::new(70, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),
            AppEvent::Key(key(KeyCode::Char('s'))), // open: sidebar rect set
            draw_force(),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.drawer_open, "drawer open with live rects");

    // Shrink below 32: the too-small screen draws; the stale drawer rect
    // must not hit-test.
    term.backend_mut().resize(31, 10);
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            draw_force(),     // too-small draw: terminal_width latches
            mouse_down(5, 4), // would select s2 through the stale rect
            mouse_up(5, 4),
            AppEvent::Paste("zzz".into()),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(
        app.active_session,
        Some(SessionId("s1".into())),
        "clicks are inert below 32"
    );
    assert!(app.composer.is_empty(), "paste is inert below 32");
    assert_eq!(app.selection, None, "no selection below 32");
}
