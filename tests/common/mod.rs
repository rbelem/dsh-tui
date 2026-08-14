//! Shared keyless mock gateway for the integration test crates (included via
//! `mod common;` from tests/wire_client.rs and tests/app_shell.rs).
//!
//! Not every test crate uses every fixture, so dead-code warnings are
//! suppressed here (each integration test file compiles as its own crate).

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// What the mock gateway does with one POSTed method.
#[derive(Clone)]
pub enum MockAction {
    /// HTTP 200 with this body; `{rpcId}` is substituted with the request's
    /// rpcId.
    Ok(&'static str),
    /// HTTP 404 (carrier error).
    NotFound,
    /// HTTP 400 with a non-JSON body (carrier error).
    BadJson,
    /// Accept the connection and never answer (client timeout path).
    Hang,
    /// HTTP 200 after `delay_ms` (a slow gateway; the app loop must keep
    /// pumping while the request is in flight).
    Delayed { delay_ms: u64, body: &'static str },
}

#[derive(Clone, Debug)]
pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    pub head: String,
    pub body: String,
}

/// `settings.describe` fixture (the `{rpcId}` placeholder is substituted
/// like every `MockAction::Ok` template): two nav-mapped namespaces —
/// "general" (string, number, boolean, and enum fields, revision 1) and
/// "plugins" (one boolean) — plus "locale", which no v1 nav section maps.
pub const SETTINGS_DESCRIBE_OK: &str = r#"{
    "type": "server-response",
    "rpcId": "{rpcId}",
    "result": {"ok": true, "value": {
        "writable": true,
        "hasDocument": true,
        "namespaces": [
            {
                "ns": "general",
                "schema": {"type": "object", "properties": {
                    "language": {"type": "string", "title": "Language"},
                    "maxTokens": {"type": "number", "title": "Max tokens"},
                    "verbose": {"type": "boolean", "title": "Verbose logging"},
                    "logLevel": {"type": "string", "enum": ["quiet", "normal", "debug"], "title": "Log level"},
                    "metadata": {"type": "object", "title": "Metadata"}
                }},
                "value": {"language": "en", "maxTokens": 4096, "verbose": false, "logLevel": "normal", "metadata": {"a": 1}},
                "applies": "live",
                "secrets": [],
                "revision": 1
            },
            {
                "ns": "plugins",
                "schema": {"type": "object", "properties": {
                    "webSearch": {"type": "boolean", "title": "Web search"}
                }},
                "value": {"webSearch": true},
                "applies": "restart",
                "secrets": [],
                "revision": 3
            },
            {
                "ns": "locale",
                "schema": {"type": "object", "properties": {
                    "locale": {"type": "string", "title": "Locale"}
                }},
                "value": {"locale": "en"},
                "applies": "live",
                "secrets": [],
                "revision": 1
            }
        ]
    }}
}"#;

/// A `settings.update` ok response template: the namespace's new redacted
/// view (revision bumped, `value_json` spliced in verbatim). The literal
/// `{rpcId}` placeholder is substituted by `MockAction::Ok`.
pub fn settings_update_ok(ns: &str, revision: u64, value_json: serde_json::Value) -> String {
    serde_json::json!({
        "type": "server-response",
        "rpcId": "{rpcId}",
        "result": {"ok": true, "value": {
            "ns": ns,
            "schema": {"type": "object", "properties": {}},
            "value": value_json,
            "applies": "live",
            "secrets": [],
            "revision": revision,
        }},
    })
    .to_string()
}

/// A `settings.update` `settings-conflict` error template
/// (rpc.schema.ts:63 — details carry ns/expected/actual).
pub fn settings_conflict(ns: &str, expected: u64, actual: u64) -> String {
    serde_json::json!({
        "type": "server-response",
        "rpcId": "{rpcId}",
        "result": {"ok": false, "error": {
            "code": "settings-conflict",
            "message": "settings changed underneath",
            "details": {"ns": ns, "expected": expected, "actual": actual},
        }},
    })
    .to_string()
}

/// Leak a generated template into a `&'static str` for [`MockAction::Ok`]
/// (test-scoped; the process is the arena).
pub fn leaked(template: String) -> &'static str {
    Box::leak(template.into_boxed_str())
}

