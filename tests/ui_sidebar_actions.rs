//! Sidebar session actions (rename/fork/archive): the `r`/`f`/`a` keymap,
//! inline rename editor, back-channel POSTs, and toasts — against the mock
//! gateway (keyless).

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{Action, App, AppEvent, EventChannel, Focus};
use dsh_tui::client::WireClient;
use dsh_tui::ui::SidebarGroup;
use dsh_tui::wire::events::HostFrame;
use dsh_tui::wire::session::{SessionId, SessionSummary};

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
        updated_at: 0.0,
        running: false,
        blank: false,
        parent_session_id: None,
        origin: None,
        cwd: None,
        agent_preset: None,
        projections: None,
    }
}

fn sidebar_app(mock: &MockGateway) -> App {
    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.sessions = vec![summary("s1"), summary("s2"), summary("s3")];
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Sidebar;
    app
}

fn rename_ok(title: &str, seq: i64) -> String {
    format!(
        r#"{{"type":"server-response","rpcId":"{{rpcId}}","result":{{"ok":true,"value":{{"title":"{title}","seq":{seq}}}}}}}"#
    )
}

fn fork_ok(session_id: &str) -> &'static str {
    Box::leak(
        format!(
            r#"{{"type":"server-response","rpcId":"{{rpcId}}","result":{{"ok":true,"value":{{"sessionId":"{session_id}"}}}}}}"#
        )
        .into_boxed_str(),
    )
}

fn archive_ok(ids: &[&str]) -> String {
    let ids = ids
        .iter()
        .map(|id| format!("\"{id}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"type":"server-response","rpcId":"{{rpcId}}","result":{{"ok":true,"value":{{"archivedSessionIds":[{ids}]}}}}}}"#
    )
}

/// Run the loop in a spawned task, let the back-channel land, then quit
/// and return the app.
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

fn run_app() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(120, 30)).unwrap()
}

