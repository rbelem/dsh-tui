//! App-shell integration tests: attach flow, event loop, coalesced draw,
//! keymap, follow, resize. Keyless: the shared mock gateway, injected events,
//! and `TestBackend` only — no real terminal.

mod common;
use common::{MockAction, MockGateway};

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use tokio::sync::mpsc;

use dsh_tui::app::{Action, App, AppEvent, Focus, attach, spawn_frame_bridge};
use dsh_tui::client::WireClient;
use dsh_tui::store::SessionStore;
use dsh_tui::ui::takeover::{ApprovalTakeover, Mode, QuestionTakeover};
use dsh_tui::wire::approvals::{ApprovalRequestId, ApprovalResponseOutcome};
use dsh_tui::wire::events::{
    ApprovalOutcome, AskUserQuestionItem, MuxFrame, QuestionOption, QuestionOutcome,
};
use dsh_tui::wire::rpc::RpcId;
use dsh_tui::wire::session::{SessionEvent, SessionId};

// ---------------------------------------------------------------------------
// fixture helpers
// ---------------------------------------------------------------------------

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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

fn mux_subscribed(session: &str, last_seq: i64) -> String {
    format!(
        r#"{{"type":"server-request","rpcId":"ws","method":"events.mux","payload":{{"type":"session/subscribed","sessionId":"{session}","lastSeq":{last_seq}}}}}"#
    )
}

fn mux_chunk(session: &str, seq: i64, chunk: serde_json::Value) -> String {
    let payload = json!({
        "type": "session/event",
        "sessionId": session,
        "event": {
            "type": "assistant/chunk",
            "seq": seq,
            "time": seq as f64,
            "data": {"turn": 2, "step": 1, "chunk": chunk},
        },
    });
    serde_json::to_string(&json!({
        "type": "server-request",
        "rpcId": "ws",
        "method": "events.mux",
        "payload": payload,
    }))
    .expect("serialize mux frame")
}

fn mux_assistant_message(session: &str, seq: i64, text: &str) -> String {
    let payload = json!({
        "type": "session/event",
        "sessionId": session,
        "event": {
            "type": "assistant/message",
            "seq": seq,
            "time": seq as f64,
            "data": {
                "turn": 2,
                "step": 1,
                "message": {
                    "id": "m3",
                    "role": "assistant",
                    "content": [{"type": "text", "text": text}],
                    "source": {"kind": "model", "provider": "p", "model": "m"},
                },
            },
        },
    });
    serde_json::to_string(&json!({
        "type": "server-request",
        "rpcId": "ws",
        "method": "events.mux",
        "payload": payload,
    }))
    .expect("serialize mux frame")
}

/// Feed buffered events into a fresh channel and run the loop to completion
/// (the quit key breaks it).
async fn run_with(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    for event in events {
        tx.send(event).expect("event channel");
    }
    app.run(term, &mut rx).await.expect("run must not fail");
}

