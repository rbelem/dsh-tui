//! First-run onboarding (#47): the state machine (no config → onboarding
//! at startup, the Q&A transitions, completion persists the flag to the
//! config file), the Esc semantics, and the full UI flow against the mock
//! gateway (onboarding renders on first run, completion reaches the chat).

use std::path::Path;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{Action, App, AppEvent, EventChannel};
use dsh_tui::client::WireClient;
use dsh_tui::theme::Config;
use dsh_tui::ui::onboarding::{OnboardingStep, OnboardingView};
use dsh_tui::ui::takeover::Mode;
use dsh_tui::wire::session::{SessionId, WorkspaceId};
use dsh_tui::wire::workspace::WorkspaceView;

mod common;
use common::MockGateway;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

// ---------------------------------------------------------------------------
// env isolation (the XDG config root, like tests/i18n.rs)
// ---------------------------------------------------------------------------

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_env_var(key: &str, value: Option<&str>, f: impl FnOnce()) {
    let previous = std::env::var(key).ok();
    match value {
        Some(value) => unsafe { std::env::set_var(key, value) },
        None => unsafe { std::env::remove_var(key) },
    }
    f();
    match previous {
        Some(previous) => unsafe { std::env::set_var(key, previous) },
        None => unsafe { std::env::remove_var(key) },
    }
}

fn with_config_root(base: &Path, f: impl FnOnce(&Path)) {
    with_env_var("XDG_CONFIG_HOME", Some(base.to_str().unwrap()), || {
        let root = dirs::config_dir().expect("config dir").join("dsh-tui");
        f(&root);
    });
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("dsh-tui-onboard-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// 1. the state machine
// ---------------------------------------------------------------------------

#[test]
fn first_run_detection_and_completion_persists_the_flag() {
    let dir = TempDir::new("sm");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(&dir.0, |config_root| {
        // No config file: onboarding enters at startup.
        let mut app = App::default();
        app.load_theme_config();
        app.maybe_enter_onboarding();
        assert!(
            matches!(app.mode, Mode::Onboarding(_)),
            "no config → onboarding"
        );

        // With the completed flag: onboarding is skipped.
        Config {
            onboarding_complete: true,
            ..Config::default()
        }
        .save()
        .expect("save");
        let mut app = App::default();
        app.load_theme_config();
        app.maybe_enter_onboarding();
        assert!(matches!(app.mode, Mode::Chat), "flag → no onboarding");
        assert!(Config::load().onboarding_complete, "flag persisted");
        let _ = config_root;
    });
}

#[test]
fn qa_transitions_and_completion_writes_the_values() {
    let dir = TempDir::new("qa");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(&dir.0, |_config_root| {
        let mut app = App::default();
        app.load_theme_config();
        app.maybe_enter_onboarding();
        let Mode::Onboarding(state) = &app.mode else {
            panic!("onboarding mode");
        };
        assert_eq!(state.step, OnboardingStep::Workspace);

        // Workspace step: empty Enter stays + hints.
        app.handle_key(key(KeyCode::Enter));
        assert!(
            app.hint
                .as_deref()
                .is_some_and(|h| h.contains("directory path")),
            "empty-path hint: {:?}",
            app.hint
        );
        let Mode::Onboarding(state) = &app.mode else {
            panic!("still onboarding");
        };
        assert_eq!(state.step, OnboardingStep::Workspace);

        // Type a path, Enter → the preset question.
        for c in "/tmp/ws".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        let Mode::Onboarding(state) = &app.mode else {
            panic!("still onboarding");
        };
        assert_eq!(state.step, OnboardingStep::Preset);

        // Esc goes back to the workspace question.
        app.handle_key(key(KeyCode::Esc));
        let Mode::Onboarding(state) = &app.mode else {
            panic!("still onboarding");
        };
        assert_eq!(state.step, OnboardingStep::Workspace, "Esc goes back");

        // Re-advance, type a preset, Enter completes and dispatches create.
        app.handle_key(key(KeyCode::Enter));
        for c in "devops".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        let action = app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            action,
            Some(Action::CreateWorkspace("/tmp/ws".into())),
            "completion dispatches workspace.create"
        );
        assert!(matches!(app.mode, Mode::Chat), "back to the chat");
        assert_eq!(app.toast_text(), Some("welcome — workspace set up"));

        // The config file carries the flag + chosen values.
        let config = Config::load();
        assert!(config.onboarding_complete, "flag persisted");
        assert_eq!(config.workspace_path.as_deref(), Some("/tmp/ws"));
        assert_eq!(config.agent_preset.as_deref(), Some("devops"));
    });
}

#[test]
fn esc_quits_on_the_first_question() {
    let mut app = App::default();
    app.load_theme_config();
    app.maybe_enter_onboarding();
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Some(Action::Quit));
}

