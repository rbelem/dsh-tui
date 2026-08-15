//! Typed chat-loop method helpers over the wire::session types.
//!
//! Each helper posts the method's typed request payload via
//! [`crate::client::WireClient::call`] and parses the response value.
//! The answer helpers (`respond_*`) carry the envelope rpcId of the
//! answerable frame — the wire contract requires the ClientResponse to echo
//! it ("rpcId echoed, never minted anew", rpc.ts:178; "wire correlation is
//! governed by the echoed rpcId", approvals.ts:15). The envelope rpcId is
//! consumed by the mux subscriber while parsing the payload, so the consumer
//! lane must capture it before folding the frame — see the mux-stream TODO in
//! `client::mod`.

use crate::client::{ClientError, WireClient};
use crate::wire::approvals::{ApprovalRequestId, ApprovalResponseOutcome, ApprovalResponsePayload};
use crate::wire::questions::{AskUserQuestionAnswer, QuestionResponsePayload};
use crate::wire::rpc::{RpcId, RpcReceipt};
use crate::wire::session::{
    AttachmentId, MessageId, PromptContentPart, PromptMode, SessionAttachmentRequest,
    SessionAttachmentValue, SessionCancelRequest, SessionCancelValue, SessionCreateRequest,
    SessionCreateValue, SessionForkRequest, SessionForkValue, SessionHistoryRequest,
    SessionHistoryValue, SessionId, SessionListRequest, SessionListValue, SessionModelsRequest,
    SessionModelsValue, SessionPromptRequest, SessionPromptValue, SessionRenameRequest,
    SessionRenameValue, SessionSearchRequest, SessionSearchValue, SessionSelectModelRequest,
    SessionSelectModelValue, SessionSummary, SessionUpdateQueueRequest, SessionUpdateQueueValue,
    UpdateQueueAction, WorkspaceId,
};
use crate::wire::settings::{
    SettingsDescribeRequest, SettingsDescribeValue, SettingsUpdateRequest, SettingsWriteValue,
};
use crate::wire::skills::{SkillListRequest, SkillListValue};
use crate::wire::workspace::{
    WorkspaceArchiveSessionRequest, WorkspaceArchiveSessionValue, WorkspaceListRequest,
    WorkspaceListValue,
};

impl WireClient {
    /// `session.list` — the summary rows.
    pub async fn session_list(&self) -> Result<Vec<SessionSummary>, ClientError> {
        let value: SessionListValue = self
            .call("session.list", SessionListRequest { cursor: None })
            .await?;
        Ok(value.items)
    }

    /// `session.create` — at most one of `workspace_id` / `cwd`
    /// (host-validated). Idempotent by `session_id`.
    pub async fn session_create(
        &self,
        workspace_id: Option<WorkspaceId>,
        cwd: Option<String>,
        session_id: Option<SessionId>,
        agent_preset: Option<String>,
    ) -> Result<SessionCreateValue, ClientError> {
        self.call(
            "session.create",
            SessionCreateRequest {
                workspace_id,
                cwd,
                session_id,
                agent_preset,
            },
        )
        .await
    }

    /// `session.history` — events page backwards from the window tail.
    pub async fn session_history(
        &self,
        session_id: SessionId,
        before_seq: Option<i64>,
        max_messages: Option<i64>,
    ) -> Result<SessionHistoryValue, ClientError> {
        self.call(
            "session.history",
            SessionHistoryRequest {
                session_id,
                before_seq,
                max_messages,
            },
        )
        .await
    }

    /// `session.prompt` — submit a turn prompt.
    pub async fn session_prompt(
        &self,
        session_id: SessionId,
        mode: PromptMode,
        content: Vec<PromptContentPart>,
        client_time_zone: Option<String>,
    ) -> Result<SessionPromptValue, ClientError> {
        self.call(
            "session.prompt",
            SessionPromptRequest {
                session_id,
                mode,
                content,
                client_time_zone,
            },
        )
        .await
    }

    /// `session.cancel` — interrupt the running turn.
    pub async fn session_cancel(
        &self,
        session_id: SessionId,
    ) -> Result<SessionCancelValue, ClientError> {
        self.call("session.cancel", SessionCancelRequest { session_id })
            .await
    }

    /// `session.models` — current selection, provider groups, and failures.
    pub async fn session_models(
        &self,
        session_id: SessionId,
    ) -> Result<SessionModelsValue, ClientError> {
        self.call("session.models", SessionModelsRequest { session_id })
            .await
    }

    /// `session.selectModel` — switch the session's model route.
    pub async fn session_select_model(
        &self,
        session_id: SessionId,
        provider: String,
        model: String,
        reasoning_effort: Option<String>,
    ) -> Result<SessionSelectModelValue, ClientError> {
        self.call(
            "session.selectModel",
            SessionSelectModelRequest {
                session_id,
                provider,
                model,
                reasoning_effort,
            },
        )
        .await
    }

