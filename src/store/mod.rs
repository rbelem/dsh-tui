//! SessionStore: the pure-Rust conversation projection mirroring the web
//! client's conversation fold.
//!
//! No terminal, no tokio, no network: pure data structures + logic. The wire
//! client lane (tokio/WS) feeds [`SessionStore::ingest`] with `MuxFrame`
//! values.
//!
//! Seq bookkeeping (mirrors the web):
//! - Events append iff `seq > last_seq` (higher-seq-wins).
//! - Projections apply iff `seq > existing.seq`; frames at or below the
//!   `session/subscribed` durable baseline are stale and ignored.
//! - `session/subscribed` sets the durable baseline: buffered events and
//!   projection rows beyond it are dropped (they will be re-sent).
//! - `session/queue` is a full snapshot replacement.
//! - The event window is capped (see [`MAX_BUFFERED_EVENTS`]); the head is
//!   evicted and the node list rebuilt. Evicted history reloads via
//!   [`SessionStore::prepend_events`].
//!
//! Deferred (v1 TODO, matching the web surface this store mirrors):
//! - command/run + command/done nodes (slash commands), manual-compaction
//!   command integration
//! - retry (llm/retry) nodes and hidden-retry semantics
//! - the turn-tail full closing card
//! - synthetic fractional anchor-seq offsets (interrupted assistant/tool
//!   placement between nodes) — v1 orders by the creating event's integer seq
//! - subagent recursion in-tree (child sessions are separate SessionState
//!   entries)
//! - the steering/claimed distinction (queue-claimed user messages render by
//!   source kind only)
//! - queue-mirror `acceptDurable` (steering rows retire once the durable
//!   user/message enters the log)
//! - tool code-dispatch subcall nesting (children/parents tree)
//! - tool/result surface-rewrite validation (a rewrite re-settles the node)

pub mod event_data;
pub mod fold;
pub mod node;
pub mod session;

pub use event_data::*;
pub use fold::fold_events;
pub use node::*;
pub use session::{ProjectionValue, QueueSnapshot, SessionState, StoredEvent};

use std::collections::HashMap;
use std::collections::HashSet;

use crate::wire::events::MuxFrame;
use crate::wire::rpc::RpcError;
use crate::wire::session::ToolEventView;
use crate::wire::session::{SessionEvent, SessionId};

/// Default per-session buffered-event window cap (web-client order of
/// magnitude; overridable via [`SessionStore::with_max_buffered_events`]).
pub const MAX_BUFFERED_EVENTS: usize = 5000;

/// Store-level failure. v1 rejects a frame only when a KNOWN event type's
/// payload is malformed (unknown types degrade to [`EventData::Unknown`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("invalid data on session event {session_id} seq {seq} of type {event_type}: {detail}")]
    InvalidEventData {
        session_id: SessionId,
        seq: i64,
        event_type: String,
        detail: String,
    },
}

