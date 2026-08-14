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
use tokio::sync::{Mutex, oneshot};
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

pub struct MockGateway {
    port: u16,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    handlers: Arc<Mutex<HashMap<String, MockAction>>>,
    /// Per-session `session.history` response templates (keyed by the
    /// request payload's sessionId); falls back to the method handler map.
    history_fixtures: Arc<Mutex<HashMap<String, String>>>,
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
