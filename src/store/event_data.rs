//! Typed parsing of `SessionEvent.data`.
//!
//! The wire keeps `data` as `serde_json::Value`; this module types it. Shapes
//! mirror the deepseek-harness reference repo: `packages/core/session/src/types.ts`
//! (SessionEventMap, TurnEndReasonMap, TodoItem), `packages/llm/llm/src/message.ts`
//! (message value types), and `packages/llm/llm/src/types.ts` (ContentBlockMap,
//! StreamChunk, FinishReasonMap, TokenUsage, LlmFailure).
//!
//! Parsing is tolerant where the reference types are merge-extensible
//! (unknown event types -> [`EventData::Unknown`], unknown content-block types
//! -> [`ContentBlock::Raw`]) and strict where a known type's payload is
//! malformed (-> [`EventDataError`], which rejects the frame at ingest).

use serde::Deserialize;
use serde::de::Deserializer;
use serde_json::Value;

use crate::wire::session::ImageAttachmentRef;

/// Malformed known-type event payload.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid session event data: {0}")]
pub struct EventDataError(pub String);

/// Opaque provider-issued tool-call id (pairs `tool/call` with `tool/result`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct CallId(pub String);

impl std::fmt::Display for CallId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for CallId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// One content block (ContentBlockMap, types.ts:99-105). Merge-extensible:
/// known blocks are typed, everything else stays raw — never dropped.
/// Parsed by hand dispatching on the `type` tag (untagged matching cannot
/// distinguish `text` from `reasoning`, which share the `text` field). The
/// `type` tag is not part of the Rust value (display data, never re-serialized
/// to the wire).
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Image {
        attachment: ImageAttachmentRef,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        tool_call_id: String,
        content: Vec<ContentBlock>,
        is_error: Option<bool>,
    },
    /// Unknown or plugin-owned block: preserved verbatim.
    Raw(Value),
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        parse_content_block(&value).map_err(serde::de::Error::custom)
    }
}

fn parse_content_block(value: &Value) -> Result<ContentBlock, EventDataError> {
    let block_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match block_type {
        "text" => Ok(ContentBlock::Text {
            text: field_str(value, "text")?,
        }),
        "reasoning" => Ok(ContentBlock::Reasoning {
            text: field_str(value, "text")?,
        }),
        "image" => Ok(ContentBlock::Image {
            attachment: typed_field(value, "attachment")?,
        }),
        "tool-call" => Ok(ContentBlock::ToolCall {
            id: field_str(value, "id")?,
            name: field_str(value, "name")?,
            arguments: field_str(value, "arguments")?,
        }),
        "tool-result" => Ok(ContentBlock::ToolResult {
            tool_call_id: field_str(value, "toolCallId")?,
            content: opt_field(value, "content")?.unwrap_or_default(),
            is_error: opt_bool(value, "isError")?,
        }),
        _ => Ok(ContentBlock::Raw(value.clone())),
    }
}

/// Token accounting for one model call (types.ts:135-141). Counts are disjoint:
/// `inputTokens` is uncached input only; cached input is reported separately.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: Option<i64>,
    #[serde(default)]
    pub cache_write_tokens: Option<i64>,
    #[serde(default)]
    pub reasoning_tokens: Option<i64>,
}

/// Serializable provider or transport failure facts (types.ts:40-51).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmFailure {
    pub message: String,
    pub code: String,
    #[serde(default)]
    pub status: Option<i64>,
    #[serde(default, rename = "providerRetryAfterMs")]
    pub provider_retry_after_ms: Option<i64>,
    #[serde(default, rename = "requestId")]
    pub request_id: Option<String>,
}

/// Why a model response stopped (FinishReasonMap, types.ts:116-122).
/// Merge-extensible: unknown reasons are preserved as [`FinishReason::Unknown`].
#[derive(Debug, Clone, PartialEq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    MaxTokens,
    Aborted { failure: LlmFailure },
    Error { failure: LlmFailure },
    Unknown(String),
}

fn deserialize_finish_reason<'de, D>(deserializer: D) -> Result<FinishReason, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    parse_finish_reason(&value).map_err(serde::de::Error::custom)
}

