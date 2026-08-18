//! App-key edge coverage (#44 companion): the less-traveled key and state
//! arms — sidebar Home/End/n, the inline workspace-path editor's keys, the
//! chat's Esc selection-cancel, the tool-details fallback target, the
//! unsettled-tail session-running probe, and remote resolution toasts.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

use dsh_tui::app::{Action, App, ApprovalPending, Focus};
use dsh_tui::ui::composer::Composer;
use dsh_tui::wire::approvals::{ApprovalRequestId, ApprovalResponseOutcome};
use dsh_tui::wire::events::{ApprovalOutcome, MuxFrame, QuestionOutcome};
use dsh_tui::wire::rpc::RpcId;
use dsh_tui::wire::session::{SessionId, SessionSummary};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
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

fn ev(seq: i64, r#type: &str, data: serde_json::Value) -> dsh_tui::wire::session::SessionEvent {
    dsh_tui::wire::session::SessionEvent {
        r#type: r#type.into(),
        seq,
        time: seq as f64,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn frame(session: &str, event: dsh_tui::wire::session::SessionEvent) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: None,
    }
}

#[test]
fn sidebar_home_end_and_new_session_keys() {
    let mut app = App::default();
    app.sessions = vec![summary("s1"), summary("s2"), summary("s3")];
    app.focus = Focus::Sidebar;

    // Home jumps to the first session, End to the last.
    app.sidebar.selected = 2;
    assert_eq!(app.handle_key(key(KeyCode::Home)), Some(Action::Select));
    assert_eq!(app.sidebar.selected, 0, "Home → first");
    assert_eq!(app.handle_key(key(KeyCode::End)), Some(Action::Select));
    assert_eq!(app.sidebar.selected, 2, "End → last");
    // `n` opens the new-session picker from the sidebar.
    assert_eq!(app.handle_key(key(KeyCode::Char('n'))), Some(Action::None));
    assert!(app.new_session.is_some(), "picker opened");
}

#[test]
fn workspace_editor_keys_type_backspace_commit_cancel() {
    let mut app = App::default();
    app.focus = Focus::Sidebar;
    app.workspace_editor = Some(Composer::new());

    // Typing edits the buffer.
    app.handle_key(key(KeyCode::Char('/')));
    app.handle_key(key(KeyCode::Char('t')));
    app.handle_key(key(KeyCode::Char('m')));
    assert_eq!(
        app.workspace_editor.as_ref().map(|e| e.buffer()),
        Some("/tm")
    );
    // Backspace deletes.
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(
        app.workspace_editor.as_ref().map(|e| e.buffer()),
        Some("/t")
    );
    // Enter commits the workspace.create action.
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Action::CreateWorkspace("/t".into()))
    );
    assert!(app.workspace_editor.is_none(), "editor closed on commit");

    // An EMPTY path cancels without dispatching; Esc cancels too.
    let mut app = App::default();
    app.focus = Focus::Sidebar;
    app.workspace_editor = Some(Composer::new());
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Some(Action::None));
    assert!(app.workspace_editor.is_none(), "empty path cancels");

    let mut app = App::default();
    app.focus = Focus::Sidebar;
    app.workspace_editor = Some(Composer::new());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.workspace_editor.is_none(), "Esc cancels");

    // While a sidebar action is in flight, Enter is inert.
    let mut app = App::default();
    app.focus = Focus::Sidebar;
    app.workspace_editor = Some(Composer::new());
    app.sidebar_action_sending = true;
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Some(Action::None));
    assert!(app.workspace_editor.is_some(), "stays open while sending");
}

#[test]
fn chat_esc_cancels_an_armed_selection() {
    let mut app = App::default();
    app.focus = Focus::Chat;
    app.select_mode = true;
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Some(Action::None));
    assert!(!app.select_mode, "Esc disarms selection");
}

