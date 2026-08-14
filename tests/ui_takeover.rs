//! Takeover tests: approval/question full-screen rendering, mode transitions,
//! toasts, and the respond echo flow against the mock gateway. Keyless:
//! injected events + `TestBackend` only.

use std::time::Duration;

mod common;
use common::{MockAction, MockGateway};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::app::{App, AppEvent, EventChannel};
use dsh_tui::client::WireClient;
use dsh_tui::ui::takeover::Mode;
use dsh_tui::wire::approvals::ApprovalRequestId;
use dsh_tui::wire::events::{ApprovalOutcome, MuxFrame};
use dsh_tui::wire::rpc::RpcId;
use dsh_tui::wire::session::SessionId;

// ---------------------------------------------------------------------------
// fixture helpers
// ---------------------------------------------------------------------------

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn approval_requested(id: &str, tool: &str, reason: Option<&str>) -> MuxFrame {
    MuxFrame::ApprovalRequested {
        session_id: SessionId("s1".into()),
        approval_id: ApprovalRequestId(id.into()),
        tool_name: tool.into(),
        call_id: Some(format!("call-{id}")),
        reason: reason.map(str::to_string),
    }
}

fn question_requested(questions: serde_json::Value) -> MuxFrame {
    let frame = json!({
        "type": "question/requested",
        "sessionId": "s1",
        "questions": questions,
    });
    serde_json::from_value(frame).expect("question frame")
}

/// Feed buffered events into a fresh channel and run the loop to completion
/// (the quit event breaks it). During a takeover `q` is inert — quit via
/// Ctrl+C.
async fn run_with(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut channel = EventChannel::new();
    for event in events {
        channel.tx.send(event).expect("event channel");
    }
    app.run(term, &mut channel)
        .await
        .expect("run must not fail");
}

/// End-to-end answer flow: inject events, let the spawned respond land its
/// AnswerDone, then quit (Ctrl+C) and return the app + terminal for
/// assertions.
async fn run_answer_flow(
    mut app: App,
    mut term: Terminal<TestBackend>,
    events: Vec<AppEvent>,
    settle: Duration,
) -> (App, Terminal<TestBackend>) {
    let mut channel = EventChannel::new();
    let tx = channel.tx.clone();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut channel).await;
        (result, app, term)
    });
    for event in events {
        tx.send(event).expect("event channel");
    }
    tokio::time::sleep(settle).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c'))))
        .expect("quit");
    let (result, app, term) = run_task.await.expect("run task");
    result.expect("run must not fail");
    (app, term)
}

// ---------------------------------------------------------------------------
// approval rendering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn approval_takeover_with_reason_120x30() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-a1".into()),
                frame: approval_requested("a1", "bash", Some("runs a shell command")),
            },
            AppEvent::Key(ctrl(KeyCode::Char('c'))),
        ],
    )
    .await;

    let view = format!("{}", term.backend());
    assert!(matches!(app.mode, Mode::Approval(_)), "takeover opened");
    assert!(view.contains("approval"), "title: {view}");
    assert!(view.contains("bash"), "tool name prominent: {view}");
    assert!(
        view.contains("reason: runs a shell command"),
        "reason: {view}"
    );
    assert!(
        view.contains("session s1 · call call-a1"),
        "context: {view}"
    );
    assert!(view.contains("allow once"), "action line: {view}");
    assert!(view.contains("reject"), "action line: {view}");
    assert!(!view.contains("Sessions"), "three-region layout replaced");
}

#[tokio::test]
async fn approval_takeover_without_reason_60x15() {
    let mut app = App::default();
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-a2".into()),
                frame: approval_requested("a2", "read_file", None),
            },
            AppEvent::Key(ctrl(KeyCode::Char('c'))),
        ],
    )
    .await;

    let view = format!("{}", term.backend());
    assert!(view.contains("read_file"), "tool name: {view}");
    assert!(!view.contains("reason:"), "no reason line: {view}");
    assert!(view.contains("allow once"), "action line fits: {view}");
}

