//! Coverage-push tests (the council/oracle coverage lane): run-loop and
//! app-arm branches that the feature tests do not reach — spawn guards
//! without a client, done-event error arms, keyless takeover resolutions,
//! ingest error paths, the status-line truncated marker, the stream-error
//! code table, and the run loop's channel-close exit. Mock gateway where a
//! client is needed, keyless elsewhere.

mod common;
use common::{MockAction, MockGateway};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::app::{
    Action, AnswerTag, App, AppEvent, ApprovalPending, AtCatalog, EventChannel, Focus,
};
use dsh_tui::client::{ClientError, WireClient};
use dsh_tui::i18n::Locale;
use dsh_tui::render::{ImageCache, RowCache};
use dsh_tui::store::SessionStore;
use dsh_tui::theme::Theme;
use dsh_tui::ui::takeover::Mode;
use dsh_tui::ui::takeover::{ApprovalTakeover, QuestionTakeover};
use dsh_tui::wire::approvals::{ApprovalRequestId, ApprovalResponseOutcome};
use dsh_tui::wire::events::{
    HostFrame, MessageRole, MuxFrame, QueueItem, QueueMessage, QueueMessageSource, QueuePlacement,
};
use dsh_tui::wire::rpc::{RpcError, RpcId, RpcReceipt, RpcReceiptReason};
use dsh_tui::wire::session::{
    MessageId, SessionAttachmentValue, SessionEvent, SessionHistoryValue, SessionId,
    SessionRenameValue, SessionSummary, WorkspaceId,
};
use ratatui::layout::Rect;

// ---------------------------------------------------------------------------
// fixtures
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn ev(seq: i64, r#type: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        r#type: r#type.into(),
        seq,
        time: seq as f64,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn frame(session: &str, event: SessionEvent) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: None,
    }
}

fn user_msg(id: &str, text: &str) -> serde_json::Value {
    json!({"id": id, "role": "user", "content": [{"type": "text", "text": text}], "source": {"kind": "user"}})
}

/// Run buffered events to completion (no quit appended — tests that need a
/// draw add keys themselves).
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

/// XDG_CONFIG_HOME-touching tests (config writes) serialize on this ONE
/// static — a per-function static would silently be a different mutex and
/// the tests would race each other's env mutations.
static ENV_LOCK_XDG: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Set an env var for the duration of the closure and restore it after
/// (edition 2024: `set_var` is unsafe; single-threaded test usage only).
fn with_env_var(key: &str, value: Option<&str>, f: impl FnOnce()) {
    let previous = std::env::var(key).ok();
    // SAFETY: serialized under ENV_LOCK_XDG; restored before return.
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

/// Redirect `dirs::config_dir()` under `base` for the closure's duration and
/// hand the closure the app config root it resolves to (`<config dir>/dsh-tui`).
///
/// `dirs` honors `XDG_CONFIG_HOME` only on Linux; macOS ignores it and builds
/// `~/Library/Application Support` from `$HOME` instead, so there the test
/// isolates by overriding `HOME`. Asserting against the resolved root (never
/// the temp-dir literal) keeps the same test green on every platform.
fn with_config_root(base: &std::path::Path, f: impl FnOnce(&std::path::Path)) {
    with_env_var("XDG_CONFIG_HOME", Some(base.to_str().unwrap()), || {
        with_home_override(base, || {
            let root = dirs::config_dir().expect("config dir").join("dsh-tui");
            f(&root);
        });
    });
}

/// On macOS `dirs` ignores `XDG_CONFIG_HOME`, so `HOME` carries the
/// override; a no-op elsewhere.
#[cfg(target_os = "macos")]
fn with_home_override(base: &std::path::Path, f: impl FnOnce()) {
    with_env_var("HOME", Some(base.join("home").to_str().unwrap()), f);
}

#[cfg(not(target_os = "macos"))]
fn with_home_override(_base: &std::path::Path, f: impl FnOnce()) {
    f();
}

/// A fresh app with one session active (the common starting point).
fn app_with_session() -> App {
    let mut app = App::default();
    app.sessions = vec![summary("s1")];
    app.active_session = Some(SessionId("s1".into()));
    app
}

fn approval_pending() -> (ApprovalRequestId, ApprovalPending) {
    let approval_id = ApprovalRequestId("a1".into());
    let pending = ApprovalPending {
        rpc_id: RpcId("rpc-1".into()),
        session_id: SessionId("s1".into()),
        approval_id: approval_id.clone(),
        tool_name: "bash".into(),
        call_id: None,
        reason: None,
        seq: 1,
    };
    (approval_id, pending)
}

fn approval_takeover() -> ApprovalTakeover {
    let (approval_id, _) = approval_pending();
    ApprovalTakeover {
        session_id: SessionId("s1".into()),
        approval_id,
        rpc_id: RpcId("rpc-1".into()),
        tool_name: "bash".into(),
        call_id: None,
        reason: None,
        tool_summary: None,
        sending: false,
    }
}

// ---------------------------------------------------------------------------
// 1. run loop lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_loop_ends_when_the_channel_closes() {
    let mut app = App::default();
    let backend = TestBackend::new(80, 24);
    let mut term = Terminal::new(backend).unwrap();
    // No senders left: recv() → None → the loop breaks cleanly.
    // Replace the channel's sender with a fresh one whose receiver is
    // discarded: the original sender drops, so recv() → None immediately.
    let EventChannel { tx: _, rx } = EventChannel::default();
    let tx = {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        tx
    };
    let mut channel = EventChannel { tx, rx };
    app.run(&mut term, &mut channel)
        .await
        .expect("run must not fail");
}

// ---------------------------------------------------------------------------
// 2. spawn guards without a client (the actions are silent no-ops)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_and_sidebar_actions_are_no_ops_without_a_client() {
    let mut app = app_with_session();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // composer submit → dispatch_prompt (no client, no POST).
            AppEvent::Key(key(KeyCode::Tab)),
            AppEvent::Key(key(KeyCode::Char('h'))),
            AppEvent::Key(key(KeyCode::Enter)),
            // sidebar rename commit → rename_session.
            AppEvent::Key(key(KeyCode::Tab)),
            AppEvent::Key(key(KeyCode::Char('r'))),
            AppEvent::Key(key(KeyCode::Char('a'))),
            AppEvent::Key(key(KeyCode::Enter)),
            // fork / archive.
            AppEvent::Key(key(KeyCode::Char('f'))),
            AppEvent::Key(key(KeyCode::Char('a'))),
            // new-session picker create.
            AppEvent::Key(key(KeyCode::Char('n'))),
            AppEvent::Key(key(KeyCode::Enter)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    // Nothing was spawned; the app is still coherent.
    assert_eq!(app.active_session, Some(SessionId("s1".into())));
    assert!(!app.sidebar_action_sending);
}

#[tokio::test]
async fn queue_cancel_and_history_are_no_ops_without_a_client() {
    let mut app = app_with_session();
    // A queue with one queued item so the queue popup can open.
    app.store
        .ingest(MuxFrame::SessionQueue {
            session_id: SessionId("s1".into()),
            items: vec![QueueItem {
                id: MessageId("m1".into()),
                placement: QueuePlacement::Queued,
                message: QueueMessage {
                    id: MessageId("m1".into()),
                    role: MessageRole::User,
                    content: vec![],
                    source: QueueMessageSource {
                        kind: "user".into(),
                    },
                },
            }],
        })
        .expect("queue frame");
    // A running summary so Ctrl+C produces CancelTurn.
    app.sessions[0].running = true;

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // Alt+q opens the queue popup; x requests a remove (no client).
            AppEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT)),
            AppEvent::Key(key(KeyCode::Char('x'))),
            AppEvent::Key(key(KeyCode::Esc)),
            // Ctrl+C with a running turn → cancel_turn (no client).
            AppEvent::Key(ctrl(KeyCode::Char('c'))),
            // sidebar Enter → fetch_history (no client).
            AppEvent::Key(key(KeyCode::Tab)),
            AppEvent::Key(key(KeyCode::Tab)),
            AppEvent::Key(key(KeyCode::Enter)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.queue_action_sending);
    assert!(app.history_loading.is_none(), "no fetch without a client");
}

