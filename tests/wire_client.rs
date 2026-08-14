//! Integration tests: a keyless Rust mock gateway + the WireClient.
//!
//! The mock listens on 127.0.0.1:0, serves scripted POST handlers (echoing
//! the request's rpcId) and scripted WS downlink frame sequences, and captures
//! every raw request head so the tests can assert the loopback fence
//! (`Host: 127.0.0.1:<port>`, no Origin).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot};
use tokio_tungstenite::tungstenite::Message;

use dsh_tui::client::{ClientError, WireClient};
use dsh_tui::store::SessionStore;
use dsh_tui::wire::approvals::{ApprovalRequestId, ApprovalResponseOutcome};
use dsh_tui::wire::events::HostFrame;
use dsh_tui::wire::rpc::{RpcError, RpcId};
use dsh_tui::wire::session::{Origin, PromptMode, SessionId};

// ---------------------------------------------------------------------------
// mock gateway
// ---------------------------------------------------------------------------

/// What the mock gateway does with one POSTed method.
#[derive(Clone)]
enum MockAction {
    /// HTTP 200 with this body; `{rpcId}` is substituted with the request's
    /// rpcId.
    Ok(&'static str),
    /// HTTP 404 (carrier error).
    NotFound,
    /// HTTP 400 with a non-JSON body (carrier error).
    BadJson,
    /// Accept the connection and never answer (client timeout path).
    Hang,
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    head: String,
    body: String,
}

struct MockGateway {
    port: u16,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    handlers: Arc<Mutex<HashMap<String, MockAction>>>,
    ws_frames: Arc<Mutex<HashMap<String, Vec<String>>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockGateway {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handlers = Arc::new(Mutex::new(HashMap::new()));
        let ws_frames = Arc::new(Mutex::new(HashMap::new()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task_requests = Arc::clone(&requests);
        let task_handlers = Arc::clone(&handlers);
        let task_ws_frames = Arc::clone(&ws_frames);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { continue };
                        tokio::spawn(handle_connection(
                            stream,
                            Arc::clone(&task_requests),
                            Arc::clone(&task_handlers),
                            Arc::clone(&task_ws_frames),
                        ));
                    }
                }
            }
        });
        MockGateway {
            port,
            requests: Arc::clone(&requests),
            handlers: Arc::clone(&handlers),
            ws_frames: Arc::clone(&ws_frames),
            shutdown: Some(shutdown_tx),
            task,
        }
    }

    fn port(&self) -> u16 {
        self.port
    }

    async fn set_handler(&self, method: &str, action: MockAction) {
        self.handlers
            .lock()
            .await
            .insert(method.to_string(), action);
    }

    async fn set_ws_frames(&self, path: &str, frames: Vec<String>) {
        self.ws_frames.lock().await.insert(path.to_string(), frames);
    }

    async fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().await.clone()
    }

    async fn stop(self) {
        if let Some(tx) = self.shutdown {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    handlers: Arc<Mutex<HashMap<String, MockAction>>>,
    ws_frames: Arc<Mutex<HashMap<String, Vec<String>>>>,
) {
    // Peek (non-consuming) until the request head is complete. An upgrade
    // request must be handed to `accept_async` with its bytes still in the
    // stream — a consuming pre-read would deadlock the handshake.
    let mut probe = [0u8; 4096];
    let mut head = String::new();
    for _ in 0..200 {
        let n = stream.peek(&mut probe).await.unwrap_or(0);
        if probe[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            head = String::from_utf8_lossy(&probe[..n]).into_owned();
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let path = request_path(&head);

    // WebSocket upgrade: serve the scripted frame sequence, then hold the
    // socket open until the client drops (downlink only — never send).
    if head.to_ascii_lowercase().contains("upgrade: websocket") {
        let frames = ws_frames
            .lock()
            .await
            .get(&path)
            .cloned()
            .unwrap_or_default();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        for frame in frames {
            socket.send(Message::Text(frame)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        while let Some(Ok(_)) = socket.next().await {}
        return;
    }

    // POST: read the head + body (consuming now).
    let mut buffer = Vec::new();
    let mut scratch = [0u8; 2048];
    loop {
        let n = stream.read(&mut scratch).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buffer.extend_from_slice(&scratch[..n]);
        if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 1 << 16 {
            return;
        }
    }
    let raw = String::from_utf8_lossy(&buffer);
    let head_end = raw.find("\r\n\r\n").unwrap();
    let headers: HashMap<String, String> = raw[..head_end]
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    // Read the body per Content-Length.
    let content_length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or_default();
    while buffer.len() < head_end + 4 + content_length {
        let n = stream.read(&mut scratch).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buffer.extend_from_slice(&scratch[..n]);
    }
    let body =
        String::from_utf8_lossy(&buffer[head_end + 4..head_end + 4 + content_length]).into_owned();
    // Handlers are keyed by the RPC method name (the path's last segment).
    let method = path.strip_prefix("/api/").unwrap_or(&path).to_string();
    requests.lock().await.push(CapturedRequest {
        method: method.clone(),
        path: path.clone(),
        head: head.clone(),
        body: body.clone(),
    });

    let action = handlers
        .lock()
        .await
        .get(&method)
        .cloned()
        .unwrap_or(MockAction::NotFound);
    match action {
        MockAction::Ok(template) => {
            // Echo the request's rpcId into the scripted response.
            let rpc_id = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("rpcId").cloned())
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            respond(&mut stream, 200, &template.replace("{rpcId}", &rpc_id)).await;
        }
        MockAction::NotFound => respond(&mut stream, 404, "not found").await,
        MockAction::BadJson => respond(&mut stream, 400, "{not json").await,
        MockAction::Hang => tokio::time::sleep(Duration::from_secs(60)).await,
    }
}

/// The path from a request head's request line (second whitespace token).
fn request_path(head: &str) -> String {
    head.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string()
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "X",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.shutdown().await;
}

// ---------------------------------------------------------------------------
// rpc round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_list_round_trip_and_loopback_fence() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[
                {"sessionId":"s1","updatedAt":100.0,"running":true,"blank":false,"cwd":"/work"},
                {"sessionId":"s2","updatedAt":200.0,"running":false,"blank":true,"parentSessionId":"s0","origin":"subagent"}
            ]}}}"#,
        ),
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let summaries = client.session_list().await.unwrap();
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].session_id, SessionId("s1".into()));
    assert!(summaries[0].running);
    assert_eq!(summaries[0].cwd.as_deref(), Some("/work"));
    assert_eq!(summaries[1].parent_session_id, Some(SessionId("s0".into())));
    assert_eq!(summaries[1].origin, Some(Origin::Subagent));

    // The loopback fence (api-request-trust.ts:96-123): Host header present,
    // no Origin, no sec-fetch-site.
    let requests = mock.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "session.list");
    assert_eq!(requests[0].path, "/api/session.list");
    let head = &requests[0].head;
    // hyper lowercases header names; the gateway's fence parses them
    // case-insensitively.
    let lower = head.to_ascii_lowercase();
    assert!(
        lower.contains(&format!("host: 127.0.0.1:{}", mock.port())),
        "request must carry the loopback Host header: {head}"
    );
    assert!(
        !head.to_ascii_lowercase().contains("origin"),
        "request must not carry an Origin header: {head}"
    );

    // The POST body is a client-request full form with a generated rpcId.
    let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["type"], "client-request");
    assert_eq!(body["method"], "session.list");
    assert!(body["rpcId"].as_str().unwrap().starts_with("dsh-tui-"));

    mock.stop().await;
}

