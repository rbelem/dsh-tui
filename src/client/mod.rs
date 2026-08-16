//! The chat-loop wire client: drives the deepseek-harness gateway exactly
//! like the web client (one WS subscribe, attach mode).
//!
//! Wire contract (`.scratch/dsh-tui/issues/01-wire-protocol-surface.md`,
//! `packages/host/apiproxy/src/api/rpc.schema.ts`):
//! - HTTP: every method is `POST /api/<method>` with a ClientRequest full
//!   form body; the response body is a ServerResponse full form. Business
//!   errors arrive as HTTP 200 with `ok:false`; carrier errors are
//!   404/415/400/500.
//! - Loopback fence (api-request-trust.ts:96-123): requests carry
//!   `Host: 127.0.0.1:<port>` and no Origin / sec-fetch-site. reqwest derives
//!   the Host header from the loopback URL, so a spawned local client passes
//!   the fence as-is.
//! - WS downlinks `/api/events.mux` and `/api/events.host`: text frames that
//!   are ServerRequest full forms whose payload is a MuxFrame/HostFrame.
//!   Downlink-only: this client never sends on those sockets.
//! - `/api/respond`: ClientResponse full form; the response body is an
//!   RpcReceipt. The ClientResponse rpcId MUST echo the answerable frame's
//!   envelope rpcId ("rpcId echoed, never minted anew" — rpc.ts:178).
//!
//! The client does NOT touch the SessionStore: the consumer feeds
//! [`SessionStore::ingest`] from the mux stream.

pub mod rpc;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::wire::events::{HostFrame, MuxFrame};
use crate::wire::rpc::{
    ClientRequest, ClientRequestType, ClientResponse, ClientResponseType, RpcError, RpcId,
    RpcReceipt, RpcResult, ServerRequest, ServerResponse,
};

/// Default per-request timeout.
pub const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// WS downlink reconnect backoff bounds (attach-mode resilience: the gateway
/// may be restarting).
const WS_BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const WS_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Transport-level failure of the wire client.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("gateway returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("rpc timed out")]
    Timeout,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("rpc failed: {0:?}")]
    Rpc(RpcError),
}

/// Shared client state: the handle and the downlink subscriber tasks all hold
/// an `Arc` of this.
struct Inner {
    port: u16,
    /// `http://127.0.0.1:{port}`
    base: String,
    /// `ws://127.0.0.1:{port}`
    ws_base: String,
    http: reqwest::Client,
    /// rpcId → response slot for in-flight requests (correlation table).
    pending: tokio::sync::Mutex<HashMap<RpcId, oneshot::Sender<ServerResponse>>>,
    rpc_timeout: Duration,
    /// Last WS downlink failure (connect/parse/read), log-able.
    last_ws_error: Mutex<Option<String>>,
    /// Set on `WireClient::drop`; stops the downlink tasks.
    stop: AtomicBool,
    next_seq: AtomicU64,
}

/// The chat-loop wire client. Attach mode: connects to whatever serves the
/// resolved port on the loopback (the gateway lifecycle lives in
/// [`crate::gateway`] — this client never boots anything).
#[derive(Clone)]
pub struct WireClient {
    inner: Arc<Inner>,
}

impl Drop for WireClient {
    fn drop(&mut self) {
        // Only the LAST clone stops the downlink tasks: transient clones
        // held by spawned answer/prompt tasks drop when those tasks finish,
        // and must not tear the subscription down mid-session.
        if Arc::strong_count(&self.inner) == 1 {
            self.inner.stop.store(true, Ordering::Relaxed);
        }
    }
}

impl WireClient {
    /// Connect to a running gateway on the loopback port. Probes that
    /// something is listening (the gateway itself is never started).
    pub fn attach(port: u16) -> Result<Self, ClientError> {
        Self::with_timeout(port, DEFAULT_RPC_TIMEOUT)
    }

