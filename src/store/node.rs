//! Chat node types — the derived conversation tree.
//!
//! Mirrors the web's `conversation-nodes` v1 subset (assistant.ts, tool.ts,
//! message.ts, compaction.ts, turn-error.ts, turn-max-tokens.ts, fallback.ts).

use serde_json::Value;

use crate::store::event_data::{ContentBlock, TokenUsage, ToolErrorIdentity};
use crate::wire::session::ToolEventView;

/// Stable identity of a chat node. Survives node-list rebuilds — the fold
/// state map is keyed by it. User nodes: message id; assistant nodes:
/// `"turn:step"`; tool nodes: call id; compaction nodes: compaction id;
/// turn notices and unknown rows: synthetic `"turn-error:{n}"` / `"unknown:{seq}"`.
pub type NodeKey = String;

/// The chat node kinds this store derives (web ChatNodeKind, v1 subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatNodeKind {
    User,
    Assistant,
    Tool,
    Compaction,
    TurnError,
    TurnMaxTokens,
    Unknown,
}

/// User-node classification (message.ts messageDefinition). The `Steering`
/// variant (queue-claimed user message) is a v1 TODO — v1 renders by
/// `source.kind` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserNodeKind {
    User,
    Steering,
    Context,
}

/// Display block of an assistant node (web AssistantBlock): streamed text,
/// reasoning, or tool-call scaffolding.
#[derive(Debug, Clone, PartialEq)]
pub enum AssistantBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        args_raw: String,
    },
}

impl AssistantBlock {
    /// Text/reasoning blocks with non-empty trimmed content.
    pub fn has_visible_text(&self) -> bool {
        match self {
            AssistantBlock::Text { text } | AssistantBlock::Reasoning { text } => {
                !text.trim().is_empty()
            }
            AssistantBlock::ToolCall { .. } => false,
        }
    }
}

/// The originating call of a tool node, captured at `tool/call` time.
#[derive(Debug, Clone, PartialEq)]
pub struct RunningToolCall {
    pub call_id: String,
    pub name: String,
    /// Raw JSON arguments string exactly as the model produced it.
    pub args_raw: String,
    pub turn: i64,
    pub step: i64,
    pub time: f64,
    /// The frame's `view` when it targeted `for: 'call'`.
    pub call_view: Option<ToolEventView>,
}

/// Call identity backfilled onto a settled result.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallBackfill {
    pub name: String,
    pub args_raw: String,
}

/// The settled state of a tool node (web ToolResultNode, v1 subset).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResultNode {
    pub call_id: String,
    /// `Some` when the `tool/call` event is in-window; `None` on a window cut.
    pub call: Option<ToolCallBackfill>,
    pub call_time: Option<f64>,
    /// The result event's wall-clock time (#37): `call_time` → `result_time`
    /// is the tool's duration. `None` on a synthesized (interrupted) result —
    /// there is no real result event, so no duration is reported.
    pub result_time: Option<f64>,
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    pub error: Option<ToolErrorIdentity>,
    pub meta: Option<Value>,
    pub call_view: Option<ToolEventView>,
    /// The frame's `view` when it targeted `for: 'result'`.
    pub result_view: Option<ToolEventView>,
}

/// Per-kind node payload.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeData {
    User {
        kind: UserNodeKind,
        message_id: String,
        content: Vec<ContentBlock>,
        source: Value,
    },
    Assistant {
        turn: i64,
        step: i64,
        blocks: Vec<AssistantBlock>,
        usage: Option<TokenUsage>,
        /// `assistant/message` finalize seen (with non-empty content).
        finalized: bool,
        /// Boundary closed without finalize, with interruption evidence.
        interrupted: bool,
    },
    Tool {
        call: Option<RunningToolCall>,
        /// Boxed: [`ToolResultNode`] is the largest payload and would bloat
        /// the enum past clippy's `large_enum_variant` threshold.
        result: Option<Box<ToolResultNode>>,
        /// Boundary closed without a result (synthesized error result).
        interrupted: bool,
    },
    Compaction {
        summary: Option<String>,
        summary_event_seq: Option<i64>,
        shadowed_item_count: Option<usize>,
        shadowed_token_count: Option<i64>,
    },
    TurnError {
        turn: i64,
        message: String,
        code: Option<String>,
    },
    TurnMaxTokens {
        turn: i64,
    },
    Unknown {
        r#type: String,
        data: Value,
    },
}

/// One derived chat node, in display order (ascending [`ChatNode::anchor_seq`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ChatNode {
    pub key: NodeKey,
    pub kind: ChatNodeKind,
    /// Sortable render position — the creating event's integer seq (v1; the
    /// web uses fractional synthetic offsets for some nodes — deferred).
    pub anchor_seq: i64,
    pub data: NodeData,
}

/// User-fold state for one node (Q11). Stored outside the node list, keyed by
/// [`NodeKey`], so collapse choices survive node-list rebuilds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FoldState {
    pub collapsed: bool,
}

impl FoldState {
    pub const fn collapsed() -> Self {
        FoldState { collapsed: true }
    }

    pub const fn expanded() -> Self {
        FoldState { collapsed: false }
    }

    /// v1 defaults mirroring the web: tool nodes COLLAPSED (the web's
    /// ToolRow starts `useState(false)` — "every card kind starts
    /// collapsed, so a run of tool calls stays scannable"; click the
    /// header to expand), compaction and context nodes collapsed,
    /// assistant nodes expanded (streaming and settled alike — a running
    /// assistant is never collapsed by default), plain user messages
    /// collapsed, notices and unknown rows expanded.
    pub fn default_for(node: &ChatNode) -> Self {
        match &node.data {
            NodeData::Tool { .. } => FoldState::collapsed(),
            NodeData::Assistant { .. } | NodeData::Unknown { .. } => FoldState::expanded(),
            NodeData::User { .. }
            | NodeData::Compaction { .. }
            | NodeData::TurnError { .. }
            | NodeData::TurnMaxTokens { .. } => FoldState::collapsed(),
        }
    }
}