// ---------------------------------------------------------------------------
// 1. attach → history → mux → draw (end to end)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attach_history_draw_end_to_end() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[
                {"sessionId":"s1","updatedAt":200.0,"running":false,"blank":false},
                {"sessionId":"s2","updatedAt":100.0,"running":false,"blank":true}
            ]}}}"#,
        ),
    )
    .await;
    mock.set_handler(
        "session.history",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"events":[
                {"event":{"type":"user/message","seq":1,"time":1.0,"data":{"id":"m1","role":"user","content":[{"type":"text","text":"from history\n\n```rust\nfn main() {}\n```"}],"source":{"kind":"user"}}}},
                {"event":{"type":"assistant/message","seq":2,"time":2.0,"data":{"turn":1,"step":1,"message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"from history assistant"}],"source":{"kind":"model","provider":"p","model":"m"}}}}}
            ],"hasMore":false}}}"#,
        ),
    )
    .await;
    mock.set_ws_frames(
        "/api/events.mux",
        vec![
            mux_subscribed("s1", 2),
            mux_chunk(
                "s1",
                3,
                json!({"type": "block-start", "index": 0, "blockType": "text"}),
            ),
            mux_chunk(
                "s1",
                4,
                json!({"type": "text-delta", "index": 0, "text": "streamed hello"}),
            ),
            mux_assistant_message("s1", 5, "streamed hello"),
        ],
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut store = SessionStore::new();
    let (opened, sessions) = attach(&client, &mut store).await.expect("attach");
    assert_eq!(opened, Some(SessionId("s1".into())));
    assert_eq!(sessions.len(), 2, "attach hands the sidebar the full list");

    let mut app = App::default();
    app.store = store;
    app.active_session = opened;
    app.sessions = sessions;

    let (tx, mut rx) = mpsc::unbounded_channel();
    spawn_frame_bridge(client.mux_stream(), tx.clone());
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut rx).await;
        (result, term)
    });
    // Let the mock's scripted frames arrive and be drawn.
    tokio::time::sleep(Duration::from_millis(400)).await;
    tx.send(AppEvent::Key(key(KeyCode::Char('q'))))
        .expect("quit key");
    let (result, term) = run_task.await.expect("run task");
    result.expect("run");

    let view = format!("{}", term.backend());
    assert!(
        view.contains("from history"),
        "user text from the history page"
    );
    assert!(view.contains("│ fn main()"), "code fence from history");
    assert!(
        view.contains("streamed hello"),
        "streamed assistant text via mux"
    );
    assert!(view.contains("session s1"), "status line shows the session");

    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 2. coalescing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn coalescing_draws_once_per_batch() {
    let mut app = App::default();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut events = Vec::new();
    for seq in 1..=3 {
        events.push(AppEvent::Frame(frame(
            "s1",
            ev(seq, "user/message", user_msg(&format!("m{seq}"), "hi")),
        )));
    }
    events.push(AppEvent::Tick);
    events.push(AppEvent::Key(key(KeyCode::Char('q'))));
    run_with(&mut app, &mut term, events).await;

    // One draw for the whole batch: the first frame draws, the rest coalesce
    // until the (not-yet-elapsed) tick; the quit key does not draw.
    assert_eq!(app.draws, 1, "rapid frame batch coalesces into one draw");
}

// ---------------------------------------------------------------------------
// 3. keymap table
// ---------------------------------------------------------------------------