    /// [`WireClient::attach`] with a custom per-request timeout.
    pub fn with_timeout(port: u16, rpc_timeout: Duration) -> Result<Self, ClientError> {
        // Loopback probe (blocking, ~instant against a dead loopback port).
        std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
            ClientError::Transport(format!("gateway not reachable on 127.0.0.1:{port}: {e}"))
        })?;
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(WireClient {
            inner: Arc::new(Inner {
                port,
                base: format!("http://127.0.0.1:{port}"),
                ws_base: format!("ws://127.0.0.1:{port}"),
                http,
                pending: tokio::sync::Mutex::new(HashMap::new()),
                rpc_timeout,
                last_ws_error: Mutex::new(None),
                stop: AtomicBool::new(false),
                next_seq: AtomicU64::new(0),
            }),
        })
    }

    /// Read `DSH_PORT` and attach. `None` when the variable is unset — a pure
    /// client never boots anything (the gateway lifecycle is
    /// [`crate::gateway`]'s job).
    pub fn attach_from_env() -> Result<Option<Self>, ClientError> {
        match std::env::var("DSH_PORT") {
            Ok(port) => {
                let port: u16 = port.parse().map_err(|e| {
                    ClientError::Protocol(format!("invalid DSH_PORT `{port}`: {e}"))
                })?;
                Ok(Some(Self::attach(port)?))
            }
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(ClientError::Protocol(
                "DSH_PORT is not valid unicode".into(),
            )),
        }
    }

    /// Generic POST dispatch: sends a ClientRequest, correlates the
    /// ServerResponse by rpcId through the pending table, and parses
    /// `result.value` as `R`.
    ///
    /// Error mapping: `ok:false` → [`ClientError::Rpc`]; carrier status
    /// (404/415/400/500) → [`ClientError::HttpStatus`]; no response within
    /// the timeout → [`ClientError::Timeout`]; connection failure →
    /// [`ClientError::Transport`].
    pub async fn call<P, R>(&self, method: &str, payload: P) -> Result<R, ClientError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let request = ClientRequest {
            r#type: ClientRequestType::ClientRequest,
            rpc_id: self.next_rpc_id(),
            method: method.to_string(),
            payload: serde_json::to_value(payload)
                .map_err(|e| ClientError::Protocol(format!("payload serialization: {e}")))?,
        };
        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .await
            .insert(request.rpc_id.clone(), tx);

        // The POST completes synchronously (HTTP request/response), but the
        // response is delivered through the pending slot so rpcId correlation
        // is enforced in exactly one place.
        let outcome = tokio::time::timeout(self.inner.rpc_timeout, self.post(&request)).await;
        self.inner.pending.lock().await.remove(&request.rpc_id);

        match outcome {
            Err(_) => Err(ClientError::Timeout),
            Ok(Err(error)) => Err(error),
            Ok(Ok(())) => {
                let response = rx
                    .await
                    .map_err(|_| ClientError::Protocol("response slot closed".into()))?;
                Self::into_result::<R>(response, method)
            }
        }
    }

    /// POST a ClientResponse answer to `/api/respond` (approval/question
    /// answers). The rpcId MUST echo the answerable frame's envelope rpcId —
    /// "rpcId echoed, never minted anew" (rpc.ts:178). The response body is an
    /// RpcReceipt, not a ServerResponse.
    pub async fn respond(
        &self,
        rpc_id: RpcId,
        payload: serde_json::Value,
    ) -> Result<RpcReceipt, ClientError> {
        let message = ClientResponse {
            r#type: ClientResponseType::ClientResponse,
            rpc_id,
            result: RpcResult {
                ok: true,
                value: Some(payload),
                error: None,
            },
        };
        let url = format!("{}/api/respond", self.inner.base);
        let outcome = tokio::time::timeout(self.inner.rpc_timeout, async {
            let response = self
                .inner
                .http
                .post(&url)
                .json(&message)
                .send()
                .await
                .map_err(|e| ClientError::Transport(e.to_string()))?;
            if !response.status().is_success() {
                return Err(ClientError::HttpStatus(response.status().as_u16()));
            }
            response
                .json()
                .await
                .map_err(|e| ClientError::Protocol(format!("invalid receipt body: {e}")))
        })
        .await;
        match outcome {
            Err(_) => Err(ClientError::Timeout),
            Ok(result) => result,
        }
    }

    /// Subscribe to the mux downlink (`/api/events.mux`). Spawns a subscriber
    /// task on first call that reconnects with capped backoff while the
    /// gateway restarts; each yielded value is the ServerRequest payload
    /// parsed into a [`MuxFrame`] paired with the envelope's rpcId — the echo
    /// target for answerable frames (approval/question requested), a fresh
    /// push id otherwise ("rpcId echoed, never minted anew", rpc.ts:178).
    /// Downlink-only: nothing is ever sent on the socket. Drop the receiver
    /// to stop the subscriber. Call once per consumer.
    pub fn mux_stream(&self) -> mpsc::UnboundedReceiver<DownlinkFrame<MuxFrame>> {
        spawn_downlink(&self.inner, "/api/events.mux")
    }

    /// Subscribe to the host downlink (`/api/events.host`); see
    /// [`WireClient::mux_stream`].
    pub fn host_stream(&self) -> mpsc::UnboundedReceiver<DownlinkFrame<HostFrame>> {
        spawn_downlink(&self.inner, "/api/events.host")
    }

    /// The gateway loopback port this client is attached to.
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// Last WS downlink failure (connect/parse/read), for the status line.
    pub fn last_ws_error(&self) -> Option<String> {
        self.inner
            .last_ws_error
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn next_rpc_id(&self) -> RpcId {
        let seq = self.inner.next_seq.fetch_add(1, Ordering::Relaxed);
        let micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or_default();
        RpcId(format!("dsh-tui-{micros}-{seq}"))
    }

    /// The POST leg of [`WireClient::call`]: send, check the carrier status,
    /// validate the rpcId echo, and deliver the ServerResponse into the
    /// pending slot.
    async fn post(&self, request: &ClientRequest) -> Result<(), ClientError> {
        let url = format!("{}/api/{}", self.inner.base, request.method);
        let response = self
            .inner
            .http
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        if !response.status().is_success() {
            return Err(ClientError::HttpStatus(response.status().as_u16()));
        }
        let server_response: ServerResponse = response
            .json()
            .await
            .map_err(|e| ClientError::Protocol(format!("invalid server-response body: {e}")))?;
        if server_response.rpc_id != request.rpc_id {
            return Err(ClientError::Protocol(format!(
                "rpcId mismatch: expected {}, got {}",
                request.rpc_id, server_response.rpc_id
            )));
        }
        // Remove the slot and deliver; a caller that already timed out
        // dropped its receiver, so a failed send is fine.
        if let Some(slot) = self.inner.pending.lock().await.remove(&request.rpc_id) {
            let _ = slot.send(server_response);
        }
        Ok(())
    }

    fn into_result<R: DeserializeOwned>(
        response: ServerResponse,
        method: &str,
    ) -> Result<R, ClientError> {
        match response.result {
            RpcResult {
                ok: true, value, ..
            } => {
                let value = value.unwrap_or(serde_json::Value::Null);
                serde_json::from_value(value).map_err(|e| {
                    ClientError::Protocol(format!("invalid result value for {method}: {e}"))
                })
            }
            RpcResult {
                ok: false,
                error: Some(error),
                ..
            } => Err(ClientError::Rpc(error)),
            RpcResult {
                ok: false,
                error: None,
                ..
            } => Err(ClientError::Protocol(format!(
                "ok:false without error for {method}"
            ))),
        }
    }
}