/// Raw streaming protocol emitted by adapters (types.ts:291-303).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum StreamChunk {
    BlockStart {
        index: i64,
        #[serde(rename = "blockType")]
        block_type: String,
    },
    TextDelta {
        index: i64,
        text: String,
    },
    ReasoningDelta {
        index: i64,
        text: String,
    },
    ToolCallDelta {
        index: i64,
        id: CallId,
        #[serde(default)]
        name: Option<String>,
        #[serde(rename = "argumentsDelta")]
        arguments_delta: String,
    },
    BlockEnd {
        index: i64,
        block: ContentBlock,
    },
    Usage {
        usage: TokenUsage,
    },
    Finish {
        #[serde(deserialize_with = "deserialize_finish_reason")]
        reason: FinishReason,
        #[serde(default, rename = "replayState")]
        replay_state: Option<Value>,
    },
}

/// Why a turn ended (TurnEndReasonMap, types.ts:155-174). On the wire the
/// reason is either a bare string (`completed`, `blocked`, `max-tokens`,
/// `interrupted`) or an object (`{kind}` for aborted/error). Merge-extensible:
/// unknown kinds are preserved as [`TurnEndReason::Unknown`].
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEndReason {
    Completed,
    Aborted { reason: TurnEndCancelCause },
    Blocked,
    Error { error: LlmFailure },
    MaxTokens,
    Interrupted,
    Unknown(String),
}

/// Durable cancellation cause (TurnEndCancelCause, types.ts:143-150).
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEndCancelCause {
    User,
    Parent,
    Hook { reason: String },
    Disposed,
    Legacy,
    Unknown(String),
}

/// One entry in an agent's todo list (types.ts:189-194). Log-only in v1;
/// `status` stays an open string (the three-state vocabulary may grow).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
}

/// How a surface event entered the ordered surface (types.ts:372-374).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceOp {
    Append,
    Replace { start: i64, end: i64 },
}

/// Parse `event.surfaceOp` (`'append'` | `{op:'replace',start,end}`). A missing
/// or unparseable marker yields `None` — v1 treats that as append (tolerant).
pub fn parse_surface_op(value: Option<&Value>) -> Option<SurfaceOp> {
    let value = value?;
    if value.as_str() == Some("append") {
        return Some(SurfaceOp::Append);
    }
    let obj = value.as_object()?;
    if obj.get("op")?.as_str() != Some("replace") {
        return None;
    }
    Some(SurfaceOp::Replace {
        start: obj.get("start")?.as_i64()?,
        end: obj.get("end")?.as_i64()?,
    })
}

/// User-role message on the model-visible surface (message.ts:141-144).
/// `source` stays wide: the fold reads `kind`/`plugin`/`compactionId` from it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserMessage {
    pub id: String,
    pub content: Vec<ContentBlock>,
    pub source: Value,
}

impl UserMessage {
    /// MessageSource.kind (`'user'`, `'plugin'`, `'model'`, `'tool'`, ...).
    pub fn source_kind(&self) -> Option<&str> {
        self.source.get("kind").and_then(Value::as_str)
    }

    /// MessageSource.plugin, present when `kind == 'plugin'`.
    pub fn source_plugin(&self) -> Option<&str> {
        self.source.get("plugin").and_then(Value::as_str)
    }
}

/// Model-produced assistant message (message.ts:146-149).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantMessage {
    pub id: String,
    pub content: Vec<ContentBlock>,
    pub source: Value,
}

/// Tool-result message: a user-role message whose single block is the result
/// of one call (message.ts:152-156).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolResultMessage {
    pub id: String,
    pub content: Vec<ContentBlock>,
    pub source: Value,
}

impl ToolResultMessage {
    /// The `tool/result` block — `content[0]` when it is one.
    pub fn tool_result_block(&self) -> Option<&ContentBlock> {
        match self.content.first() {
            Some(block @ ContentBlock::ToolResult { .. }) => Some(block),
            _ => None,
        }
    }

    /// Whether the result is flagged as an error (block `isError == true`).
    pub fn is_error(&self) -> bool {
        matches!(
            self.tool_result_block(),
            Some(ContentBlock::ToolResult {
                is_error: Some(true),
                ..
            })
        )
    }

    /// MessageSource.callId (the `tool` source's call correlation).
    pub fn source_call_id(&self) -> Option<&str> {
        self.source.get("callId").and_then(Value::as_str)
    }
}

/// Tool-failure identity carried on `tool/result` (types.ts `error` slot).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolErrorIdentity {
    pub name: String,
    pub code: String,
}