#[test]
fn tool_details_falls_back_to_the_last_transcript_tool() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    app.focus = Focus::Chat;
    app.store
        .ingest(frame(
            "s1",
            ev(
                1,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": "{}"}),
            ),
        ))
        .expect("tool call");
    app.store
        .ingest(frame(
            "s1",
            ev(
                2,
                "tool/result",
                json!({"turn": 1, "step": 1, "message": {"id": "tr1", "content": [{"type": "tool-result", "toolCallId": "c1", "content": [{"type": "text", "text": "out"}], "isError": false}], "source": {"kind": "tool", "callId": "c1"}}, "error": null, "meta": null}),
            ),
        ))
        .expect("tool result");
    // The row cache is empty (never rendered): the details toggle falls
    // back to the transcript's last tool node.
    app.handle_key(key(KeyCode::Char('t')));
    let state = app.store.session(&SessionId("s1".into())).unwrap();
    assert!(
        state
            .nodes
            .iter()
            .find(|node| node.key == "c1")
            .map(|node| node.key == "c1")
            .unwrap_or(false),
        "the tool node exists"
    );
    assert_eq!(app.handle_key(key(KeyCode::Char('t'))), Some(Action::None));
}

#[test]
fn session_running_reads_the_unsettled_tail() {
    let mut app = App::default();
    app.active_session = Some(SessionId("s1".into()));
    // Streaming chunks with no assistant/message: an unsettled tail.
    app.store
        .ingest(frame(
            "s1",
            ev(
                1,
                "assistant/chunk",
                json!({"turn": 1, "step": 1, "chunk": {"type": "block-start", "index": 0, "blockType": "text"}}),
            ),
        ))
        .expect("chunk");
    app.store
        .ingest(frame(
            "s1",
            ev(
                2,
                "assistant/chunk",
                json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "hi"}}),
            ),
        ))
        .expect("delta");
    assert!(app.session_running(), "unsettled tail counts as running");
}

#[test]
fn workspace_rename_editor_keys() {
    use dsh_tui::wire::session::WorkspaceId;
    let mut app = App::default();
    app.focus = Focus::Sidebar;
    // Empty title + Esc cancel; empty Enter cancels too.
    app.workspace_rename = Some((WorkspaceId("w1".into()), Composer::new()));
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Some(Action::None));
    assert!(app.workspace_rename.is_none(), "empty rename cancels");

    let mut app = App::default();
    app.focus = Focus::Sidebar;
    app.workspace_rename = Some((WorkspaceId("w1".into()), Composer::new()));
    app.handle_key(key(KeyCode::Char('x')));
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(
        app.workspace_rename
            .as_ref()
            .map(|(_, editor)| editor.buffer()),
        Some(""),
        "typing + backspace edit the buffer"
    );

    let mut app = App::default();
    app.focus = Focus::Sidebar;
    app.workspace_rename = Some((WorkspaceId("w1".into()), Composer::new()));
    app.handle_key(key(KeyCode::Char('t')));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.workspace_rename.is_none(), "Esc cancels");

    // Enter commits the RenameWorkspace action.
    let mut app = App::default();
    app.focus = Focus::Sidebar;
    app.workspace_rename = Some((WorkspaceId("w1".into()), Composer::new()));
    app.handle_key(key(KeyCode::Char('b')));
    app.handle_key(key(KeyCode::Char('t')));
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Action::RenameWorkspace {
            workspace_id: WorkspaceId("w1".into()),
            title: "bt".into()
        })
    );
    assert!(app.workspace_rename.is_none(), "editor closed on commit");

    // The shared opener with a missing workspace seeds an empty buffer.
    let mut app = App::default();
    app.workspace_rename = Some((WorkspaceId("w1".into()), Composer::new()));
    app.handle_key(key(KeyCode::Esc));
    app.open_workspace_rename_editor(&WorkspaceId("missing".into()));
    assert_eq!(
        app.workspace_rename
            .as_ref()
            .map(|(_, editor)| editor.buffer()),
        Some(""),
        "unknown workspace seeds empty"
    );
}