#[test]
fn keymap_table() {
    fn composer_focus(app: &mut App) {
        app.focus = Focus::Composer;
    }
    fn sidebar_focus(app: &mut App) {
        app.focus = Focus::Sidebar;
    }
    fn slash_popup(app: &mut App) {
        app.focus = Focus::Composer;
        app.composer.insert_char('/');
    }
    fn approval_mode(app: &mut App) {
        app.mode = Mode::Approval(ApprovalTakeover {
            session_id: SessionId("s1".into()),
            approval_id: ApprovalRequestId("a1".into()),
            rpc_id: RpcId("rpc-1".into()),
            tool_name: "bash".into(),
            call_id: None,
            reason: None,
            tool_summary: None,
        });
    }
    fn question_mode(app: &mut App) {
        app.mode = Mode::Question(QuestionTakeover::new(
            SessionId("s1".into()),
            RpcId("rpc-1".into()),
            vec![AskUserQuestionItem {
                id: "q1".into(),
                question: "pick one".into(),
                header: None,
                detail: None,
                options: Some(vec![QuestionOption {
                    label: "a".into(),
                    description: None,
                }]),
                multi_select: None,
                intent: None,
            }],
        ));
    }
    /// One binding case: setup, the key, the expected action.
    type Case = (fn(&mut App), KeyEvent, Option<Action>);
    let cases: Vec<Case> = vec![
        // chat (default focus): scroll keys, quit, Esc no-op
        (|_| {}, key(KeyCode::Char('q')), Some(Action::Quit)),
        (|_| {}, ctrl(KeyCode::Char('c')), Some(Action::Quit)),
        (|_| {}, key(KeyCode::Char('j')), Some(Action::Scroll(1))),
        (|_| {}, key(KeyCode::Down), Some(Action::Scroll(1))),
        (|_| {}, key(KeyCode::Char('k')), Some(Action::Scroll(-1))),
        (|_| {}, key(KeyCode::Up), Some(Action::Scroll(-1))),
        (
            |_| {},
            key(KeyCode::Char('g')),
            Some(Action::Scroll(i64::MIN)),
        ),
        (|_| {}, key(KeyCode::Home), Some(Action::Scroll(i64::MIN))),
        (
            |_| {},
            key(KeyCode::Char('G')),
            Some(Action::Scroll(i64::MAX)),
        ),
        (|_| {}, key(KeyCode::End), Some(Action::Scroll(i64::MAX))),
        (|_| {}, ctrl(KeyCode::Char('d')), Some(Action::Scroll(12))),
        (|_| {}, ctrl(KeyCode::Char('u')), Some(Action::Scroll(-12))),
        (|_| {}, key(KeyCode::Esc), Some(Action::None)),
        (|_| {}, key(KeyCode::Char('x')), None),
        // Tab cycles chat → composer → sidebar → chat
        (
            |_| {},
            key(KeyCode::Tab),
            Some(Action::Focus(Focus::Composer)),
        ),
        (
            composer_focus,
            key(KeyCode::Tab),
            Some(Action::Focus(Focus::Sidebar)),
        ),
        (
            sidebar_focus,
            key(KeyCode::Tab),
            Some(Action::Focus(Focus::Chat)),
        ),
        // Ctrl+C quits from any surface
        (composer_focus, ctrl(KeyCode::Char('c')), Some(Action::Quit)),
        (sidebar_focus, ctrl(KeyCode::Char('c')), Some(Action::Quit)),
        // composer: editing, Enter/Shift+Enter, Esc back to the chat
        (composer_focus, key(KeyCode::Char('a')), Some(Action::Input)),
        (composer_focus, key(KeyCode::Char('q')), Some(Action::Input)),
        (composer_focus, key(KeyCode::Backspace), Some(Action::Input)),
        (composer_focus, key(KeyCode::Left), Some(Action::Input)),
        (composer_focus, key(KeyCode::Right), Some(Action::Input)),
        (composer_focus, key(KeyCode::Home), Some(Action::Input)),
        (composer_focus, key(KeyCode::End), Some(Action::Input)),
        (composer_focus, key(KeyCode::Up), Some(Action::Input)),
        (composer_focus, key(KeyCode::Down), Some(Action::Input)),
        (composer_focus, key(KeyCode::Enter), Some(Action::None)), // empty buffer
        (composer_focus, shift(KeyCode::Enter), Some(Action::Input)), // newline
        (
            composer_focus,
            key(KeyCode::Esc),
            Some(Action::Focus(Focus::Chat)),
        ),
        (composer_focus, ctrl(KeyCode::Char('d')), Some(Action::None)),
        // composer with the seed popup open: arrows navigate, Enter accepts,
        // Esc closes
        (slash_popup, key(KeyCode::Up), Some(Action::Input)),
        (slash_popup, key(KeyCode::Down), Some(Action::Input)),
        (slash_popup, key(KeyCode::Enter), Some(Action::Input)),
        (slash_popup, key(KeyCode::Esc), Some(Action::None)),
        // sidebar: navigate, switch, Esc back to the chat
        (sidebar_focus, key(KeyCode::Char('j')), Some(Action::Select)),
        (sidebar_focus, key(KeyCode::Char('k')), Some(Action::Select)),
        (sidebar_focus, key(KeyCode::Down), Some(Action::Select)),
        (sidebar_focus, key(KeyCode::Up), Some(Action::Select)),
        (sidebar_focus, key(KeyCode::Char('g')), Some(Action::Select)),
        (sidebar_focus, key(KeyCode::Char('G')), Some(Action::Select)),
        (sidebar_focus, key(KeyCode::Enter), Some(Action::None)), // empty list
        (
            sidebar_focus,
            key(KeyCode::Esc),
            Some(Action::Focus(Focus::Chat)),
        ),
        (sidebar_focus, key(KeyCode::Char('q')), Some(Action::Quit)),
        // approval takeover: y allow once, n/Esc reject; chat keys inert
        (
            approval_mode,
            key(KeyCode::Char('y')),
            Some(Action::AnswerApproval(ApprovalResponseOutcome::AllowedOnce)),
        ),
        (
            approval_mode,
            key(KeyCode::Char('n')),
            Some(Action::AnswerApproval(ApprovalResponseOutcome::Rejected)),
        ),
        (
            approval_mode,
            key(KeyCode::Esc),
            Some(Action::AnswerApproval(ApprovalResponseOutcome::Rejected)),
        ),
        (approval_mode, key(KeyCode::Char('q')), Some(Action::None)),
        (approval_mode, key(KeyCode::Char('j')), Some(Action::None)),
        (approval_mode, key(KeyCode::Tab), Some(Action::None)),
        // question takeover: nav/toggle/submit; chat keys inert
        (question_mode, key(KeyCode::Tab), Some(Action::Input)),
        (question_mode, key(KeyCode::Down), Some(Action::Input)),
        (question_mode, key(KeyCode::Char('j')), Some(Action::Input)),
        (question_mode, key(KeyCode::Char(' ')), Some(Action::Input)),
        (
            question_mode,
            key(KeyCode::Enter),
            Some(Action::AnswerQuestion),
        ),
        (question_mode, key(KeyCode::Esc), Some(Action::None)),
        (question_mode, key(KeyCode::Char('q')), Some(Action::None)),
        // Ctrl+C stays the global quit, even during a takeover
        (approval_mode, ctrl(KeyCode::Char('c')), Some(Action::Quit)),
        (question_mode, ctrl(KeyCode::Char('c')), Some(Action::Quit)),
    ];
    for (setup, event, expected) in cases {
        let mut app = App::default();
        setup(&mut app);
        assert_eq!(app.handle_key(event), expected, "key {event:?}");
    }
}

