//! Settings view tests: open/close, schema-driven form rendering, edit +
//! save (payload asserted against the mock gateway), the settings-conflict
//! refresh, and key routing. Keyless: the shared mock gateway, injected
//! events, and `TestBackend` only.

mod common;
use common::{MockAction, MockGateway, SETTINGS_DESCRIBE_OK, leaked, settings_conflict};

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::app::{App, AppEvent, EventChannel};
use dsh_tui::client::WireClient;
use dsh_tui::ui::takeover::Mode;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

/// Run the loop in a spawned task (the describe/save back-channels need a
/// live loop); returns the sender + join handle.
async fn spawn_run(
    mut app: App,
    mut term: Terminal<TestBackend>,
) -> (
    tokio::sync::mpsc::UnboundedSender<AppEvent>,
    tokio::task::JoinHandle<(
        Result<(), dsh_tui::app::AppError>,
        App,
        Terminal<TestBackend>,
    )>,
) {
    let mut channel = EventChannel::new();
    let tx = channel.tx.clone();
    let task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut channel).await;
        (result, app, term)
    });
    (tx, task)
}

async fn join_run(
    task: tokio::task::JoinHandle<(
        Result<(), dsh_tui::app::AppError>,
        App,
        Terminal<TestBackend>,
    )>,
) -> (App, Terminal<TestBackend>) {
    let (result, app, term) = task.await.expect("run task");
    result.expect("run");
    (app, term)
}

/// An app attached to a mock gateway that serves the describe fixture.
async fn settings_app() -> (MockGateway, App) {
    let mock = MockGateway::start().await;
    mock.set_handler("settings.describe", MockAction::Ok(SETTINGS_DESCRIBE_OK))
        .await;
    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    (mock, app)
}

/// Open the settings view and wait for the describe to land.
async fn open_settings(tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>) {
    tx.send(AppEvent::Key(ctrl(KeyCode::Char(',')))).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
}

// ---------------------------------------------------------------------------
// 1. open / close
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ctrl_comma_opens_two_pane_view_and_esc_closes() {
    let (mock, app) = settings_app().await;
    let term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let (tx, task) = spawn_run(app, term).await;
    open_settings(&tx).await;

    // Esc closes; then Ctrl+Q quits the loop (events are processed in
    // channel order, so the mode change is deterministic before the quit).
    tx.send(AppEvent::Key(key(KeyCode::Esc))).unwrap();
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q')))).unwrap();
    let (app, term) = join_run(task).await;

    assert!(matches!(app.mode, Mode::Chat), "Esc returned to the chat");
    let view = format!("{}", term.backend());
    // The last draw is the chat again; the describe POST happened.
    let requests = mock.requests().await;
    assert!(
        requests
            .iter()
            .any(|request| request.path == "/api/settings.describe"),
        "opening the view POSTs settings.describe: {requests:?}"
    );
    mock.stop().await;
    drop(view);
}