/// The conversation projection: per-session state keyed by session id.
#[derive(Debug)]
pub struct SessionStore {
    sessions: HashMap<SessionId, SessionState>,
    max_events: usize,
    /// Last `stream/error` frame, formatted `code: message` (log-able).
    pub last_stream_error: Option<String>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self::with_max_buffered_events(MAX_BUFFERED_EVENTS)
    }

    /// Constructor with an overridable event-window cap (tests use a tiny cap).
    pub fn with_max_buffered_events(max: usize) -> Self {
        SessionStore {
            sessions: HashMap::new(),
            max_events: max.max(1),
            last_stream_error: None,
        }
    }

    /// The one entry point for mux frames. Handled: `session/event`,
    /// `session/subscribed`, `session/projection`, `session/queue`,
    /// `stream/error`. All other frame types are ignored silently (no v1
    /// store state — approvals/questions are answered directly by the wire
    /// lane; `session/jobs` has no TUI state yet).
    pub fn ingest(&mut self, frame: MuxFrame) -> Result<(), StoreError> {
        match frame {
            MuxFrame::SessionEvent {
                session_id,
                event,
                view,
            } => self.ingest_event(session_id, event, view),
            MuxFrame::SessionSubscribed {
                session_id,
                last_seq,
            } => {
                self.state_mut(session_id.clone()).truncate_to(last_seq);
                self.rebuild(&session_id);
                Ok(())
            }
            MuxFrame::SessionProjection {
                session_id,
                key,
                value,
                seq,
            } => {
                self.state_mut(session_id).apply_projection(key, value, seq);
                Ok(())
            }
            MuxFrame::SessionQueue { session_id, items } => {
                self.state_mut(session_id).apply_queue(items);
                Ok(())
            }
            MuxFrame::SessionJobs { session_id, jobs } => {
                self.state_mut(session_id).apply_jobs(&jobs);
                Ok(())
            }
            MuxFrame::StreamError { error } => {
                self.last_stream_error = Some(stream_error_text(&error));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub fn session(&self, session_id: &SessionId) -> Option<&SessionState> {
        self.sessions.get(session_id)
    }

    /// Ensure a session state exists (history-first attach: the history page
    /// may arrive before any `session/subscribed` frame).
    pub fn open_session(&mut self, session_id: SessionId) {
        let max_events = self.max_events;
        self.sessions
            .entry(session_id.clone())
            .or_insert_with(|| SessionState::new(session_id, max_events));
    }

    /// Ingest a `session.history` page: entries are `(event, optional tool
    /// view)` pairs, NOT mux frames. Same seq bookkeeping as `session/event`
    /// ingest (`seq > last_seq` wins); each entry's view pairs with its event
    /// so the fold can attach the tool/result view (the same `StoredEvent`
    /// carrier the mux path uses).
    pub fn ingest_history(
        &mut self,
        session_id: &SessionId,
        entries: Vec<(SessionEvent, Option<ToolEventView>)>,
    ) -> Result<(), StoreError> {
        {
            let max_events = self.max_events;
            let state = self
                .sessions
                .entry(session_id.clone())
                .or_insert_with(|| SessionState::new(session_id.clone(), max_events));
            for (event, view) in entries {
                let seq = event.seq;
                let event_type = event.r#type.clone();
                let stored = StoredEvent::try_new(event, view).map_err(|error| {
                    StoreError::InvalidEventData {
                        session_id: session_id.clone(),
                        seq,
                        event_type,
                        detail: error.0,
                    }
                })?;
                state.apply_event(stored);
            }
        }
        self.rebuild(session_id);
        Ok(())
    }

    pub fn session_mut(&mut self, session_id: &SessionId) -> Option<&mut SessionState> {
        self.sessions.get_mut(session_id)
    }

    /// All sessions, in arbitrary order (the sidebar iterates this).
    pub fn sessions(&self) -> impl Iterator<Item = &SessionState> {
        self.sessions.values()
    }

    /// Effective fold state for a node key: an explicit [`SessionStore::set_fold`]
    /// override wins; otherwise the node-kind default (see
    /// [`FoldState::default_for`]). Unknown sessions/keys default to expanded.
    pub fn fold_state(&self, session_id: &SessionId, node_key: &str) -> FoldState {
        let Some(state) = self.sessions.get(session_id) else {
            return FoldState::default();
        };
        if let Some(fold) = state.fold.get(node_key) {
            return *fold;
        }
        state
            .nodes
            .iter()
            .find(|node| node.key == node_key)
            .map(FoldState::default_for)
            .unwrap_or_default()
    }

    /// Set the fold state for a node key (survives node-list rebuilds).
    pub fn set_fold(&mut self, session_id: &SessionId, node_key: &str, state: FoldState) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.fold.insert(node_key.to_string(), state);
        }
    }

    /// Splice a history page at the head of the window (see
    /// [`SessionState`] prepend semantics) and rebuild the node list.
    pub fn prepend_events(&mut self, session_id: &SessionId, events: Vec<StoredEvent>) {
        if let Some(state) = self.sessions.get_mut(session_id) {
            state.prepend(events);
            self.rebuild(session_id);
        }
    }

    fn ingest_event(
        &mut self,
        session_id: SessionId,
        event: SessionEvent,
        view: Option<ToolEventView>,
    ) -> Result<(), StoreError> {
        let seq = event.seq;
        let event_type = event.r#type.clone();
        let stored =
            StoredEvent::try_new(event, view).map_err(|error| StoreError::InvalidEventData {
                session_id: session_id.clone(),
                seq,
                event_type,
                detail: error.0,
            })?;
        self.state_mut(session_id.clone()).apply_event(stored);
        self.rebuild(&session_id);
        Ok(())
    }

    fn state_mut(&mut self, session_id: SessionId) -> &mut SessionState {
        let max_events = self.max_events;
        self.sessions
            .entry(session_id.clone())
            .or_insert_with(|| SessionState::new(session_id, max_events))
    }

    /// Re-run the pure fold over the retained window and prune fold state to
    /// surviving keys (fold state for surviving keys is preserved).
    fn rebuild(&mut self, session_id: &SessionId) {
        let Some(state) = self.sessions.get_mut(session_id) else {
            return;
        };
        state.nodes = fold_events(state.events());
        let alive: HashSet<&str> = state.nodes.iter().map(|node| node.key.as_str()).collect();
        state.fold.retain(|key, _| alive.contains(key.as_str()));
    }
}
/// Aggregate session metrics for the stats bar (#39) and the context meter
/// (#38), as a pure function of one session's derived state:
/// - turns: distinct `turn`s among assistant / turn-error / turn-max-tokens
///   nodes (turns with model activity — a prompt-only turn has no node);
/// - steps: distinct `(turn, step)` pairs across assistant AND tool nodes;
/// - tokens: usage summed across assistant nodes (`cache_*` are optional on
///   the wire; absent counts as 0);
/// - timing: derived from the event window's envelope `time` — LLM duration
///   = Σ(TurnEnd.time − TurnStart.time), TTFT = mean over turns of
///   (first AssistantChunk.time − TurnStart.time), tool time = Σ
///   (result_time − call_time) per settled tool node. A metric stays None /
///   0-measurable until BOTH of its events are present in the window — a
///   head-evicted start hides the metric, never fabricates a duration;
/// - `context_window`: the LAST `request/context` event's `contextWindow`
///   in the retained event window — the fold ignores that event, but the
///   window still holds it (head-eviction only drops the OLDEST events, so
///   the newest window wins). `None` when no context was ever reported.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionStats {
    pub turns: u64,
    pub steps: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub context_window: Option<i64>,
    /// Summed real LLM duration over turns with BOTH events in-window.
    pub llm_seconds: f64,
    /// Turns whose LLM duration was measurable (an in-window start+end).
    pub measured_turns: u32,
    /// Mean first-token latency over in-window turns (None when none).
    pub ttft_seconds: Option<f64>,
    /// Σ (result_time − call_time) over settled tool nodes with both times.
    pub tool_seconds: f64,
    /// Tool nodes whose duration was measurable (both times present).
    pub measured_tools: u32,
}

