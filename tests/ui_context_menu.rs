//! Sidebar context menus (#46): the `m` key and the kebab clicks open the
//! per-workspace / per-session action menus (Rename / Fork / Archive,
//! Rename / Delete workspace), the popup owns the keyboard while open
//! (j/k move, Enter executes, Esc closes), and the executed actions
//! dispatch through the mock gateway.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{App, AppEvent, EventChannel, Focus};
use dsh_tui::client::WireClient;
use dsh_tui::wire::session::{SessionId, SessionSummary, WorkspaceId};
use dsh_tui::wire::workspace::WorkspaceView;

use common::MockGateway;

mod common;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
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

/// The grouped fixture: alpha (s1, s2) and beta (s3) in durable order.
fn grouped_app() -> App {
    let mut app = App::default();
    app.sessions = vec![summary("s1"), summary("s2"), summary("s3")];
    app.active_session = Some(SessionId("s1".into()));
    app.workspaces = vec![
        workspace("wA", "alpha", &["s1", "s2"]),
        workspace("wB", "beta", &["s3"]),
    ];
    app.workspace_order = vec![WorkspaceId("wA".into()), WorkspaceId("wB".into())];
    app.focus = Focus::Sidebar;
    app
}

/// Run the loop in a spawned task, let the back-channel land, then quit.
async fn run_with_settle(
    mut app: App,
    mut term: Terminal<TestBackend>,
    events: Vec<AppEvent>,
    settle: Duration,
) -> App {
    let mut channel = EventChannel::new();
    let tx = channel.tx.clone();
    let task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut channel).await;
        (result, app, term)
    });
    for event in events {
        tx.send(event).expect("event channel");
    }
    tokio::time::sleep(settle).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    let (result, app, _term) = task.await.expect("run task");
    result.expect("run");
    app
}

async fn posts_to(mock: &MockGateway, path: &str) -> Vec<serde_json::Value> {
    mock.requests()
        .await
        .iter()
        .filter(|request| request.path == path)
        .filter_map(|request| serde_json::from_str(&request.body).ok())
        .collect()
}

/// Draw the current state (F1 forces a draw) and quit.
async fn draw_and_quit(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut events = events;
    events.push(AppEvent::Key(key(KeyCode::F(1))));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    let mut channel = EventChannel::new();
    for event in events {
        channel.tx.send(event).expect("event channel");
    }
    app.run(term, &mut channel).await.expect("run");
}

fn mouse_down(column: u16, row: u16) -> AppEvent {
    AppEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn workspace_rename_ok(title: &str) -> String {
    format!(
        r#"{{"type":"server-response","rpcId":"{{rpcId}}","result":{{"ok":true,"value":{{"workspace":{{"workspaceId":"wA","path":"/tmp/wA","title":"{title}","sessionIds":["s1","s2"],"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z"}}}}}}}}"#
    )
}

fn workspace_delete_ok() -> &'static str {
    r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"deleted":true}}}"#
}

fn fork_ok(session_id: &str) -> &'static str {
    Box::leak(
        format!(
            r#"{{"type":"server-response","rpcId":"{{rpcId}}","result":{{"ok":true,"value":{{"sessionId":"{session_id}"}}}}}}"#
        )
        .into_boxed_str(),
    )
}

// ---------------------------------------------------------------------------
// 1. the session menu (`m` key): opens, renders, navigates, closes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m_key_opens_the_session_menu_and_esc_closes_it() {
    let mut app = grouped_app();
    app.focus = Focus::Sidebar;
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('m'))),
            AppEvent::Key(key(KeyCode::F(1))),
        ],
    )
    .await;

    let view = format!("{}", term.backend());
    assert!(app.context_menu.is_some(), "menu open");
    assert!(view.contains(" session "), "session title: {view}");
    assert!(view.contains("Rename"), "rename entry: {view}");
    assert!(view.contains("Fork session"), "fork entry: {view}");
    assert!(view.contains("Archive session"), "archive entry: {view}");

    // j moves the cursor, Esc closes.
    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.context_menu.as_ref().map(|m| m.selected), Some(1));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.context_menu.is_none(), "Esc closes");
    // Keys are inert while open except navigation/execute/close.
    app.handle_key(key(KeyCode::Char('m')));
    app.handle_key(key(KeyCode::Char('z')));
    assert_eq!(app.context_menu.as_ref().map(|m| m.selected), Some(0));
    app.handle_key(key(KeyCode::Esc));
}

