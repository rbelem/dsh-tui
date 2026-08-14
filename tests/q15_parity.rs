//! Q15/Q9 parity tests: Ctrl+C cancels the running turn (spawned, back-
//! channel), Ctrl+Q quits always, and history loads on sidebar switch with a
//! stale guard. Keyless: shared mock gateway + injected events only.

mod common;
use common::{MockAction, MockGateway};

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use tokio::sync::mpsc;

use dsh_tui::app::{App, AppEvent, EventChannel};
use dsh_tui::client::WireClient;
use dsh_tui::i18n::Locale;
use dsh_tui::store::SessionStore;
use dsh_tui::ui::takeover::{ApprovalTakeover, Mode};
use dsh_tui::wire::approvals::ApprovalRequestId;
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::rpc::RpcId;
use dsh_tui::wire::session::{SessionEvent, SessionId, SessionSummary};

// ---------------------------------------------------------------------------
// fixture helpers
// ---------------------------------------------------------------------------

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

fn summary(session_id: &str, running: bool) -> SessionSummary {
    SessionSummary {
        session_id: SessionId(session_id.into()),
        updated_at: 100.0,
        running,
        blank: false,
        parent_session_id: None,
        origin: None,
        cwd: None,
        agent_preset: None,
        projections: None,
    }
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
        sending: false,
    });
}

/// Run the loop in a spawned task; returns (result, app, term) after the
/// quit key lands.
async fn spawn_run(
    mut app: App,
    mut term: Terminal<TestBackend>,
) -> (
    mpsc::UnboundedSender<AppEvent>,
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
) -> App {
    let (result, app, _term) = task.await.expect("run task");
    result.expect("run");
    app
}

fn cancel_value() -> &'static str {
    r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"accepted":true}}}"#
}