pub fn session_stats(state: &SessionState) -> SessionStats {
    let mut stats = SessionStats::default();
    let mut turns: HashSet<i64> = HashSet::new();
    let mut steps: HashSet<(i64, i64)> = HashSet::new();
    for node in &state.nodes {
        match &node.data {
            NodeData::Assistant {
                turn, step, usage, ..
            } => {
                turns.insert(*turn);
                steps.insert((*turn, *step));
                if let Some(usage) = usage {
                    stats.input_tokens += usage.input_tokens;
                    stats.output_tokens += usage.output_tokens;
                    stats.cache_read_tokens += usage.cache_read_tokens.unwrap_or(0);
                    stats.cache_write_tokens += usage.cache_write_tokens.unwrap_or(0);
                }
            }
            NodeData::TurnError { turn, .. } | NodeData::TurnMaxTokens { turn } => {
                turns.insert(*turn);
            }
            NodeData::Tool {
                call: Some(call), ..
            } => {
                steps.insert((call.turn, call.step));
            }
            _ => {}
        }
    }
    stats.turns = turns.len() as u64;
    stats.steps = steps.len() as u64;
    // Tool durations: sum (result_time − call_time) over settled tools.
    for node in &state.nodes {
        if let NodeData::Tool {
            result: Some(result),
            ..
        } = &node.data
            && let (Some(start), Some(end)) = (result.call_time, result.result_time)
        {
            stats.tool_seconds += (end - start).max(0.0);
            stats.measured_tools += 1;
        }
    }
    // Per-turn timing: the events are sequential (one open turn at a time),
    // so a single open-turn cursor suffices. A turn whose start, first
    // chunk, or end fell outside the retained window contributes nothing.
    let mut open_turn: Option<(i64, f64)> = None; // (turn, TurnStart.time)
    let mut first_chunk: Option<f64> = None;
    let mut ttft_sum = 0.0f64;
    let mut ttft_turns = 0u64;
    for stored in state.events() {
        let time = stored.event.time;
        match &stored.data {
            EventData::TurnStart { turn } => {
                open_turn = Some((*turn, time));
                first_chunk = None;
            }
            EventData::AssistantChunk { turn, .. } => {
                if let Some((open, _)) = open_turn
                    && open == *turn
                    && first_chunk.is_none()
                {
                    first_chunk = Some(time);
                }
            }
            EventData::TurnEnd { turn, .. } => {
                if let Some((open, start)) = open_turn
                    && open == *turn
                {
                    stats.llm_seconds += (time - start).max(0.0);
                    stats.measured_turns += 1;
                    if let Some(chunk) = first_chunk {
                        ttft_sum += (chunk - start).max(0.0);
                        ttft_turns += 1;
                    }
                }
                open_turn = None;
                first_chunk = None;
            }
            _ => {}
        }
    }
    if ttft_turns > 0 {
        stats.ttft_seconds = Some(ttft_sum / ttft_turns as f64);
    }
    for stored in state.events().iter().rev() {
        if let EventData::RequestContext { context_window, .. } = &stored.data {
            stats.context_window = *context_window;
            break;
        }
    }
    stats
}