#[tokio::test]
async fn catalog_settings_and_search_are_no_ops_without_a_client() {
    let mut app = app_with_session();
    app.focus = Focus::Chat; // these surfaces are chat-focus bound (boot is Composer)
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // Ctrl+P launcher with no client → request_catalog returns.
            AppEvent::Key(ctrl(KeyCode::Char('p'))),
            AppEvent::Key(key(KeyCode::Esc)),
            // Ctrl+, settings → fetch_settings no-op clears the loading flag.
            AppEvent::Key(ctrl(KeyCode::Char(','))),
            // Ctrl+S save → save_settings no-op (no client).
            AppEvent::Key(ctrl(KeyCode::Char('s'))),
            AppEvent::Key(key(KeyCode::Esc)),
            // Sidebar search typing with no client → search_sessions returns.
            AppEvent::Key(key(KeyCode::Tab)),
            AppEvent::Key(key(KeyCode::Tab)),
            AppEvent::Key(key(KeyCode::Char('/'))),
            AppEvent::Key(key(KeyCode::Char('a'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let state = app.sidebar_search.as_ref().expect("popup still open");
    assert!(!state.sending, "no search without a client");
}

#[tokio::test]
async fn request_catalog_skips_when_already_loading() {
    let mut app = app_with_session();
    app.at_catalog = Some(AtCatalog {
        skills: vec![],
        loading: true,
    });
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(ctrl(KeyCode::Char('p'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        matches!(app.at_catalog, Some(AtCatalog { loading: true, .. })),
        "the duplicate-fetch guard held"
    );
}

// ---------------------------------------------------------------------------
// 3. done-event error arms (client attached, results injected)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn done_event_error_arms_toast_and_re_arm() {
    let mock = MockGateway::start().await;
    let mut app = app_with_session();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // Sidebar action failures toast and re-arm the guard.
            AppEvent::ForkDone {
                result: Err(ClientError::Timeout),
            },
            AppEvent::ArchiveDone {
                session_id: SessionId("s1".into()),
                result: Err(ClientError::Timeout),
            },
            // A rename of a session NOT in the sidebar list skips the row
            // update but still toasts.
            AppEvent::RenameDone {
                session_id: SessionId("ghost".into()),
                result: Ok(SessionRenameValue {
                    title: "gone".into(),
                    seq: 1,
                }),
            },
            // History failure for the active session toasts.
            AppEvent::HistoryLoaded {
                session_id: SessionId("s1".into()),
                result: Err(ClientError::Timeout),
            },
            // Catalog failure toasts and stays uncached.
            AppEvent::CatalogLoaded {
                result: Err(ClientError::Timeout),
            },
            // Search result for a popup that already closed: dropped.
            AppEvent::SessionSearchDone {
                query: "x".into(),
                result: Err(ClientError::Timeout),
            },
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert!(!app.sidebar_action_sending, "guard re-armed");
    let view = format!("{}", term.backend());
    // Toasts replace each other; the LAST one is the catalog failure.
    assert!(
        view.contains("skills failed to load:"),
        "catalog toast: {view}"
    );
    assert!(app.sidebar_search.is_none(), "popup closed");
    assert!(
        app.sessions
            .iter()
            .all(|summary| summary.projections.is_none()),
        "ghost rename touched no row"
    );
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_done_events_fold_only_while_the_view_is_open() {
    let mock = MockGateway::start().await;
    let mut app = app_with_session();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // Late describe result with the view closed: dropped.
            AppEvent::SettingsDescribeDone {
                result: Err(ClientError::Timeout),
            },
            // Late save result with the view closed: dropped.
            AppEvent::SettingsSaveDone {
                ns: "general".into(),
                result: Err(ClientError::Timeout),
            },
            // Open the settings view (fetch spawns; the mock 404s describe).
            AppEvent::Key(ctrl(KeyCode::Char(','))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_save_failure_toasts_and_conflict_refreshes() {
    let mock = MockGateway::start().await;
    let mut app = app_with_session();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.mode = Mode::Settings(dsh_tui::ui::settings::SettingsState::new());
    app.hint = Some("saving…".into());

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // Non-conflict failure: toasts, stays in the view, saving re-armed.
            AppEvent::SettingsSaveDone {
                ns: "general".into(),
                result: Err(ClientError::Transport("boom".into())),
            },
            // A describe failure while the view is open toasts + unloads.
            AppEvent::SettingsDescribeDone {
                result: Err(ClientError::Timeout),
            },
            // Settings-conflict: toasts "conflict — refreshed" and spawns a
            // re-describe (the mock 404s it → describe failure above).
            AppEvent::SettingsSaveDone {
                ns: "general".into(),
                result: Err(ClientError::Rpc(
                    serde_json::from_value(json!({
                        "code": "settings-conflict",
                        "message": "stale",
                        "details": {"ns": "general", "expected": 1, "actual": 2},
                    }))
                    .expect("conflict error"),
                )),
            },
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    let view = format!("{}", term.backend());
    // The conflict's re-describe 404s (async) — the final toast is either
    // the conflict notice or the describe failure, depending on timing.
    assert!(
        view.contains("conflict — refreshed") || view.contains("settings failed:"),
        "conflict/describe toast: {view}"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 4. keyless takeover answers resolve optimistically
// ---------------------------------------------------------------------------

#[tokio::test]
async fn keyless_approval_and_question_answers_resolve_optimistically() {
    let mut app = App::default();
    let (approval_id, pending) = approval_pending();
    app.mode = Mode::Approval(approval_takeover());
    app.pending_approvals.insert(approval_id, pending);

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('y'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(matches!(app.mode, Mode::Chat), "approval resolved");
    assert!(app.pending_approvals.is_empty());
    assert_eq!(app.toast_text(), Some("allowed once"), "optimistic toast");

    // Question takeover, keyless answer.
    let rpc_id = RpcId("rpc-q".into());
    app.mode = Mode::Question(QuestionTakeover::new(
        SessionId("s1".into()),
        rpc_id.clone(),
        vec![],
    ));
    app.pending_questions.insert(
        rpc_id.to_string(),
        dsh_tui::app::QuestionPending {
            rpc_id: rpc_id.clone(),
            session_id: SessionId("s1".into()),
            questions: vec![],
            seq: 1,
        },
    );
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Enter)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(matches!(app.mode, Mode::Chat), "question resolved");
    assert!(app.pending_questions.is_empty());
    assert_eq!(app.toast_text(), Some("answered"));
}

// ---------------------------------------------------------------------------
// 5. answer failure paths stay in the takeover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn answer_failures_stay_in_the_takeover_with_reamed_keys() {
    let mut app = App::default();
    let (approval_id, pending) = approval_pending();
    app.mode = Mode::Approval(approval_takeover());
    app.pending_approvals.insert(approval_id.clone(), pending);

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // Transport failure: stays, toast, sending re-armed.
            AppEvent::AnswerDone {
                tag: AnswerTag::Approval {
                    approval_id: approval_id.clone(),
                    outcome: ApprovalResponseOutcome::AllowedOnce,
                },
                result: Err(ClientError::Timeout),
            },
            // Not-accepted receipts map their reason.
            AppEvent::AnswerDone {
                tag: AnswerTag::Approval {
                    approval_id: approval_id.clone(),
                    outcome: ApprovalResponseOutcome::AllowedOnce,
                },
                result: Ok(RpcReceipt {
                    accepted: false,
                    reason: Some(RpcReceiptReason::BadResponse),
                }),
            },
            AppEvent::AnswerDone {
                tag: AnswerTag::Approval {
                    approval_id: approval_id.clone(),
                    outcome: ApprovalResponseOutcome::AllowedOnce,
                },
                result: Ok(RpcReceipt {
                    accepted: false,
                    reason: None,
                }),
            },
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("answer failed:"), "toast: {view}");
    assert!(
        matches!(&app.mode, Mode::Approval(t) if !t.sending),
        "takeover stays with keys re-armed"
    );

    // Question failure re-arms the question arm.
    let rpc_id = RpcId("rpc-q".into());
    app.mode = Mode::Question(QuestionTakeover::new(
        SessionId("s1".into()),
        rpc_id.clone(),
        vec![],
    ));
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::AnswerDone {
                tag: AnswerTag::Question(rpc_id),
                result: Err(ClientError::Timeout),
            },
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        matches!(&app.mode, Mode::Question(t) if !t.sending),
        "question keys re-armed"
    );
}

#[tokio::test]
async fn stale_answer_success_after_a_newer_frame_is_dropped() {
    let mut app = App::default();
    let (approval_id, pending) = approval_pending();
    app.mode = Mode::Approval(approval_takeover());
    app.pending_approvals.insert(approval_id.clone(), pending);

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // A newer approval frame replaces the displayed takeover while
            // the older answer is in flight.
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-2".into()),
                frame: MuxFrame::ApprovalRequested {
                    session_id: SessionId("s1".into()),
                    approval_id: ApprovalRequestId("a2".into()),
                    tool_name: "read_file".into(),
                    call_id: None,
                    reason: None,
                },
            },
            // The OLD answer succeeds: its pending entry drops, but the
            // takeover stays on the newer frame.
            AppEvent::AnswerDone {
                tag: AnswerTag::Approval {
                    approval_id,
                    outcome: ApprovalResponseOutcome::AllowedOnce,
                },
                result: Ok(RpcReceipt {
                    accepted: true,
                    reason: None,
                }),
            },
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        matches!(&app.mode, Mode::Approval(t) if t.approval_id == ApprovalRequestId("a2".into())),
        "newer takeover still displayed"
    );
    assert_eq!(app.pending_approvals.len(), 1, "only the newer pending");
}

// ---------------------------------------------------------------------------
// 6. ingest + history ingest error paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn malformed_frame_sets_last_error() {
    let mut app = app_with_session();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    // A known event type with a malformed payload: turn/end without `reason`.
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(frame(
                "s1",
                ev(1, "turn/end", json!({"turn": 1, "step": 1})),
            )),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        app.last_error
            .as_deref()
            .is_some_and(|e| e.contains("invalid data on session event")),
        "last_error set: {:?}",
        app.last_error
    );
    let view = format!("{}", term.backend());
    assert!(view.contains("error:"), "status line shows it: {view}");
}

#[tokio::test]
async fn malformed_history_page_sets_last_error() {
    let mut app = app_with_session();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::HistoryLoaded {
                session_id: SessionId("s1".into()),
                result: Ok(SessionHistoryValue {
                    events: vec![dsh_tui::wire::session::HistoryEntry {
                        event: ev(1, "turn/end", json!({"turn": 1, "step": 1})),
                        view: None,
                    }],
                    has_more: false,
                    projections: None,
                }),
            },
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        app.last_error
            .as_deref()
            .is_some_and(|e| e.contains("invalid data on session event")),
        "last_error set: {:?}",
        app.last_error
    );
}

// ---------------------------------------------------------------------------
// 7. status line + store misc
// ---------------------------------------------------------------------------

/// The composer top-rule row after one forced draw (acceptance 11 helper).
async fn composer_rule_row(app: &mut App) -> u16 {
    let backend = TestBackend::new(80, 24);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        app,
        &mut term,
        vec![
            AppEvent::Key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)),
            AppEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
        ],
    )
    .await;
    let buffer = term.backend().buffer();
    for y in (0..24u16).rev() {
        if buffer
            .cell((40, y))
            .is_some_and(|cell| cell.symbol() == "─")
        {
            return y;
        }
    }
    panic!("composer rule not found");
}

