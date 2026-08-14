//! events domain wire models (events.schema.ts): the MuxFrame / HostFrame
//! unions — the payload slot of a downlink ServerRequest full form.

use serde::{Deserialize, Serialize};

use crate::wire::approvals::ApprovalRequestId;
use crate::wire::jobs::TaskView;
use crate::wire::rpc::{RpcError, RpcId, ServerRequest};
use crate::wire::session::{
    ContentBlock, MessageId, Origin, SessionEvent, SessionId, ToolEventView,
};
use crate::wire::workspace::{WorkspaceId, WorkspaceView};

/// Mirrors one options entry of askUserQuestionItemSchema (events.schema.ts:25).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Mirrors the `intent` union of askUserQuestionItemSchema (events.schema.ts:29-31):
/// an unknown tag rejects the frame rather than rendering generically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum QuestionIntent {
    PlanReview { approve: String },
}

/// Mirrors askUserQuestionItemSchema (events.schema.ts:20-32): question fields
/// validated strictly against core dsh-user-questions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskUserQuestionItem {
    pub id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<QuestionOption>>,
    #[serde(
        rename = "multiSelect",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub multi_select: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<QuestionIntent>,
}

/// Mirrors the `role` union of messageSchema (events.schema.ts:37).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// Mirrors the `source` loose object of messageSchema (events.schema.ts:39):
/// only `kind` is read, unknown extras pass through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueMessageSource {
    pub kind: String,
}

/// Mirrors messageSchema (events.schema.ts:35-40): the unified message envelope
/// carried by transient queue frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueMessage {
    pub id: MessageId,
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub source: QueueMessageSource,
}

/// Mirrors the `placement` union of the session/queue frame (events.schema.ts:58).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueuePlacement {
    Queued,
    Steering,
    Context,
}

/// Mirrors one session/queue frame item (events.schema.ts:56-60).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: MessageId,
    pub placement: QueuePlacement,
    pub message: QueueMessage,
}

/// Mirrors the `outcome` union of the approval/resolved frame (events.schema.ts:47).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalOutcome {
    AllowedOnce,
    Rejected,
    Cancelled,
    Unavailable,
}

/// Mirrors the `outcome` union of the question/resolved frame (events.schema.ts:52).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuestionOutcome {
    Answered,
    Cancelled,
}

/// Mirrors muxFrameSchema (events.schema.ts:43-67): the payload slot of a
/// mux-stream ServerRequest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MuxFrame {
    #[serde(rename = "session/event")]
    SessionEvent {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        event: SessionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ToolEventView>,
    },
    #[serde(rename = "session/subscribed")]
    SessionSubscribed {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "lastSeq")]
        last_seq: i64,
    },
    #[serde(rename = "approval/requested")]
    ApprovalRequested {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "approvalId")]
        approval_id: ApprovalRequestId,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[serde(rename = "callId", default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "approval/resolved")]
    ApprovalResolved {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "approvalId")]
        approval_id: ApprovalRequestId,
        outcome: ApprovalOutcome,
    },
    #[serde(rename = "question/requested")]
    QuestionRequested {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        questions: Vec<AskUserQuestionItem>,
    },
    #[serde(rename = "question/resolved")]
    QuestionResolved {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "questionRpcId")]
        question_rpc_id: RpcId,
        outcome: QuestionOutcome,
    },
    #[serde(rename = "session/queue")]
    SessionQueue {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        items: Vec<QueueItem>,
    },
    #[serde(rename = "session/jobs")]
    SessionJobs {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        jobs: Vec<TaskView>,
    },
    #[serde(rename = "session/projection")]
    SessionProjection {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        key: String,
        value: serde_json::Value,
        seq: i64,
    },
    #[serde(rename = "stream/error")]
    StreamError { error: RpcError },
}

/// Mirrors hostFrameSchema (events.schema.ts:70-93): the payload slot of a
/// host-stream ServerRequest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HostFrame {
    #[serde(rename = "host/session-added")]
    HostSessionAdded {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        blank: bool,
        #[serde(
            rename = "parentSessionId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        parent_session_id: Option<SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Origin>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(
            rename = "agentPreset",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        agent_preset: Option<String>,
    },
    #[serde(rename = "host/session-removed")]
    HostSessionRemoved {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
    },
    #[serde(rename = "host/session-status")]
    HostSessionStatus {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        running: bool,
    },
    #[serde(rename = "host/agent-error")]
    HostAgentError {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        message: String,
    },
    #[serde(rename = "host/workspace-changed")]
    HostWorkspaceChanged { workspace: WorkspaceView },
    #[serde(rename = "host/workspace-removed")]
    HostWorkspaceRemoved {
        #[serde(rename = "workspaceId")]
        workspace_id: WorkspaceId,
    },
    #[serde(rename = "host/workspace-order-changed")]
    HostWorkspaceOrderChanged {
        #[serde(rename = "workspaceIds")]
        workspace_ids: Vec<WorkspaceId>,
    },
    #[serde(rename = "host/archived-sessions-changed")]
    HostArchivedSessionsChanged {
        #[serde(rename = "archivedSessionIds")]
        archived_session_ids: Vec<SessionId>,
    },
    #[serde(rename = "host/remote-event")]
    HostRemoteEvent {
        event: String,
        /// Wide by design (events.schema.ts:87-90): every element is already a
        /// JSON value; the structural contract belongs to the owner package.
        args: Vec<serde_json::Value>,
    },
    #[serde(rename = "stream/error")]
    StreamError { error: RpcError },
}

impl ServerRequest {
    /// Second-level parse of the downlink payload: mux-stream frames.
    pub fn into_mux_frame(self) -> Result<MuxFrame, serde_json::Error> {
        serde_json::from_value(self.payload)
    }

    /// Second-level parse of the downlink payload: host-stream frames.
    pub fn into_host_frame(self) -> Result<HostFrame, serde_json::Error> {
        serde_json::from_value(self.payload)
    }
}

impl MuxFrame {
    /// Parse a mux frame from an already-decoded JSON value.
    pub fn from_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }
}

impl HostFrame {
    /// Parse a host frame from an already-decoded JSON value.
    pub fn from_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }
}
