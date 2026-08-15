//! New-session hero + workspace picker tests (Q2): the empty-chat hero,
//! the `n` picker (workspace list from the app's workspace snapshot + a
//! "no workspace" entry), the create flow through the mock gateway
//! (payload asserted), the no-workspace create, the error path with a
//! re-armed guard, and keymap inertness while other popups/editors own
//! the keyboard. Keyless + `TestBackend` throughout; the mock serves
//! `session.create` only.

mod common;
use common::{MockAction, MockGateway};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{Action, App, AppEvent, EventChannel, Focus};
use dsh_tui::client::WireClient;
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

fn workspace(id: &str, title: &str) -> WorkspaceView {
    WorkspaceView {
        workspace_id: WorkspaceId(id.into()),
        path: format!("/tmp/{id}"),
        title: title.into(),
        session_ids: vec![],
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }
}

/// An app with two workspaces (durable order beta first) and no sessions.
/// The picker opens with `n`, a chat/sidebar-bound key — pin the focus
/// explicitly (the app boots in the composer).
fn picker_app() -> App {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.workspaces = vec![workspace("wA", "alpha"), workspace("wB", "beta")];
    app.workspace_order = vec![WorkspaceId("wB".into()), WorkspaceId("wA".into())];
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
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

/// Draw the current state (Esc forces an immediate draw) and quit.
async fn draw_and_quit(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut events = events;
    events.push(AppEvent::Key(key(KeyCode::Esc)));
    events.push(AppEvent::Key(ctrl(KeyCode::Char('q'))));
    run_with(app, term, events).await;
}

async fn view_at(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(app, &mut term, Vec::new()).await;
    format!("{}", term.backend())
}

/// Drive the loop in a task so an async back-channel result (the spawned
/// `session.create`) lands BEFORE the quit key: send the events, wait for
/// the deterministic request capture, a short bounded wait for the done
/// event to fold (the response tail has no test-side observable — the
/// mock's `wait_for_posts` doc), then quit. Returns the app + final view.
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

// ---------------------------------------------------------------------------
// 1. the hero
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hero_renders_in_the_empty_state_120x30() {
    let mut app = App::default();
    let view = view_at(&mut app, 120, 30).await;
    assert!(view.contains("dsh-tui"), "title: {view}");
    assert!(
        view.contains("a terminal client for the deepseek harness"),
        "subtitle: {view}"
    );
    assert!(view.contains("n — new session"), "new-session hint: {view}");
    assert!(
        view.contains("tab — find a session in the sidebar"),
        "sidebar hint: {view}"
    );
}

#[tokio::test]
async fn hero_renders_at_60x15() {
    let mut app = App::default();
    let view = view_at(&mut app, 60, 15).await;
    assert!(view.contains("dsh-tui"), "title at 60x15: {view}");
    assert!(view.contains("n — new session"), "hint at 60x15: {view}");
}

#[tokio::test]
async fn hero_disappears_with_a_selected_session() {
    let mut app = App::default();
    app.sessions = vec![summary("s1")];
    app.active_session = Some(SessionId("s1".into()));
    let view = view_at(&mut app, 120, 30).await;
    assert!(
        !view.contains("a terminal client for the deepseek harness"),
        "no hero with a session: {view}"
    );
}

// ---------------------------------------------------------------------------
// 2. picker open/close
// ---------------------------------------------------------------------------

#[tokio::test]
async fn n_opens_the_picker_and_esc_closes_without_a_post() {
    let mock = MockGateway::start().await;
    let mut app = picker_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('n'))),
            AppEvent::Key(key(KeyCode::Esc)),
        ],
    )
    .await;
    assert!(app.new_session.is_none(), "Esc closed the picker");
    assert!(
        mock.requests().await.is_empty(),
        "no POSTs from open + close"
    );
    mock.stop().await;
}

#[tokio::test]
async fn picker_lists_workspaces_in_durable_order_plus_no_workspace() {
    let mut app = picker_app();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    // No Esc before the quit: it would close the picker and redraw over it
    // (the quit key itself doesn't draw).
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('n'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());

    assert!(view.contains(" new session "), "picker title: {view}");
    let beta = view.find("beta").expect("beta row");
    let alpha = view.find("alpha").expect("alpha row");
    let none = view.find("no workspace").expect("no-workspace row");
    assert!(
        beta < alpha && alpha < none,
        "durable order, no-workspace last: {view}"
    );
    assert!(
        view.contains("enter creates · esc cancels"),
        "hint row: {view}"
    );
}

// ---------------------------------------------------------------------------
// 3. picker navigation
// ---------------------------------------------------------------------------

#[test]
fn picker_nav_moves_and_clamps() {
    let mut app = picker_app();
    app.handle_key(key(KeyCode::Char('n')));
    // Entries: beta, alpha, no workspace.
    assert_eq!(app.handle_key(key(KeyCode::Char('j'))), Some(Action::None));
    assert_eq!(app.new_session.as_ref().unwrap().selected, 1, "alpha");
    assert_eq!(app.handle_key(key(KeyCode::Char('j'))), Some(Action::None));
    assert_eq!(
        app.new_session.as_ref().unwrap().selected,
        2,
        "no workspace"
    );
    assert_eq!(app.handle_key(key(KeyCode::Char('j'))), Some(Action::None));
    assert_eq!(app.new_session.as_ref().unwrap().selected, 2, "clamped");
    assert_eq!(app.handle_key(key(KeyCode::Char('k'))), Some(Action::None));
    assert_eq!(app.new_session.as_ref().unwrap().selected, 1, "back up");
}