/// A downlink frame paired with its ServerRequest envelope's rpcId.
#[derive(Debug, Clone, PartialEq)]
pub struct DownlinkFrame<F> {
    /// The envelope rpcId: for answerable frames this is the value the
    /// answering ClientResponse must echo (rpc.ts:178); for pure pushes it
    /// identifies that one push.
    pub rpc_id: RpcId,
    pub frame: F,
}

/// A frame parsed from a downlink ServerRequest payload slot.
trait DownlinkPayload: Sized + Send + 'static {
    fn from_server_request(request: ServerRequest) -> Result<Self, String>;
}

impl DownlinkPayload for MuxFrame {
    fn from_server_request(request: ServerRequest) -> Result<Self, String> {
        request
            .into_mux_frame()
            .map_err(|e| format!("invalid mux frame: {e}"))
    }
}

impl DownlinkPayload for HostFrame {
    fn from_server_request(request: ServerRequest) -> Result<Self, String> {
        request
            .into_host_frame()
            .map_err(|e| format!("invalid host frame: {e}"))
    }
}

/// Spawn one downlink subscriber task (mux or host) and hand back its frame
/// receiver. Reconnects on socket drop with capped backoff (the gateway may
/// be restarting); stops when the client is dropped or the receiver is.
fn spawn_downlink<F: DownlinkPayload>(
    inner: &Arc<Inner>,
    path: &str,
) -> mpsc::UnboundedReceiver<DownlinkFrame<F>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let inner = Arc::clone(inner);
    let url = format!("{}{}", inner.ws_base, path);
    tokio::spawn(async move {
        let mut backoff = WS_BACKOFF_INITIAL;
        while !inner.stop.load(Ordering::Relaxed) {
            match tokio_tungstenite::connect_async(&url).await {
                Ok((mut socket, _)) => {
                    backoff = WS_BACKOFF_INITIAL;
                    while !inner.stop.load(Ordering::Relaxed) {
                        match socket.next().await {
                            Some(Ok(Message::Text(text))) => {
                                let parsed = serde_json::from_str::<ServerRequest>(&text)
                                    .map_err(|e| format!("invalid server-request envelope: {e}"))
                                    .and_then(|request| {
                                        // Capture the envelope rpcId BEFORE the
                                        // payload parse consumes the request.
                                        let rpc_id = request.rpc_id.clone();
                                        F::from_server_request(request)
                                            .map(|frame| DownlinkFrame { rpc_id, frame })
                                    });
                                match parsed {
                                    Ok(downlink) => {
                                        // Downlink-only socket: never send back.
                                        if tx.send(downlink).is_err() {
                                            return; // consumer gone — stop
                                        }
                                    }
                                    Err(error) => record_ws_error(&inner, error),
                                }
                            }
                            // Text frames carry the protocol; anything else on
                            // a downlink socket is ignorable.
                            Some(Ok(_)) => {}
                            Some(Err(error)) => {
                                record_ws_error(&inner, format!("ws read error: {error}"));
                                break;
                            }
                            None => break, // socket closed (gateway restart)
                        }
                    }
                }
                Err(error) => record_ws_error(&inner, format!("ws connect error: {error}")),
            }
            tokio::time::sleep(backoff).await;
            backoff = std::cmp::min(backoff.saturating_mul(2), WS_BACKOFF_MAX);
        }
    });
    rx
}

fn record_ws_error(inner: &Arc<Inner>, error: String) {
    if let Ok(mut guard) = inner.last_ws_error.lock() {
        *guard = Some(error);
    }
}
