//! Workspace-grouped sidebar tests (Q2): group headers + nested sessions in
//! durable order, the ungrouped section, the collapsed archived footer,
//! header-skipping navigation with Enter-switch (history fetch asserted),
//! live host/workspace-changed|removed|order-changed + archived-sessions-
//! changed frames, and the attach flow's `workspace.list` seeding. Keyless
//! where possible (injected events + `TestBackend`); the attach seeding
//! test rides the mock gateway.

mod common;
use common::{MockAction, MockGateway};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{Action, App, AppEvent, EventChannel, Focus, attach};
use dsh_tui::client::WireClient;
use dsh_tui::i18n::Locale;
use dsh_tui::store::SessionStore;
use dsh_tui::wire::events::HostFrame;
use dsh_tui::wire::session::{SessionId, SessionSummary, WorkspaceId};
use dsh_tui::wire::workspace::{WorkspaceCreateValue, WorkspaceView};

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

fn workspace_view(id: &str, title: &str) -> WorkspaceView {
    WorkspaceView {
        workspace_id: WorkspaceId(id.into()),
        path: format!("/tmp/{id}"),
        title: title.into(),
        session_ids: vec![],
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }
}

/// A finished `workspace.create` folds the returned row into the sidebar:
/// a new workspace appends to `workspaces` and the durable order, the
/// editor closes and the sending flag clears (6g).
#[tokio::test]
async fn workspace_create_done_folds_new_workspace_in() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::WorkspaceCreateDone {
                result: Ok(WorkspaceCreateValue {
                    workspace: workspace_view("wNew", "new-ws"),
                    created: true,
                }),
            },
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        app.workspaces
            .iter()
            .any(|ws| ws.workspace_id == WorkspaceId("wNew".into())),
        "workspace folded in"
    );
    assert!(
        app.workspace_order.contains(&WorkspaceId("wNew".into())),
        "durable order appended"
    );
    assert!(app.workspace_editor.is_none(), "editor closed");
    assert!(!app.sidebar_action_sending, "sending flag cleared");
}

/// A failed `workspace.create` toasts through the shared sidebar-action
/// failure surface and closes the editor too (6g).
#[tokio::test]
async fn workspace_create_done_failure_toasts() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::WorkspaceCreateDone {
                result: Err(dsh_tui::client::ClientError::Transport("boom".into())),
            },
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let toast = app.toast.as_ref().expect("failure toast");
    assert!(toast.0.contains("boom"), "toast text: {}", toast.0);
    assert!(app.workspace_editor.is_none(), "editor closed on error");
    assert!(!app.sidebar_action_sending, "sending flag cleared on error");
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

/// The view after drawing `app` at `width`×`height`.
async fn view_at(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(app, &mut term, Vec::new()).await;
    format!("{}", term.backend())
}

// ---------------------------------------------------------------------------
// 1. grouped rendering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn grouped_sidebar_renders_headers_and_nested_sessions_120x30() {
    let mut app = grouped_app();
    let view = view_at(&mut app, 120, 30).await;

    let alpha = view.find("alpha").expect("workspace alpha header");
    // "● s1" — the header row's `Session: s1` would win a bare "s1" find.
    let s1 = view.find("● s1").expect("s1 row");
    let s2 = view.find("      s2").expect("s2 row");
    let beta = view.find("beta").expect("workspace beta header");
    let s3 = view.find("s3").expect("s3 row");
    let ungrouped = view.find("ungrouped").expect("ungrouped header");
    let s4 = view.find("s4").expect("s4 row");
    let archived = view.find("▸ archived (1)").expect("archived header");
    assert!(
        alpha < s1
            && s1 < s2
            && s2 < beta
            && beta < s3
            && s3 < ungrouped
            && ungrouped < s4
            && s4 < archived,
        "group order: alpha sessions, beta session, ungrouped, archived\n{view}"
    );
    // Archived sessions are collapsed: the header shows, the row does not.
    assert!(
        !view.contains("● s5") && !view.contains("  s5"),
        "s5 hidden: {view}"
    );
    // The active marker survives nesting (one-space indent in grouped mode).
    assert!(view.contains("● s1"), "active marker nested: {view}");
}

