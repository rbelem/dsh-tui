//! Sidebar search (`/`) + archived-group expand (`e`) tests: the popup
//! open/Esc close (no POST), live search-as-you-type through the mock
//! gateway (query payload asserted) with results rendering, result
//! navigation + Enter switch (history fetch asserted), the empty-results
//! hint, Esc restoring the full grouped list, the failure path (toast +
//! restored list + re-armed guard), `/` inertness while other surfaces
//! own the keyboard, the `e` expand/collapse toggle with nav reaching
//! archived rows and the selection re-clamping, and live archive frames
//! re-clamping with the expanded group.

mod common;
use common::{MockAction, MockGateway, leaked, search_ok};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{Action, App, AppEvent, EventChannel, Focus};
use dsh_tui::client::WireClient;
use dsh_tui::ui::SidebarGroup;
use dsh_tui::wire::events::HostFrame;
use dsh_tui::wire::session::{SessionId, SessionSummary, WorkspaceId};
use dsh_tui::wire::workspace::WorkspaceView;

// ---------------------------------------------------------------------------
// fixture helpers
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

fn workspace(id: &str, title: &str, session_ids: &[&str]) -> WorkspaceView {
    WorkspaceView {
        workspace_id: WorkspaceId(id.into()),
        path: format!("/tmp/{id}"),
        title: title.into(),
        session_ids: session_ids
            .iter()
            .map(|id| SessionId((*id).into()))
            .collect(),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }
}

/// The grouped fixture: alpha (s1, s2) and beta (s3) workspaces in durable
/// order, s4 ungrouped, s5 archived.
fn grouped_app() -> App {
    let mut app = App::default();
    app.sessions = vec![
        summary("s1"),
        summary("s2"),
        summary("s3"),
        summary("s4"),
        summary("s5"),
    ];
    app.active_session = Some(SessionId("s1".into()));
    app.workspaces = vec![
        workspace("wA", "alpha", &["s1", "s2"]),
        workspace("wB", "beta", &["s3"]),
    ];
    app.workspace_order = vec![WorkspaceId("wA".into()), WorkspaceId("wB".into())];
    app.archived_session_ids = vec![SessionId("s5".into())];
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
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

/// Draw the current state (Esc forces an immediate draw) and quit.
async fn draw_and_quit(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut events = events;
    events.push(AppEvent::Key(key(KeyCode::Esc)));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(app, term, events).await;
}

/// Drive the loop in a task so async back-channel results land BEFORE the
/// quit key (the mock's `wait_for_posts` doc). Returns the app + final view.
async fn run_live(
    app: App,
    events: Vec<AppEvent>,
    wait: impl std::future::Future<Output = ()>,
) -> (App, String) {
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = EventChannel::new();
    let tx = channel.tx.clone();
    let run_task = tokio::spawn(async move {
        let mut app = app;
        let result = app.run(&mut term, &mut channel).await;
        result.expect("run must not fail");
        (app, term)
    });
    for event in events {
        tx.send(event).expect("event channel");
    }
    wait.await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit key");
    let (app, term) = run_task.await.expect("run task");
    let view = format!("{}", term.backend());
    (app, view)
}

/// A bounded wait for the done-event tail after the request is captured.
async fn after_posts(mock: &MockGateway, path: &str, count: usize) {
    mock.wait_for_posts(path, count).await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
}

/// The search fixture: one POST per query, items echoing the query.
fn search_app() -> App {
    let mut app = grouped_app();
    app.focus = Focus::Sidebar;
    app
}

// ---------------------------------------------------------------------------
// 1. search open/close
// ---------------------------------------------------------------------------

#[tokio::test]
async fn slash_opens_search_and_esc_closes_without_a_post() {
    let mock = MockGateway::start().await;
    let mut app = search_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(key(KeyCode::Esc)),
        ],
    )
    .await;
    assert!(app.sidebar_search.is_none(), "Esc closed the search popup");
    assert!(
        mock.requests().await.is_empty(),
        "no POSTs from open + close"
    );
    mock.stop().await;
}

#[tokio::test]
async fn search_popup_renders_over_the_sidebar() {
    let mut app = search_app();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    // No Esc before the quit: it would close the popup and redraw over it
    // (the quit key itself doesn't draw).
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());

    assert!(view.contains(" search sessions "), "popup title: {view}");
    assert!(view.contains("type to search"), "placeholder: {view}");
    // #19: the popup never exceeds its anchor pane (the 22-col sidebar),
    // so the hint row truncates — assert its visible prefix.
    assert!(
        view.contains("enter opens"),
        "hint row (truncated to the pane): {view}"
    );
}

