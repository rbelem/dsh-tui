//! sessions domain wire models (sessions.schema.ts). The SessionEvent `data`
//! slot and the projections `values` record stay wide — a later SessionStore
//! lane types them.

use serde::{Deserialize, Serialize};

brand!(
    SessionId,
    "Mirrors sessionIdSchema (sessions.schema.ts:27): `z.string().min(1)`."
);
brand!(
    MessageId,
    "Mirrors messageIdSchema (sessions.schema.ts:30): `z.string().min(1)`."
);
brand!(
    WorkspaceId,
    "Mirrors workspaceIdSchema (sessions.schema.ts:38): `z.string().min(1)`."
);
brand!(
    AttachmentId,
    "Mirrors attachmentIdSchema (sessions.schema.ts:305): `z.string().min(1)`."
);

/// Mirrors the `origin` literal union of sessionSummarySchema /
/// host/session-added (sessions.schema.ts:58, events.schema.ts:76).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Subagent,
}

/// Mirrors sessionEventSchema (sessions.schema.ts:41-49): strict envelope,
/// wide `data` (the client fold handles unknown event types via its documented
/// default).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    pub r#type: String,
    pub seq: i64,
    pub time: f64,
    pub data: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_seqs: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_op: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignorable: Option<bool>,
}

/// Mirrors sessionSummarySchema (sessions.schema.ts:52-62): the session.list row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub updated_at: f64,
    pub running: bool,
    pub blank: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projections: Option<SessionProjectionsBlock>,
}

/// Mirrors sessionListRequestSchema (sessions.schema.ts:65-67): `cursor` is a
/// reserved seat, unimplemented in v1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Mirrors sessionListValueSchema (sessions.schema.ts:70-72).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionListValue {
    pub items: Vec<SessionSummary>,
}

/// Mirrors sessionSearchRequestSchema (sessions.schema.ts:78-81).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSearchRequest {
    pub query: String,
}

/// Mirrors sessionSearchItemSchema (sessions.schema.ts:84-93).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchItem {
    pub session_id: SessionId,
    pub snippet: String,
}

/// Mirrors sessionSearchValueSchema (sessions.schema.ts:96-99).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSearchValue {
    pub items: Vec<SessionSearchItem>,
    #[serde(rename = "hasMore")]
    pub has_more: bool,
}

/// Mirrors sessionCreateRequestSchema (sessions.schema.ts:102-110): at most
/// one of workspaceId / cwd (host-validated).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
}

/// Mirrors sessionCreateValueSchema (sessions.schema.ts:113-116).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateValue {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
}

/// Mirrors sessionRenameRequestSchema (sessions.schema.ts:119-122).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRenameRequest {
    pub session_id: SessionId,
    pub title: String,
}

/// Mirrors sessionRenameValueSchema (sessions.schema.ts:125-128): the
/// normalized accepted title and its event seq.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRenameValue {
    pub title: String,
    pub seq: i64,
}

/// Mirrors sessionForkRequestSchema (sessions.schema.ts:131-134): `atSeq`
/// anchors the completed-turn cut.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForkRequest {
    pub session_id: SessionId,
    #[serde(rename = "atSeq", default, skip_serializing_if = "Option::is_none")]
    pub at_seq: Option<i64>,
}

/// Mirrors sessionForkValueSchema (sessions.schema.ts:137-139): the child
/// session id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForkValue {
    pub session_id: SessionId,
}

/// Mirrors sessionHistoryRequestSchema (sessions.schema.ts:142-146):
/// beforeSeq/maxMessages page backwards from the window tail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryRequest {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_messages: Option<i64>,
}

/// Mirrors modelSelectionSchema (sessions.schema.ts:149-153): complete
/// provider/model selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// Mirrors modelReasoningEffortSchema (sessions.schema.ts:156-160): one
/// adapter-owned reasoning effort.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelReasoningEffort {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Mirrors modelReasoningSchema (sessions.schema.ts:163-166): exact-model
/// reasoning metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReasoning {
    pub efforts: Vec<ModelReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

/// Mirrors modelCatalogModelSchema (sessions.schema.ts:169-174): one advisory
/// model entry inside a provider group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogModel {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ModelReasoning>,
}

/// Mirrors modelProviderGroupSchema (sessions.schema.ts:177-181): one
/// successfully loaded provider group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelProviderGroup {
    pub id: String,
    pub name: String,
    pub models: Vec<ModelCatalogModel>,
}

/// Mirrors modelCatalogFailureSchema (sessions.schema.ts:184-188): one
/// provider-local catalog failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogFailure {
    pub id: String,
    pub name: String,
    pub message: String,
}

/// Mirrors the `view` payload of toolEventViewSchema (sessions.schema.ts:197):
/// a loose object — only the listed keys are read, unknown extras pass through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolEventViewCard {
    pub card: String,
}

/// Mirrors toolEventViewSchema (sessions.schema.ts:196-199): lock only the
/// `for` discriminant and the card-tagged `view`; the view interior stays
/// host-computed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "for", rename_all = "kebab-case")]
pub enum ToolEventView {
    Call { view: ToolEventViewCard },
    Result { view: ToolEventViewCard },
}