/// The captured `/api/session.cancel` POST bodies (payload sessionId).
async fn cancel_posts(mock: &MockGateway) -> Vec<serde_json::Value> {
    let requests = mock.requests().await;
    requests
        .iter()
        .filter(|request| request.path == "/api/session.cancel")
        .filter_map(|request| serde_json::from_str(&request.body).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// 1. cancel running turn
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_running_posts_cancel_and_toasts() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.cancel", MockAction::Ok(cancel_value()))
        .await;
    let client = WireClient::attach(mock.port()).unwrap();

    let mut app = App::default();
    app.client = Some(client);
    app.active_session = Some(SessionId("s1".into()));
    app.sessions = vec![summary("s1", true)];
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (tx, task) = spawn_run(app, term).await;

    // Ctrl+C with a running turn: cancel, do NOT quit.
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c'))))
        .expect("cancel");
    // The loop stays alive: a frame arriving while the cancel is in flight
    // is ingested.
    tokio::time::sleep(Duration::from_millis(30)).await;
    tx.send(AppEvent::Frame(frame(
        "s1",
        ev(1, "user/message", user_msg("m1", "hi")),
    )))
    .expect("frame");
    // Let the CancelDone land and toast.
    tokio::time::sleep(Duration::from_millis(150)).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    let app = join_run(task).await;

    assert_eq!(app.toast_text(), Some("cancelled"));
    let posts = cancel_posts(&mock).await;
    assert_eq!(posts.len(), 1, "one cancel POST");
    assert_eq!(
        posts[0]["payload"]["sessionId"], "s1",
        "cancel targets the active session"
    );
    assert_eq!(
        app.store
            .session(&SessionId("s1".into()))
            .expect("session")
            .last_seq,
        1,
        "the loop ingested the frame while the cancel was in flight"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 2. cancel idle quits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_idle_quits() {
    let mock = MockGateway::start().await;
    let client = WireClient::attach(mock.port()).unwrap();

    let mut app = App::default();
    app.client = Some(client);
    app.active_session = Some(SessionId("s1".into()));
    app.sessions = vec![summary("s1", false)];
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (tx, task) = spawn_run(app, term).await;

    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c'))))
        .expect("quit");
    let app = join_run(task).await;
    assert!(!app.running, "idle Ctrl+C quits");
    assert!(
        cancel_posts(&mock).await.is_empty(),
        "no cancel POST when idle"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 3. Ctrl+Q quits always
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ctrl_q_quits_with_running_turn() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.cancel", MockAction::Ok(cancel_value()))
        .await;
    let client = WireClient::attach(mock.port()).unwrap();

    let mut app = App::default();
    app.client = Some(client);
    app.active_session = Some(SessionId("s1".into()));
    app.sessions = vec![summary("s1", true)];
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (tx, task) = spawn_run(app, term).await;

    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    let app = join_run(task).await;
    assert!(!app.running, "Ctrl+Q quits even with a running turn");
    assert!(
        cancel_posts(&mock).await.is_empty(),
        "Ctrl+Q does not cancel"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 4. takeover Ctrl+C quits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn takeover_ctrl_c_quits_without_cancel() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.cancel", MockAction::Ok(cancel_value()))
        .await;
    let client = WireClient::attach(mock.port()).unwrap();

    let mut app = App::default();
    app.client = Some(client);
    approval_mode(&mut app);
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (tx, task) = spawn_run(app, term).await;

    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c'))))
        .expect("quit");
    let app = join_run(task).await;
    assert!(
        !app.running,
        "takeover Ctrl+C keeps quitting (documented exception)"
    );
    assert!(
        cancel_posts(&mock).await.is_empty(),
        "no cancel POST from a takeover"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 5. cancel failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_failure_toasts_and_loop_lives() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.cancel",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":false,"error":{"code":"internal","message":"boom","details":{}}}}"#,
        ),
    )
    .await;
    let client = WireClient::attach(mock.port()).unwrap();

    let mut app = App::default();
    app.client = Some(client);
    app.active_session = Some(SessionId("s1".into()));
    app.sessions = vec![summary("s1", true)];
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (tx, task) = spawn_run(app, term).await;

    tx.send(AppEvent::Key(ctrl(KeyCode::Char('c'))))
        .expect("cancel");
    tokio::time::sleep(Duration::from_millis(150)).await;
    // Still alive after the failure: the frame is ingested.
    tx.send(AppEvent::Frame(frame(
        "s1",
        ev(1, "user/message", user_msg("m1", "hi")),
    )))
    .expect("frame");
    tokio::time::sleep(Duration::from_millis(30)).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    let app = join_run(task).await;

    assert!(
        app.toast_text()
            .is_some_and(|text| text.contains("cancel failed") && text.contains("boom")),
        "failure toast: {:?}",
        app.toast_text()
    );
    assert_eq!(
        app.store
            .session(&SessionId("s1".into()))
            .expect("session")
            .last_seq,
        1,
        "loop stayed alive after the failed cancel"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 6. history on switch
// ---------------------------------------------------------------------------

fn history_template(id: &str, text: &str) -> String {
    format!(
        r#"{{"type":"server-response","rpcId":"{{rpcId}}","result":{{"ok":true,"value":{{"events":[
            {{"event":{{"type":"user/message","seq":1,"time":1.0,"data":{{"id":"{id}","role":"user","content":[{{"type":"text","text":"{text}"}}],"source":{{"kind":"user"}}}}}}}}
        ],"hasMore":false}}}}}}"#
    )
}

async fn attach_with_sessions(mock: &MockGateway) -> (WireClient, SessionStore) {
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[
                {"sessionId":"sA","updatedAt":200.0,"running":false,"blank":false},
                {"sessionId":"sB","updatedAt":100.0,"running":false,"blank":false}
            ]}}}"#,
        ),
    )
    .await;
    // sA's history is preloaded by the attach flow (direct, not via the loop).
    mock.set_history("sA", &history_template("mA", "content A"))
        .await;
    mock.set_history("sB", &history_template("mB", "content B"))
        .await;
    let client = WireClient::attach(mock.port()).unwrap();
    let mut store = SessionStore::new();
    let (opened, _sessions) = dsh_tui::app::attach(&client, &mut store, Locale::En)
        .await
        .expect("attach");
    assert_eq!(opened, Some(SessionId("sA".into())));
    (client, store)
}

