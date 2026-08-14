//! Queue-item action tests (session.updateQueue): remove/steer/edit via the
//! popup's back-channel, the inline editor, placement guards, the in-flight
//! guard, and failure handling. Keyless: mock gateway + injected events.

mod common;
use common::{MockAction, MockGateway};

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use dsh_tui::app::{App, AppEvent, EventChannel};
use dsh_tui::client::WireClient;
use dsh_tui::wire::events::{
    MessageRole, MuxFrame, QueueItem, QueueMessage, QueueMessageSource, QueuePlacement,
};
use dsh_tui::wire::session::{MessageId, SessionId};

// ---------------------------------------------------------------------------
// fixture helpers
// ---------------------------------------------------------------------------

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn text_block(text: &str) -> dsh_tui::wire::session::ContentBlock {
    dsh_tui::wire::session::ContentBlock {
        r#type: "text".into(),
        extra: serde_json::Map::from_iter([(
            "text".to_string(),
            serde_json::Value::String(text.into()),
        )]),
    }
}

fn queue_item(id: &str, placement: QueuePlacement, text: &str) -> QueueItem {
    QueueItem {
        id: MessageId(id.into()),
        placement,
        message: QueueMessage {
            id: MessageId(id.into()),
            role: MessageRole::User,
            content: vec![text_block(text)],
            source: QueueMessageSource {
                kind: "composer".into(),
            },
        },
    }
}

fn queue_frame(items: Vec<QueueItem>) -> MuxFrame {
    MuxFrame::SessionQueue {
        session_id: SessionId("s1".into()),
        items,
    }
}

fn update_queue_ok() -> &'static str {
    r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"accepted":true}}}"#
}

/// An app with a client attached, an active session, and the queue popup
/// open on a queue of the given items.
fn queue_app(mock: &MockGateway, items: Vec<QueueItem>) -> App {
    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.active_session = Some(SessionId("s1".into()));
    app.store.ingest(queue_frame(items)).expect("ingest queue");
    app.queue_popup_open = true;
    app
}

/// Run the loop with the given events; returns the app after quitting.
async fn run_with(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut channel = EventChannel::new();
    for event in events {
        channel.tx.send(event).expect("event channel");
    }
    app.run(term, &mut channel)
        .await
        .expect("run must not fail");
}

/// Run the loop in a spawned task, let the back-channel land, then quit and
/// return the app (for actions whose done-event must be processed before the
/// assertions).
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

/// The captured `/api/session.updateQueue` POST bodies.
async fn update_queue_posts(mock: &MockGateway) -> Vec<serde_json::Value> {
    let requests = mock.requests().await;
    requests
        .iter()
        .filter(|request| request.path == "/api/session.updateQueue")
        .filter_map(|request| serde_json::from_str(&request.body).ok())
        .collect()
}