#[tokio::test]
async fn session_menu_executes_fork_and_archive_via_the_gateway() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.fork", common::MockAction::Ok(fork_ok("sF")))
        .await;
    mock.set_handler(
        "workspace.archiveSession",
        common::MockAction::Ok(Box::leak(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"archivedSessionIds":["s1"]}}}"#
                .to_string()
                .into_boxed_str(),
        )),
    )
    .await;
    let mut app = grouped_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    // Open the menu, move to Fork, execute: session.fork posts.
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('m'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;
    let forks = posts_to(&mock, "/api/session.fork").await;
    assert_eq!(forks.len(), 1);
    assert_eq!(forks[0]["payload"]["sessionId"], "s1");
    assert!(result.context_menu.is_none(), "menu closed on execute");
    assert!(!result.sidebar_action_sending);

    // Open the menu again, move past Fork to Archive, execute.
    let mut result = result;
    result.focus = Focus::Sidebar;
    let result = run_with_settle(
        result,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('m'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;
    let archives = posts_to(&mock, "/api/workspace.archiveSession").await;
    assert_eq!(archives.len(), 1);
    assert_eq!(archives[0]["payload"]["sessionId"], "s1");
    assert_eq!(result.toast_text(), Some("archived"));
}

#[tokio::test]
async fn session_menu_rename_opens_the_inline_editor() {
    let mut app = grouped_app();
    app.focus = Focus::Sidebar;
    app.handle_key(key(KeyCode::Char('m')));
    // Cursor 0 = Rename: Enter opens the seeded inline editor.
    app.handle_key(key(KeyCode::Enter));
    assert!(app.context_menu.is_none(), "menu closed");
    assert!(app.rename_editor.is_some(), "rename editor open");
    let seeded = app
        .rename_editor
        .as_ref()
        .map(|(_, editor)| editor.buffer());
    assert_eq!(seeded, Some("s1"), "seeded with the session id");
}

// ---------------------------------------------------------------------------
// 2. the workspace menu (header kebab click)
// ---------------------------------------------------------------------------

/// The kebab click coordinates: the sidebar's inner right edge on the
/// alpha header row (inner.x=2 + inner.width-1=17 → col 19; LIST_TOP=6).
const WORKSPACE_KEBAB: (u16, u16) = (19, 6);
/// The session-row kebab for s1 (the row under the alpha header).
const SESSION_KEBAB: (u16, u16) = (19, 7);

#[tokio::test]
async fn workspace_kebab_click_opens_the_workspace_menu() {
    let mut app = grouped_app();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::F(1))), // draw: hit-test rects stored
            mouse_down(WORKSPACE_KEBAB.0, WORKSPACE_KEBAB.1),
            AppEvent::Key(key(KeyCode::F(1))),
        ],
    )
    .await;
    let menu = app.context_menu.expect("workspace menu open");
    let labels: Vec<String> = menu.items.iter().map(|item| item.label.clone()).collect();
    assert_eq!(labels, vec!["Rename", "Delete workspace"]);
    let view = format!("{}", term.backend());
    assert!(view.contains(" workspace "), "workspace title: {view}");
    assert!(view.contains("Delete workspace"), "delete entry: {view}");
}

#[tokio::test]
async fn session_kebab_click_opens_the_session_menu() {
    let mut app = grouped_app();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::F(1))), // draw: hit-test rects stored
            mouse_down(SESSION_KEBAB.0, SESSION_KEBAB.1),
        ],
    )
    .await;
    assert!(
        matches!(
            app.context_menu,
            Some(dsh_tui::ui::ContextMenuState {
                ref items,
                ..
            }) if items.len() == 3
        ),
        "session menu via the row kebab"
    );
}

