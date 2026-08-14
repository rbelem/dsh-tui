//! The image-cache producer lane: `session.attachment` fetched lazily when
//! the render encounters a caption-only placeholder (a durable image ref
//! with no cached bytes). Drives the app shell against the mock gateway —
//! base64 payload → ImageCache → invalidate → inline filler rows.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::app::{App, AppEvent, EventChannel};
use dsh_tui::client::WireClient;
use dsh_tui::render::{ImageProtocol, picker_for};
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{AttachmentId, SessionEvent, SessionId};

use common::MockGateway;

mod common;

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

/// A user message with one durable image ref (no bytes on the wire — the
/// caption placeholder renders until the fetch populates the cache).
fn image_frame(att: &str, name: &str) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId("s1".into()),
        event: ev(
            1,
            "user/message",
            json!({
                "id": "m1", "role": "user",
                "content": [{
                    "type": "image",
                    "attachment": {
                        "attachmentId": att,
                        "mediaType": "image/png",
                        "bytes": 45000,
                        "width": 640,
                        "height": 480,
                        "name": name,
                    },
                }],
                "source": {"kind": "user"},
            }),
        ),
        view: None,
    }
}

/// A tiny deterministic PNG (4x2, half red half blue) as base64, matching
/// the wire's `data` field.
fn png_b64() -> String {
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(4, 2, |x, _| {
        if x < 2 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        }
    }))
    .write_to(&mut png, image::ImageFormat::Png)
    .expect("encode png");
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(png.into_inner())
}

fn attachment_ok(att: &str) -> String {
    format!(
        r#"{{"type":"server-response","rpcId":"{{rpcId}}","result":{{"ok":true,"value":{{"attachment":{{"attachmentId":"{att}","mediaType":"image/png","bytes":8,"width":4,"height":2}},"data":"{}"}}}}}}"#,
        png_b64()
    )
}

/// An app with a client, an active session, and a working halfblocks picker
/// (the None default renders captions only and never fetches).
fn attachment_app(mock: &MockGateway) -> App {
    let mut app = App::default();
    app.client = Some(WireClient::attach(mock.port()).unwrap());
    app.active_session = Some(SessionId("s1".into()));
    app.image_picker = picker_for(ImageProtocol::Halfblocks);
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

/// The captured `/api/session.attachment` POSTs.
async fn attachment_posts(mock: &MockGateway) -> Vec<serde_json::Value> {
    let requests = mock.requests().await;
    requests
        .iter()
        .filter(|request| request.path == "/api/session.attachment")
        .filter_map(|request| serde_json::from_str(&request.body).ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lazy_fetch_populates_the_cache() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.attachment",
        common::MockAction::Ok(leaked(attachment_ok("att-1"))),
    )
    .await;
    let app = attachment_app(&mock);

    let result = run_with_settle(
        app,
        run_app(),
        vec![AppEvent::Frame(image_frame("att-1", "plot.png"))],
        Duration::from_millis(300),
    )
    .await;

    // Exactly one POST with the session + attachment id.
    let posts = attachment_posts(&mock).await;
    assert_eq!(posts.len(), 1, "one lazy fetch for the placeholder");
    assert_eq!(posts[0]["payload"]["sessionId"], "s1");
    assert_eq!(posts[0]["payload"]["attachmentId"], "att-1");

    // The base64 payload decoded into the cache; the pending guard cleared.
    assert!(result.pending_attachments.is_empty());
    let loaded = result
        .image_cache
        .get(&AttachmentId("att-1".into()))
        .expect("decoded and cached");
    assert_eq!(loaded.source.width(), 4, "scripted png round-trips");
    assert_eq!(loaded.source.height(), 2);

    // The invalidated row cache re-rendered the inline tier: caption +
    // filler segment for the attachment.
    let rows = result.row_cache.lines();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].images.len(), 1, "inline segment after the fetch");
    assert_eq!(
        rows[0].images[0].attachment_id,
        AttachmentId("att-1".into())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dedup_while_in_flight() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.attachment",
        common::MockAction::Ok(leaked(attachment_ok("att-1"))),
    )
    .await;
    let app = attachment_app(&mock);

    // Frame + a second event before the done lands: the drain runs after
    // every event, but the pending guard must stop a second POST.
    let result = run_with_settle(
        app,
        run_app(),
        vec![
            AppEvent::Frame(image_frame("att-1", "plot.png")),
            AppEvent::Key(key(KeyCode::Char('x'))),
        ],
        Duration::from_millis(300),
    )
    .await;

    let posts = attachment_posts(&mock).await;
    assert_eq!(posts.len(), 1, "in-flight keys are never re-requested");
    assert!(result.pending_attachments.is_empty(), "guard cleared");
    assert!(
        result
            .image_cache
            .get(&AttachmentId("att-1".into()))
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failure_toasts_and_retries_on_next_encounter() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.attachment", common::MockAction::NotFound)
        .await;
    let app = attachment_app(&mock);

    let result = run_with_settle(
        app,
        run_app(),
        vec![AppEvent::Frame(image_frame("att-1", "plot.png"))],
        Duration::from_millis(300),
    )
    .await;

    // Failure: toast, no cache insert, pending cleared — the caption-only
    // row stays.
    assert!(
        result
            .toast_text()
            .is_some_and(|text| text.contains("attachment failed") && text.contains("404")),
        "failure toast: {:?}",
        result.toast_text()
    );
    assert!(
        result
            .image_cache
            .get(&AttachmentId("att-1".into()))
            .is_none()
    );
    assert!(result.pending_attachments.is_empty(), "cleared for retry");

    // The next render encounter retries: flip the handler and re-frame.
    mock.set_handler(
        "session.attachment",
        common::MockAction::Ok(leaked(attachment_ok("att-1"))),
    )
    .await;
    let app = result;
    let result = run_with_settle(
        app,
        run_app(),
        vec![AppEvent::Frame(image_frame("att-1", "plot.png"))],
        Duration::from_millis(300),
    )
    .await;
    let posts = attachment_posts(&mock).await;
    assert_eq!(posts.len(), 2, "retry after failure");
    assert!(
        result
            .image_cache
            .get(&AttachmentId("att-1".into()))
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_keys_never_fetched() {
    let mock = MockGateway::start().await;
    let app = attachment_app(&mock);

    // Pre-seed the cache directly (the producer's own output).
    let mut app = app;
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(4, 2, |x, _| {
        if x < 2 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        }
    }))
    .write_to(&mut png, image::ImageFormat::Png)
    .expect("encode png");
    app.image_cache
        .insert(
            app.image_picker.as_ref().unwrap(),
            AttachmentId("att-1".into()),
            &png.into_inner(),
        )
        .expect("seed cache");

    let result = run_with_settle(
        app,
        run_app(),
        vec![AppEvent::Frame(image_frame("att-1", "plot.png"))],
        Duration::from_millis(300),
    )
    .await;
    assert!(attachment_posts(&mock).await.is_empty(), "cached: no fetch");
    let rows = result.row_cache.lines();
    assert_eq!(rows[0].images.len(), 1, "inline tier rendered directly");
}

/// Leak a generated fixture into a `'static str` (MockAction::Ok requires
/// it; the mock substitutes `{rpcId}` at respond time).
fn leaked(template: String) -> &'static str {
    Box::leak(template.into_boxed_str())
}