/// Wait until the updateQueue POSTs appear (the spawned action round-trips).
async fn wait_for_posts(mock: &MockGateway, count: usize) -> Vec<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let posts = update_queue_posts(mock).await;
        if posts.len() >= count {
            return posts;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "updateQueue POST never arrived"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

fn run_app() -> Terminal<TestBackend> {
    let backend = TestBackend::new(120, 30);
    Terminal::new(backend).unwrap()
}

// ---------------------------------------------------------------------------
// 1. remove
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn remove_posts_and_toasts() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.updateQueue", MockAction::Ok(update_queue_ok()))
        .await;
    let app = queue_app(
        &mock,
        vec![queue_item("m1", QueuePlacement::Queued, "first")],
    );
    let term = run_app();
    let mut app = run_with_settle(
        app,
        term,
        vec![AppEvent::Key(key(KeyCode::Char('x')))],
        Duration::from_millis(150),
    )
    .await;

    let posts = wait_for_posts(&mock, 1).await;
    let post = &posts[0];
    assert_eq!(post["payload"]["sessionId"], "s1");
    assert_eq!(post["payload"]["itemId"], "m1");
    assert_eq!(post["payload"]["action"]["kind"], "remove");
    // The toast fired (the done handler ran before the quit).
    assert_eq!(app.toast_text(), Some("queue item removed"));
    assert!(!app.queue_action_sending, "guard cleared");

    // The next session/queue frame reflects the change (item gone).
    app.running = true;
    let mut term = run_app();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Frame(queue_frame(vec![])),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(
        app.active_queue().is_empty(),
        "queue frame replaced the snapshot"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 2. steer
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn steer_posts_steer() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.updateQueue", MockAction::Ok(update_queue_ok()))
        .await;
    let app = queue_app(
        &mock,
        vec![queue_item("m1", QueuePlacement::Queued, "first")],
    );
    let term = run_app();
    let app = run_with_settle(
        app,
        term,
        vec![AppEvent::Key(key(KeyCode::Char('s')))],
        Duration::from_millis(150),
    )
    .await;
    let posts = wait_for_posts(&mock, 1).await;
    assert_eq!(posts[0]["payload"]["itemId"], "m1");
    assert_eq!(posts[0]["payload"]["action"]["kind"], "steer");
    assert_eq!(app.toast_text(), Some("moved to steering"));
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 3. edit
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn edit_opens_editor_commits_text() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.updateQueue", MockAction::Ok(update_queue_ok()))
        .await;
    let app = queue_app(
        &mock,
        vec![queue_item("m1", QueuePlacement::Queued, "fix the tests")],
    );
    let term = run_app();

    // e opens the editor seeded with the item's text; type + Enter commits.
    let app = run_with_settle(
        app,
        term,
        vec![
            AppEvent::Key(key(KeyCode::Char('e'))),
            AppEvent::Key(key(KeyCode::Char('!'))),
            AppEvent::Key(key(KeyCode::Enter)),
        ],
        Duration::from_millis(150),
    )
    .await;
    assert!(app.queue_editor.is_none(), "editor closed on commit");
    let posts = wait_for_posts(&mock, 1).await;
    let post = &posts[0];
    assert_eq!(post["payload"]["action"]["kind"], "edit");
    assert_eq!(post["payload"]["action"]["content"][0]["type"], "text");
    assert_eq!(
        post["payload"]["action"]["content"][0]["text"],
        "fix the tests!"
    );
    assert_eq!(app.toast_text(), Some("queue item updated"));
    assert!(app.queue_editor.is_none(), "editor closed on commit");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 4. Esc cancels edit
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn esc_cancels_edit_without_post() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.updateQueue", MockAction::Ok(update_queue_ok()))
        .await;
    let mut app = queue_app(
        &mock,
        vec![queue_item("m1", QueuePlacement::Queued, "fix the tests")],
    );
    let mut term = run_app();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('e'))),
            AppEvent::Key(key(KeyCode::Char('x'))), // typed while editing (inert nav)
            AppEvent::Key(key(KeyCode::Esc)),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    assert!(app.queue_editor.is_none(), "editor closed on Esc");
    assert!(
        update_queue_posts(&mock).await.is_empty(),
        "no POST after Esc"
    );
    // The typed 'x' while editing went into the buffer, not into a remove.
    assert!(app.queue_popup_open, "popup still open");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 5. inert on steering/context placements
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn actions_inert_on_host_owned_items() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.updateQueue", MockAction::Ok(update_queue_ok()))
        .await;
    for placement in [QueuePlacement::Steering, QueuePlacement::Context] {
        let mut app = queue_app(&mock, vec![queue_item("m1", placement, "host owned")]);
        let mut term = run_app();
        run_with(
            &mut app,
            &mut term,
            vec![
                AppEvent::Key(key(KeyCode::Char('x'))),
                AppEvent::Key(key(KeyCode::Char('s'))),
                AppEvent::Key(key(KeyCode::Char('e'))),
                AppEvent::Key(ctrl(KeyCode::Char('q'))),
            ],
        )
        .await;
        assert!(app.queue_editor.is_none(), "no editor on {placement:?}");
    }
    assert!(
        update_queue_posts(&mock).await.is_empty(),
        "no POSTs for host-owned items"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 6. in-flight guard
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn double_action_sends_one_post() {
    let mock = MockGateway::start().await;
    // A slow handler so the second key lands while the first is in flight.
    mock.set_handler(
        "session.updateQueue",
        MockAction::Delayed {
            delay_ms: 150,
            body: update_queue_ok(),
        },
    )
    .await;
    let mut app = queue_app(
        &mock,
        vec![queue_item("m1", QueuePlacement::Queued, "first")],
    );
    let mut term = run_app();
    run_with(
        &mut app,
        &mut term,
        vec![
            AppEvent::Key(key(KeyCode::Char('x'))),
            AppEvent::Key(key(KeyCode::Char('x'))),
            AppEvent::Key(ctrl(KeyCode::Char('q'))),
        ],
    )
    .await;
    wait_for_posts(&mock, 1).await;
    // Allow the in-flight action to land; only one POST total.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        update_queue_posts(&mock).await.len(),
        1,
        "second x ignored while sending"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 7. failure
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn action_failure_toasts_and_keeps_popup() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.updateQueue",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":false,"error":{"code":"internal","message":"boom","details":{}}}}"#,
        ),
    )
    .await;
    let app = queue_app(
        &mock,
        vec![queue_item("m1", QueuePlacement::Queued, "first")],
    );
    let term = run_app();
    let app = run_with_settle(
        app,
        term,
        vec![AppEvent::Key(key(KeyCode::Char('x')))],
        Duration::from_millis(150),
    )
    .await;
    wait_for_posts(&mock, 1).await;
    assert!(
        app.toast_text()
            .is_some_and(|text| text.contains("queue action failed") && text.contains("boom")),
        "failure toast: {:?}",
        app.toast_text()
    );
    assert!(app.queue_popup_open, "popup stays open after a failure");
    assert!(!app.queue_action_sending, "guard re-armed");
    mock.stop().await;
}