#[tokio::test]
async fn workspace_menu_rename_updates_the_header_title() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "workspace.rename",
        common::MockAction::Ok(Box::leak(workspace_rename_ok("new alpha").into_boxed_str())),
    )
    .await;
    let mut app = grouped_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    // Kebab → Rename (cursor 0) → the inline editor opens seeded with
    // "alpha"; clear and retype, Enter commits.
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::F(1))), // draw: hit-test rects stored
            mouse_down(WORKSPACE_KEBAB.0, WORKSPACE_KEBAB.1),
            AppEvent::Key(key(KeyCode::Enter)),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Char('n'))),
            AppEvent::Key(key(KeyCode::Char('e'))),
            AppEvent::Key(key(KeyCode::Char('w'))),
            AppEvent::Key(key(KeyCode::Char(' '))),
            AppEvent::Key(key(KeyCode::Char('a'))),
            AppEvent::Key(key(KeyCode::Char('l'))),
            AppEvent::Key(key(KeyCode::Char('p'))),
            AppEvent::Key(key(KeyCode::Char('h'))),
            AppEvent::Key(key(KeyCode::Char('a'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;

    let posts = posts_to(&mock, "/api/workspace.rename").await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["payload"]["workspaceId"], "wA");
    assert_eq!(posts[0]["payload"]["title"], "new alpha");
    assert_eq!(
        result
            .workspaces
            .iter()
            .find(|ws| ws.workspace_id == WorkspaceId("wA".into()))
            .map(|ws| ws.title.as_str()),
        Some("new alpha"),
        "header title refreshed"
    );
    assert_eq!(result.toast_text(), Some("renamed"));
    assert!(result.workspace_rename.is_none(), "editor closed");
}

#[tokio::test]
async fn workspace_menu_delete_removes_the_workspace() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "workspace.delete",
        common::MockAction::Ok(workspace_delete_ok()),
    )
    .await;
    let mut app = grouped_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    // Kebab → j (Delete workspace) → Enter: workspace.delete posts and the
    // row drops (sessions reflow to ungrouped via the membership rule).
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::F(1))), // draw: hit-test rects stored
            mouse_down(WORKSPACE_KEBAB.0, WORKSPACE_KEBAB.1),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;

    let posts = posts_to(&mock, "/api/workspace.delete").await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["payload"]["workspaceId"], "wA");
    assert!(
        !result
            .workspaces
            .iter()
            .any(|ws| ws.workspace_id == WorkspaceId("wA".into())),
        "workspace removed"
    );
    assert!(
        !result.workspace_order.contains(&WorkspaceId("wA".into())),
        "durable order pruned"
    );
    assert_eq!(result.toast_text(), Some("workspace deleted"));
}

#[tokio::test]
async fn workspace_menu_actions_fail_gracefully() {
    // Both workspace actions toast through the shared failure surface and
    // re-arm the guard on a NotFound.
    let mock = MockGateway::start().await;
    mock.set_handler("workspace.rename", common::MockAction::NotFound)
        .await;
    mock.set_handler("workspace.delete", common::MockAction::NotFound)
        .await;
    let mut app = grouped_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    // Rename failure: editor commits, POST 404s, toast.
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::F(1))),
            mouse_down(WORKSPACE_KEBAB.0, WORKSPACE_KEBAB.1),
            AppEvent::Key(key(KeyCode::Enter)),
            AppEvent::Key(key(KeyCode::Char('x'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;
    assert!(
        result
            .toast_text()
            .is_some_and(|text| text.contains("action failed") && text.contains("404")),
        "rename failure toast: {:?}",
        result.toast_text()
    );
    assert!(!result.sidebar_action_sending, "guard re-armed");
    assert!(result.workspace_rename.is_none(), "workspace editor closed");

    // Delete failure: same surface.
    let mut result = result;
    result.focus = Focus::Sidebar;
    let result = run_with_settle(
        result,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::F(1))),
            mouse_down(WORKSPACE_KEBAB.0, WORKSPACE_KEBAB.1),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;
    assert!(
        result
            .toast_text()
            .is_some_and(|text| text.contains("action failed") && text.contains("404")),
        "delete failure toast: {:?}",
        result.toast_text()
    );
    assert!(!result.sidebar_action_sending);
    assert_eq!(result.workspaces.len(), 2, "no state change on failure");
}

#[tokio::test]
async fn context_menu_actions_are_inert_while_sending() {
    let mut app = grouped_app();
    app.sidebar_action_sending = true;
    // `m` does not open the menu while an action is in flight…
    app.handle_key(key(KeyCode::Char('m')));
    assert!(app.context_menu.is_none(), "no menu while sending");
    // …and a menu opened before the flight ignores executes.
    let mut app = grouped_app();
    app.handle_key(key(KeyCode::Char('m')));
    assert!(app.context_menu.is_some());
    app.sidebar_action_sending = true;
    let action = app.handle_key(key(KeyCode::Enter));
    assert_eq!(action, Some(dsh_tui::app::Action::None), "execute inert");
    assert!(app.context_menu.is_none(), "closed without dispatch");
}

fn run_app() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(120, 30)).unwrap()
}