/// Tokens per second over the measurable LLM duration (#39): 0 guards the
/// div-by-zero. `None` when no model call was measured (older sessions /
/// evicted event windows — the stats bar omits the segment).
pub fn tokens_per_second(stats: &SessionStats) -> Option<f64> {
    if stats.llm_seconds > 0.0 && stats.output_tokens > 0 {
        Some(stats.output_tokens as f64 / stats.llm_seconds)
    } else {
        None
    }
}

/// One log line per RpcError branch (code + message; details are irrelevant).
fn stream_error_text(error: &RpcError) -> String {
    let code = match error {
        RpcError::BadRequest { .. } => "bad-request",
        RpcError::Cancelled { .. } => "cancelled",
        RpcError::SessionNotFound { .. } => "session-not-found",
        RpcError::ModelUnavailable { .. } => "model-unavailable",
        RpcError::SessionConflict { .. } => "session-conflict",
        RpcError::InvalidTimeZone { .. } => "invalid-time-zone",
        RpcError::WorkspaceAttachFailed { .. } => "workspace-attach-failed",
        RpcError::WorkspaceNotFound { .. } => "workspace-not-found",
        RpcError::WorkspaceInvalidPath { .. } => "workspace-invalid-path",
        RpcError::WorkspaceNameConflict { .. } => "workspace-name-conflict",
        RpcError::WorkspaceMoveInvalid { .. } => "workspace-move-invalid",
        RpcError::DirectoryUnreadable { .. } => "directory-unreadable",
        RpcError::DirectoryExists { .. } => "directory-exists",
        RpcError::DirectoryCreateFailed { .. } => "directory-create-failed",
        RpcError::DirectoryPickerUnavailable { .. } => "directory-picker-unavailable",
        RpcError::AgentPresetReadOnly { .. } => "agent-preset-read-only",
        RpcError::AgentPresetLocked { .. } => "agent-preset-locked",
        RpcError::AgentPresetConflict { .. } => "agent-preset-conflict",
        RpcError::AgentPresetNotFound { .. } => "agent-preset-not-found",
        RpcError::AgentPresetInvalid { .. } => "agent-preset-invalid",
        RpcError::AgentBusy { .. } => "agent-busy",
        RpcError::AttachmentError { .. } => "attachment-error",
        RpcError::QueueItemNotFound { .. } => "queue-item-not-found",
        RpcError::SteerUnavailable { .. } => "steer-unavailable",
        RpcError::CommandError { .. } => "command-error",
        RpcError::UnknownCommand { .. } => "unknown-command",
        RpcError::SettingsRejected { .. } => "settings-rejected",
        RpcError::SettingsNotExposed { .. } => "settings-not-exposed",
        RpcError::SettingsConflict { .. } => "settings-conflict",
        RpcError::CredentialRejected { .. } => "credential-rejected",
        RpcError::ModelDiscoveryFailed { .. } => "model-discovery-failed",
        RpcError::TitleInvalid { .. } => "title-invalid",
        RpcError::ForkUnavailable { .. } => "fork-unavailable",
        RpcError::SubagentParentUnavailable { .. } => "subagent-parent-unavailable",
        RpcError::SubagentNotFound { .. } => "subagent-not-found",
        RpcError::SubagentCatalogDiagnostic { .. } => "subagent-catalog-diagnostic",
        RpcError::SubagentNotResumable { .. } => "subagent-not-resumable",
        RpcError::SubagentUnauthorized { .. } => "subagent-unauthorized",
        RpcError::SubagentDeliveryUnavailable { .. } => "subagent-delivery-unavailable",
        RpcError::Internal { .. } => "internal",
    };
    let message = match error {
        RpcError::BadRequest { message, .. } => message,
        RpcError::Cancelled { message, .. } => message,
        RpcError::SessionNotFound { message, .. } => message,
        RpcError::ModelUnavailable { message, .. } => message,
        RpcError::SessionConflict { message, .. } => message,
        RpcError::InvalidTimeZone { message, .. } => message,
        RpcError::WorkspaceAttachFailed { message, .. } => message,
        RpcError::WorkspaceNotFound { message, .. } => message,
        RpcError::WorkspaceInvalidPath { message, .. } => message,
        RpcError::WorkspaceNameConflict { message, .. } => message,
        RpcError::WorkspaceMoveInvalid { message, .. } => message,
        RpcError::DirectoryUnreadable { message, .. } => message,
        RpcError::DirectoryExists { message, .. } => message,
        RpcError::DirectoryCreateFailed { message, .. } => message,
        RpcError::DirectoryPickerUnavailable { message, .. } => message,
        RpcError::AgentPresetReadOnly { message, .. } => message,
        RpcError::AgentPresetLocked { message, .. } => message,
        RpcError::AgentPresetConflict { message, .. } => message,
        RpcError::AgentPresetNotFound { message, .. } => message,
        RpcError::AgentPresetInvalid { message, .. } => message,
        RpcError::AgentBusy { message, .. } => message,
        RpcError::AttachmentError { message, .. } => message,
        RpcError::QueueItemNotFound { message, .. } => message,
        RpcError::SteerUnavailable { message, .. } => message,
        RpcError::CommandError { message, .. } => message,
        RpcError::UnknownCommand { message, .. } => message,
        RpcError::SettingsRejected { message, .. } => message,
        RpcError::SettingsNotExposed { message, .. } => message,
        RpcError::SettingsConflict { message, .. } => message,
        RpcError::CredentialRejected { message, .. } => message,
        RpcError::ModelDiscoveryFailed { message, .. } => message,
        RpcError::TitleInvalid { message, .. } => message,
        RpcError::ForkUnavailable { message, .. } => message,
        RpcError::SubagentParentUnavailable { message, .. } => message,
        RpcError::SubagentNotFound { message, .. } => message,
        RpcError::SubagentCatalogDiagnostic { message, .. } => message,
        RpcError::SubagentNotResumable { message, .. } => message,
        RpcError::SubagentUnauthorized { message, .. } => message,
        RpcError::SubagentDeliveryUnavailable { message, .. } => message,
        RpcError::Internal { message, .. } => message,
    };
    format!("{code}: {message}")
}