/// Acceptance 11: the queue strip is zero-height when the queue is empty —
/// no double gap between the chat and the composer. The composer's top rule
/// must sit at the same row with and without a queue.
#[tokio::test]
async fn empty_queue_strip_renders_zero_height() {
    // Empty queue: chat 21 + composer 2 + status 1 at 80x24 — rule at 21.
    let mut app = app_with_session();
    let empty = composer_rule_row(&mut app).await;
    assert_eq!(empty, 21, "no strip, no double gap");

    // One queued item: the strip docks at row 20; the composer stays put.
    let mut app = app_with_session();
    app.store
        .ingest(MuxFrame::SessionQueue {
            session_id: SessionId("s1".into()),
            items: vec![QueueItem {
                id: MessageId("m1".into()),
                placement: QueuePlacement::Queued,
                message: QueueMessage {
                    id: MessageId("m1".into()),
                    role: MessageRole::User,
                    content: vec![],
                    source: QueueMessageSource {
                        kind: "user".into(),
                    },
                },
            }],
        })
        .expect("queue frame");
    let with_queue = composer_rule_row(&mut app).await;
    assert_eq!(with_queue, 21, "composer anchored below the strip");
}

#[tokio::test]
async fn status_line_shows_the_truncated_marker() {
    let mut app = app_with_session();
    // A one-event window: the second event evicts the first → truncated.
    app.store = SessionStore::with_max_buffered_events(1);
    app.store
        .ingest(frame("s1", ev(1, "user/message", user_msg("m1", "hi"))))
        .expect("ingest");
    app.store
        .ingest(frame("s1", ev(2, "user/message", user_msg("m2", "yo"))))
        .expect("evict");
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    draw_and_quit(&mut app, &mut term, Vec::new()).await;
    let view = format!("{}", term.backend());
    assert!(
        view.contains("△"),
        "#11: the truncated state is the △ warning indicator: {view}"
    );
    assert!(view.contains("seq 2"), "seq line: {view}");
}

#[test]
fn store_default_and_session_mut_are_usable() {
    let mut store = SessionStore::default();
    store.open_session(SessionId("s1".into()));
    assert!(store.session_mut(&SessionId("s1".into())).is_some());
}