// ---------------------------------------------------------------------------
// 4. follow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn follow_sticks_to_bottom_until_manual_scroll() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let backend = TestBackend::new(100, 5);
    let mut term = Terminal::new(backend).unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut rx).await;
        (result, app)
    });
    for seq in 1..=5 {
        tx.send(AppEvent::Frame(frame(
            "s1",
            ev(seq, "user/message", user_msg(&format!("m{seq}"), "hi")),
        )))
        .expect("frame");
    }
    // Let a tick draw the accumulated state: follow clamps to the bottom
    // (5 rows; layout: chat 2 + composer 2 + status 1 → offset 3).
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(AppEvent::Key(key(KeyCode::Char('j')))).expect("j");
    tokio::time::sleep(Duration::from_millis(20)).await;
    tx.send(AppEvent::Key(key(KeyCode::Char('q')))).expect("q");
    let (result, app) = run_task.await.expect("run task");
    result.expect("run");

    assert!(!app.view.follow, "manual scroll disables follow");
    // Followed bottom was offset 3; j moves one row past it.
    assert_eq!(app.view.offset, 4);
}

// ---------------------------------------------------------------------------
// 5. no-session attach
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_session_attach_renders_empty_chat() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[]}}}"#,
        ),
    )
    .await;
    let client = WireClient::attach(mock.port()).unwrap();
    let mut store = SessionStore::new();
    let (opened, sessions) = attach(&client, &mut store).await.expect("attach");
    assert_eq!(opened, None, "no sessions on the gateway");
    assert!(sessions.is_empty());

    let mut app = App::default();
    app.store = store;
    app.last_error = Some("gateway has no sessions".into());
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    // A frame still arrives (any session); the empty chat must not panic.
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(frame("s1", ev(1, "user/message", user_msg("m1", "hi")))),
            AppEvent::Tick,
            AppEvent::Key(key(KeyCode::Char('q'))),
        ],
    )
    .await;

    let view = format!("{}", term.backend());
    assert!(view.contains("no session"), "status line: {view}");
    assert!(view.contains("no sessions"), "hint in status line: {view}");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 6. resize
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resize_invalidates_and_rerenders() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    let long_text = "This is a fairly long paragraph line that will definitely wrap at narrower widths like sixty columns or so. And here is another sentence to make the paragraph long enough that the wrap counts differ clearly between widths.";
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let wide_lines_before = {
        run_with(
            &mut app,
            &mut term,
            vec![
                AppEvent::Frame(frame(
                    "s1",
                    ev(1, "user/message", user_msg("m1", long_text)),
                )),
                AppEvent::Resize(60, 15),
                AppEvent::Key(key(KeyCode::Char('q'))),
            ],
        )
        .await;
        app.row_cache.lines()[0].lines.len()
    };

    let view = format!("{}", term.backend());
    assert!(
        view.contains("long paragraph"),
        "content survives the resize"
    );

    // Re-wrap at the new width (backend resized between runs).
    term.backend_mut().resize(60, 15);
    app.running = true;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut rx).await;
        (result, app)
    });
    // Duplicate seq: ignored by the store, still triggers a draw.
    tx.send(AppEvent::Frame(frame(
        "s1",
        ev(1, "user/message", user_msg("m1", long_text)),
    )))
    .expect("frame");
    // Let a tick draw the re-wrapped state.
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(AppEvent::Key(key(KeyCode::Char('q')))).expect("q");
    let (result, app) = run_task.await.expect("run task");
    result.expect("run");
    let wide = wide_lines_before;
    let narrow = app.row_cache.lines()[0].lines.len();
    assert!(
        narrow > wide,
        "rows re-wrap at 60 columns (wide={wide}, narrow={narrow})"
    );
}