// ---------------------------------------------------------------------------
// 2. the UI flow against the mock gateway
// ---------------------------------------------------------------------------

fn workspace_create_ok() -> &'static str {
    r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"workspace":{"workspaceId":"w1","path":"/tmp/ws","title":"/tmp/ws","sessionIds":[],"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z"},"created":true}}}"#
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

#[tokio::test]
// The ENV_LOCK serializes the env-dependent tests; it must stay held across
// the async run (the completion writes the config mid-await) — intentional.
#[allow(clippy::await_holding_lock)]
async fn onboarding_renders_on_first_run_and_reaches_chat() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TempDir::new("ui");
    // Redirect the config root for the whole async run (the completion
    // writes the flag mid-await).
    let prev_xdg = std::env::var("XDG_CONFIG_HOME").ok();
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.0.to_str().unwrap()) };

    let mock = MockGateway::start().await;
    mock.set_handler(
        "workspace.create",
        common::MockAction::Ok(workspace_create_ok()),
    )
    .await;

    let mut app = App::default();
    app.load_theme_config();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.maybe_enter_onboarding(); // first run: no config file yet
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();

    // Draw the first-run state, assert the onboarding screen renders.
    let mut channel = EventChannel::new();
    channel
        .tx
        .send(AppEvent::Key(key(KeyCode::F(1))))
        .expect("draw");
    channel
        .tx
        .send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    app.run(&mut term, &mut channel).await.expect("run");
    let view = format!("{}", term.backend());
    assert!(view.contains("first run"), "title: {view}");
    assert!(
        view.contains("Where should the workspace live?"),
        "workspace question: {view}"
    );

    // Complete the flow: path → Enter → preset → Enter → chat.
    let result = run_with_settle(
        app,
        Terminal::new(TestBackend::new(120, 30)).unwrap(),
        vec![
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(key(KeyCode::Char('t'))),
            AppEvent::Key(key(KeyCode::Char('m'))),
            AppEvent::Key(key(KeyCode::Char('p'))),
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(key(KeyCode::Char('w'))),
            AppEvent::Key(key(KeyCode::Char('s'))),
            AppEvent::Key(key(KeyCode::Enter)),
            AppEvent::Key(key(KeyCode::Char('d'))),
            AppEvent::Key(key(KeyCode::Char('v'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(400),
    )
    .await;

    assert!(matches!(result.mode, Mode::Chat), "reached the chat");
    let posts = mock
        .requests()
        .await
        .iter()
        .filter(|request| request.path == "/api/workspace.create")
        .filter_map(|request| serde_json::from_str::<serde_json::Value>(&request.body).ok())
        .collect::<Vec<_>>();
    assert_eq!(posts.len(), 1, "workspace.create dispatched");
    assert_eq!(posts[0]["payload"]["path"], "/tmp/ws");
    let config = Config::load();
    assert!(config.onboarding_complete, "flag persisted in the UI flow");
    assert_eq!(config.workspace_path.as_deref(), Some("/tmp/ws"));

    match prev_xdg {
        Some(previous) => unsafe { std::env::set_var("XDG_CONFIG_HOME", previous) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
}

/// The onboarding view renders standalone (widget test, keyless) — including
/// a transient notice and the zh locale.
#[test]
fn onboarding_view_renders_with_notice_and_zh() {
    use ratatui::backend::TestBackend;
    let state = dsh_tui::ui::onboarding::OnboardingState::new();
    for locale in [dsh_tui::i18n::Locale::En, dsh_tui::i18n::Locale::Zh] {
        let backend = TestBackend::new(60, 12);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                OnboardingView {
                    state: &state,
                    notice: Some("notice"),
                    theme: &dsh_tui::theme::Theme::default(),
                    locale,
                },
                f.area(),
            )
        })
        .unwrap();
        let view = format!("{}", term.backend());
        assert!(view.contains("notice"), "notice renders: {view}");
        assert!(!view.contains("panic"), "no panic: {view}");
    }
}

// Keep WorkspaceView/SessionId/WorkspaceId imports exercised (the create
// fixture references the wire shapes).
#[allow(dead_code)]
fn type_anchors() {
    let _ = WorkspaceView {
        workspace_id: WorkspaceId("w1".into()),
        path: "/tmp/ws".into(),
        title: "ws".into(),
        session_ids: vec![],
        created_at: "".into(),
        updated_at: "".into(),
    };
    let _ = SessionId("s1".into());
}