/// The typed payload of one session event, mirroring SessionEventMap
/// (types.ts:236-333) plus the plugin-merged `compaction/*` vocabulary (handled
/// by type-name string + wide data in the fold — see fold.rs).
#[derive(Debug, Clone, PartialEq)]
pub enum EventData {
    TurnStart {
        turn: i64,
    },
    TurnEnd {
        turn: i64,
        reason: TurnEndReason,
    },
    StepStart {
        turn: i64,
        step: i64,
    },
    StepEnd {
        turn: i64,
        step: i64,
    },
    UserMessage(UserMessage),
    AssistantChunk {
        turn: i64,
        step: i64,
        chunk: StreamChunk,
    },
    AssistantMessage {
        turn: i64,
        step: i64,
        message: AssistantMessage,
        usage: Option<TokenUsage>,
    },
    ToolCall {
        turn: i64,
        step: i64,
        call_id: CallId,
        name: String,
        arguments: String,
    },
    ToolResult {
        turn: i64,
        step: i64,
        message: ToolResultMessage,
        error: Option<ToolErrorIdentity>,
        meta: Option<Value>,
    },
    TodoWrite {
        todos: Vec<TodoItem>,
    },
    RequestHeader {
        header: Value,
    },
    RequestContext {
        provider: String,
        model: String,
        context_window: Option<i64>,
    },
    EndSeed,
    /// An event type this store does not recognize. `ignorable: true` events
    /// are skipped silently; required ones degrade to an UnknownNode.
    Unknown {
        ignorable: bool,
    },
}

/// Parse one event payload into its typed form.
pub fn parse_event_data(
    r#type: &str,
    data: &Value,
    ignorable: bool,
) -> Result<EventData, EventDataError> {
    match r#type {
        "turn/start" => Ok(EventData::TurnStart {
            turn: field_i64(data, "turn")?,
        }),
        "turn/end" => Ok(EventData::TurnEnd {
            turn: field_i64(data, "turn")?,
            reason: parse_turn_end_reason(
                data.get("reason")
                    .ok_or_else(|| EventDataError("turn/end missing `reason`".into()))?,
            )?,
        }),
        "step/start" => Ok(EventData::StepStart {
            turn: field_i64(data, "turn")?,
            step: field_i64(data, "step")?,
        }),
        "step/end" => Ok(EventData::StepEnd {
            turn: field_i64(data, "turn")?,
            step: field_i64(data, "step")?,
        }),
        "user/message" => Ok(EventData::UserMessage(typed(data)?)),
        "assistant/chunk" => Ok(EventData::AssistantChunk {
            turn: field_i64(data, "turn")?,
            step: field_i64(data, "step")?,
            chunk: typed_field(data, "chunk")?,
        }),
        "assistant/message" => Ok(EventData::AssistantMessage {
            turn: field_i64(data, "turn")?,
            step: field_i64(data, "step")?,
            message: typed_field(data, "message")?,
            usage: opt_field(data, "usage")?,
        }),
        "tool/call" => Ok(EventData::ToolCall {
            turn: field_i64(data, "turn")?,
            step: field_i64(data, "step")?,
            call_id: typed_field(data, "callId")?,
            name: field_str(data, "name")?,
            arguments: field_str(data, "arguments")?,
        }),
        "tool/result" => Ok(EventData::ToolResult {
            turn: field_i64(data, "turn")?,
            step: field_i64(data, "step")?,
            message: typed_field(data, "message")?,
            error: opt_field(data, "error")?,
            meta: opt_field(data, "meta")?,
        }),
        "todo/write" => Ok(EventData::TodoWrite {
            todos: typed_field(data, "todos")?,
        }),
        "request/header" => Ok(EventData::RequestHeader {
            header: field_value(data, "header")?,
        }),
        "request/context" => Ok(EventData::RequestContext {
            provider: field_str(data, "provider")?,
            model: field_str(data, "model")?,
            context_window: opt_i64(data, "contextWindow")?,
        }),
        "session/end-seed" => Ok(EventData::EndSeed),
        // `compaction/start|summary|end` are plugin-merged events; the fold
        // handles them by type-name string + wide data, so they land in
        // Unknown (required) and the fold dispatches on the raw type.
        _ => Ok(EventData::Unknown { ignorable }),
    }
}

