//! approvals domain wire models (approvals.schema.ts): the approval answer
//! payload carried in the result.value slot of a client-response to
//! `/api/respond`.

use serde::{Deserialize, Serialize};

use crate::wire::session::SessionId;

brand!(
    ApprovalRequestId,
    "Mirrors approvalRequestIdSchema (approvals.schema.ts:14): `z.string().min(1)`."
);

/// Mirrors the `outcome` union of approvalResponsePayloadSchema
/// (approvals.schema.ts:20): narrower than the frame-level
/// [`crate::wire::events::ApprovalOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalResponseOutcome {
    AllowedOnce,
    Rejected,
}

/// Mirrors approvalResponsePayloadSchema (approvals.schema.ts:17-21): the
/// result.value slot of a client-response answering an approval/requested
/// frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResponsePayload {
    pub session_id: SessionId,
    pub approval_id: ApprovalRequestId,
    pub outcome: ApprovalResponseOutcome,
}
