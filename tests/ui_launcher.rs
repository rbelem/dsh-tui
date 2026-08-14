//! The Ctrl+P global launcher (Q17): fuzzy search over commands, skills,
//! and settings actions; Enter dispatches immediately. State assertions go
//! through `handle_key` (keyless); dispatch tests drive the mock gateway.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{Action, App, AppEvent, EventChannel};
use dsh_tui::client::WireClient;
use dsh_tui::ui::takeover::Mode;
use dsh_tui::wire::session::SessionId;

use common::MockGateway;

mod common;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

/// The `skill.list` fixture (two skills; one with a whenToUse hint).
fn skill_list_ok() -> &'static str {
    r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"skills":[
        {"name":"commit","description":"write a commit message","whenToUse":null,"modelInvocable":true},
        {"name":"triage","description":"sort the inbox","whenToUse":"mail piles up","modelInvocable":false}
    ]}}}"#
}

fn launcher_app(mock: &MockGateway) -> App {
    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.active_session = Some(SessionId("s1".into()));
    app
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

#[tokio::test]
async fn ctrl_p_opens_closes_and_clears_search() {
    let mock = MockGateway::start().await;
    let mut app = launcher_app(&mock);

    // Ctrl+P opens the launcher (and requests the skill catalog).
    assert_eq!(
        app.handle_key(ctrl(KeyCode::Char('p'))),
        Some(Action::RequestCatalog)
    );
    assert!(app.launcher.is_some(), "launcher open");

    // Typing filters; Esc closes.
    for c in "xy".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.launcher.as_ref().unwrap().search.buffer(), "xy");
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Some(Action::None));
    assert!(app.launcher.is_none(), "launcher closed");

    // Reopening starts with a clean search line.
    app.handle_key(ctrl(KeyCode::Char('p')));
    assert_eq!(
        app.launcher.as_ref().unwrap().search.buffer(),
        "",
        "search cleared"
    );
    assert!(app.launcher.is_some());
}

#[tokio::test]
async fn fuzzy_ranks_runs_above_scattered() {
    let mock = MockGateway::start().await;
    let mut app = launcher_app(&mock);
    app.at_catalog = Some(dsh_tui::app::AtCatalog {
        skills: vec![dsh_tui::wire::skills::SkillEntry {
            name: "commit".into(),
            description: "write a commit message".into(),
            when_to_use: None,
            model_invocable: true,
        }],
        loading: false,
    });
    app.handle_key(ctrl(KeyCode::Char('p')));

    // "cle" is a run inside /clear (63) and scattered in "cycle locale"
    // (43) — the run ranks first.
    for c in "cle".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    let entries = app.launcher_entries_filtered();
    assert_eq!(
        entries[0].label, "/clear",
        "run match ranks above scattered"
    );
    assert_eq!(entries[1].label, "cycle locale");

    // No matches: empty list.
    for c in "zzzz".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert!(app.launcher_entries_filtered().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_dispatch_posts_the_prompt() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.prompt",
        common::MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"accepted":true}}}"#,
        ),
    )
    .await;
    let app = launcher_app(&mock);

    // Keys go through the run loop so the Submit action dispatches the
    // prompt POST (the first Ctrl+P also fetches skill.list — the mock
    // 404s it, harmless).
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(ctrl(KeyCode::Char('p'))),
            AppEvent::Key(key(KeyCode::Char('c'))),
            AppEvent::Key(key(KeyCode::Char('o'))),
            AppEvent::Key(key(KeyCode::Char('m'))),
            AppEvent::Key(key(KeyCode::Char('p'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;
    assert!(result.launcher.is_none(), "launcher closed on dispatch");
    assert_eq!(result.composer.buffer(), "", "composer taken on submit");

    // The prompt POST carried the command text, dispatched immediately.
    let requests = mock.requests().await;
    let prompt = requests
        .iter()
        .find(|request| request.path == "/api/session.prompt")
        .expect("session.prompt POST");
    let body: serde_json::Value = serde_json::from_str(&prompt.body).unwrap();
    assert_eq!(body["payload"]["content"][0]["text"], "/compact");
    assert_eq!(body["payload"]["mode"], "queue");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_dispatch_posts_the_skill_token() {
    let mock = MockGateway::start().await;
    mock.set_handler("skill.list", common::MockAction::Ok(skill_list_ok()))
        .await;
    mock.set_handler(
        "session.prompt",
        common::MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"accepted":true}}}"#,
        ),
    )
    .await;
    let app = launcher_app(&mock);

    // Phase 1: open the launcher, let the skill.list cache land.
    let app = run_with_settle(
        app,
        run_app(),
        vec![AppEvent::Key(ctrl(KeyCode::Char('p')))],
        Duration::from_millis(300),
    )
    .await;
    assert!(app.launcher.is_some(), "launcher stays open");

    // Phase 2: type the filter and dispatch through the run loop.
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Key(key(KeyCode::Char('t'))),
            AppEvent::Key(key(KeyCode::Char('r'))),
            AppEvent::Key(key(KeyCode::Char('i'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(300),
    )
    .await;
    assert!(result.launcher.is_none());

    let requests = mock.requests().await;
    assert!(
        requests.iter().any(|r| r.path == "/api/skill.list"),
        "skill.list fetched on first open"
    );
    let prompt = requests
        .iter()
        .find(|request| request.path == "/api/session.prompt")
        .expect("session.prompt POST");
    let body: serde_json::Value = serde_json::from_str(&prompt.body).unwrap();
    assert_eq!(body["payload"]["content"][0]["text"], "@triage");
}

#[tokio::test]
async fn action_dispatch_executes_in_place() {
    let mock = MockGateway::start().await;
    let mut app = launcher_app(&mock);
    app.handle_key(ctrl(KeyCode::Char('p')));

    // "open settings" — the action label localizes; "set" matches it.
    for c in "set".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    let entries = app.launcher_entries_filtered();
    assert_eq!(entries[0].label, "open settings");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Action::FetchSettings)
    );
    assert!(app.launcher.is_none());
    assert!(matches!(app.mode, Mode::Settings(_)), "settings opened");

    // Reopen (back to Chat — the settings takeover swallowed keys), pick
    // the theme picker action.
    app.mode = Mode::Chat;
    app.handle_key(ctrl(KeyCode::Char('p')));
    for c in "the".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    let entries = app.launcher_entries_filtered();
    assert_eq!(entries[0].label, "theme picker");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.theme_picker.open, "theme picker opened");
    assert!(app.launcher.is_none());

    // Reopen (close the picker first — it swallows keys), pick quit.
    app.theme_picker.open = false;
    app.handle_key(ctrl(KeyCode::Char('p')));
    for c in "qui".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Some(Action::Quit));
    assert!(app.launcher.is_none());
}

