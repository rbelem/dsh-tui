//! Message-layer wire models: the four full forms, the error body, the carrier
//! receipt (rpc.schema.ts). Payload/value slots stay wide — the business layer
//! runs the second parse (two-level parse discipline).

use serde::{Deserialize, Serialize};

brand!(
    RpcId,
    "Mirrors rpcIdSchema (rpc.schema.ts:31): an opaque echo token with no min-length."
);

/// Mirrors rpcResultSchema (rpc.schema.ts:86-91): `{ok:true,value}` | `{ok:false,error}`.
///
/// Modeled as a tolerant struct rather than a serde internally-tagged enum
/// because the wire tag is a JSON boolean (`ok: true`), which serde's internal
/// tagging does not accept. `value` is optional: a void business result
/// serializes with no `value` field at all (the zod union widens it to
/// `unknown | undefined`, and absent == undefined on the wire).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcResult<T = serde_json::Value> {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Mirrors rpcReceiptSchema (rpc.schema.ts:137-140): `{accepted:true}` |
/// `{accepted:false, reason:'not-pending'|'bad-response'}`. Tolerant struct for
/// the same boolean-tag reason as [`RpcResult`]; `reason` is only meaningful
/// when `accepted` is false.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcReceipt {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<RpcReceiptReason>,
}

/// Mirrors the `reason` union of rpcReceiptSchema (rpc.schema.ts:139).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RpcReceiptReason {
    NotPending,
    BadResponse,
}

/// The fixed `type` literal of a client-request full form (rpc.schema.ts:100).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientRequestType {
    #[serde(rename = "client-request")]
    ClientRequest,
}

/// The fixed `type` literal of a server-response full form (rpc.schema.ts:108).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerResponseType {
    #[serde(rename = "server-response")]
    ServerResponse,
}

/// The fixed `type` literal of a server-request full form (rpc.schema.ts:115).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerRequestType {
    #[serde(rename = "server-request")]
    ServerRequest,
}

/// The fixed `type` literal of a client-response full form (rpc.schema.ts:123).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientResponseType {
    #[serde(rename = "client-response")]
    ClientResponse,
}

/// Mirrors clientRequestSchema (rpc.schema.ts:99-104). POST body of
/// `/api/<method>`; `payload` stays wide — the business layer runs the second
/// parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientRequest {
    #[serde(rename = "type")]
    pub r#type: ClientRequestType,
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    pub method: String,
    pub payload: serde_json::Value,
}

/// Mirrors serverResponseSchema (rpc.schema.ts:107-111). Answer to a
/// client-request; `result.value` stays wide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerResponse {
    #[serde(rename = "type")]
    pub r#type: ServerResponseType,
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    pub result: RpcResult<serde_json::Value>,
}

/// Mirrors serverRequestSchema (rpc.schema.ts:114-119). Downlink frame carrier
/// on both WebSocket streams; `payload` is a [`crate::wire::events::MuxFrame`]
/// or [`crate::wire::events::HostFrame`] (see `into_mux_frame` /
/// `into_host_frame`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerRequest {
    #[serde(rename = "type")]
    pub r#type: ServerRequestType,
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    pub method: String,
    pub payload: serde_json::Value,
}

/// Mirrors clientResponseSchema (rpc.schema.ts:122-126). POST body of
/// `/api/respond` (answers to approval/question server-request frames);
/// `result.value` stays wide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientResponse {
    #[serde(rename = "type")]
    pub r#type: ClientResponseType,
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    pub result: RpcResult<serde_json::Value>,
}