#[tokio::test]
async fn grouped_sidebar_renders_at_60x15() {
    // #19: at 60 cols the sidebar lives in the `s`-toggled drawer — open
    // it and assert the grouped rows render there.
    let mut app = grouped_app();
    app.focus = Focus::Chat; // 's' is focus-gated (boot is Composer)
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = vec![AppEvent::Key(key(KeyCode::F(1)))]; // draw first
    events.push(AppEvent::Key(key(KeyCode::Char('s')))); // open the drawer
    events.push(AppEvent::Key(key(KeyCode::F(1))));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;
    let view = format!("{}", term.backend());
    assert!(app.drawer_open, "s opened the drawer");
    assert!(view.contains("alpha"), "alpha header at 60x15: {view}");
    assert!(view.contains("beta"), "beta header at 60x15: {view}");
    assert!(view.contains("ungrouped"), "ungrouped at 60x15: {view}");
    assert!(
        view.contains("▸ archived (1)"),
        "archived header at 60x15: {view}"
    );
}

#[tokio::test]
async fn zh_locale_group_labels() {
    let mut app = grouped_app();
    app.locale = Locale::Zh;
    let view = view_at(&mut app, 120, 30).await;
    assert!(view.contains("未分组"), "zh ungrouped: {view}");
    assert!(view.contains("▸ 已归档 (1)"), "zh archived: {view}");
}

// ---------------------------------------------------------------------------
// 2. navigation across group boundaries + Enter switch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn navigation_skips_headers_and_enter_switches() {
    let mut app = grouped_app();
    app.focus = Focus::Sidebar;

    // Visible session order: s1, s2 (alpha), s3 (beta), s4 (ungrouped);
    // s5 is collapsed away. Headers are not selectable.
    assert_eq!(app.sidebar.selected, 0);
    assert_eq!(
        app.handle_key(key(KeyCode::Char('j'))),
        Some(Action::Select)
    );
    assert_eq!(app.sidebar.selected, 1, "s2");
    assert_eq!(
        app.handle_key(key(KeyCode::Char('j'))),
        Some(Action::Select)
    );
    assert_eq!(app.sidebar.selected, 2, "s3 — crossed the beta header");
    assert_eq!(
        app.handle_key(key(KeyCode::Char('j'))),
        Some(Action::Select)
    );
    assert_eq!(app.sidebar.selected, 3, "s4 — crossed the ungrouped header");
    // One more j clamps at s4: the archived group is unreachable.
    assert_eq!(
        app.handle_key(key(KeyCode::Char('j'))),
        Some(Action::Select)
    );
    assert_eq!(app.sidebar.selected, 3, "clamped before archived");
    // k crosses headers on the way back up.
    assert_eq!(
        app.handle_key(key(KeyCode::Char('k'))),
        Some(Action::Select)
    );
    assert_eq!(app.sidebar.selected, 2, "back to s3");

    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Action::SwitchSession(SessionId("s3".into()))),
        "Enter switches to the selected session"
    );
    assert_eq!(app.active_session, Some(SessionId("s3".into())));
}