// ---------------------------------------------------------------------------
// question rendering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn question_takeover_with_options_120x30() {
    let mut app = App::default();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-q1".into()),
                frame: question_requested(json!([{
                    "id": "q1",
                    "question": "Which files should I touch?",
                    "header": "Scope",
                    "detail": "pick all that apply",
                    "multiSelect": true,
                    "options": [
                        {"label": "src", "description": "library code"},
                        {"label": "tests", "description": "test code"}
                    ]
                }])),
            },
            AppEvent::Key(ctrl(KeyCode::Char('c'))),
        ],
    )
    .await;

    let view = format!("{}", term.backend());
    assert!(matches!(app.mode, Mode::Question(_)));
    assert!(view.contains("question"), "title: {view}");
    assert!(view.contains("Scope"), "header: {view}");
    assert!(
        view.contains("Which files should I touch?"),
        "question: {view}"
    );
    assert!(view.contains("pick all that apply"), "detail: {view}");
    assert!(
        view.contains("[ ] src — library code"),
        "option + description: {view}"
    );
    assert!(view.contains("space"), "action line: {view}");
}

#[tokio::test]
async fn plan_review_synthesizes_approve_refuse_60x15() {
    let mut app = App::default();
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-q2".into()),
                frame: question_requested(json!([{
                    "id": "q1",
                    "question": "Proceed with this plan?",
                    "intent": {"kind": "plan-review", "approve": "Looks good, go ahead"}
                }])),
            },
            AppEvent::Key(ctrl(KeyCode::Char('c'))),
        ],
    )
    .await;

    let view = format!("{}", term.backend());
    assert!(view.contains("plan review"), "title: {view}");
    assert!(view.contains("Proceed with this plan?"), "question: {view}");
    assert!(
        view.contains("Looks good, go ahead"),
        "approve option: {view}"
    );
    assert!(view.contains("Refuse"), "synthesized refuse: {view}");
}

// ---------------------------------------------------------------------------
// approval flow, end to end (mock gateway)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn approval_answer_echoes_rpc_id_and_resolves() {
    let mock = MockGateway::start().await;
    mock.set_handler("respond", MockAction::Ok(r#"{"accepted":true}"#))
        .await;

    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    // The answer spawns; the settle lets the AnswerDone land before quitting.
    (app, term) = run_answer_flow(
        app,
        term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-approval-9".into()),
                frame: approval_requested("a9", "bash", None),
            },
            AppEvent::Key(key(KeyCode::Char('y'))),
        ],
        Duration::from_millis(150),
    )
    .await;

    // Answered via the back-channel: back to chat, toast set.
    assert!(matches!(app.mode, Mode::Chat), "answered → chat");
    assert!(app.pending_approvals.is_empty());
    assert_eq!(app.toast_text(), Some("allowed once"));

    assert_eq!(
        mock.respond_rpc_ids().await,
        vec!["rpc-approval-9".to_string()],
        "respond echoes the requested frame's rpcId"
    );
    let requests = mock.requests().await;
    let respond = requests
        .iter()
        .find(|request| request.path == "/api/respond")
        .expect("respond POST");
    let body: serde_json::Value = serde_json::from_str(&respond.body).expect("json body");
    assert_eq!(body["result"]["value"]["sessionId"], "s1");
    assert_eq!(body["result"]["value"]["approvalId"], "a9");
    assert_eq!(body["result"]["value"]["outcome"], "allowed-once");

    // The resolved echo arrives later: no pending entry, so no second toast.
    app.running = true;
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(MuxFrame::ApprovalResolved {
                session_id: SessionId("s1".into()),
                approval_id: ApprovalRequestId("a9".into()),
                outcome: ApprovalOutcome::AllowedOnce,
            }),
            AppEvent::Key(key(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert_eq!(
        app.toast_text(),
        Some("allowed once"),
        "the resolved echo does not re-toast"
    );
    mock.stop().await;
}

#[tokio::test]
async fn approval_reject_posts_rejected_outcome() {
    let mock = MockGateway::start().await;
    mock.set_handler("respond", MockAction::Ok(r#"{"accepted":true}"#))
        .await;

    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (app, _term) = run_answer_flow(
        app,
        term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-approval-10".into()),
                frame: approval_requested("a10", "write_file", None),
            },
            AppEvent::Key(key(KeyCode::Char('n'))),
        ],
        Duration::from_millis(150),
    )
    .await;

    assert!(matches!(app.mode, Mode::Chat));
    assert_eq!(app.toast_text(), Some("rejected"));
    let requests = mock.requests().await;
    let respond = requests
        .iter()
        .find(|request| request.path == "/api/respond")
        .expect("respond POST");
    let body: serde_json::Value = serde_json::from_str(&respond.body).expect("json body");
    assert_eq!(body["result"]["value"]["outcome"], "rejected");
    mock.stop().await;
}

#[tokio::test]
async fn remote_resolution_toasts_and_returns_to_chat() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-a11".into()),
                frame: approval_requested("a11", "bash", None),
            },
            // Another client answers: the resolved frame arrives without a
            // local keypress.
            AppEvent::Frame(MuxFrame::ApprovalResolved {
                session_id: SessionId("s1".into()),
                approval_id: ApprovalRequestId("a11".into()),
                outcome: ApprovalOutcome::AllowedOnce,
            }),
            // Esc forces an immediate draw of the chat (the resolved frame's
            // own draw is coalesced into the one the takeover just drew).
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(key(KeyCode::Char('q'))),
        ],
    )
    .await;

    assert!(matches!(app.mode, Mode::Chat), "resolved → chat");
    assert_eq!(app.toast_text(), Some("approved by another client"));
    let view = format!("{}", term.backend());
    assert!(view.contains("approved by another client"), "toast: {view}");
}