fn parse_turn_end_reason(value: &Value) -> Result<TurnEndReason, EventDataError> {
    if let Some(kind) = value.as_str() {
        return Ok(match kind {
            "completed" => TurnEndReason::Completed,
            "blocked" => TurnEndReason::Blocked,
            "max-tokens" => TurnEndReason::MaxTokens,
            "interrupted" => TurnEndReason::Interrupted,
            other => TurnEndReason::Unknown(other.to_string()),
        });
    }
    let obj = value
        .as_object()
        .ok_or_else(|| EventDataError("turn/end reason must be a string or object".into()))?;
    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| EventDataError("turn/end reason object missing `kind`".into()))?;
    Ok(match kind {
        "completed" => TurnEndReason::Completed,
        "aborted" => TurnEndReason::Aborted {
            reason: obj
                .get("reason")
                .map(parse_cancel_cause)
                .unwrap_or_else(|| TurnEndCancelCause::Unknown("missing".to_string())),
        },
        "blocked" => TurnEndReason::Blocked,
        "error" => TurnEndReason::Error {
            error: obj
                .get("error")
                .ok_or_else(|| EventDataError("turn/end error reason missing `error`".into()))
                .and_then(typed)?,
        },
        "max-tokens" => TurnEndReason::MaxTokens,
        "interrupted" => TurnEndReason::Interrupted,
        other => TurnEndReason::Unknown(other.to_string()),
    })
}

fn parse_cancel_cause(value: &Value) -> TurnEndCancelCause {
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "user" => TurnEndCancelCause::User,
        "parent" => TurnEndCancelCause::Parent,
        "hook" => TurnEndCancelCause::Hook {
            reason: value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        "disposed" => TurnEndCancelCause::Disposed,
        "legacy" => TurnEndCancelCause::Legacy,
        other => TurnEndCancelCause::Unknown(other.to_string()),
    }
}

/// FinishReason is merge-extensible and serialized as a string OR an object
/// (`{kind:'aborted'|'error', failure}`) — the two shapes need manual dispatch.
pub fn parse_finish_reason(value: &Value) -> Result<FinishReason, EventDataError> {
    if let Some(kind) = value.as_str() {
        return Ok(match kind {
            "stop" => FinishReason::Stop,
            "tool-calls" => FinishReason::ToolCalls,
            "max-tokens" => FinishReason::MaxTokens,
            other => FinishReason::Unknown(other.to_string()),
        });
    }
    let obj = value
        .as_object()
        .ok_or_else(|| EventDataError("finish reason must be a string or object".into()))?;
    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| EventDataError("finish reason object missing `kind`".into()))?;
    Ok(match kind {
        "stop" => FinishReason::Stop,
        "tool-calls" => FinishReason::ToolCalls,
        "max-tokens" => FinishReason::MaxTokens,
        "aborted" => FinishReason::Aborted {
            failure: obj
                .get("failure")
                .ok_or_else(|| EventDataError("aborted finish requires `failure`".into()))
                .and_then(typed)?,
        },
        "error" => FinishReason::Error {
            failure: obj
                .get("failure")
                .ok_or_else(|| EventDataError("error finish requires `failure`".into()))
                .and_then(typed)?,
        },
        other => FinishReason::Unknown(other.to_string()),
    })
}

fn field_i64(data: &Value, key: &str) -> Result<i64, EventDataError> {
    data.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| EventDataError(format!("missing or non-integer `{key}`")))
}

fn field_str(data: &Value, key: &str) -> Result<String, EventDataError> {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| EventDataError(format!("missing or non-string `{key}`")))
}

fn field_value(data: &Value, key: &str) -> Result<Value, EventDataError> {
    data.get(key)
        .cloned()
        .ok_or_else(|| EventDataError(format!("missing `{key}`")))
}

fn opt_i64(data: &Value, key: &str) -> Result<Option<i64>, EventDataError> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| EventDataError(format!("non-integer `{key}`"))),
    }
}

fn opt_bool(data: &Value, key: &str) -> Result<Option<bool>, EventDataError> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| EventDataError(format!("non-boolean `{key}`"))),
    }
}

fn typed<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, EventDataError> {
    serde_json::from_value(value.clone()).map_err(|e| EventDataError(e.to_string()))
}

fn typed_field<T: serde::de::DeserializeOwned>(
    data: &Value,
    key: &str,
) -> Result<T, EventDataError> {
    data.get(key)
        .ok_or_else(|| EventDataError(format!("missing `{key}`")))
        .and_then(typed)
}

fn opt_field<T: serde::de::DeserializeOwned>(
    data: &Value,
    key: &str,
) -> Result<Option<T>, EventDataError> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => typed(value).map(Some),
    }
}