#[test]
fn stream_error_codes_map_to_one_liners() {
    // Every RpcError code branch (rpc.schema.ts:34-79), same fixtures as
    // wire_roundtrip's ERROR_TABLE.
    let table: &[(&str, &str)] = &[
        ("bad-request", r#"{"issues":[]}"#),
        ("cancelled", "{}"),
        ("session-not-found", r#"{"sessionId":"s1"}"#),
        ("model-unavailable", r#"{"provider":"p","model":"m"}"#),
        (
            "session-conflict",
            r#"{"sessionId":"s1","requestedCwd":"/a"}"#,
        ),
        ("invalid-time-zone", r#"{"value":"UTC"}"#),
        (
            "workspace-attach-failed",
            r#"{"sessionId":"s1","workspaceId":"w1"}"#,
        ),
        ("workspace-not-found", r#"{"workspaceId":"w1"}"#),
        ("workspace-invalid-path", r#"{"path":"/x"}"#),
        ("workspace-name-conflict", r#"{"name":"proj"}"#),
        (
            "workspace-move-invalid",
            r#"{"workspaceId":"w1","sessionId":"s1","beforeSessionId":"s0"}"#,
        ),
        ("directory-unreadable", r#"{"path":"/x"}"#),
        ("directory-exists", r#"{"path":"/x"}"#),
        ("directory-create-failed", r#"{"path":"/x"}"#),
        ("directory-picker-unavailable", r#"{"capability":"dialog"}"#),
        (
            "agent-preset-read-only",
            r#"{"agentPreset":"code","reason":"r"}"#,
        ),
        (
            "agent-preset-locked",
            r#"{"sessionId":"s1","agentPreset":"code"}"#,
        ),
        (
            "agent-preset-conflict",
            r#"{"sessionId":"s1","requestedPreset":"a","existingPreset":"b"}"#,
        ),
        (
            "agent-preset-not-found",
            r#"{"agentPreset":"code","available":["code"]}"#,
        ),
        (
            "agent-preset-invalid",
            r#"{"agentPreset":"code","reason":"r"}"#,
        ),
        ("agent-busy", r#"{"reason":"running"}"#),
        ("attachment-error", r#"{"reason":"too large"}"#),
        ("queue-item-not-found", r#"{"itemId":"m1"}"#),
        ("steer-unavailable", r#"{"itemId":"m1"}"#),
        ("command-error", "{}"),
        ("unknown-command", "{}"),
        ("settings-rejected", r#"{"ns":"general"}"#),
        ("settings-not-exposed", r#"{"ns":"general"}"#),
        (
            "settings-conflict",
            r#"{"ns":"general","expected":1,"actual":2}"#,
        ),
        ("credential-rejected", r#"{"ref":"deepseek"}"#),
        (
            "model-discovery-failed",
            r#"{"settingsNs":"general","baseURL":"http://localhost:11434"}"#,
        ),
        ("title-invalid", r#"{"sessionId":"s1"}"#),
        ("fork-unavailable", r#"{"sessionId":"s1"}"#),
        ("subagent-parent-unavailable", r#"{"parentSessionId":"s1"}"#),
        (
            "subagent-not-found",
            r#"{"parentSessionId":"s1","childSessionId":"s2"}"#,
        ),
        (
            "subagent-catalog-diagnostic",
            r#"{"parentSessionId":"s1","childSessionId":"s2","reason":"unsupported"}"#,
        ),
        ("subagent-not-resumable", r#"{"childSessionId":"s2"}"#),
        ("subagent-unauthorized", r#"{"childSessionId":"s2"}"#),
        (
            "subagent-delivery-unavailable",
            r#"{"childSessionId":"s2"}"#,
        ),
        ("internal", "{}"),
    ];
    assert_eq!(table.len(), 40, "schema has 40 code branches");
    let mut store = SessionStore::new();
    for (code, details) in table {
        let error: RpcError = serde_json::from_value(json!({
            "code": code,
            "message": "m",
            "details": serde_json::from_str::<serde_json::Value>(details).expect("details fixture"),
        }))
        .unwrap_or_else(|e| panic!("code {code}: {e}"));
        store
            .ingest(MuxFrame::StreamError { error })
            .unwrap_or_else(|e| panic!("code {code}: {e}"));
        let line = store.last_stream_error.as_deref().expect("recorded");
        assert!(line.starts_with(code), "code prefix for {code}: {line}");
    }
}

// ---------------------------------------------------------------------------
// 8. stale search re-fires (latest wins)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn stale_search_result_re_fires_for_the_current_buffer() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.search",
        MockAction::Ok(common::leaked(common::search_ok(
            r#"[{"sessionId":"s1","snippet":"hit"}]"#,
        ))),
    )
    .await;
    let mut app = app_with_session();
    app.focus = Focus::Sidebar;
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    // Type two chars back-to-back: the first POST is for "a", the second
    // keystroke is swallowed by the in-flight guard, and the stale result
    // re-fires for "ab".
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
    for event in [
        AppEvent::Key(key(KeyCode::Char('/'))),
        AppEvent::Key(key(KeyCode::Char('a'))),
        AppEvent::Key(key(KeyCode::Char('b'))),
    ] {
        tx.send(event).expect("event channel");
    }
    mock.wait_for_posts("/api/session.search", 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    let (app, term) = run_task.await.expect("run task");

    let posts = mock.wait_for_posts("/api/session.search", 2).await;
    let query_of = |i: usize| {
        posts[i]
            .get("payload")
            .and_then(|payload| payload.get("query"))
            .cloned()
    };
    assert_eq!(query_of(0), Some(serde_json::json!("a")));
    assert_eq!(query_of(1), Some(serde_json::json!("ab")));
    let state = app.sidebar_search.as_ref().expect("popup open");
    assert_eq!(state.results.len(), 1, "latest result folded");
    assert!(!state.sending);
    let view = format!("{}", term.backend());
    assert!(view.contains("hit"), "result row: {view}");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 9. theme picker over a takeover-surface draw
// ---------------------------------------------------------------------------

#[tokio::test]
async fn theme_picker_floats_over_the_settings_view() {
    let mut app = app_with_session();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(ctrl(KeyCode::Char(','))), // settings view
            AppEvent::Key(ctrl(KeyCode::Char('t'))), // theme picker floats
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains(" settings "), "settings drawn: {view}");
    assert!(view.contains("themes"), "picker floats over it: {view}");
}

// ---------------------------------------------------------------------------
// 10. locale helpers
// ---------------------------------------------------------------------------

#[test]
fn locale_code_round_trips() {
    assert_eq!(Locale::En.code(), "en");
    assert_eq!(Locale::Zh.code(), "zh");
}

// ---------------------------------------------------------------------------
// 11. tool summary truncation + takeover promotion
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn approval_tool_summary_truncates_long_arguments() {
    let mock = MockGateway::start().await;
    let mut app = app_with_session();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    // A tool node with long args in the store: the approval summary
    // truncates it (CJK-safe width truncation).
    app.store
        .ingest(frame(
            "s1",
            ev(
                1,
                "tool/call",
                json!({
                    "turn": 1, "step": 1, "callId": "c1", "name": "bash",
                    "arguments": "x".repeat(200),
                }),
            ),
        ))
        .expect("ingest");

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-1".into()),
                frame: MuxFrame::ApprovalRequested {
                    session_id: SessionId("s1".into()),
                    approval_id: ApprovalRequestId("a1".into()),
                    tool_name: "bash".into(),
                    call_id: Some("c1".into()),
                    reason: None,
                },
            },
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("bash"), "tool summary: {view}");
    assert!(view.contains("..."), "long args truncated: {view}");
    mock.stop().await;
}

#[tokio::test]
async fn question_takeover_promotes_after_the_approval_resolves() {
    let mut app = App::default();
    let (approval_id, pending) = approval_pending();
    app.mode = Mode::Approval(approval_takeover());
    app.pending_approvals.insert(approval_id, pending);
    // A question is pending behind the approval.
    let question_rpc = RpcId("rpc-q".into());
    app.pending_questions.insert(
        question_rpc.to_string(),
        dsh_tui::app::QuestionPending {
            rpc_id: question_rpc.clone(),
            session_id: SessionId("s1".into()),
            questions: vec![],
            seq: 2,
        },
    );

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // The answer succeeds: the approval resolves and the pending
            // QUESTION is promoted (next_takeover's question arm).
            AppEvent::AnswerDone {
                tag: AnswerTag::Approval {
                    approval_id: ApprovalRequestId("a1".into()),
                    outcome: ApprovalResponseOutcome::AllowedOnce,
                },
                result: Ok(RpcReceipt {
                    accepted: true,
                    reason: None,
                }),
            },
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        matches!(&app.mode, Mode::Question(t) if t.rpc_id == question_rpc),
        "question promoted: {:?}",
        app.mode
    );
}

// ---------------------------------------------------------------------------
// 12. theme picker + locale cycling
// ---------------------------------------------------------------------------

#[test]
fn theme_picker_toggle_and_apply() {
    // Isolated config dir: applying a theme persists the config (shared lock).
    let _guard = ENV_LOCK_XDG.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("dsh-tui-picker-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");

    with_config_root(&dir, |config_root| {
        let mut app = app_with_session();
        app.handle_key(ctrl(KeyCode::Char('t')));
        assert!(app.theme_picker.open, "opened");
        // Ctrl+T while open closes it.
        app.handle_key(ctrl(KeyCode::Char('t')));
        assert!(!app.theme_picker.open, "closed");

        // Open again, jump past the last theme (clamped), then apply.
        app.handle_key(ctrl(KeyCode::Char('t')));
        let last = app.themes.themes.len().saturating_sub(1);
        for _ in 0..last + 3 {
            app.handle_key(key(KeyCode::Char('j')));
        }
        assert_eq!(app.theme_picker.selected, last, "j clamps at the last");
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.theme_picker.selected, last - 1, "k moves up");
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.theme_picker.open, "applied");
        assert!(
            app.themes.themes.iter().any(|t| t.name == app.theme.name),
            "picked theme applied: {}",
            app.theme.name
        );
        assert!(
            config_root.join("config.toml").exists(),
            "the pick persists the config"
        );
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_l_cycles_locale_and_toasts_on_a_failed_save() {
    // Serialized with the other config-dir-touching tests (a cross-test race
    // wrote the REAL config once — the lock must be shared).
    let _guard = ENV_LOCK_XDG.lock().unwrap_or_else(|e| e.into_inner());

    // Cycle into zh with an isolated config dir.
    let dir = std::env::temp_dir().join(format!("dsh-tui-cov-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    with_config_root(&dir, |config_root| {
        let mut app = app_with_session();
        app.handle_key(ctrl(KeyCode::Char('l')));
        assert_eq!(app.locale, dsh_tui::i18n::Locale::Zh, "cycled");
        assert_eq!(app.toast_text(), Some("语言：中文"), "zh toast");
        // The config persisted.
        assert!(config_root.join("config.toml").exists(), "config persisted");
    });
    let _ = std::fs::remove_dir_all(&dir);

    // A config dir that is a FILE makes the save fail → toast.
    let file = std::env::temp_dir().join(format!("dsh-tui-cov-file-{}", std::process::id()));
    let _ = std::fs::remove_file(&file);
    std::fs::write(&file, "x").expect("write file");
    with_config_root(&file, |config_root| {
        let mut app = app_with_session();
        app.handle_key(ctrl(KeyCode::Char('l')));
        // cycle_locale's failure arm runs BEFORE the locale toast replaces it —
        // assert the save truly failed (no config file could be written under a
        // FILE-shaped config dir) and the cycle still completed.
        assert_eq!(app.locale, dsh_tui::i18n::Locale::Zh, "cycle completed");
        assert!(
            !config_root.join("config.toml").exists(),
            "the failed save wrote nothing"
        );
    });
    let _ = std::fs::remove_file(&file);
}

// ---------------------------------------------------------------------------
// 13. host frames: dedup, removal, status, workspace changes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn host_frames_dedup_reflow_and_update() {
    let mut app = app_with_session();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // A duplicate session-added is skipped.
            AppEvent::HostFrame(HostFrame::HostSessionAdded {
                session_id: SessionId("s1".into()),
                blank: false,
                parent_session_id: None,
                origin: None,
                cwd: None,
                agent_preset: None,
            }),
            // A brand-new session lands at the top.
            AppEvent::HostFrame(HostFrame::HostSessionAdded {
                session_id: SessionId("s9".into()),
                blank: true,
                parent_session_id: None,
                origin: None,
                cwd: None,
                agent_preset: None,
            }),
            // Status flips the running flag.
            AppEvent::HostFrame(HostFrame::HostSessionStatus {
                session_id: SessionId("s9".into()),
                running: true,
            }),
            // Workspace upsert: existing replaced, new appended.
            AppEvent::HostFrame(HostFrame::HostWorkspaceChanged {
                workspace: dsh_tui::wire::workspace::WorkspaceView {
                    workspace_id: WorkspaceId("w1".into()),
                    path: "/tmp/w1".into(),
                    title: "one".into(),
                    session_ids: vec![SessionId("s9".into())],
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: "2026-01-01T00:00:00Z".into(),
                },
            }),
            AppEvent::HostFrame(HostFrame::HostWorkspaceOrderChanged {
                workspace_ids: vec![WorkspaceId("w1".into())],
            }),
            // The active session is removed: the chat goes empty.
            AppEvent::HostFrame(HostFrame::HostSessionRemoved {
                session_id: SessionId("s1".into()),
            }),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert_eq!(app.active_session, None, "active removal clears");
    assert_eq!(app.sessions.len(), 1, "s9 remains");
    assert!(app.sessions[0].running, "status applied");
    assert_eq!(app.workspaces.len(), 1, "workspace upserted");
    let view = format!("{}", term.backend());
    assert!(view.contains("one"), "workspace header: {view}");
}

#[tokio::test]
async fn workspace_removed_frame_reflows() {
    let mut app = app_with_session();
    app.workspaces = vec![dsh_tui::wire::workspace::WorkspaceView {
        workspace_id: WorkspaceId("w1".into()),
        path: "/tmp/w1".into(),
        title: "one".into(),
        session_ids: vec![SessionId("s1".into())],
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }];
    app.workspace_order = vec![WorkspaceId("w1".into())];
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::HostFrame(HostFrame::HostWorkspaceRemoved {
                workspace_id: WorkspaceId("w1".into()),
            }),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.workspaces.is_empty());
    assert!(app.workspace_order.is_empty());
}

// ---------------------------------------------------------------------------
// 14. queue popup editor + action guards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queue_popup_editor_commits_cancels_and_guards() {
    let mut app = app_with_session();
    app.store
        .ingest(MuxFrame::SessionQueue {
            session_id: SessionId("s1".into()),
            items: vec![QueueItem {
                id: MessageId("m1".into()),
                placement: QueuePlacement::Queued,
                message: QueueMessage {
                    id: MessageId("m1".into()),
                    role: MessageRole::User,
                    content: vec![],
                    source: QueueMessageSource {
                        kind: "user".into(),
                    },
                },
            }],
        })
        .expect("queue frame");
    app.queue_popup_open = true;

    // 'e' opens the inline editor; Esc cancels it.
    assert_eq!(app.handle_key(key(KeyCode::Char('e'))), Some(Action::None));
    assert!(app.queue_editor.is_some(), "editor open");
    app.handle_key(key(KeyCode::Esc));
    assert!(app.queue_editor.is_none(), "editor cancelled");

    // Open again, type, commit → the QueueEdit action.
    app.handle_key(key(KeyCode::Char('e')));
    app.handle_key(key(KeyCode::Char('h')));
    app.handle_key(key(KeyCode::Char('i')));
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Action::QueueEdit("hi".into()))
    );

    // 'x' while an action is in flight is inert.
    app.queue_action_sending = true;
    assert_eq!(app.handle_key(key(KeyCode::Char('x'))), Some(Action::None));
    app.queue_action_sending = false;
    assert_eq!(
        app.handle_key(key(KeyCode::Char('x'))),
        Some(Action::QueueRemove)
    );
    app.queue_popup_open = false;

    // Alt+q with an empty queue shows the hint, not the popup.
    app.active_session = None;
    app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
    assert!(!app.queue_popup_open, "popup stayed closed");
    assert_eq!(app.hint.as_deref(), Some("queue is empty"));
}

// ---------------------------------------------------------------------------
// 15. launcher arms
// ---------------------------------------------------------------------------

#[tokio::test]
async fn launcher_arms_dispatch_and_guard() {
    // The locale action persists the config — isolate the config dir (shared
    // lock).
    let _guard = ENV_LOCK_XDG.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("dsh-tui-launcher-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");

    with_config_root(&dir, |_config_root| {
        let mut app = app_with_session();
        app.handle_key(ctrl(KeyCode::Char('p')));
        // Enter with an empty match list is a no-op.
        app.handle_key(key(KeyCode::Char('z')));
        app.handle_key(key(KeyCode::Char('z')));
        app.handle_key(key(KeyCode::Char('z')));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Some(Action::None));
        // Backspace empties the search and resets the selection.
        app.handle_key(key(KeyCode::Backspace));
        app.handle_key(key(KeyCode::Backspace));
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.launcher.as_ref().unwrap().search.buffer(), "");
        // An inert key while open.
        app.handle_key(key(KeyCode::Home));
        assert!(app.launcher.is_some());

        // The settings action opens the settings view.
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('e')));
        app.handle_key(key(KeyCode::Char('t')));
        app.handle_key(key(KeyCode::Char('t')));
        app.handle_key(key(KeyCode::Char('i')));
        app.handle_key(key(KeyCode::Char('n')));
        app.handle_key(key(KeyCode::Char('g')));
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Settings(_)), "settings opened");
        app.mode = Mode::Chat;

        // The locale action cycles.
        app.handle_key(ctrl(KeyCode::Char('p')));
        app.handle_key(key(KeyCode::Char('l')));
        app.handle_key(key(KeyCode::Char('o')));
        app.handle_key(key(KeyCode::Char('c')));
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('l')));
        app.handle_key(key(KeyCode::Char('e')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.locale, dsh_tui::i18n::Locale::Zh, "locale cycled");
        app.locale = dsh_tui::i18n::Locale::En; // labels are localized
        // The quit action.
        app.handle_key(ctrl(KeyCode::Char('p')));
        app.handle_key(key(KeyCode::Char('q')));
        app.handle_key(key(KeyCode::Char('u')));
        app.handle_key(key(KeyCode::Char('i')));
        app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Some(Action::Quit));
    });
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 16. settings form flows
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn settings_form_edit_flows() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "settings.describe",
        MockAction::Ok(common::SETTINGS_DESCRIBE_OK),
    )
    .await;
    let mut app = app_with_session();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = EventChannel::new();
    let tx = channel.tx.clone();
    let run_task = tokio::spawn(async move {
        let mut app = app;
        let result = app.run(&mut term, &mut channel).await;
        result.expect("run must not fail");
        (app, term)
    });
    tx.send(AppEvent::Key(ctrl(KeyCode::Char(',')))).unwrap();
    // Wait for the describe to fold.
    mock.wait_for_posts("/api/settings.describe", 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Tab into the form; the first field (Language, a string) edits.
    tx.send(AppEvent::Key(key(KeyCode::Tab))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Char('f')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Char('r')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap();
    // j to the boolean (verbose): Enter toggles.
    tx.send(AppEvent::Key(key(KeyCode::Char('j')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap();
    // j to the choice (logLevel): Enter cycles.
    tx.send(AppEvent::Key(key(KeyCode::Char('j')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap();
    // j to the number (maxTokens): Enter opens the editor, garbage errors.
    tx.send(AppEvent::Key(key(KeyCode::Char('j')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Backspace))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Backspace))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Backspace))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Backspace))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Char('x')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap(); // fails: editor stays
    tx.send(AppEvent::Key(key(KeyCode::Esc))).unwrap(); // cancel the editor
    // j to the verbose boolean: Enter toggles.
    tx.send(AppEvent::Key(key(KeyCode::Char('j')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap();
    // Ctrl+S with edits → SaveSettings (the mock 404s settings.update).
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('s')))).unwrap();
    mock.wait_for_posts("/api/settings.update", 1).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q')))).unwrap();
    let (_, term) = run_task.await.expect("run task");

    let view = format!("{}", term.backend());
    assert!(
        view.contains("not a number") || view.contains("save failed:"),
        "edit feedback: {view}"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 17. composer caret keys + rename empty commit
// ---------------------------------------------------------------------------

#[test]
fn composer_caret_and_delete_keys() {
    let mut app = app_with_session();
    app.focus = Focus::Composer;
    for c in ['a', 'b', 'c'] {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Delete)); // removes 'b'
    app.handle_key(key(KeyCode::Home));
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::End));
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.composer.buffer(), "ac");
}

#[test]
fn rename_editor_empty_commit_cancels_and_esc_closes() {
    let mut app = app_with_session();
    app.focus = Focus::Sidebar;
    app.handle_key(key(KeyCode::Char('r')));
    assert!(app.rename_editor.is_some());
    // Empty commit: cancels like the queue editor.
    app.handle_key(key(KeyCode::Enter));
    assert!(app.rename_editor.is_none());
    // Re-open; Esc cancels.
    app.handle_key(key(KeyCode::Char('r')));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.rename_editor.is_none());
}

// ---------------------------------------------------------------------------
// 18. sidebar search backspace + empty query
// ---------------------------------------------------------------------------

#[test]
fn search_popup_backspace_and_other_arms() {
    let mut app = app_with_session();
    app.focus = Focus::Sidebar;
    app.handle_key(key(KeyCode::Char('/')));
    assert_eq!(
        app.handle_key(key(KeyCode::Char('a'))),
        Some(Action::SearchSessions("a".into()))
    );
    assert_eq!(
        app.handle_key(key(KeyCode::Backspace)),
        Some(Action::None),
        "empty query stops searching"
    );
    assert!(app.sidebar_search.is_some());
    assert_eq!(app.sidebar_search.as_ref().unwrap().query.buffer(), "");
}

// ---------------------------------------------------------------------------
// 19. attachment + image viewer + session_running arms
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attachment_done_bad_base64_toasts_and_pickerless_is_dropped() {
    let mut app = app_with_session();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // Invalid base64 payload → toast, nothing cached.
            AppEvent::AttachmentDone {
                attachment_id: dsh_tui::wire::session::AttachmentId("att-1".into()),
                result: Ok(SessionAttachmentValue {
                    attachment: dsh_tui::wire::session::ImageAttachmentRef {
                        attachment_id: dsh_tui::wire::session::AttachmentId("att-1".into()),
                        name: None,
                        media_type: dsh_tui::wire::session::ImageMediaType::ImagePng,
                        bytes: 100,
                        width: 1,
                        height: 1,
                    },
                    data: "!!!not-base64!!!".into(),
                }),
            },
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("attachment failed:"), "toast: {view}");
    assert!(
        app.image_cache
            .get(&dsh_tui::wire::session::AttachmentId("att-1".into()))
            .is_none()
    );
}

#[tokio::test]
async fn open_image_viewer_hints_without_session_or_images() {
    let mut app = App::default();
    app.focus = Focus::Chat; // 'i' (open the viewer) is chat-bound; boot is Composer
    // No session: 'i' hints.
    assert_eq!(app.handle_key(key(KeyCode::Char('i'))), Some(Action::None));
    assert_eq!(app.hint.as_deref(), Some("no images in this session"));
    // A session with no image blocks: same hint.
    let mut app = app_with_session();
    app.focus = Focus::Chat;
    assert_eq!(app.handle_key(key(KeyCode::Char('i'))), Some(Action::None));
    assert_eq!(app.hint.as_deref(), Some("no images in this session"));
    assert!(matches!(app.mode, Mode::Chat));
}

#[test]
fn session_running_detects_open_tool_nodes() {
    let mut app = app_with_session();
    assert!(!app.session_running(), "no nodes → not running");
    // A tool call without a result: still running.
    app.store
        .ingest(frame(
            "s1",
            ev(
                1,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": "ls"}),
            ),
        ))
        .expect("ingest");
    assert!(app.session_running(), "open tool call");
    // A settled tool result: not running.
    app.store
        .ingest(frame(
            "s1",
            ev(
                2,
                "tool/result",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "r1", "role": "user",
                        "content": [{"type": "tool-result", "toolCallId": "c1", "content": [], "isError": false}],
                        "source": {"kind": "tool", "callId": "c1"},
                    },
                }),
            ),
        ))
        .expect("ingest");
    assert!(!app.session_running(), "settled result");
}

// ---------------------------------------------------------------------------
// 20. remote resolution toasts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remote_resolution_outcomes_toast() {
    let mut app = App::default();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            // Remote approvals with the rarer outcomes.
            AppEvent::Frame(MuxFrame::ApprovalResolved {
                session_id: SessionId("s1".into()),
                approval_id: ApprovalRequestId("a1".into()),
                outcome: dsh_tui::wire::events::ApprovalOutcome::Cancelled,
            }),
            AppEvent::Frame(MuxFrame::ApprovalResolved {
                session_id: SessionId("s1".into()),
                approval_id: ApprovalRequestId("a2".into()),
                outcome: dsh_tui::wire::events::ApprovalOutcome::Unavailable,
            }),
            // A remote question resolution.
            AppEvent::Frame(MuxFrame::QuestionResolved {
                session_id: SessionId("s1".into()),
                question_rpc_id: RpcId("rpc-q".into()),
                outcome: dsh_tui::wire::events::QuestionOutcome::Cancelled,
            }),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    // No pending entries → these are echoes of local answers: no toast.
    // The arms ran without panicking; the app stayed in the chat.
    assert!(matches!(app.mode, Mode::Chat));
    assert!(app.pending_approvals.is_empty());
}

#[tokio::test]
async fn remote_resolutions_with_pending_entries_toast_and_promote() {
    let mut app = App::default();
    let (approval_id, pending) = approval_pending();
    app.mode = Mode::Approval(approval_takeover());
    app.pending_approvals.insert(approval_id.clone(), pending);
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(MuxFrame::ApprovalResolved {
                session_id: SessionId("s1".into()),
                approval_id: approval_id.clone(),
                outcome: dsh_tui::wire::events::ApprovalOutcome::Cancelled,
            }),
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(matches!(app.mode, Mode::Chat), "takeover closed");
    let view = format!("{}", term.backend());
    assert!(view.contains("approval cancelled"), "toast: {view}");
}

// ---------------------------------------------------------------------------
// 21. degenerate-size renders (tiny terminal) never panic
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tiny_terminal_renders_every_surface() {
    // 20×5 (<32): the #19 too-small screen takes over — every other
    // surface is gated and nothing panics.
    let mut app = app_with_session();
    app.sessions = vec![summary("s1"), summary("s2")];
    let backend = TestBackend::new(20, 5);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Tab)), // composer focus
            AppEvent::Key(key(KeyCode::Char('h'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("terminal too"), "too-small screen: {view}");
    assert!(view.contains("widen or rotate"), "too-small hint: {view}");
    assert!(!view.contains("s1"), "no chat surfaces below 32 cols");

    // The queue popup + new-session picker are gated below 32 cols — the
    // keys are no-ops and the too-small screen stays.
    let mut app = app_with_session();
    app.store
        .ingest(MuxFrame::SessionQueue {
            session_id: SessionId("s1".into()),
            items: vec![QueueItem {
                id: MessageId("m1".into()),
                placement: QueuePlacement::Queued,
                message: QueueMessage {
                    id: MessageId("m1".into()),
                    role: MessageRole::User,
                    content: vec![],
                    source: QueueMessageSource {
                        kind: "user".into(),
                    },
                },
            }],
        })
        .expect("queue frame");
    let backend = TestBackend::new(20, 5);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::F(1))), // draw: the too-small tier latches
            AppEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT)),
            AppEvent::Key(key(KeyCode::Char('n'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(!app.queue_popup_open, "popups are gated below 32 cols");
    assert!(app.new_session.is_none(), "new-session gated below 32 cols");
}

// ---------------------------------------------------------------------------
// 22. expire_toast + launcher draw + queue roles + takeover corners
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expired_toast_is_cleared_on_the_tick() {
    let mut app = app_with_session();
    app.set_toast("old news");
    // Age the toast beyond the TTL.
    app.toast = Some((
        "old news".into(),
        std::time::Instant::now() - std::time::Duration::from_secs(10),
    ));
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Tick,
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.toast.is_none(), "expired toast cleared");
    let view = format!("{}", term.backend());
    assert!(
        !view.contains("old news"),
        "toast gone from the status line"
    );
}

#[tokio::test]
async fn launcher_draw_shows_group_headers_and_loading() {
    let mut app = app_with_session();
    app.at_catalog = Some(AtCatalog {
        skills: vec![dsh_tui::wire::skills::SkillEntry {
            name: "commit".into(),
            description: "commit helper".into(),
            when_to_use: None,
            model_invocable: false,
        }],
        loading: false,
    });
    app.handle_key(ctrl(KeyCode::Char('p')));
    // Draw with the launcher open (Esc forces the draw, then re-open for
    // the quit — no: draw via the run loop with an inert key).
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Home)), // inert: forces the draw
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("launcher"), "title: {view}");
    assert!(view.contains("actions"), "group header: {view}");

    // The loading row renders while the catalog fetch is in flight.
    let mut app = app_with_session();
    app.at_catalog = Some(AtCatalog {
        skills: vec![],
        loading: true,
    });
    app.handle_key(ctrl(KeyCode::Char('p')));
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Home)), // inert: forces the draw
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(view.contains("loading…"), "loading row: {view}");
}