#[tokio::test]
async fn enter_in_sidebar_fetches_history_via_the_run_loop() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.history",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"events":[],"hasMore":false}}}"#,
        ),
    )
    .await;

    let mut app = grouped_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.focus = Focus::Sidebar;

    // Drive j + Enter through the run loop: the SwitchSession action the
    // loop sees becomes the history fetch (Q9).
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
    )
    .await;
    assert_eq!(app.active_session, Some(SessionId("s2".into())));
    let posts = mock.wait_for_posts("/api/session.history", 1).await;
    assert_eq!(
        posts[0].get("payload").and_then(|p| p.get("sessionId")),
        Some(&serde_json::json!("s2")),
        "history fetch targets the switched session"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 3. live workspace frames
// ---------------------------------------------------------------------------

#[tokio::test]
async fn workspace_changed_upserts_membership() {
    let mut app = grouped_app();
    // s4 (ungrouped) joins beta; beta also picks up the brand-new s6.
    let mut events = vec![
        AppEvent::HostFrame(HostFrame::HostSessionAdded {
            session_id: SessionId("s6".into()),
            blank: false,
            parent_session_id: None,
            origin: None,
            cwd: None,
            agent_preset: None,
        }),
        AppEvent::HostFrame(HostFrame::HostWorkspaceChanged {
            workspace: workspace("wB", "beta", &["s3", "s6", "s4"]),
        }),
    ];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    events.push(AppEvent::Key(key(KeyCode::Esc)));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;
    let view = format!("{}", term.backend());

    let beta = view.find("beta").expect("beta header");
    let s3 = view.find("s3").expect("s3");
    let s4 = view.find("s4").expect("s4 joined beta");
    let s6 = view.find("s6").expect("new session s6");
    let ungrouped = view.find("ungrouped");
    assert!(
        beta < s3 && s3 < s6 && s6 < s4,
        "beta claims s3, s6, s4: {view}"
    );
    assert!(ungrouped.is_none(), "no ungrouped sessions left: {view}");
}

#[tokio::test]
async fn workspace_removed_reflows_sessions_to_ungrouped() {
    let mut app = grouped_app();
    let mut events = vec![AppEvent::HostFrame(HostFrame::HostWorkspaceRemoved {
        workspace_id: WorkspaceId("wA".into()),
    })];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    events.push(AppEvent::Key(key(KeyCode::Esc)));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;
    let view = format!("{}", term.backend());

    assert!(!view.contains("alpha"), "alpha header gone: {view}");
    let beta = view.find("beta").expect("beta still there");
    let ungrouped = view.find("ungrouped").expect("ungrouped header");
    // "● s1" — the header row's `Session: s1` would win a bare "s1" find.
    let s1 = view.find("● s1").expect("s1");
    let s2 = view.find("      s2").expect("s2");
    assert!(
        beta < ungrouped && ungrouped < s1 && s1 < s2,
        "alpha's sessions reflowed under ungrouped: {view}"
    );
}

#[tokio::test]
async fn workspace_order_changed_reorders_groups() {
    let mut app = grouped_app();
    let mut events = vec![AppEvent::HostFrame(HostFrame::HostWorkspaceOrderChanged {
        workspace_ids: vec![WorkspaceId("wB".into()), WorkspaceId("wA".into())],
    })];
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    events.push(AppEvent::Key(key(KeyCode::Esc)));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;
    let view = format!("{}", term.backend());

    let beta = view.find("beta").expect("beta");
    let alpha = view.find("alpha").expect("alpha");
    assert!(beta < alpha, "beta first after the order frame: {view}");
}

#[tokio::test]
async fn archived_sessions_changed_moves_sessions_out_of_navigation() {
    let mut app = grouped_app();
    // Archive s2 live: it leaves alpha (and j/k) for the collapsed footer.
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
    let view = format!("{}", term.backend());

    assert!(view.contains("▸ archived (2)"), "count follows: {view}");
    assert!(!view.contains(" s2"), "s2 row hidden: {view}");
    let groups = app.sidebar_groups();
    assert_eq!(
        dsh_tui::ui::SidebarGroup::visible_len(&groups),
        3,
        "s1, s3, s4 remain navigable"
    );
}

// ---------------------------------------------------------------------------
// 4. archived sessions render only in the collapsed footer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn archived_session_appears_only_collapsed_even_when_workspace_claims_it() {
    let mut app = App::default();
    app.sessions = vec![summary("s1")];
    app.workspaces = vec![workspace("wA", "alpha", &["s1"])];
    app.workspace_order = vec![WorkspaceId("wA".into())];
    app.archived_session_ids = vec![SessionId("s1".into())];
    let view = view_at(&mut app, 120, 30).await;

    assert!(!view.contains("alpha"), "empty alpha group drops: {view}");
    assert!(view.contains("▸ archived (1)"), "archived footer: {view}");
    assert!(
        !view.contains("● s1") && !view.contains("  s1"),
        "s1 hidden: {view}"
    );
}

