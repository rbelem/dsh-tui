//! The `/` and `@` composer catalogs (Q16/Q17): the `/` menu mirrors the
//! core slash commands statically, the `@` menu fetches `skill.list`
//! through the back-channel. These tests drive the app shell against the
//! mock gateway, exactly like `ui_queue_live`.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{App, AppEvent, EventChannel, Focus};
use dsh_tui::client::WireClient;
use dsh_tui::wire::session::SessionId;

use common::MockGateway;

mod common;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

/// The `skill.list` fixture: two skills (one model-invocable, one with a
/// whenToUse hint) so the round-trip exercises every optional field. The
/// value rides the `result.value` slot (rpc.ts's RpcResult envelope).
fn skill_list_ok() -> &'static str {
    r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"skills":[
        {"name":"commit","description":"write a commit message","whenToUse":null,"modelInvocable":true},
        {"name":"triage","description":"sort the inbox","whenToUse":"mail piles up","modelInvocable":false}
    ]}}}"#
}

fn catalog_app(mock: &MockGateway) -> App {
    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.active_session = Some(SessionId("s1".into()));
    // Keys reach the composer (and its popup) only while it holds focus —
    // the default focus is the chat surface.
    app.focus = Focus::Composer;
    app
}

/// Run the loop in a spawned task, let the back-channel land, then quit
/// and return the app (mirrors `ui_queue_live::run_with_settle`).
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slash_menu_lists_the_mirrored_commands() {
    let mock = MockGateway::start().await;
    let mut app = catalog_app(&mock);
    app.composer.insert_char('/');

    // The popup lists the mirrored core commands (no RPC involved).
    let entries = app.popup_entries();
    let labels = entries
        .iter()
        .map(|entry| entry.label.as_str())
        .collect::<Vec<_>>();
    for command in [
        "/help",
        "/compact",
        "/clear",
        "/model",
        "/plan",
        "/permission",
        "/skill",
    ] {
        assert!(
            labels.contains(&command),
            "missing {command} in: {labels:?}"
        );
    }
    assert!(
        entries
            .iter()
            .all(|entry| entry.group == "catalog.group.commands"),
        "commands group header"
    );
    assert!(mock.requests().await.is_empty(), "slash menu must not RPC");

    // The key path: typing `/` in the run loop opens the popup and a
    // second key still reaches the composer (no hang, no fetch).
    let app = catalog_app(&mock);
    let result = run_with_settle(
        app,
        run_app(),
        vec![AppEvent::Key(key(KeyCode::Char('/')))],
        Duration::from_millis(100),
    )
    .await;
    assert_eq!(
        result.composer.popup(),
        Some(dsh_tui::ui::composer::PopupKind::Slash)
    );
    assert!(mock.requests().await.is_empty(), "slash menu must not RPC");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn at_menu_fetches_skills_and_caches_them() {
    let mock = MockGateway::start().await;
    mock.set_handler("skill.list", common::MockAction::Ok(skill_list_ok()))
        .await;
    let app = catalog_app(&mock);

    // Two keys: the first opens the `@` popup (no cache yet), the second
    // asks the run loop to fetch the catalog via the back-channel.
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('@'))),
            AppEvent::Key(key(KeyCode::Char(' '))),
        ],
        Duration::from_millis(300),
    )
    .await;

    // The RPC went out with the active session.
    let requests = mock.requests().await;
    let skill_posts = requests
        .iter()
        .filter(|request| request.path == "/api/skill.list")
        .collect::<Vec<_>>();
    assert_eq!(skill_posts.len(), 1, "skill.list POST expected");
    let body: serde_json::Value = serde_json::from_str(&skill_posts[0].body).unwrap();
    assert_eq!(body["method"], "skill.list");
    assert_eq!(body["payload"]["sessionId"], "s1");

    // The loaded catalog is cached; the loading flag cleared.
    let catalog = result.at_catalog.expect("catalog cached");
    assert!(!catalog.loading);
    assert_eq!(catalog.skills.len(), 2);
    assert_eq!(catalog.skills[0].name, "commit");
    assert_eq!(
        catalog.skills[1].when_to_use.as_deref(),
        Some("mail piles up")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn at_menu_failure_toasts_and_retries_on_next_open() {
    let mock = MockGateway::start().await;
    mock.set_handler("skill.list", common::MockAction::NotFound)
        .await;
    let app = catalog_app(&mock);

    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('@'))),
            AppEvent::Key(key(KeyCode::Char(' '))),
        ],
        Duration::from_millis(300),
    )
    .await;

    // A failed fetch leaves the catalog empty (not cached) and toasts.
    let catalog = result.at_catalog.as_ref().expect("catalog slot exists");
    assert!(catalog.skills.is_empty(), "failure must not cache entries");
    assert!(!catalog.loading);
    assert!(
        result.toast.is_some(),
        "a failure toast is expected (catalog.failed)"
    );

    // Reopening the `@` popup retries: flip the handler to success, then
    // trigger + settle again (two keys: open, then request). The first run
    // left focus on the chat surface, so restore the composer.
    mock.set_handler("skill.list", common::MockAction::Ok(skill_list_ok()))
        .await;
    let mut app2 = result;
    app2.focus = Focus::Composer;
    app2.composer.insert_char('@');
    let result2 = run_with_settle(
        app2,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('@'))),
            AppEvent::Key(key(KeyCode::Char(' '))),
        ],
        Duration::from_millis(300),
    )
    .await;
    let requests = mock.requests().await;
    let skill_posts = requests
        .iter()
        .filter(|request| request.path == "/api/skill.list")
        .count();
    assert_eq!(skill_posts, 2, "reopen must re-fetch");
    assert_eq!(
        result2.at_catalog.as_ref().map(|c| c.skills.len()),
        Some(2),
        "retry succeeded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slash_menu_filters_by_typed_prefix() {
    let mock = MockGateway::start().await;
    let mut app = catalog_app(&mock);
    app.composer.insert_char('/');

    // Type "mo": only /model survives the case-insensitive substring
    // filter.
    app.composer.insert_char('m');
    app.composer.insert_char('o');
    let entries = app.popup_entries();
    assert_eq!(entries.len(), 1, "filtered entries: {entries:?}");
    assert_eq!(entries[0].label, "/model");

    // Enter accepts the filtered entry: the buffer becomes "/model ".
    app.composer.popup_accept(&entries[0].insert);
    assert_eq!(app.composer.buffer(), "/model ");
    assert_eq!(
        app.composer.popup(),
        None,
        "trailing space closes the popup"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn at_menu_accept_inserts_skill_name() {
    let mock = MockGateway::start().await;
    mock.set_handler("skill.list", common::MockAction::Ok(skill_list_ok()))
        .await;
    let mut app = catalog_app(&mock);
    app.composer.insert_char('@');
    app.at_catalog = Some(dsh_tui::app::AtCatalog {
        skills: vec![dsh_tui::wire::skills::SkillEntry {
            name: "commit".into(),
            description: "write a commit message".into(),
            when_to_use: None,
            model_invocable: true,
        }],
        loading: false,
    });

    let entries = app.popup_entries();
    assert_eq!(entries.len(), 1);
    app.composer.popup_accept(&entries[0].insert);
    assert_eq!(app.composer.buffer(), "@commit ");
    assert_eq!(app.composer.popup(), None);
}