pub struct MockGateway {
    port: u16,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    handlers: Arc<Mutex<HashMap<String, MockAction>>>,
    /// Per-session `session.history` response templates (keyed by the
    /// request payload's sessionId); falls back to the method handler map.
    history_fixtures: Arc<Mutex<HashMap<String, String>>>,
    /// Live WS downlink pushers per path (e2e scenarios push frames to an
    /// already-connected subscriber, e.g. an approval trigger after boot).
    ws_pushers: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<String>>>>,
    ws_frames: Arc<Mutex<HashMap<String, Vec<String>>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockGateway {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handlers = Arc::new(Mutex::new(HashMap::new()));
        let ws_frames = Arc::new(Mutex::new(HashMap::new()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task_requests = Arc::clone(&requests);
        let task_handlers = Arc::clone(&handlers);
        let history_fixtures = Arc::new(Mutex::new(HashMap::new()));
        let task_history = Arc::clone(&history_fixtures);
        let ws_pushers = Arc::new(Mutex::new(HashMap::new()));
        let task_pushers = Arc::clone(&ws_pushers);
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
                            Arc::clone(&task_history),
                            Arc::clone(&task_pushers),
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
            history_fixtures,
            ws_pushers,
            ws_frames: Arc::clone(&ws_frames),
            shutdown: Some(shutdown_tx),
            task,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub async fn set_handler(&self, method: &str, action: MockAction) {
        self.handlers
            .lock()
            .await
            .insert(method.to_string(), action);
    }

    /// Serve this `session.history` response template for `session_id`
    /// (rpcId substituted like the method handlers).
    pub async fn set_history(&self, session_id: &str, template: &str) {
        self.history_fixtures
            .lock()
            .await
            .insert(session_id.to_string(), template.to_string());
    }

    pub async fn set_ws_frames(&self, path: &str, frames: Vec<String>) {
        self.ws_frames.lock().await.insert(path.to_string(), frames);
    }

    /// Push one frame to the live downlink subscriber for `path` (a no-op
    /// when nobody is connected); returns whether it was delivered.
    pub async fn push_ws_frame(&self, path: &str, frame: String) -> bool {
        let pusher = self.ws_pushers.lock().await.get(path).cloned();
        match pusher {
            Some(pusher) => pusher.send(frame).is_ok(),
            None => false,
        }
    }

    pub async fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().await.clone()
    }

    pub async fn stop(self) {
        if let Some(tx) = self.shutdown {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }

    /// The rpcIds echoed in every captured `/api/respond` body (ClientResponse
    /// full forms) — lets tests assert the respond echo contract.
    pub async fn respond_rpc_ids(&self) -> Vec<String> {
        let requests = self.requests.lock().await.clone();
        requests
            .iter()
            .filter(|request| request.path == "/api/respond")
            .filter_map(|request| {
                serde_json::from_str::<serde_json::Value>(&request.body)
                    .ok()
                    .and_then(|body| body.get("rpcId")?.as_str().map(str::to_owned))
            })
            .collect()
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    handlers: Arc<Mutex<HashMap<String, MockAction>>>,
    history_fixtures: Arc<Mutex<HashMap<String, String>>>,
    ws_pushers: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<String>>>>,
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
        // Register the pusher BEFORE serving the scripted frames: pushes sent
        // while the script is still streaming must not be lost.
        let (pusher_tx, mut pusher_rx) = mpsc::unbounded_channel();
        ws_pushers.lock().await.insert(path.clone(), pusher_tx);
        for frame in frames {
            socket.send(Message::Text(frame)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        loop {
            tokio::select! {
                pushed = pusher_rx.recv() => {
                    match pushed {
                        Some(frame) => {
                            if socket.send(Message::Text(frame)).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = socket.next() => break, // client dropped (or closed)
            }
        }
        ws_pushers.lock().await.remove(&path);
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

    // Per-session history fixtures win over the method handler map.
    if method == "session.history" {
        let session_id = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("payload")?
                    .get("sessionId")?
                    .as_str()
                    .map(str::to_owned)
            });
        if let Some(session_id) = session_id
            && let Some(template) = history_fixtures.lock().await.get(&session_id).cloned()
        {
            let rpc_id = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("rpcId").cloned())
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            respond(&mut stream, 200, &template.replace("{rpcId}", &rpc_id)).await;
            return;
        }
    }

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
        MockAction::Delayed { delay_ms, body } => {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            respond(&mut stream, 200, body).await;
        }
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