// ---------------------------------------------------------------------------
// approval/question pending tracking + respond echo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn approval_pending_tracking_and_respond_echo() {
    // The mock only serves /api/respond; answerable events are injected
    // directly (the frame bridge would produce the same AppEvents).
    let mock = MockGateway::start().await;
    mock.set_handler("respond", MockAction::Ok(r#"{"accepted":true}"#))
        .await;
    let client = WireClient::attach(mock.port()).unwrap();

    let mut app = App::default();
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut rx).await;
        (result, app, term)
    });
    tx.send(AppEvent::Answerable {
        rpc_id: RpcId("rpc-approval-1".into()),
        frame: MuxFrame::ApprovalRequested {
            session_id: SessionId("s1".into()),
            approval_id: ApprovalRequestId("a1".into()),
            tool_name: "read_file".into(),
            call_id: Some("call-1".into()),
            reason: Some("reads /etc".into()),
        },
    })
    .expect("answerable");
    // The requested frame opened the approval takeover: `q` is inert there,
    // so quit via the global Ctrl+C.
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c'))))
        .expect("ctrl+c");
    let (result, mut app, mut term) = run_task.await.expect("run task");
    result.expect("run");

    // The requested frame recorded the pending approval with the echo target.
    let pending = app
        .pending_approvals
        .get(&ApprovalRequestId("a1".into()))
        .expect("pending approval recorded");
    assert_eq!(pending.rpc_id, RpcId("rpc-approval-1".into()));
    assert_eq!(pending.tool_name, "read_file");
    assert_eq!(pending.call_id.as_deref(), Some("call-1"));
    assert_eq!(pending.reason.as_deref(), Some("reads /etc"));
    let echo_target = pending.rpc_id.clone();

    // Respond flow: the recorded rpc_id is what the answer echoes.
    let receipt = client
        .respond_approval(
            echo_target,
            SessionId("s1".into()),
            ApprovalRequestId("a1".into()),
            ApprovalResponseOutcome::AllowedOnce,
        )
        .await
        .expect("respond");
    assert!(receipt.accepted);
    assert_eq!(
        mock.respond_rpc_ids().await,
        vec!["rpc-approval-1".to_string()],
        "respond echoes the requested frame's rpcId, never mints a new one"
    );

    // The resolved frame (a pure push with its own fresh rpcId) removes the
    // pending entry; correlation is by payload approvalId.
    app.running = true;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let run_task = tokio::spawn(async move {
        let result = app.run(&mut term, &mut rx).await;
        (result, app, term)
    });
    tx.send(AppEvent::Frame(MuxFrame::ApprovalResolved {
        session_id: SessionId("s1".into()),
        approval_id: ApprovalRequestId("a1".into()),
        outcome: ApprovalOutcome::AllowedOnce,
    }))
    .expect("resolved");
    tx.send(AppEvent::Key(key(KeyCode::Char('q')))).expect("q");
    let (result, app, _term) = run_task.await.expect("run task");
    result.expect("run");
    assert!(
        app.pending_approvals.is_empty(),
        "resolved removes the pending entry"
    );

    mock.stop().await;
}

#[tokio::test]
async fn question_pending_tracking_by_echo_rpc_id() {
    // question/requested records by rpcId; question/resolved (payload
    // questionRpcId) removes it.
    let mut app = App::default();
    let requested = MuxFrame::QuestionRequested {
        session_id: SessionId("s1".into()),
        questions: vec![],
    };
    app.record_answerable(RpcId("rpc-question-1".into()), &requested);
    assert_eq!(
        app.pending_questions
            .get("rpc-question-1")
            .map(|p| &p.rpc_id),
        Some(&RpcId("rpc-question-1".into()))
    );

    let resolved = MuxFrame::QuestionResolved {
        session_id: SessionId("s1".into()),
        question_rpc_id: RpcId("rpc-question-1".into()),
        outcome: QuestionOutcome::Answered,
    };
    app.record_resolved(&resolved);
    assert!(
        app.pending_questions.is_empty(),
        "question/resolved removes the entry"
    );
}