#[tokio::test]
async fn nav_sections_render_120x30() {
    let (mock, app) = settings_app().await;
    let term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let (tx, task) = spawn_run(app, term).await;
    open_settings(&tx).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c')))).unwrap();
    let (app, term) = join_run(task).await;

    assert!(matches!(app.mode, Mode::Settings(_)), "view still open");
    let view = format!("{}", term.backend());
    assert!(view.contains("settings"), "frame title: {view}");
    for label in [
        "General",
        "Models",
        "Plugins",
        "Agent presets",
        "Permission presets",
        "locale",
    ] {
        assert!(view.contains(label), "nav label {label}: {view}");
    }
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 2. form render
// ---------------------------------------------------------------------------

async fn rendered_form(width: u16, height: u16) -> String {
    let (mock, app) = settings_app().await;
    let term = Terminal::new(TestBackend::new(width, height)).unwrap();
    let (tx, task) = spawn_run(app, term).await;
    open_settings(&tx).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c')))).unwrap();
    let (_app, term) = join_run(task).await;
    let view = format!("{}", term.backend());
    mock.stop().await;
    view
}

#[tokio::test]
async fn form_renders_labels_and_values_120x30() {
    let view = rendered_form(120, 30).await;
    for (label, value) in [
        ("Language", "en"),
        ("Max tokens", "4096"),
        ("Verbose logging", "false"),
        ("Log level", "normal"),
    ] {
        assert!(view.contains(label), "label {label}: {view}");
        assert!(view.contains(value), "value {value}: {view}");
    }
    assert!(view.contains("read-only"), "raw field marked: {view}");
    assert!(view.contains("revision 1"), "revision line: {view}");
}

#[tokio::test]
async fn form_renders_labels_and_values_60x15() {
    let view = rendered_form(60, 15).await;
    assert!(view.contains("General"), "nav fits: {view}");
    assert!(view.contains("Language"), "label fits: {view}");
    assert!(view.contains("en"), "value fits: {view}");
}

// ---------------------------------------------------------------------------
// 3. edit + save
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_toggle_save_posts_patch_and_returns_to_chat() {
    let (mock, app) = settings_app().await;
    mock.set_handler(
        "settings.update",
        MockAction::Ok(leaked(common::settings_update_ok(
            "general",
            2,
            json!({"language": "zh", "maxTokens": 4096, "verbose": true, "logLevel": "normal"}),
        ))),
    )
    .await;
    let term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let (tx, task) = spawn_run(app, term).await;
    open_settings(&tx).await;

    // Fields (sorted): language, logLevel, maxTokens, metadata, verbose.
    // Tab into the form; toggle verbose (last field), then edit language.
    tx.send(AppEvent::Key(key(KeyCode::Tab))).unwrap();
    for _ in 0..4 {
        tx.send(AppEvent::Key(key(KeyCode::Down))).unwrap();
    }
    tx.send(AppEvent::Key(key(KeyCode::Char(' ')))).unwrap(); // verbose → true
    for _ in 0..4 {
        tx.send(AppEvent::Key(key(KeyCode::Up))).unwrap();
    }
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap(); // edit language ("en")
    tx.send(AppEvent::Key(key(KeyCode::Backspace))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Backspace))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Char('z')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Char('h')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap(); // commit "zh"
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('s')))).unwrap(); // save
    tokio::time::sleep(Duration::from_millis(200)).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q')))).unwrap();
    let (app, _term) = join_run(task).await;

    assert_eq!(app.toast_text(), Some("saved"));
    assert!(matches!(app.mode, Mode::Chat), "save exits to the chat");

    let updates: Vec<serde_json::Value> = mock
        .requests()
        .await
        .iter()
        .filter(|request| request.path == "/api/settings.update")
        .filter_map(|request| serde_json::from_str(&request.body).ok())
        .collect();
    assert_eq!(updates.len(), 1, "one settings.update POST");
    let payload = &updates[0]["payload"];
    assert_eq!(payload["ns"], "general");
    assert_eq!(
        payload["expectedRevision"], 1.0,
        "the described revision rides the write"
    );
    assert_eq!(
        payload["patch"],
        json!({"language": "zh", "verbose": true}),
        "the patch carries only changed keys"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 4. conflict refresh
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conflict_toasts_and_refreshes_the_form() {
    let (mock, app) = settings_app().await;
    let term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let (tx, task) = spawn_run(app, term).await;
    open_settings(&tx).await;

    // Script the failure + the refreshed describe BEFORE saving.
    mock.set_handler(
        "settings.update",
        MockAction::Ok(leaked(settings_conflict("general", 1, 5))),
    )
    .await;
    let refreshed = SETTINGS_DESCRIBE_OK
        .replace(r#""revision": 1"#, r#""revision": 5"#)
        .replace(r#""language": "en"#, r#""language": "fr"#);
    mock.set_handler("settings.describe", MockAction::Ok(leaked(refreshed)))
        .await;

    // Toggle verbose, then save → conflict → re-describe.
    tx.send(AppEvent::Key(key(KeyCode::Tab))).unwrap();
    for _ in 0..4 {
        tx.send(AppEvent::Key(key(KeyCode::Down))).unwrap();
    }
    tx.send(AppEvent::Key(key(KeyCode::Char(' ')))).unwrap();
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('s')))).unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c')))).unwrap();
    let (app, term) = join_run(task).await;

    let toast = app.toast_text().unwrap_or_default();
    assert!(toast.contains("conflict"), "conflict toast: {toast}");
    assert!(
        matches!(app.mode, Mode::Settings(_)),
        "the view stays open after a conflict"
    );
    let describes = mock
        .requests()
        .await
        .iter()
        .filter(|request| request.path == "/api/settings.describe")
        .count();
    assert_eq!(describes, 2, "the conflict re-describes");
    let view = format!("{}", term.backend());
    assert!(view.contains("fr"), "refreshed value renders: {view}");
    assert!(view.contains("revision 5"), "refreshed revision: {view}");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 5. key routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_keys_are_inert_in_settings_and_ctrl_q_quits() {
    let (mock, app) = settings_app().await;
    let term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let (tx, task) = spawn_run(app, term).await;
    open_settings(&tx).await;

    // `q` must not quit, `j` moves the nav (not the chat scroll).
    tx.send(AppEvent::Key(key(KeyCode::Char('q')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Char('j')))).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c')))).unwrap();
    let (app, term) = join_run(task).await;

    let Mode::Settings(state) = &app.mode else {
        panic!("q must not quit; j must stay in the view: {:?}", app.mode)
    };
    assert_eq!(state.selected, 1, "j moved the nav selection");
    let view = format!("{}", term.backend());
    assert!(view.contains("settings"), "view still drawn: {view}");
    mock.stop().await;
}

#[tokio::test]
async fn ctrl_q_quits_from_settings() {
    let (mock, app) = settings_app().await;
    let term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let (tx, task) = spawn_run(app, term).await;
    open_settings(&tx).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q')))).unwrap();
    let (app, _term) = join_run(task).await;
    assert!(!app.running, "Ctrl+Q quit the loop from the settings view");
    mock.stop().await;
}