// ---------------------------------------------------------------------------
// 5. no-workspace regression + attach seeding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_workspaces_keeps_the_flat_look() {
    let mut app = App::default();
    app.sessions = vec![summary("s1"), summary("s2")];
    app.active_session = Some(SessionId("s1".into()));
    let view = view_at(&mut app, 120, 30).await;

    assert!(view.contains("● s1"), "flat active row: {view}");
    assert!(view.contains("  s2"), "flat row at column 0: {view}");
    assert!(!view.contains("ungrouped"), "no group headers: {view}");
    assert!(!view.contains("archived"), "no archived header: {view}");
}

#[tokio::test]
async fn attach_seeds_workspace_grouping_from_workspace_list() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[
                {"sessionId":"s1","updatedAt":200.0,"running":false,"blank":false},
                {"sessionId":"s2","updatedAt":100.0,"running":false,"blank":false},
                {"sessionId":"s3","updatedAt":50.0,"running":false,"blank":false}
            ]}}}"#,
        ),
    )
    .await;
    mock.set_handler(
        "workspace.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{
                "items":[
                    {"workspaceId":"wA","path":"/tmp/alpha","title":"alpha","sessionIds":["s1"],"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z"}
                ],
                "archivedSessionIds":["s3"]
            }}}"#,
        ),
    )
    .await;
    mock.set_handler(
        "session.history",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"events":[],"hasMore":false}}}"#,
        ),
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut store = SessionStore::new();
    let (opened, sessions, workspace_list, _model) = attach(&client, &mut store, Locale::En)
        .await
        .expect("attach");
    assert_eq!(opened, Some(SessionId("s1".into())));
    assert_eq!(workspace_list.items.len(), 1);
    assert_eq!(
        workspace_list.archived_session_ids,
        vec![SessionId("s3".into())]
    );

    // Wire the app like main.rs does.
    let mut app = App::default();
    app.store = store;
    app.active_session = opened;
    app.sessions = sessions;
    app.workspace_order = workspace_list
        .items
        .iter()
        .map(|workspace| workspace.workspace_id.clone())
        .collect();
    app.workspaces = workspace_list.items;
    app.archived_session_ids = workspace_list.archived_session_ids;

    let view = view_at(&mut app, 120, 30).await;
    let alpha = view.find("alpha").expect("alpha header");
    // "● s1" — the header row's `Session: s1` would win a bare "s1" find.
    let s1 = view.find("● s1").expect("s1 row");
    let ungrouped = view.find("ungrouped").expect("ungrouped header");
    let s2 = view.find("      s2").expect("s2 row");
    let archived = view.find("▸ archived (1)").expect("archived footer");
    assert!(
        alpha < s1 && s1 < ungrouped && ungrouped < s2 && s2 < archived,
        "attach seeds the full group model: {view}"
    );
    mock.stop().await;
}

#[tokio::test]
async fn attach_survives_a_missing_workspace_list() {
    // Gateways without workspace.list (or a transient failure) degrade to
    // the flat sidebar instead of failing boot.
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[
                {"sessionId":"s1","updatedAt":200.0,"running":false,"blank":false}
            ]}}}"#,
        ),
    )
    .await;
    mock.set_handler(
        "session.history",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"events":[],"hasMore":false}}}"#,
        ),
    )
    .await;
    // No workspace.list handler: the mock 404s it.

    let client = WireClient::attach(mock.port()).unwrap();
    let mut store = SessionStore::new();
    let (opened, sessions, workspace_list, _model) = attach(&client, &mut store, Locale::En)
        .await
        .expect("attach tolerates workspace.list failure");
    assert_eq!(opened, Some(SessionId("s1".into())));
    assert_eq!(sessions.len(), 1);
    assert!(workspace_list.items.is_empty());
    assert!(workspace_list.archived_session_ids.is_empty());
    mock.stop().await;
}