/// Mirrors rpcErrorSchema (rpc.schema.ts:34-79): discriminated by `code`, and
/// `details` is required on every branch. 40 branches, one per literal code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code")]
pub enum RpcError {
    /// details.issues holds the raw ZodIssue objects (opaque to the client).
    #[serde(rename = "bad-request")]
    BadRequest {
        message: String,
        details: BadRequestDetails,
    },
    #[serde(rename = "cancelled")]
    Cancelled {
        message: String,
        details: EmptyDetails,
    },
    #[serde(rename = "session-not-found")]
    SessionNotFound {
        message: String,
        details: SessionNotFoundDetails,
    },
    #[serde(rename = "model-unavailable")]
    ModelUnavailable {
        message: String,
        details: ModelUnavailableDetails,
    },
    #[serde(rename = "session-conflict")]
    SessionConflict {
        message: String,
        details: SessionConflictDetails,
    },
    #[serde(rename = "invalid-time-zone")]
    InvalidTimeZone {
        message: String,
        details: InvalidTimeZoneDetails,
    },
    #[serde(rename = "workspace-attach-failed")]
    WorkspaceAttachFailed {
        message: String,
        details: WorkspaceAttachFailedDetails,
    },
    #[serde(rename = "workspace-not-found")]
    WorkspaceNotFound {
        message: String,
        details: WorkspaceNotFoundDetails,
    },
    #[serde(rename = "workspace-invalid-path")]
    WorkspaceInvalidPath {
        message: String,
        details: WorkspaceInvalidPathDetails,
    },
    #[serde(rename = "workspace-name-conflict")]
    WorkspaceNameConflict {
        message: String,
        details: WorkspaceNameConflictDetails,
    },
    #[serde(rename = "workspace-move-invalid")]
    WorkspaceMoveInvalid {
        message: String,
        details: WorkspaceMoveInvalidDetails,
    },
    #[serde(rename = "directory-unreadable")]
    DirectoryUnreadable {
        message: String,
        details: DirectoryUnreadableDetails,
    },
    #[serde(rename = "directory-exists")]
    DirectoryExists {
        message: String,
        details: DirectoryExistsDetails,
    },
    #[serde(rename = "directory-create-failed")]
    DirectoryCreateFailed {
        message: String,
        details: DirectoryCreateFailedDetails,
    },
    #[serde(rename = "directory-picker-unavailable")]
    DirectoryPickerUnavailable {
        message: String,
        details: DirectoryPickerUnavailableDetails,
    },
    #[serde(rename = "agent-preset-read-only")]
    AgentPresetReadOnly {
        message: String,
        details: AgentPresetReadOnlyDetails,
    },
    #[serde(rename = "agent-preset-locked")]
    AgentPresetLocked {
        message: String,
        details: AgentPresetLockedDetails,
    },
    #[serde(rename = "agent-preset-conflict")]
    AgentPresetConflict {
        message: String,
        details: AgentPresetConflictDetails,
    },
    #[serde(rename = "agent-preset-not-found")]
    AgentPresetNotFound {
        message: String,
        details: AgentPresetNotFoundDetails,
    },
    #[serde(rename = "agent-preset-invalid")]
    AgentPresetInvalid {
        message: String,
        details: AgentPresetInvalidDetails,
    },
    #[serde(rename = "agent-busy")]
    AgentBusy {
        message: String,
        details: AgentBusyDetails,
    },
    #[serde(rename = "attachment-error")]
    AttachmentError {
        message: String,
        details: AttachmentErrorDetails,
    },
    #[serde(rename = "queue-item-not-found")]
    QueueItemNotFound {
        message: String,
        details: QueueItemNotFoundDetails,
    },
    #[serde(rename = "steer-unavailable")]
    SteerUnavailable {
        message: String,
        details: SteerUnavailableDetails,
    },
    #[serde(rename = "command-error")]
    CommandError {
        message: String,
        details: EmptyDetails,
    },
    #[serde(rename = "unknown-command")]
    UnknownCommand {
        message: String,
        details: EmptyDetails,
    },
    #[serde(rename = "settings-rejected")]
    SettingsRejected {
        message: String,
        details: SettingsRejectedDetails,
    },
    #[serde(rename = "settings-not-exposed")]
    SettingsNotExposed {
        message: String,
        details: SettingsNotExposedDetails,
    },
    #[serde(rename = "settings-conflict")]
    SettingsConflict {
        message: String,
        details: SettingsConflictDetails,
    },
    #[serde(rename = "credential-rejected")]
    CredentialRejected {
        message: String,
        details: CredentialRejectedDetails,
    },
    #[serde(rename = "model-discovery-failed")]
    ModelDiscoveryFailed {
        message: String,
        details: ModelDiscoveryFailedDetails,
    },
    #[serde(rename = "title-invalid")]
    TitleInvalid {
        message: String,
        details: TitleInvalidDetails,
    },
    #[serde(rename = "fork-unavailable")]
    ForkUnavailable {
        message: String,
        details: ForkUnavailableDetails,
    },
    #[serde(rename = "subagent-parent-unavailable")]
    SubagentParentUnavailable {
        message: String,
        details: SubagentParentUnavailableDetails,
    },
    #[serde(rename = "subagent-not-found")]
    SubagentNotFound {
        message: String,
        details: SubagentNotFoundDetails,
    },
    #[serde(rename = "subagent-catalog-diagnostic")]
    SubagentCatalogDiagnostic {
        message: String,
        details: SubagentCatalogDiagnosticDetails,
    },
    #[serde(rename = "subagent-not-resumable")]
    SubagentNotResumable {
        message: String,
        details: SubagentNotResumableDetails,
    },
    #[serde(rename = "subagent-unauthorized")]
    SubagentUnauthorized {
        message: String,
        details: SubagentUnauthorizedDetails,
    },
    #[serde(rename = "subagent-delivery-unavailable")]
    SubagentDeliveryUnavailable {
        message: String,
        details: SubagentDeliveryUnavailableDetails,
    },
    #[serde(rename = "internal")]
    Internal {
        message: String,
        details: EmptyDetails,
    },
}