#[tokio::test]
async fn business_error_maps_to_typed_rpc_error() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.prompt",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":false,"error":{"code":"internal","message":"boom","details":{}}}}"#,
        ),
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let err = client
        .session_prompt(SessionId("s1".into()), PromptMode::Queue, vec![], None)
        .await
        .unwrap_err();
    match err {
        ClientError::Rpc(RpcError::Internal { message, .. }) => assert_eq!(message, "boom"),
        other => panic!("expected Rpc(Internal), got {other:?}"),
    }
    mock.stop().await;
}

#[tokio::test]
async fn unknown_method_maps_to_http_status_404() {
    let mock = MockGateway::start().await;
    // No handler registered → the mock answers 404.
    let client = WireClient::attach(mock.port()).unwrap();
    let err = client
        .call::<_, serde_json::Value>("session.nope", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::HttpStatus(404)), "got {err:?}");
    mock.stop().await;
}

#[tokio::test]
async fn bad_json_body_maps_to_http_status_400() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.list", MockAction::BadJson).await;
    let client = WireClient::attach(mock.port()).unwrap();
    let err = client.session_list().await.unwrap_err();
    assert!(matches!(err, ClientError::HttpStatus(400)), "got {err:?}");
    mock.stop().await;
}

#[tokio::test]
async fn unanswered_rpc_maps_to_timeout() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.models", MockAction::Hang).await;
    let client = WireClient::with_timeout(mock.port(), Duration::from_millis(200)).unwrap();
    let err = client
        .session_models(SessionId("s1".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Timeout), "got {err:?}");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// mux subscribe → store (closes the spike loop: mock gateway → client → store)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mux_subscribe_feeds_session_store() {
    let mock = MockGateway::start().await;
    mock.set_ws_frames(
        "/api/events.mux",
        vec![
            r#"{"type":"server-request","rpcId":"ws-1","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s1","lastSeq":2}}"#.into(),
            r#"{"type":"server-request","rpcId":"ws-2","method":"events.mux","payload":{"type":"session/event","sessionId":"s1","event":{"type":"user/message","seq":1,"time":1.0,"data":{"id":"m1","role":"user","content":[{"type":"text","text":"hello"}],"source":{"kind":"user"}}}}}"#.into(),
            r#"{"type":"server-request","rpcId":"ws-3","method":"events.mux","payload":{"type":"session/event","sessionId":"s1","event":{"type":"assistant/message","seq":2,"time":2.0,"data":{"turn":1,"step":1,"message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"hi"}],"source":{"kind":"model","provider":"p","model":"m"}}}}}}"#.into(),
            r#"{"type":"server-request","rpcId":"ws-4","method":"events.mux","payload":{"type":"session/projection","sessionId":"s1","key":"session.list","value":{"blank":false},"seq":2}}"#.into(),
        ],
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut frames = client.mux_stream();
    let sid = SessionId("s1".into());
    let drain_sid = sid.clone();
    let drain = tokio::spawn(async move {
        let mut store = SessionStore::new();
        while let Some(frame) = frames.recv().await {
            store
                .ingest(frame)
                .expect("store must accept scripted frames");
            let done = store.session(&drain_sid).is_some_and(|state| {
                state.last_seq == 2 && state.projections.contains_key("session.list")
            });
            if done {
                break;
            }
        }
        store
    });
    let store = tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("drain must finish")
        .expect("drain task must not panic");

    let state = store.session(&sid).expect("session state must exist");
    assert_eq!(state.last_seq, 2);
    assert_eq!(state.oldest_seq, 1);
    assert_eq!(state.durable_seq, 2);
    assert_eq!(
        state
            .projections
            .get("session.list")
            .expect("projection")
            .value,
        serde_json::json!({"blank": false})
    );
    assert_eq!(state.nodes.len(), 2, "user + assistant nodes folded");
    assert_eq!(state.nodes[0].key, "m1");
    assert_eq!(state.nodes[1].key, "1:1");

    mock.stop().await;
}