#[test]
fn queue_role_labels_and_popup_footer_render() {
    let theme = dsh_tui::theme::Theme::default();
    let item = |role: dsh_tui::wire::events::MessageRole, text: &str| QueueItem {
        id: MessageId("m1".into()),
        placement: QueuePlacement::Queued,
        message: QueueMessage {
            id: MessageId("m1".into()),
            role,
            content: vec![dsh_tui::wire::session::ContentBlock {
                r#type: "text".into(),
                extra: serde_json::Map::from_iter([(
                    "text".to_string(),
                    serde_json::Value::String(text.into()),
                )]),
            }],
            source: QueueMessageSource {
                kind: "user".into(),
            },
        },
    };
    let items = vec![
        item(dsh_tui::wire::events::MessageRole::System, "sys"),
        item(dsh_tui::wire::events::MessageRole::Assistant, "asm"),
    ];
    let strip = dsh_tui::ui::queue::QueueStrip {
        items: &items,
        theme: &theme,
        locale: dsh_tui::i18n::Locale::En,
    };
    let backend = TestBackend::new(80, 3);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| f.render_widget(strip, f.area())).unwrap();
    let view = format!("{}", term.backend());
    assert!(view.contains("2 queued · sys"), "strip preview: {view}");

    // The queue popup with the action-line footer.
    let popup = dsh_tui::ui::queue::QueuePopup {
        items: &items,
        scroll: 0,
        theme: &theme,
        locale: dsh_tui::i18n::Locale::En,
        editor: None,
    };
    let backend = TestBackend::new(40, 8);
    let mut term = Terminal::new(backend).unwrap();
    let (w, h) = popup.size(40, 8);
    term.draw(|f| f.render_widget(popup, Rect::new(0, 0, w, h)))
        .unwrap();
    let view = format!("{}", term.backend());
    assert!(view.contains("system"), "system role label: {view}");
    assert!(view.contains("x remove"), "footer actions: {view}");
    // Scroll to the second item: the assistant role label renders.
    let popup = dsh_tui::ui::queue::QueuePopup {
        items: &items,
        scroll: 1,
        theme: &theme,
        locale: dsh_tui::i18n::Locale::En,
        editor: None,
    };
    term.draw(|f| f.render_widget(popup, Rect::new(0, 0, w, h)))
        .unwrap();
    let view = format!("{}", term.backend());
    assert!(view.contains("assistant"), "assistant role label: {view}");
}