// ---------------------------------------------------------------------------
// 4/5. the create flow
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn enter_creates_under_the_selected_workspace_and_switches() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.create",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"sessionId":"s9"}}}"#,
        ),
    )
    .await;
    let mut app = picker_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    // n → j (select "alpha") → Enter → create → done folds → quit.
    let (app, view) = run_live(
        app,
        vec![
            AppEvent::Key(key(KeyCode::Char('n'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        after_posts(&mock, "/api/session.create", 1),
    )
    .await;

    let posts = mock.wait_for_posts("/api/session.create", 1).await;
    let payload = posts[0].get("payload").cloned().unwrap_or_default();
    assert_eq!(
        payload.get("workspaceId"),
        Some(&serde_json::json!("wA")),
        "alpha's workspace id: {payload}"
    );
    assert_eq!(payload.get("cwd"), None, "no cwd sent: {payload}");

    assert!(app.new_session.is_none(), "picker closed on success");
    assert_eq!(app.active_session, Some(SessionId("s9".into())));
    assert!(
        app.sessions
            .iter()
            .any(|summary| summary.session_id.0 == "s9"),
        "the new session lands in the sidebar list"
    );
    assert!(view.contains("created: s9"), "toast: {view}");
    assert!(view.contains("session s9"), "status line switched: {view}");
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_workspace_entry_creates_without_a_workspace_id() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.create",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"sessionId":"s8"}}}"#,
        ),
    )
    .await;
    let mut app = picker_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let (app, _view) = run_live(
        app,
        vec![
            AppEvent::Key(key(KeyCode::Char('n'))),
            // j j past both workspaces → the trailing "no workspace" entry.
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        after_posts(&mock, "/api/session.create", 1),
    )
    .await;

    let posts = mock.wait_for_posts("/api/session.create", 1).await;
    let payload = posts[0].get("payload").cloned().unwrap_or_default();
    assert_eq!(
        payload.get("workspaceId"),
        None,
        "no workspaceId in the payload: {payload}"
    );
    assert_eq!(app.active_session, Some(SessionId("s8".into())));
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 6. error path: toast, picker back, guard re-armed
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn create_failure_toasts_keeps_the_picker_and_re_arms() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.create",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":false,"error":{"code":"workspace-not-found","message":"gone","details":{"workspaceId":"wB"}}}}"#,
        ),
    )
    .await;
    let mut app = picker_app();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    // First attempt fails (beta selected by default — durable order).
    let (app, view) = run_live(
        app,
        vec![
            AppEvent::Key(key(KeyCode::Char('n'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        after_posts(&mock, "/api/session.create", 1),
    )
    .await;

    assert!(app.new_session.is_some(), "picker stays open on failure");
    assert!(!app.new_session.as_ref().unwrap().sending, "guard re-armed");
    assert_eq!(app.active_session, None, "no session on failure");
    assert!(view.contains("create failed:"), "error toast: {view}");

    // Retry works: the guard re-armed, the picker kept its selection.
    mock.set_handler(
        "session.create",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"sessionId":"s7"}}}"#,
        ),
    )
    .await;
    let (app, _view) = run_live(
        app,
        vec![AppEvent::Key(key(KeyCode::Enter))],
        after_posts(&mock, "/api/session.create", 2),
    )
    .await;
    assert_eq!(app.active_session, Some(SessionId("s7".into())), "retry");
    assert!(app.new_session.is_none(), "picker closed on the retry");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 7. keymap: n is inert while another surface owns the keyboard
// ---------------------------------------------------------------------------

#[test]
fn n_is_inert_while_popups_or_editors_are_open() {
    let mut app = picker_app();
    // Queue popup owns keys (opened via Alt+q with a queue present — here
    // the flag suffices for the swallow order).
    app.queue_popup_open = true;
    assert_eq!(app.handle_key(key(KeyCode::Char('n'))), Some(Action::None));
    assert!(app.new_session.is_none(), "queue popup swallows n");
    app.queue_popup_open = false;

    // The rename editor owns sidebar keys.
    app.sessions = vec![summary("s1")];
    app.focus = Focus::Sidebar;
    app.handle_key(key(KeyCode::Char('r')));
    assert!(app.rename_editor.is_some(), "editor open");
    assert_eq!(app.handle_key(key(KeyCode::Char('n'))), Some(Action::None));
    assert!(app.new_session.is_none(), "rename editor swallows n");
    app.handle_key(key(KeyCode::Esc));

    // The picker itself: n is inert inside it.
    app.handle_key(key(KeyCode::Char('n')));
    assert!(app.new_session.is_some(), "picker open");
    let before = app.new_session.as_ref().unwrap().selected;
    assert_eq!(app.handle_key(key(KeyCode::Char('n'))), Some(Action::None));
    assert_eq!(
        app.new_session.as_ref().unwrap().selected,
        before,
        "n does not navigate or reopen"
    );
}