// ---------------------------------------------------------------------------
// 2. search-as-you-type
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn typing_posts_the_query_payload_and_renders_results() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.search",
        MockAction::Ok(leaked(search_ok(
            r#"[{"sessionId":"s5","snippet":"snippet for s5"}]"#,
        ))),
    )
    .await;
    let mut app = search_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    // One char per round trip: 'a' POSTs, the result folds, then 'b'.
    let (app, _) = run_live(
        app,
        vec![
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(key(KeyCode::Char('a'))),
        ],
        after_posts(&mock, "/api/session.search", 1),
    )
    .await;
    let (app, view) = run_live(
        app,
        vec![AppEvent::Key(key(KeyCode::Char('b')))],
        after_posts(&mock, "/api/session.search", 2),
    )
    .await;

    let posts = mock.wait_for_posts("/api/session.search", 2).await;
    let query_of = |i: usize| {
        posts[i]
            .get("payload")
            .and_then(|payload| payload.get("query"))
            .cloned()
    };
    assert_eq!(query_of(0), Some(serde_json::json!("a")), "first POST");
    assert_eq!(query_of(1), Some(serde_json::json!("ab")), "second POST");
    assert!(
        view.contains("snippet for s5"),
        "result row renders: {view}"
    );
    let results = app
        .sidebar_search
        .as_ref()
        .expect("popup still open")
        .results
        .clone();
    assert_eq!(results.len(), 1, "one result folded");
    assert!(!app.sidebar_search.as_ref().unwrap().sending, "guard clear");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 3. results nav + Enter switch
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn results_nav_and_enter_switch_to_the_highlighted_session() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.search",
        MockAction::Ok(leaked(search_ok(
            r#"[{"sessionId":"s5","snippet":"m s5"},{"sessionId":"s2","snippet":"m s2"}]"#,
        ))),
    )
    .await;
    mock.set_handler(
        "session.history",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"events":[],"hasMore":false}}}"#,
        ),
    )
    .await;
    let mut app = search_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    // Run 1: open + search; the results fold before the next round.
    let (app, _) = run_live(
        app,
        vec![
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(key(KeyCode::Char('a'))),
        ],
        after_posts(&mock, "/api/session.search", 1),
    )
    .await;
    // Run 2: j moves to the second result (s2); Enter switches there.
    let (app, _) = run_live(
        app,
        vec![
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        after_posts(&mock, "/api/session.history", 1),
    )
    .await;

    assert_eq!(app.active_session, Some(SessionId("s2".into())));
    assert!(app.sidebar_search.is_none(), "popup closed on switch");
    let posts = mock.wait_for_posts("/api/session.history", 1).await;
    assert_eq!(
        posts[0].get("payload").and_then(|p| p.get("sessionId")),
        Some(&serde_json::json!("s2")),
        "history fetch targets the switched session"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 4. empty results + restore on Esc
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn empty_results_show_a_hint_line() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.search", MockAction::Ok(leaked(search_ok("[]"))))
        .await;
    let mut app = search_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let (app, view) = run_live(
        app,
        vec![
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(key(KeyCode::Char('a'))),
        ],
        after_posts(&mock, "/api/session.search", 1),
    )
    .await;

    assert!(view.contains("no matches"), "empty-results hint: {view}");
    assert!(
        app.sidebar_search
            .as_ref()
            .is_some_and(|state| state.results.is_empty()),
        "no rows folded"
    );
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_restores_the_full_grouped_list() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.search",
        MockAction::Ok(leaked(search_ok(
            r#"[{"sessionId":"s5","snippet":"snippet for s5"}]"#,
        ))),
    )
    .await;
    let mut app = search_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let (app, _) = run_live(
        app,
        vec![
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(key(KeyCode::Char('a'))),
        ],
        after_posts(&mock, "/api/session.search", 1),
    )
    .await;
    let (app, view) = run_live(app, vec![AppEvent::Key(key(KeyCode::Esc))], async {}).await;

    assert!(app.sidebar_search.is_none(), "Esc closed the popup");
    assert!(!view.contains(" search sessions "), "popup gone: {view}");
    assert!(view.contains("alpha"), "workspace header back: {view}");
    assert!(
        view.contains("▸ archived (1)"),
        "archived footer back: {view}"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 5. failure path
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn search_failure_toasts_and_restores_the_list() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.search", MockAction::NotFound)
        .await;
    let mut app = search_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let (app, view) = run_live(
        app,
        vec![
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(key(KeyCode::Char('a'))),
        ],
        after_posts(&mock, "/api/session.search", 1),
    )
    .await;

    let state = app.sidebar_search.as_ref().expect("popup stays open");
    assert!(state.results.is_empty(), "rows restored");
    assert!(!state.sending, "guard re-armed");
    assert!(view.contains("search failed:"), "status toast: {view}");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 6. keymap inertness
// ---------------------------------------------------------------------------

#[test]
fn slash_is_inert_in_composer_and_behind_other_popups() {
    let mut app = grouped_app();
    // Composer: `/` types into the buffer (its own slash popup opens) —
    // the sidebar search must not open.
    app.focus = Focus::Composer;
    app.handle_key(key(KeyCode::Char('/')));
    assert!(app.sidebar_search.is_none(), "no search from the composer");
    assert_eq!(app.composer.buffer(), "/");
    app.composer.set_text("");
    app.composer.popup_dismiss();

    // The queue popup owns keys.
    app.queue_popup_open = true;
    app.handle_key(key(KeyCode::Char('/')));
    assert!(app.sidebar_search.is_none(), "queue popup swallows /");
    app.queue_popup_open = false;

    // The new-session picker owns keys.
    app.focus = Focus::Sidebar;
    app.handle_key(key(KeyCode::Char('n')));
    assert!(app.new_session.is_some(), "picker open");
    app.handle_key(key(KeyCode::Char('/')));
    assert!(
        app.sidebar_search.is_none(),
        "new-session picker swallows /"
    );
    app.handle_key(key(KeyCode::Esc));

    // The rename editor owns sidebar keys.
    app.handle_key(key(KeyCode::Char('r')));
    assert!(app.rename_editor.is_some(), "rename editor open");
    app.handle_key(key(KeyCode::Char('/')));
    assert!(app.sidebar_search.is_none(), "rename editor swallows /");
}

// ---------------------------------------------------------------------------
// 7. archived-group expand (`e`)
// ---------------------------------------------------------------------------

#[test]
fn e_toggles_the_archived_group_and_navigation_reaches_it() {
    let mut app = grouped_app();
    app.focus = Focus::Sidebar;
    // Collapsed: s5 unreachable (visible: s1..s4).
    assert_eq!(SidebarGroup::visible_len(&app.sidebar_groups()), 4);

    // e expands: s5 joins navigation.
    assert_eq!(app.handle_key(key(KeyCode::Char('e'))), Some(Action::None));
    assert!(app.archived_expanded, "expanded");
    assert_eq!(SidebarGroup::visible_len(&app.sidebar_groups()), 5);

    // Nav to the bottom reaches s5, and Enter switches to it.
    app.sidebar
        .last(SidebarGroup::visible_len(&app.sidebar_groups()));
    assert_eq!(
        SidebarGroup::visible_session(&app.sidebar_groups(), app.sidebar.selected),
        Some(4),
        "s5 selected"
    );
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Action::SwitchSession(SessionId("s5".into())))
    );
    assert_eq!(app.active_session, Some(SessionId("s5".into())));

    // e collapses: s5 unreachable again, selection clamped back to s4.
    assert_eq!(app.handle_key(key(KeyCode::Char('e'))), Some(Action::None));
    assert!(!app.archived_expanded, "collapsed");
    assert_eq!(app.sidebar.selected, 3, "clamped to the last visible row");
    assert_eq!(
        SidebarGroup::visible_session(&app.sidebar_groups(), app.sidebar.selected),
        Some(3),
        "s4"
    );
}

