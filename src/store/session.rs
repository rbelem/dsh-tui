//! Per-session state: the event window, projections, queue snapshot, derived
//! nodes, and fold state for one session.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use serde_json::Value;

use crate::store::event_data::{EventData, EventDataError, parse_event_data};
use crate::store::node::{ChatNode, FoldState, NodeKey};
use crate::wire::events::QueueItem;
use crate::wire::session::ToolEventView;
use crate::wire::session::{SessionEvent, SessionId};

/// One applied session event: the wire envelope (raw data preserved for
/// degradation/logging), its optional tool view, and its typed data.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub event: SessionEvent,
    pub view: Option<ToolEventView>,
    pub data: EventData,
}

impl StoredEvent {
    /// Parse the typed event data. Rejects malformed known-type payloads
    /// (unknown types parse to [`EventData::Unknown`] instead).
    pub fn try_new(
        event: SessionEvent,
        view: Option<ToolEventView>,
    ) -> Result<Self, EventDataError> {
        let ignorable = event.ignorable == Some(true);
        let data = parse_event_data(&event.r#type, &event.data, ignorable)?;
        Ok(StoredEvent { event, view, data })
    }
}

/// One projection value plus its watermark seq (higher-seq-wins).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionValue {
    pub value: Value,
    pub seq: i64,
}

/// Full queue snapshot carried by `session/queue` frames (full replacement —
/// the web's queue-mirror `replace`, queue-mirror.ts:49).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueueSnapshot {
    pub items: Vec<QueueItem>,
}

/// Mutable state of one session.
#[derive(Debug)]
pub struct SessionState {
    pub session_id: SessionId,
    /// Contiguous-ish window of applied events (seq gaps tolerated).
    events: Vec<StoredEvent>,
    /// Watermark: seq of `events[0]` (-1 when the window is empty).
    pub oldest_seq: i64,
    /// Highest applied seq (-1 when empty; matches session/subscribed convention).
    pub last_seq: i64,
    /// Last durable baseline from `session/subscribed`.
    pub durable_seq: i64,
    /// Whether the head was evicted (window no longer reaches seq 0).
    pub truncated: bool,
    /// key -> {value, seq}.
    pub projections: HashMap<String, ProjectionValue>,
    /// Full snapshot from the last `session/queue` frame.
    pub queue: Option<QueueSnapshot>,
    /// Derived chat nodes in display order (rebuilt on window changes).
    pub nodes: Vec<ChatNode>,
    /// Fold state keyed by node key; survives node-list rebuilds.
    pub fold: HashMap<NodeKey, FoldState>,
    /// Reserved: session-attributable stream errors (v1 frames carry no
    /// session id; the store-level `last_stream_error` is written instead).
    pub last_stream_error: Option<String>,
    /// Window cap for this state (from the store).
    max_events: usize,
}

impl SessionState {
    pub(crate) fn new(session_id: SessionId, max_events: usize) -> Self {
        SessionState {
            session_id,
            events: Vec::new(),
            oldest_seq: -1,
            last_seq: -1,
            durable_seq: -1,
            truncated: false,
            projections: HashMap::new(),
            queue: None,
            nodes: Vec::new(),
            fold: HashMap::new(),
            last_stream_error: None,
            max_events,
        }
    }

    /// Append one event, accepted iff `seq > last_seq` (higher-seq-wins;
    /// duplicates and out-of-order lower seqs are ignored — the web's buffer
    /// and assembler tolerate them). Surface-replace events follow the same
    /// gate — the exception is their fold behavior (they target existing
    /// surface positions), not admission.
    pub(crate) fn apply_event(&mut self, stored: StoredEvent) {
        let seq = stored.event.seq;
        if seq <= self.last_seq {
            return;
        }
        if self.events.is_empty() {
            self.oldest_seq = seq;
        }
        self.events.push(stored);
        self.last_seq = seq;
        self.evict_to_cap();
    }

    /// Apply one projection frame: accepted iff `seq > existing.seq`
    /// (higher-seq-wins; projection-store.ts:134-139). No durable-baseline
    /// guard on admission: after `session/subscribed` the host re-emits
    /// projection units at watermarks at or below the baseline (the normal
    /// attach/replay flow), and stale in-flight frames self-correct when the
    /// re-emitted value lands.
    pub(crate) fn apply_projection(&mut self, key: String, value: Value, seq: i64) {
        match self.projections.entry(key) {
            Entry::Occupied(mut entry) => {
                if seq > entry.get().seq {
                    let row = entry.get_mut();
                    row.value = value;
                    row.seq = seq;
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(ProjectionValue { value, seq });
            }
        }
    }

    /// Replace the queue snapshot wholesale (full replacement, no incremental ops).
    pub(crate) fn apply_queue(&mut self, items: Vec<QueueItem>) {
        self.queue = Some(QueueSnapshot { items });
    }

    /// `session/subscribed` baseline: set `durable_seq`, drop buffered events
    /// beyond it (they will be re-sent), truncate projections whose seq is
    /// beyond it (projection-store `truncate`, projection-store.ts:171).
    pub(crate) fn truncate_to(&mut self, durable_seq: i64) {
        self.durable_seq = durable_seq;
        let before = self.events.len();
        self.events.retain(|stored| stored.event.seq <= durable_seq);
        if self.events.len() != before {
            self.oldest_seq = self.events.first().map(|s| s.event.seq).unwrap_or(-1);
            self.last_seq = self.events.iter().map(|s| s.event.seq).max().unwrap_or(-1);
        }
        self.projections.retain(|_, row| row.seq <= durable_seq);
    }

    /// Splice a history page at the head. Contiguous-window assumption:
    /// prepended seqs must be `< oldest_seq`; overlap (seqs >= oldest_seq or
    /// duplicates within the page) is dropped. The window then grows backward
    /// and the node list is rebuilt by the store.
    pub(crate) fn prepend(&mut self, events: Vec<StoredEvent>) {
        let window_min = self.oldest_seq; // -1 when the window is empty
        let mut kept: Vec<StoredEvent> = events
            .into_iter()
            .filter(|stored| self.events.is_empty() || stored.event.seq < window_min)
            .collect();
        kept.sort_by_key(|stored| stored.event.seq);
        kept.dedup_by_key(|stored| stored.event.seq);
        if kept.is_empty() {
            return;
        }
        let tail = std::mem::take(&mut self.events);
        self.events = kept.into_iter().chain(tail).collect();
        self.oldest_seq = self.events.first().map(|s| s.event.seq).unwrap_or(-1);
        self.last_seq = self.events.last().map(|s| s.event.seq).unwrap_or(-1);
    }

    /// The retained event window (read-only; the store folds it on rebuild).
    pub(crate) fn events(&self) -> &[StoredEvent] {
        &self.events
    }

    fn evict_to_cap(&mut self) {
        if self.events.len() <= self.max_events {
            return;
        }
        let excess = self.events.len() - self.max_events;
        self.events.drain(..excess);
        self.oldest_seq = self.events[0].event.seq;
        self.truncated = true;
    }
}