// ---------------------------------------------------------------------------
// question flow, end to end (mock gateway)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn question_submit_posts_all_answers() {
    let mock = MockGateway::start().await;
    mock.set_handler("respond", MockAction::Ok(r#"{"accepted":true}"#))
        .await;

    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (app, _term) = run_answer_flow(
        app,
        term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-question-9".into()),
                frame: question_requested(json!([
                    {
                        "id": "q1",
                        "question": "Which areas?",
                        "multiSelect": true,
                        "options": [{"label": "src"}, {"label": "tests"}, {"label": "docs"}]
                    },
                    {
                        "id": "q2",
                        "question": "Commit after?",
                        "options": [{"label": "yes"}, {"label": "no"}]
                    }
                ])),
            },
            // q1 (multi): toggle "src" and "tests".
            AppEvent::Key(key(KeyCode::Char(' '))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char(' '))),
            // q2 (single): move to "no" (cursor is the selection).
            AppEvent::Key(key(KeyCode::Tab)),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(150),
    )
    .await;

    assert!(matches!(app.mode, Mode::Chat), "submitted → chat");
    assert_eq!(app.toast_text(), Some("answered"));
    assert_eq!(
        mock.respond_rpc_ids().await,
        vec!["rpc-question-9".to_string()]
    );
    let requests = mock.requests().await;
    let respond = requests
        .iter()
        .find(|request| request.path == "/api/respond")
        .expect("respond POST");
    let body: serde_json::Value = serde_json::from_str(&respond.body).expect("json body");
    let answers = &body["result"]["value"]["answer"]["answers"];
    assert_eq!(
        answers[0],
        json!({"id": "q1", "selected": ["src", "tests"]}),
        "multi-select carries both labels"
    );
    assert_eq!(
        answers[1],
        json!({"id": "q2", "selected": ["no"]}),
        "single-select carries the cursor label"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// key routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_keys_are_inert_during_a_takeover() {
    let mut app = App::default();
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Answerable {
                rpc_id: RpcId("rpc-a12".into()),
                frame: approval_requested("a12", "bash", None),
            },
            // None of these answer or quit while the takeover is open.
            AppEvent::Key(key(KeyCode::Char('q'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Tab)),
            AppEvent::Key(ctrl(KeyCode::Char('c'))),
        ],
    )
    .await;

    assert!(
        matches!(app.mode, Mode::Approval(_)),
        "q/j/tab left the takeover open"
    );
    assert!(
        app.pending_approvals
            .contains_key(&ApprovalRequestId("a12".into())),
        "nothing was answered"
    );
}
