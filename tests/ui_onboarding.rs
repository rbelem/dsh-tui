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
use dsh_tui::ui::onboarding::{OnboardingState, OnboardingStep, OnboardingView};
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

/// Enter first-run onboarding the hermetic way: `maybe_enter_onboarding`
/// seeds the workspace paths + cwd from the env, and the zoxide fetch is
/// marked done so nothing shells out (the #50 zoxide fetch is a lazy
/// first-touch shell-out — tests inject their own candidate lists).
fn enter_onboarding(app: &mut App) {
    app.maybe_enter_onboarding();
    if let Mode::Onboarding(state) = &mut app.mode {
        state.zoxide_fetched = true;
        state.workspace_paths.clear();
    }
}

/// The picker state for the run-loop tests: explicit candidates + cwd, no
/// shelling.
fn picker_app(workspace_paths: Vec<&str>, zoxide: Vec<&str>, cwd: &str) -> App {
    let mut app = App::default();
    app.mode = Mode::Onboarding(OnboardingState::with_candidates(
        workspace_paths.into_iter().map(str::to_string).collect(),
        zoxide.into_iter().map(str::to_string).collect(),
        cwd.into(),
    ));
    app
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
    /// A pid + nanosecond-nonce path: unique per run, so a recycled pid can
    /// never collide with a stale dir from an earlier run (that stale dir
    /// may hold an `onboarding_complete = true` config written by a
    /// previous test process, which would corrupt this run's first-run
    /// detection).
    fn new(label: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "dsh-tui-onboard-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Redirect `XDG_CONFIG_HOME` for the closure's lifetime, restoring it on
/// drop — even when the body panics (a leaked env would corrupt the other
/// env-touching tests running in parallel).
struct XdgGuard(Option<String>);

impl XdgGuard {
    fn set(path: &Path) -> Self {
        let previous = std::env::var("XDG_CONFIG_HOME").ok();
        // SAFETY: serialized under ENV_LOCK; restored by Drop.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", path.to_str().unwrap()) };
        XdgGuard(previous)
    }
}

impl Drop for XdgGuard {
    fn drop(&mut self) {
        // SAFETY: mirrors the set_var above.
        match &self.0 {
            Some(previous) => unsafe { std::env::set_var("XDG_CONFIG_HOME", previous) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
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
        enter_onboarding(&mut app);
        let Mode::Onboarding(state) = &app.mode else {
            panic!("onboarding mode");
        };
        assert_eq!(state.step, OnboardingStep::Workspace);
        assert!(state.workspace_paths.is_empty(), "no workspaces yet");

        // Type a path, Enter → the preset question. The typed path wins
        // (no candidates: empty workspace list, empty zoxide) and is kept
        // in the editor as the committed value.
        for c in "/tmp/ws".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        let Mode::Onboarding(state) = &app.mode else {
            panic!("still onboarding");
        };
        assert_eq!(state.step, OnboardingStep::Preset);
        assert_eq!(state.path_editor.buffer(), "/tmp/ws");

        // Esc goes back to the workspace question (the committed path is
        // retained).
        app.handle_key(key(KeyCode::Esc));
        let Mode::Onboarding(state) = &app.mode else {
            panic!("still onboarding");
        };
        assert_eq!(state.step, OnboardingStep::Workspace, "Esc goes back");
        assert_eq!(state.path_editor.buffer(), "/tmp/ws");

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
    // Hermetic: isolated config root + the env lock (the test reads the
    // config to decide onboarding, so it must not see another test's XDG).
    let dir = TempDir::new("esc");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(&dir.0, |_config_root| {
        let mut app = App::default();
        app.load_theme_config();
        app.maybe_enter_onboarding();
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Some(Action::Quit));
    });
}

// ---------------------------------------------------------------------------
// 3. #50: paste, the candidate picker, and the cwd default
// ---------------------------------------------------------------------------

#[test]
fn paste_inserts_into_the_active_onboarding_editor() {
    // Workspace step: paste lands in the path editor (and resets the
    // picker selection to the top of the re-filtered list).
    let mut app = picker_app(vec!["/ws/alpha"], vec![], "/home/u");
    assert_eq!(
        app.handle_paste("/tmp/pasted/ws".into()),
        Action::None,
        "paste is consumed by the onboarding editor"
    );
    if let Mode::Onboarding(state) = &app.mode {
        assert_eq!(state.path_editor.buffer(), "/tmp/pasted/ws");
        assert_eq!(state.selection, 0);
    }

    // Preset step: paste lands in the preset editor.
    app.handle_key(key(KeyCode::Enter)); // → Preset (typed path wins)
    assert_eq!(app.handle_paste("devops".into()), Action::None);
    if let Mode::Onboarding(state) = &app.mode {
        assert_eq!(state.preset_editor.buffer(), "devops");
    }
}

#[tokio::test]
async fn paste_through_the_run_loop_lands_in_the_path_buffer() {
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut app = picker_app(vec![], vec![], "/home/u");
    let mut channel = EventChannel::new();
    channel
        .tx
        .send(AppEvent::Paste("/from/paste".into()))
        .expect("paste");
    channel
        .tx
        .send(AppEvent::Key(key(KeyCode::F(1))))
        .expect("draw");
    channel
        .tx
        .send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    app.run(&mut term, &mut channel).await.expect("run");
    if let Mode::Onboarding(state) = &app.mode {
        assert_eq!(
            state.path_editor.buffer(),
            "/from/paste",
            "the paste reached the onboarding path editor"
        );
    } else {
        panic!("still onboarding");
    }
}

#[test]
fn enter_picks_the_highlighted_candidate() {
    // Blank editor with candidates: the first is highlighted; Down moves
    // the highlight; Enter commits it.
    let mut app = picker_app(vec!["/ws/alpha", "/ws/beta"], vec![], "/home/u");
    app.handle_key(key(KeyCode::Char('j'))); // Down → selection 1
    app.handle_key(key(KeyCode::Enter));
    if let Mode::Onboarding(state) = &app.mode {
        assert_eq!(state.step, OnboardingStep::Preset);
        assert_eq!(state.path_editor.buffer(), "/ws/beta", "candidate picked");
    } else {
        panic!("advanced to the preset question");
    }
}

#[test]
fn typing_filters_the_list_and_commits_the_match() {
    // Typing is not hijacked by j/k nav, and the filter narrows the list to
    // the match, which Enter then commits.
    let mut app = picker_app(vec!["/ws/alpha", "/ws/beta"], vec![], "/home/u");
    for c in "beta".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter));
    if let Mode::Onboarding(state) = &app.mode {
        assert_eq!(
            state.path_editor.buffer(),
            "/ws/beta",
            "filtered match picked"
        );
    }
}

#[test]
fn blank_enter_commits_the_cwd() {
    // No candidates + blank editor: Enter commits the injected cwd. The
    // completion writes the config, so this test needs the env lock +
    // an isolated config root (like the other completing tests) — without
    // it the save would land in another test's ambient XDG.
    let dir = TempDir::new("cwd");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(&dir.0, |_config_root| {
        let mut app = picker_app(vec![], vec![], "/home/u/office");
        app.handle_key(key(KeyCode::Enter));
        if let Mode::Onboarding(state) = &app.mode {
            assert_eq!(state.step, OnboardingStep::Preset);
            assert_eq!(state.path_editor.buffer(), "/home/u/office");
        }
        // Continue: completion dispatches workspace.create on the cwd.
        app.handle_key(key(KeyCode::Esc)); // back to the workspace question
        app.handle_key(key(KeyCode::Enter)); // re-commit the cwd
        let action = app.handle_key(key(KeyCode::Enter)); // preset blank → done
        assert_eq!(
            action,
            Some(Action::CreateWorkspace("/home/u/office".into()))
        );
        assert!(Config::load().onboarding_complete, "flag written");
    });
}

#[test]
fn empty_zoxide_falls_back_to_workspaces_and_typing() {
    // zoxide missing/failing → empty list: the workspaces-only picker still
    // serves Enter (first workspace) and manual typed paths.
    let mut app = picker_app(vec!["/ws/alpha"], vec![], "/home/u");
    app.handle_key(key(KeyCode::Enter));
    if let Mode::Onboarding(state) = &app.mode {
        assert_eq!(
            state.path_editor.buffer(),
            "/ws/alpha",
            "workspace committed"
        );
    }
    // And a manual typed path still wins over the ghost candidates.
    let mut app = picker_app(vec!["/ws/alpha"], vec![], "/home/u");
    app.handle_key(key(KeyCode::Esc)); // ensure on Workspace
    for c in "/tmp/custom".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter));
    if let Mode::Onboarding(state) = &app.mode {
        assert_eq!(state.path_editor.buffer(), "/tmp/custom");
    }
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
    // writes the flag mid-await); restored on drop.
    let _xdg = XdgGuard::set(&dir.0);

    let mock = MockGateway::start().await;
    mock.set_handler(
        "workspace.create",
        common::MockAction::Ok(workspace_create_ok()),
    )
    .await;

    let mut app = App::default();
    app.load_theme_config();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    enter_onboarding(&mut app); // first run: no config file yet (hermetic)
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