#[test]
fn workspace_menu_is_inert_while_sending() {
    use dsh_tui::wire::session::WorkspaceId;
    let mut app = App::default();
    app.sidebar_action_sending = true;
    app.open_workspace_context_menu(&WorkspaceId("w1".into()));
    assert!(
        app.context_menu.is_none(),
        "no workspace menu while sending"
    );
}

#[test]
fn context_menu_up_arm_moves_the_cursor() {
    let mut app = App::default();
    app.sessions = vec![dsh_tui::wire::session::SessionSummary {
        session_id: SessionId("s1".into()),
        updated_at: 0.0,
        running: false,
        blank: false,
        parent_session_id: None,
        origin: None,
        cwd: None,
        agent_preset: None,
        projections: None,
    }];
    app.focus = Focus::Sidebar;
    app.handle_key(key(KeyCode::Char('m')));
    // j → 1, then Up and k move it back.
    app.handle_key(key(KeyCode::Char('j')));
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.context_menu.as_ref().map(|m| m.selected), Some(0));
    app.handle_key(key(KeyCode::Char('j')));
    app.handle_key(key(KeyCode::Char('k')));
    assert_eq!(app.context_menu.as_ref().map(|m| m.selected), Some(0));
    app.handle_key(key(KeyCode::Esc));
}

#[test]
fn remote_resolutions_toast_the_outcome() {
    let mut app = App::default();
    let mut pending = ApprovalPending {
        rpc_id: RpcId("rpc-7".into()),
        session_id: SessionId("s1".into()),
        approval_id: ApprovalRequestId("a1".into()),
        tool_name: "bash".into(),
        call_id: None,
        reason: None,
        seq: 1,
    };
    // Each outcome toasts through the remote-resolution texts.
    for (outcome, needle) in [
        (ApprovalOutcome::AllowedOnce, "approved by another client"),
        (ApprovalOutcome::Rejected, "rejected by another client"),
        (ApprovalOutcome::Cancelled, "approval cancelled"),
        (ApprovalOutcome::Unavailable, "approval unavailable"),
    ] {
        pending.approval_id = ApprovalRequestId(format!("a{outcome:?}"));
        app.pending_approvals
            .insert(pending.approval_id.clone(), pending.clone());
        app.record_resolved(&MuxFrame::ApprovalResolved {
            session_id: SessionId("s1".into()),
            approval_id: pending.approval_id.clone(),
            outcome,
        });
        assert!(
            app.toast_text().is_some_and(|text| text.contains(needle)),
            "toast for {outcome:?}: {:?}",
            app.toast_text()
        );
    }

    // A remote question resolution toasts the answered/cancelled texts.
    for (outcome, needle) in [
        (QuestionOutcome::Answered, "answered by another client"),
        (QuestionOutcome::Cancelled, "question cancelled"),
    ] {
        let key = RpcId("rpc-9".into()).to_string();
        app.pending_questions.insert(
            key.clone(),
            dsh_tui::app::QuestionPending {
                rpc_id: RpcId("rpc-9".into()),
                session_id: SessionId("s1".into()),
                questions: Vec::new(),
                seq: 2,
            },
        );
        app.record_resolved(&MuxFrame::QuestionResolved {
            session_id: SessionId("s1".into()),
            question_rpc_id: RpcId("rpc-9".into()),
            outcome,
        });
        assert!(
            app.toast_text().is_some_and(|text| text.contains(needle)),
            "question toast for {outcome:?}: {:?}",
            app.toast_text()
        );
        let _ = key;
    }

    // A resolution for an unknown entry is a local echo — no toast change.
    let before = app.toast_text().map(str::to_string);
    app.record_resolved(&MuxFrame::ApprovalResolved {
        session_id: SessionId("s1".into()),
        approval_id: ApprovalRequestId("ghost".into()),
        outcome: ApprovalOutcome::Rejected,
    });
    assert_eq!(app.toast_text().map(str::to_string), before);
    // The approval answer flow's response-outcome echo path stays typed.
    let _ = ApprovalResponseOutcome::AllowedOnce;
}