#[tokio::test]
async fn expanded_archived_sessions_render_as_rows() {
    let mut app = grouped_app();
    app.archived_expanded = true;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(&mut app, &mut term, Vec::new()).await;
    let view = format!("{}", term.backend());

    assert!(view.contains("  s5"), "archived row renders: {view}");
    assert!(
        view.contains("▸ archived (1)"),
        "header keeps the count: {view}"
    );
}

#[tokio::test]
async fn live_archive_frames_re_clamp_with_the_expanded_group() {
    let mut app = grouped_app();
    app.archived_expanded = true;
    // Select the deep archived row (s5, the last visible session).
    app.sidebar
        .last(SidebarGroup::visible_len(&app.sidebar_groups()));
    assert_eq!(app.sidebar.selected, 4);

    // Frame 1: archive s2 as well — the expanded group keeps every row
    // visible, so the clamp leaves the selection on s5 (the count grows).
    let mut events = vec![AppEvent::HostFrame(
        HostFrame::HostArchivedSessionsChanged {
            archived_session_ids: vec![SessionId("s5".into()), SessionId("s2".into())],
        },
    )];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    events.push(AppEvent::Key(key(KeyCode::Esc)));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;
    let groups = app.sidebar_groups();
    assert_eq!(SidebarGroup::visible_len(&groups), 5, "all still visible");
    assert_eq!(
        SidebarGroup::visible_session(&groups, app.sidebar.selected),
        Some(4),
        "still on s5"
    );
    let view = format!("{}", term.backend());
    assert!(view.contains("▸ archived (2)"), "count follows: {view}");

    // Frame 2: un-archive s5 — s5 rejoins the ungrouped group; the clamp
    // keeps the selection on a valid (archived) row.
    let mut events = vec![AppEvent::HostFrame(
        HostFrame::HostArchivedSessionsChanged {
            archived_session_ids: vec![SessionId("s2".into())],
        },
    )];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    events.push(AppEvent::Key(key(KeyCode::Esc)));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;
    let groups = app.sidebar_groups();
    assert_eq!(SidebarGroup::visible_len(&groups), 5);
    assert_eq!(
        SidebarGroup::visible_session(&groups, app.sidebar.selected),
        Some(1),
        "selection rides the clamp onto s2 (an archived row)"
    );
}