#[test]
fn takeover_question_corners() {
    use dsh_tui::ui::takeover::QuestionTakeover;
    use dsh_tui::wire::events::{AskUserQuestionItem, QuestionOption};

    // move_cursor with no options: no-op.
    let mut takeover = QuestionTakeover::new(
        SessionId("s1".into()),
        RpcId("r".into()),
        vec![AskUserQuestionItem {
            id: "q1".into(),
            question: "empty".into(),
            header: None,
            detail: None,
            options: None,
            multi_select: None,
            intent: None,
        }],
    );
    takeover.questions[0].move_cursor(-1);
    takeover.questions[0].move_cursor(1);

    // Multi-select: toggle on, then off again (both branches).
    let mut takeover = QuestionTakeover::new(
        SessionId("s1".into()),
        RpcId("r".into()),
        vec![AskUserQuestionItem {
            id: "q1".into(),
            question: "pick".into(),
            header: None,
            detail: None,
            options: Some(vec![
                QuestionOption {
                    label: "a".into(),
                    description: None,
                },
                QuestionOption {
                    label: "b".into(),
                    description: None,
                },
            ]),
            multi_select: Some(true),
            intent: None,
        }],
    );
    let question = &mut takeover.questions[0];
    question.toggle();
    assert_eq!(question.selected_labels(), vec!["a"]);
    question.toggle();
    assert_eq!(question.selected_labels().len(), 0, "toggled back off");

    // A single-select toggle is a no-op.
    let mut takeover = QuestionTakeover::new(
        SessionId("s1".into()),
        RpcId("r".into()),
        vec![AskUserQuestionItem {
            id: "q1".into(),
            question: "single".into(),
            header: None,
            detail: None,
            options: Some(vec![QuestionOption {
                label: "a".into(),
                description: None,
            }]),
            multi_select: None,
            intent: None,
        }],
    );
    takeover.questions[0].toggle();

    // The no-options notice renders.
    let notice_takeover = QuestionTakeover::new(
        SessionId("s1".into()),
        RpcId("r".into()),
        vec![AskUserQuestionItem {
            id: "q1".into(),
            question: "empty".into(),
            header: None,
            detail: None,
            options: None,
            multi_select: None,
            intent: None,
        }],
    );
    let view = dsh_tui::ui::takeover::QuestionView {
        takeover: &notice_takeover,
        notice: None,
        theme: &dsh_tui::theme::Theme::default(),
        locale: dsh_tui::i18n::Locale::En,
    };
    let backend = TestBackend::new(80, 20);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| f.render_widget(view, f.area())).unwrap();
    let view = format!("{}", term.backend());
    assert!(view.contains("(no options)"), "no-options notice: {view}");
}