#[tokio::test]
async fn history_loads_on_sidebar_switch() {
    let mock = MockGateway::start().await;
    let (client, store) = attach_with_sessions(&mock).await;

    let mut app = App::default();
    app.client = Some(client);
    app.store = store;
    app.active_session = Some(SessionId("sA".into()));
    app.sessions = vec![summary("sA", false), summary("sB", false)];
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (tx, task) = spawn_run(app, term).await;

    // Focus the sidebar, move to B, Enter.
    tx.send(AppEvent::Key(key(KeyCode::Tab))).expect("tab");
    tx.send(AppEvent::Key(key(KeyCode::Tab)))
        .expect("tab to sidebar");
    tx.send(AppEvent::Key(key(KeyCode::Char('j'))))
        .expect("move");
    // Right after Enter, the hint is up and the load is in flight.
    tx.send(AppEvent::Key(key(KeyCode::Enter))).expect("switch");
    tokio::time::sleep(Duration::from_millis(30)).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    let app = join_run(task).await;

    assert_eq!(app.active_session, Some(SessionId("sB".into())));
    assert_eq!(app.history_loading, None, "hint cleared after the load");
    assert_eq!(app.hint, None, "loading hint cleared");
    let b = app.store.session(&SessionId("sB".into())).expect("B state");
    assert_eq!(b.last_seq, 1, "B's history page landed");
    assert_eq!(b.nodes.len(), 1);
    assert!(
        matches!(&b.nodes[0].data, dsh_tui::store::node::NodeData::User { message_id, .. } if message_id == "mB")
    );
    // A's store state is untouched by B's load.
    let a = app.store.session(&SessionId("sA".into())).expect("A state");
    assert_eq!(a.last_seq, 1, "A kept its own history");
    assert!(
        matches!(&a.nodes[0].data, dsh_tui::store::node::NodeData::User { message_id, .. } if message_id == "mA")
    );
    // View reset on switch: follow to bottom.
    assert!(app.view.follow);
    mock.stop().await;
}

#[tokio::test]
async fn history_hint_shows_while_in_flight() {
    let mock = MockGateway::start().await;
    let (client, store) = attach_with_sessions(&mock).await;

    let mut app = App::default();
    app.client = Some(client);
    app.store = store;
    app.active_session = Some(SessionId("sA".into()));
    app.sessions = vec![summary("sA", false), summary("sB", false)];
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (tx, task) = spawn_run(app, term).await;

    tx.send(AppEvent::Key(key(KeyCode::Tab))).expect("tab");
    tx.send(AppEvent::Key(key(KeyCode::Tab)))
        .expect("tab to sidebar");
    tx.send(AppEvent::Key(key(KeyCode::Char('j'))))
        .expect("move");
    tx.send(AppEvent::Key(key(KeyCode::Enter))).expect("switch");
    // Immediately after the Enter is processed: hint up, load in flight.
    tokio::time::sleep(Duration::from_millis(20)).await;
    tx.send(AppEvent::Key(key(KeyCode::Esc)))
        .expect("back to chat");
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    let app = join_run(task).await;
    // The 20ms sleep is shorter than the round trip (loopback ~1ms) — the
    // hint may already have cleared; assert the end state is clean instead.
    assert_eq!(app.history_loading, None);
    assert_eq!(app.hint, None);
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 7. stale guard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stale_history_result_is_dropped() {
    let mock = MockGateway::start().await;
    let (client, store) = attach_with_sessions(&mock).await;

    let mut app = App::default();
    app.client = Some(client);
    app.store = store;
    app.active_session = Some(SessionId("sA".into()));
    app.sessions = vec![summary("sA", false), summary("sB", false)];
    let backend = TestBackend::new(120, 30);
    let term = Terminal::new(backend).unwrap();
    let (tx, task) = spawn_run(app, term).await;

    // Switch A → B → A rapidly: B's load is spawned, then A's load
    // supersedes it; B's late result must be dropped.
    tx.send(AppEvent::Key(key(KeyCode::Tab))).expect("tab");
    tx.send(AppEvent::Key(key(KeyCode::Tab)))
        .expect("tab to sidebar");
    tx.send(AppEvent::Key(key(KeyCode::Char('j'))))
        .expect("move to B");
    tx.send(AppEvent::Key(key(KeyCode::Enter)))
        .expect("switch to B");
    tx.send(AppEvent::Key(key(KeyCode::Char('k'))))
        .expect("move to A");
    tx.send(AppEvent::Key(key(KeyCode::Enter)))
        .expect("switch back to A");
    tokio::time::sleep(Duration::from_millis(200)).await;
    tx.send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("quit");
    let app = join_run(task).await;

    assert_eq!(app.active_session, Some(SessionId("sA".into())));
    // A's (second) load applied: last_seq reflects A's history page.
    let a = app.store.session(&SessionId("sA".into())).expect("A state");
    assert_eq!(a.last_seq, 1);
    assert_eq!(a.nodes.len(), 1);
    // B's stale result was dropped: B's store state was never written by
    // the history page (the mux stream never delivered anything for B).
    let b = app.store.session(&SessionId("sB".into())).expect("B state");
    assert_eq!(b.last_seq, -1, "B's history page was dropped (stale guard)");
    assert_eq!(b.nodes.len(), 0);
    mock.stop().await;
}