    /// `session.search` — full-text search over sessions.
    pub async fn session_search(&self, query: String) -> Result<SessionSearchValue, ClientError> {
        self.call("session.search", SessionSearchRequest { query })
            .await
    }

    /// `session.rename` — set the session title (host-side normalization).
    pub async fn session_rename(
        &self,
        session_id: SessionId,
        title: String,
    ) -> Result<SessionRenameValue, ClientError> {
        self.call("session.rename", SessionRenameRequest { session_id, title })
            .await
    }

    /// `session.fork` — fork the session at `at_seq` (or the completed-turn cut).
    pub async fn session_fork(
        &self,
        session_id: SessionId,
        at_seq: Option<i64>,
    ) -> Result<SessionForkValue, ClientError> {
        self.call("session.fork", SessionForkRequest { session_id, at_seq })
            .await
    }

    /// `session.attachment` — fetch a durable attachment by id.
    pub async fn session_attachment(
        &self,
        session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<SessionAttachmentValue, ClientError> {
        self.call(
            "session.attachment",
            SessionAttachmentRequest {
                session_id,
                attachment_id,
            },
        )
        .await
    }

    /// `session.updateQueue` — edit/remove/steer one queue item.
    pub async fn session_update_queue(
        &self,
        session_id: SessionId,
        item_id: MessageId,
        action: UpdateQueueAction,
    ) -> Result<SessionUpdateQueueValue, ClientError> {
        self.call(
            "session.updateQueue",
            SessionUpdateQueueRequest {
                session_id,
                item_id,
                action,
            },
        )
        .await
    }

    /// `settings.describe` — every exposed namespace (schema + redacted
    /// value + revision) in one shot; the request payload is empty
    /// (settings.schema.ts:30). There is no per-namespace describe and no
    /// list/read method — the settings view builds its nav from this.
    pub async fn settings_describe(&self) -> Result<SettingsDescribeValue, ClientError> {
        self.call("settings.describe", SettingsDescribeRequest {})
            .await
    }

    /// `settings.update` — patch one namespace. `expected_revision` rides the
    /// optimistic-concurrency slot (`settings-conflict` on a stale write;
    /// rpc.schema.ts:63); pass `None` to skip the check.
    pub async fn settings_update(
        &self,
        ns: &str,
        expected_revision: Option<f64>,
        patch: serde_json::Map<String, serde_json::Value>,
    ) -> Result<SettingsWriteValue, ClientError> {
        self.call(
            "settings.update",
            SettingsUpdateRequest {
                ns: ns.to_string(),
                patch,
                expected_revision,
            },
        )
        .await
    }

    /// `skill.list` — the user-invocable skill catalog for the session's
    /// project (the `@` menu's source; skills invoke via `session.prompt`).
    pub async fn skill_list(&self, session_id: SessionId) -> Result<SkillListValue, ClientError> {
        self.call("skill.list", SkillListRequest { session_id })
            .await
    }

    /// `workspace.list` — the sidebar's grouping snapshot: workspace rows
    /// (each carrying its member `sessionIds`) plus the archived session
    /// id set. The full value is returned (both halves feed app state).
    pub async fn workspace_list(&self) -> Result<WorkspaceListValue, ClientError> {
        self.call("workspace.list", WorkspaceListRequest {}).await
    }

    /// `workspace.archiveSession` — archive one session; the value is the
    /// FULL updated archive set (workspace.schema.ts:98-100), which the
    /// sidebar swaps in directly.
    pub async fn workspace_archive_session(
        &self,
        session_id: SessionId,
    ) -> Result<WorkspaceArchiveSessionValue, ClientError> {
        self.call(
            "workspace.archiveSession",
            WorkspaceArchiveSessionRequest { session_id },
        )
        .await
    }

    /// Answer an `approval/requested` frame. `rpc_id` MUST echo the frame's
    /// envelope rpcId (rpc.ts:178). The final outcome arrives later as an
    /// `approval/resolved` frame on the mux stream.
    pub async fn respond_approval(
        &self,
        rpc_id: RpcId,
        session_id: SessionId,
        approval_id: ApprovalRequestId,
        outcome: ApprovalResponseOutcome,
    ) -> Result<RpcReceipt, ClientError> {
        let payload = ApprovalResponsePayload {
            session_id,
            approval_id,
            outcome,
        };
        let value = serde_json::to_value(payload)
            .map_err(|e| ClientError::Protocol(format!("approval payload serialization: {e}")))?;
        self.respond(rpc_id, value).await
    }

    /// Answer a `question/requested` frame (see [`WireClient::respond_approval`]
    /// for the rpcId echo rule).
    pub async fn respond_question(
        &self,
        rpc_id: RpcId,
        session_id: SessionId,
        answer: AskUserQuestionAnswer,
    ) -> Result<RpcReceipt, ClientError> {
        let payload = QuestionResponsePayload { session_id, answer };
        let value = serde_json::to_value(payload)
            .map_err(|e| ClientError::Protocol(format!("question payload serialization: {e}")))?;
        self.respond(rpc_id, value).await
    }
}