/// Mirrors historyEntrySchema (sessions.schema.ts:202-205): one session.history
/// item — the session event plus its optional host-computed tool view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub event: SessionEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ToolEventView>,
}

/// Mirrors sessionProjectionsBlockSchema (sessions.schema.ts:212-216):
/// `asOfSeq` -1 = empty log; `values` stays a wide record (each value was
/// already parsed by its provider's own schema on the host).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProjectionsBlock {
    pub as_of_seq: i64,
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// Mirrors sessionHistoryValueSchema (sessions.schema.ts:238-242): projections
/// rides the tail page only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHistoryValue {
    pub events: Vec<HistoryEntry>,
    #[serde(rename = "hasMore")]
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projections: Option<SessionProjectionsBlock>,
}

/// Mirrors sessionModelsRequestSchema (sessions.schema.ts:245-247).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelsRequest {
    pub session_id: SessionId,
}

/// Mirrors sessionModelsValueSchema (sessions.schema.ts:250-255).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionModelsValue {
    pub current: ModelSelection,
    pub routable: bool,
    pub groups: Vec<ModelProviderGroup>,
    pub failures: Vec<ModelCatalogFailure>,
}

/// Mirrors sessionSelectModelRequestSchema (sessions.schema.ts:258-263).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSelectModelRequest {
    pub session_id: SessionId,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// Mirrors sessionSelectModelValueSchema (sessions.schema.ts:266-268).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSelectModelValue {
    pub selected: ModelSelection,
}

/// Mirrors contentBlockSchema (sessions.schema.ts:271): a loose object — the
/// `type` discriminant envelope is strict, the rest stays wide passthrough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlock {
    pub r#type: String,
}

/// Mirrors imageMediaTypeSchema (sessions.schema.ts:274-279): raster image
/// media types accepted by the version-one browser wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageMediaType {
    #[serde(rename = "image/png")]
    ImagePng,
    #[serde(rename = "image/jpeg")]
    ImageJpeg,
    #[serde(rename = "image/webp")]
    ImageWebp,
    #[serde(rename = "image/gif")]
    ImageGif,
}

/// Mirrors promptContentPartSchema (sessions.schema.ts:282-285): prompt wire
/// content, narrower than merge-extensible durable core content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PromptContentPart {
    Text {
        text: String,
    },
    Image {
        #[serde(rename = "mediaType")]
        media_type: ImageMediaType,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

/// Mirrors the `mode` union of sessionPromptRequestSchema (sessions.schema.ts:290).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptMode {
    Queue,
    Steer,
}

/// Mirrors the `kind` literal of the command slot of sessionPromptValueSchema
/// (sessions.schema.ts:299).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptCommandKind {
    Success,
}

/// Mirrors the command slot of sessionPromptValueSchema (sessions.schema.ts:298-301):
/// present only when the prompt dispatched a slash command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptCommand {
    pub kind: PromptCommandKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Mirrors sessionPromptRequestSchema (sessions.schema.ts:288-293), including
/// optional browser-local request provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptRequest {
    pub session_id: SessionId,
    pub mode: PromptMode,
    pub content: Vec<PromptContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_time_zone: Option<String>,
}

/// Mirrors sessionPromptValueSchema (sessions.schema.ts:296-302).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionPromptValue {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<PromptCommand>,
}

/// Mirrors imageAttachmentRefSchema (sessions.schema.ts:308-315): durable image
/// reference returned from the authenticated session lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachmentRef {
    pub attachment_id: AttachmentId,
    pub media_type: ImageMediaType,
    pub bytes: i64,
    pub width: i64,
    pub height: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Mirrors sessionAttachmentRequestSchema (sessions.schema.ts:318-321).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAttachmentRequest {
    pub session_id: SessionId,
    pub attachment_id: AttachmentId,
}

/// Mirrors sessionAttachmentValueSchema (sessions.schema.ts:324-327).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionAttachmentValue {
    pub attachment: ImageAttachmentRef,
    pub data: String,
}

/// Mirrors the `action` union of sessionUpdateQueueRequestSchema
/// (sessions.schema.ts:333-337).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum UpdateQueueAction {
    Edit { content: Vec<ContentBlock> },
    Remove,
    Steer,
}

/// Mirrors sessionUpdateQueueRequestSchema (sessions.schema.ts:330-338).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateQueueRequest {
    pub session_id: SessionId,
    pub item_id: MessageId,
    pub action: UpdateQueueAction,
}

/// Mirrors sessionUpdateQueueValueSchema (sessions.schema.ts:341-343).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUpdateQueueValue {
    pub accepted: bool,
}

/// Mirrors sessionCancelRequestSchema (sessions.schema.ts:346-348).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCancelRequest {
    pub session_id: SessionId,
}

/// Mirrors sessionCancelValueSchema (sessions.schema.ts:351-353).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCancelValue {
    pub accepted: bool,
}