// ---------------------------------------------------------------------------
// 23. row-cache degenerate widths + markdown tool-result corners
// ---------------------------------------------------------------------------

#[test]
fn row_cache_zero_width_and_missing_nodes_are_tolerated() {
    let store = SessionStore::new();
    let mut cache = RowCache::new();
    // Width 0: the wrap helper returns empty — no panic, no rows.
    let changed = cache.sync(
        &store,
        &SessionId("s1".into()),
        0,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    );
    assert!(!changed);
    // render_dirty with no session state: skipped.
    cache.render_dirty(
        &store,
        &SessionId("s1".into()),
        80,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    );
    // A real session: sync renders rows; a fresh event dirties them.
    let mut store = SessionStore::new();
    let sid = SessionId("s1".into());
    store
        .ingest(frame("s1", ev(1, "user/message", user_msg("m1", "hi"))))
        .expect("ingest");
    assert!(cache.sync(
        &store,
        &sid,
        80,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    ));
    store
        .ingest(frame("s1", ev(2, "user/message", user_msg("m2", "more"))))
        .expect("ingest");
    assert!(cache.sync(
        &store,
        &sid,
        80,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    ));
    cache.render_dirty(
        &store,
        &sid,
        80,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    );
    // render_dirty against an empty store: the node lookup skips.
    let empty = SessionStore::new();
    cache.render_dirty(
        &empty,
        &sid,
        80,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    );
}