#[tokio::test]
async fn launcher_swallows_keys_and_stays_inert_in_settings() {
    let mock = MockGateway::start().await;
    let mut app = launcher_app(&mock);

    // j/k move the selection while open; Esc closes.
    app.handle_key(ctrl(KeyCode::Char('p')));
    assert_eq!(app.launcher.as_ref().unwrap().selected, 0);
    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.launcher.as_ref().unwrap().selected, 1);
    app.handle_key(key(KeyCode::Char('k')));
    assert_eq!(app.launcher.as_ref().unwrap().selected, 0);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.launcher.is_none());
    // The composer buffer was never touched by launcher keys.
    assert_eq!(app.composer.buffer(), "");

    // Inert in the seed popup.
    app.composer.insert_char('/');
    app.handle_key(ctrl(KeyCode::Char('p')));
    assert!(app.launcher.is_none(), "inert while the seed popup is open");

    // Inert in the settings view (takeovers swallow all keys).
    app.composer.take();
    app.mode = Mode::Settings(dsh_tui::ui::settings::SettingsState::new());
    app.handle_key(ctrl(KeyCode::Char('p')));
    assert!(app.launcher.is_none(), "inert in the settings takeover");
    assert!(matches!(app.mode, Mode::Settings(_)), "settings untouched");

    // Ctrl+P toggles the launcher closed when already open.
    app.mode = Mode::Chat;
    app.handle_key(ctrl(KeyCode::Char('p')));
    assert!(app.launcher.is_some());
    app.handle_key(ctrl(KeyCode::Char('p')));
    assert!(app.launcher.is_none(), "Ctrl+P toggles closed");
}

#[tokio::test]
async fn launcher_renders_title_and_search() {
    let mock = MockGateway::start().await;
    let mut app = launcher_app(&mock);

    // Open through the run loop; Ctrl+C quits without redrawing, so the
    // buffer keeps the launcher draw (mirrors the ui_surfaces popup test).
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = EventChannel::new();
    let events = vec![
        AppEvent::Key(ctrl(KeyCode::Char('p'))),
        AppEvent::Key(ctrl(KeyCode::Char('c'))),
    ];
    for event in events {
        channel.tx.send(event).expect("event channel");
    }
    app.run(&mut term, &mut channel).await.expect("run");
    let surface = format!("{}", term.backend());
    assert!(surface.contains("launcher"), "title: {surface}");
    assert!(
        surface.contains("search commands, skills, actions"),
        "placeholder: {surface}"
    );
    assert!(surface.contains("/help"), "commands listed: {surface}");
    assert!(
        surface.contains("open settings"),
        "actions listed: {surface}"
    );
}