#[tokio::test]
async fn ws_parse_failure_is_recorded_not_fatal() {
    let mock = MockGateway::start().await;
    mock.set_ws_frames("/api/events.mux", vec!["this is not json".into()])
        .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut frames = client.mux_stream();
    // The garbage frame must not reach the consumer, and the error must be
    // recorded on the client.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if client.last_ws_error().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "ws parse error not recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(frames.try_recv().is_err(), "garbage frame must be dropped");

    mock.stop().await;
}

// ---------------------------------------------------------------------------
// host stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn host_stream_receives_session_added() {
    let mock = MockGateway::start().await;
    mock.set_ws_frames(
        "/api/events.host",
        vec![
            r#"{"type":"server-request","rpcId":"h-1","method":"events.host","payload":{"type":"host/session-added","sessionId":"s9","blank":true}}"#.into(),
        ],
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut frames = client.host_stream();
    let frame = tokio::time::timeout(Duration::from_secs(5), frames.recv())
        .await
        .expect("host frame must arrive")
        .expect("stream must not end");
    match frame {
        HostFrame::HostSessionAdded {
            session_id, blank, ..
        } => {
            assert_eq!(session_id, SessionId("s9".into()));
            assert!(blank);
        }
        other => panic!("expected host/session-added, got {other:?}"),
    }
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// respond
// ---------------------------------------------------------------------------

#[tokio::test]
async fn respond_approval_echoes_frame_rpc_id_and_returns_receipt() {
    let mock = MockGateway::start().await;
    mock.set_handler("respond", MockAction::Ok(r#"{"accepted":true}"#))
        .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let receipt = client
        .respond_approval(
            RpcId("ws-1".into()),
            SessionId("s1".into()),
            ApprovalRequestId("a1".into()),
            ApprovalResponseOutcome::AllowedOnce,
        )
        .await
        .unwrap();
    assert!(receipt.accepted);

    // The answer is a client-response full form echoing the frame rpcId with
    // the approval payload in result.value ("rpcId echoed, never minted anew").
    let requests = mock.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "respond");
    assert_eq!(requests[0].path, "/api/respond");
    let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["type"], "client-response");
    assert_eq!(body["rpcId"], "ws-1");
    assert_eq!(body["result"]["ok"], true);
    assert_eq!(body["result"]["value"]["sessionId"], "s1");
    assert_eq!(body["result"]["value"]["approvalId"], "a1");
    assert_eq!(body["result"]["value"]["outcome"], "allowed-once");

    mock.stop().await;
}

// ---------------------------------------------------------------------------
// attach_from_env
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn attach_from_env_reads_dsh_port() {
    let mock = MockGateway::start().await;
    // SAFETY: single-threaded test and no other test reads DSH_PORT, so the
    // process-wide env mutation cannot race with a reader.
    unsafe { std::env::set_var("DSH_PORT", mock.port().to_string()) };
    let attached = WireClient::attach_from_env().unwrap();
    assert!(attached.is_some(), "DSH_PORT set → Some(client)");

    unsafe { std::env::remove_var("DSH_PORT") };
    let detached = WireClient::attach_from_env().unwrap();
    assert!(detached.is_none(), "DSH_PORT unset → None (pure client)");

    mock.stop().await;
}