async fn posts_to(mock: &MockGateway, path: &str) -> Vec<serde_json::Value> {
    mock.requests()
        .await
        .iter()
        .filter(|request| request.path == path)
        .filter_map(|request| serde_json::from_str(&request.body).ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_flow_edits_row_and_toasts() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.rename",
        common::MockAction::Ok(Box::leak(rename_ok("new title", 9).into_boxed_str())),
    )
    .await;
    let app = sidebar_app(&mock);

    // Sidebar focus, selected row 0 (s1): `r` opens the editor seeded with
    // the displayed title (no projection → the session id), clear it and
    // type a new one, Enter commits.
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('r'))),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Char('n'))),
            AppEvent::Key(key(KeyCode::Char('e'))),
            AppEvent::Key(key(KeyCode::Char('w'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;

    // The POST carried sessionId + the typed title.
    let posts = posts_to(&mock, "/api/session.rename").await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["payload"]["sessionId"], "s1");
    assert_eq!(posts[0]["payload"]["title"], "new");

    // Success: the row's title projection updated in place + toast.
    let summary = &result.sessions[0];
    assert_eq!(
        summary
            .projections
            .as_ref()
            .and_then(|b| b.values.get("title"))
            .and_then(|v| v.as_str()),
        Some("new title"),
        "row updated in place"
    );
    assert_eq!(result.toast_text(), Some("renamed"));
    assert!(result.rename_editor.is_none(), "editor closed");
    assert!(!result.sidebar_action_sending, "guard re-armed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_esc_cancels_without_post() {
    let mock = MockGateway::start().await;
    let app = sidebar_app(&mock);

    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('r'))),
            AppEvent::Key(key(KeyCode::Char('x'))),
            AppEvent::Key(key(KeyCode::Esc)),
        ],
        Duration::from_millis(200),
    )
    .await;

    assert!(result.rename_editor.is_none(), "editor closed");
    assert!(
        posts_to(&mock, "/api/session.rename").await.is_empty(),
        "no POST"
    );
    assert!(result.sessions[0].projections.is_none(), "row untouched");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_flow_posts_and_toasts_new_id() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.fork", common::MockAction::Ok(fork_ok("sF")))
        .await;
    let app = sidebar_app(&mock);

    let result = run_with_settle(
        app,
        run_app(),
        vec![AppEvent::Key(key(KeyCode::Char('f')))],
        Duration::from_millis(300),
    )
    .await;

    let posts = posts_to(&mock, "/api/session.fork").await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["payload"]["sessionId"], "s1");
    assert!(
        posts[0]["payload"].get("atSeq").is_none(),
        "v1 forks anchor the cut host-side"
    );
    assert_eq!(result.toast_text(), Some("forked: sF"));
    assert!(!result.sidebar_action_sending);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_child_appears_via_host_session_added() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.fork", common::MockAction::Ok(fork_ok("sF")))
        .await;
    let app = sidebar_app(&mock);
    let host = HostFrame::HostSessionAdded {
        session_id: SessionId("sF".into()),
        blank: true,
        parent_session_id: Some(SessionId("s1".into())),
        origin: None,
        cwd: None,
        agent_preset: None,
    };

    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('f'))),
            AppEvent::HostFrame(host),
        ],
        Duration::from_millis(300),
    )
    .await;

    assert!(
        result
            .sessions
            .iter()
            .any(|s| s.session_id == SessionId("sF".into())),
        "fork child listed via host/session-added"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_flow_updates_set_and_unreachable_by_nav() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "workspace.archiveSession",
        common::MockAction::Ok(Box::leak(archive_ok(&["s1", "s5"]).into_boxed_str())),
    )
    .await;
    let app = sidebar_app(&mock);

    let result = run_with_settle(
        app,
        run_app(),
        vec![AppEvent::Key(key(KeyCode::Char('a')))],
        Duration::from_millis(300),
    )
    .await;

    let posts = posts_to(&mock, "/api/workspace.archiveSession").await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["payload"]["sessionId"], "s1");

    assert_eq!(result.toast_text(), Some("archived"));
    assert_eq!(
        result.archived_session_ids,
        vec![SessionId("s1".into()), SessionId("s5".into())]
    );
    // Archived beats membership: s1 is unreachable by nav, s2 is now the
    // first visible session.
    let groups = result.sidebar_groups();
    assert_eq!(SidebarGroup::visible_len(&groups), 2);
    let index = SidebarGroup::visible_session(&groups, 0).expect("first row");
    assert_eq!(result.sessions[index].session_id, SessionId("s2".into()));
    assert!(!result.sidebar_action_sending);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failure_toasts_and_guard_rearms_for_retry() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.rename", common::MockAction::NotFound)
        .await;
    let app = sidebar_app(&mock);

    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('r'))),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Backspace)),
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
        "failure toast: {:?}",
        result.toast_text()
    );
    assert!(!result.sidebar_action_sending, "guard re-armed");
    assert!(result.sessions[0].projections.is_none(), "no state change");

    // Second attempt against a healthy handler succeeds.
    mock.set_handler(
        "session.rename",
        common::MockAction::Ok(Box::leak(rename_ok("ok", 1).into_boxed_str())),
    )
    .await;
    let mut app = result;
    app.focus = Focus::Sidebar;
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('r'))),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Backspace)),
            AppEvent::Key(key(KeyCode::Char('y'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;
    assert_eq!(posts_to(&mock, "/api/session.rename").await.len(), 2);
    assert_eq!(result.toast_text(), Some("renamed"), "retry succeeded");
}

#[tokio::test]
async fn keymap_inert_without_selection_or_while_sending() {
    let mock = MockGateway::start().await;
    let mut app = App::default();
    app.focus = Focus::Sidebar;
    // No sessions: fork/archive/rename are inert.
    assert_eq!(app.handle_key(key(KeyCode::Char('f'))), Some(Action::None));
    assert_eq!(app.handle_key(key(KeyCode::Char('a'))), Some(Action::None));
    assert!(app.rename_editor.is_none(), "no editor without a selection");

    // While an action is in flight, every sidebar action is inert.
    let mut app = sidebar_app(&mock);
    app.sidebar_action_sending = true;
    assert_eq!(app.handle_key(key(KeyCode::Char('f'))), Some(Action::None));
    assert_eq!(app.handle_key(key(KeyCode::Char('a'))), Some(Action::None));
    assert_eq!(app.handle_key(key(KeyCode::Char('r'))), Some(Action::None));
    assert!(app.rename_editor.is_none(), "no editor while sending");
}

#[tokio::test]
async fn rename_editor_navigation_is_inert_while_open() {
    let mock = MockGateway::start().await;
    let mut app = sidebar_app(&mock);
    app.handle_key(key(KeyCode::Char('r')));
    assert!(app.rename_editor.is_some(), "editor open");

    // j/k/Enter-with-empty nav keys stay in the editor and move nothing.
    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.sidebar.selected, 0, "nav inert while editing");
    app.handle_key(key(KeyCode::Esc));
    assert!(app.rename_editor.is_none(), "Esc cancels");
}