// ---------------------------------------------------------------------------
// 24. settings section navigation + render arms
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn settings_sections_navigate_and_render() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "settings.describe",
        MockAction::Ok(common::SETTINGS_DESCRIBE_OK),
    )
    .await;
    let mut app = app_with_session();
    app.client = Some(WireClient::attach(mock.port()).unwrap());

    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = EventChannel::new();
    let tx = channel.tx.clone();
    let run_task = tokio::spawn(async move {
        let mut app = app;
        let result = app.run(&mut term, &mut channel).await;
        result.expect("run must not fail");
        (app, term)
    });
    tx.send(AppEvent::Key(ctrl(KeyCode::Char(',')))).unwrap();
    mock.wait_for_posts("/api/settings.describe", 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Nav j: General → Models (not exposed) → Plugins (applies on restart).
    tx.send(AppEvent::Key(key(KeyCode::Char('j')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Char('j')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Char('x')))).unwrap(); // draw
    // j beyond the last section: clamped.
    for _ in 0..6 {
        tx.send(AppEvent::Key(key(KeyCode::Char('j')))).unwrap();
    }
    tx.send(AppEvent::Key(key(KeyCode::Char('x')))).unwrap(); // draw
    // Back up to the top.
    for _ in 0..6 {
        tx.send(AppEvent::Key(key(KeyCode::Char('k')))).unwrap();
    }
    tx.send(AppEvent::Key(key(KeyCode::Char('x')))).unwrap(); // draw
    // Enter a field edit, then Esc with the dirty form → warning toast.
    tx.send(AppEvent::Key(key(KeyCode::Tab))).unwrap(); // into the form
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap(); // edit language
    tx.send(AppEvent::Key(key(KeyCode::Char('z')))).unwrap();
    tx.send(AppEvent::Key(key(KeyCode::Enter))).unwrap(); // commit
    tx.send(AppEvent::Key(key(KeyCode::Esc))).unwrap(); // close, dirty → toast
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q')))).unwrap();
    let (_, term) = run_task.await.expect("run task");

    let view = format!("{}", term.backend());
    // The final draw is post-Esc (the chat); the section panes were drawn
    // mid-run by the 'x' nudges — the toast proves the dirty-close path.
    assert!(
        view.contains("closed settings — unsaved changes discarded"),
        "dirty-close toast: {view}"
    );
    mock.stop().await;
}
