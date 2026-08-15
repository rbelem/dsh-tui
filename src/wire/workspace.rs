//! workspace domain wire models (workspace.schema.ts). The WorkspaceId brand
//! lives in the session module (mirroring the zod note at sessions.schema.ts:33-38)
//! and is re-exported here as the domain-local name.

use serde::{Deserialize, Serialize};

use crate::wire::session::SessionId;

pub use crate::wire::session::WorkspaceId;

/// Mirrors workspaceViewSchema (workspace.schema.ts:16-23): the row of every
/// workspace.* response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceView {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub title: String,
    pub session_ids: Vec<SessionId>,
    pub created_at: String,
    pub updated_at: String,
}

/// Mirrors workspaceListRequestSchema (workspace.schema.ts:26): empty request
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkspaceListRequest {}

/// Mirrors workspaceListValueSchema (workspace.schema.ts:29-32). Default =
/// the flat sidebar (no groups, no archived set) — the attach flow degrades
/// to it when `workspace.list` fails.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkspaceListValue {
    pub items: Vec<WorkspaceView>,
    #[serde(rename = "archivedSessionIds")]
    pub archived_session_ids: Vec<SessionId>,
}

/// Mirrors workspaceCreateRequestSchema (workspace.schema.ts:35-37): the
/// existing directory to adopt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreateRequest {
    pub path: String,
}

/// Mirrors workspaceCreateValueSchema (workspace.schema.ts:40-43).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreateValue {
    pub workspace: WorkspaceView,
    pub created: bool,
}

/// Mirrors workspaceRenameRequestSchema (workspace.schema.ts:46-52): the new
/// title must be non-blank (host-validated).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRenameRequest {
    pub workspace_id: WorkspaceId,
    pub title: String,
}

/// Mirrors workspaceRenameValueSchema (workspace.schema.ts:55-57).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRenameValue {
    pub workspace: WorkspaceView,
}

/// Mirrors workspaceDeleteRequestSchema (workspace.schema.ts:60-62).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDeleteRequest {
    pub workspace_id: WorkspaceId,
}

/// Mirrors workspaceDeleteValueSchema (workspace.schema.ts:65-67).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDeleteValue {
    pub deleted: bool,
}

/// Mirrors workspaceInsertBeforeRequestSchema (workspace.schema.ts:70-73):
/// anchor omitted = append to end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInsertBeforeRequest {
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_workspace_id: Option<WorkspaceId>,
}

/// Mirrors workspaceInsertBeforeValueSchema (workspace.schema.ts:76-78): the
/// complete durable display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInsertBeforeValue {
    #[serde(rename = "workspaceIds")]
    pub workspace_ids: Vec<WorkspaceId>,
}

/// Mirrors workspaceInsertSessionBeforeRequestSchema (workspace.schema.ts:81-85):
/// anchor omitted = append to end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInsertSessionBeforeRequest {
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_session_id: Option<SessionId>,
}

/// Mirrors workspaceInsertSessionBeforeValueSchema (workspace.schema.ts:88-90).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInsertSessionBeforeValue {
    pub workspace: WorkspaceView,
}

/// Mirrors workspaceArchiveSessionRequestSchema (workspace.schema.ts:93-95).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceArchiveSessionRequest {
    pub session_id: SessionId,
}

/// Mirrors workspaceArchiveSessionValueSchema (workspace.schema.ts:98-100): the
/// full updated archive set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceArchiveSessionValue {
    #[serde(rename = "archivedSessionIds")]
    pub archived_session_ids: Vec<SessionId>,
}