/// Mirrors the `bad-request` details of rpcErrorSchema (rpc.schema.ts:35):
/// the raw ZodIssue objects, opaque to the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BadRequestDetails {
    pub issues: Vec<serde_json::Value>,
}

/// Shared details for branches whose schema details is an empty object
/// (`cancelled`, `command-error`, `unknown-command`, `settings-not-exposed`,
/// `internal`; rpc.schema.ts:36,59-60,62,78).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EmptyDetails {}

/// Mirrors the `session-not-found` details of rpcErrorSchema (rpc.schema.ts:37).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotFoundDetails {
    pub session_id: String,
}

/// Mirrors the `model-unavailable` details of rpcErrorSchema (rpc.schema.ts:38).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelUnavailableDetails {
    pub provider: String,
    pub model: String,
}

/// Mirrors the `session-conflict` details of rpcErrorSchema (rpc.schema.ts:39).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConflictDetails {
    pub session_id: String,
    pub requested_cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_cwd: Option<String>,
}

/// Mirrors the `invalid-time-zone` details of rpcErrorSchema (rpc.schema.ts:40).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvalidTimeZoneDetails {
    pub value: String,
}

/// Mirrors the `workspace-attach-failed` details of rpcErrorSchema (rpc.schema.ts:41).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAttachFailedDetails {
    pub session_id: String,
    pub workspace_id: String,
}

/// Mirrors the `workspace-not-found` details of rpcErrorSchema (rpc.schema.ts:42).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceNotFoundDetails {
    pub workspace_id: String,
}

/// Mirrors the `workspace-invalid-path` details of rpcErrorSchema (rpc.schema.ts:43).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInvalidPathDetails {
    pub path: String,
}

/// Mirrors the `workspace-name-conflict` details of rpcErrorSchema (rpc.schema.ts:44).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceNameConflictDetails {
    pub name: String,
}

/// Mirrors the `workspace-move-invalid` details of rpcErrorSchema (rpc.schema.ts:45).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMoveInvalidDetails {
    pub workspace_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_session_id: Option<String>,
}

/// Mirrors the `directory-unreadable` details of rpcErrorSchema (rpc.schema.ts:46).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryUnreadableDetails {
    pub path: String,
}

/// Mirrors the `directory-exists` details of rpcErrorSchema (rpc.schema.ts:47).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryExistsDetails {
    pub path: String,
}

/// Mirrors the `directory-create-failed` details of rpcErrorSchema (rpc.schema.ts:48).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryCreateFailedDetails {
    pub path: String,
}

/// Mirrors the `directory-picker-unavailable` details of rpcErrorSchema (rpc.schema.ts:49).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryPickerUnavailableDetails {
    pub capability: String,
}

/// Mirrors the `agent-preset-read-only` details of rpcErrorSchema (rpc.schema.ts:50).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetReadOnlyDetails {
    pub agent_preset: String,
    pub reason: String,
}

/// Mirrors the `agent-preset-locked` details of rpcErrorSchema (rpc.schema.ts:51).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetLockedDetails {
    pub session_id: String,
    pub agent_preset: String,
}

/// Mirrors the `agent-preset-conflict` details of rpcErrorSchema (rpc.schema.ts:52).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetConflictDetails {
    pub session_id: String,
    pub requested_preset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_preset: Option<String>,
}

/// Mirrors the `agent-preset-not-found` details of rpcErrorSchema (rpc.schema.ts:53).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetNotFoundDetails {
    pub agent_preset: String,
    pub available: Vec<String>,
}

/// Mirrors the `agent-preset-invalid` details of rpcErrorSchema (rpc.schema.ts:54).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetInvalidDetails {
    pub agent_preset: String,
    pub reason: String,
}

/// Mirrors the `agent-busy` details of rpcErrorSchema (rpc.schema.ts:55).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentBusyDetails {
    pub reason: String,
}

/// Mirrors the `attachment-error` details of rpcErrorSchema (rpc.schema.ts:56).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentErrorDetails {
    pub reason: String,
}

/// Mirrors the `queue-item-not-found` details of rpcErrorSchema (rpc.schema.ts:57).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItemNotFoundDetails {
    pub item_id: String,
}

/// Mirrors the `steer-unavailable` details of rpcErrorSchema (rpc.schema.ts:58).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerUnavailableDetails {
    pub item_id: String,
}

/// Mirrors the `settings-rejected` details of rpcErrorSchema (rpc.schema.ts:61).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsRejectedDetails {
    pub ns: String,
}

/// Mirrors the `settings-not-exposed` details of rpcErrorSchema (rpc.schema.ts:62).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsNotExposedDetails {
    pub ns: String,
}

/// Mirrors the `settings-conflict` details of rpcErrorSchema (rpc.schema.ts:63).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsConflictDetails {
    pub ns: String,
    pub expected: f64,
    pub actual: f64,
}

/// Mirrors the `credential-rejected` details of rpcErrorSchema (rpc.schema.ts:64).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialRejectedDetails {
    pub r#ref: String,
}

/// Mirrors the `model-discovery-failed` details of rpcErrorSchema (rpc.schema.ts:65).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDiscoveryFailedDetails {
    pub settings_ns: String,
    #[serde(rename = "baseURL", default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// Mirrors the `title-invalid` details of rpcErrorSchema (rpc.schema.ts:66).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleInvalidDetails {
    pub session_id: String,
}

/// Mirrors the `fork-unavailable` details of rpcErrorSchema (rpc.schema.ts:67).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkUnavailableDetails {
    pub session_id: String,
}

/// Mirrors the `subagent-parent-unavailable` details of rpcErrorSchema (rpc.schema.ts:68).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentParentUnavailableDetails {
    pub parent_session_id: String,
}

/// Mirrors the `subagent-not-found` details of rpcErrorSchema (rpc.schema.ts:69).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentNotFoundDetails {
    pub parent_session_id: String,
    pub child_session_id: String,
}

/// Mirrors the `reason` union of the `subagent-catalog-diagnostic` details
/// (rpc.schema.ts:73).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubagentCatalogDiagnosticReason {
    Corrupt,
    Unsupported,
    Unavailable,
}

/// Mirrors the `subagent-catalog-diagnostic` details of rpcErrorSchema
/// (rpc.schema.ts:70-74).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentCatalogDiagnosticDetails {
    pub parent_session_id: String,
    pub child_session_id: String,
    pub reason: SubagentCatalogDiagnosticReason,
}

/// Mirrors the `subagent-not-resumable` details of rpcErrorSchema (rpc.schema.ts:75).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentNotResumableDetails {
    pub child_session_id: String,
}

/// Mirrors the `subagent-unauthorized` details of rpcErrorSchema (rpc.schema.ts:76).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentUnauthorizedDetails {
    pub child_session_id: String,
}

/// Mirrors the `subagent-delivery-unavailable` details of rpcErrorSchema (rpc.schema.ts:77).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentDeliveryUnavailableDetails {
    pub child_session_id: String,
}
