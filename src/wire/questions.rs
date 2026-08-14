//! questions domain wire models (questions.schema.ts): the question answer
//! payload carried in the result.value slot of a client-response to
//! `/api/respond`. The question identifier is the echoed rpcId; the payload
//! carries no resource id.

use serde::{Deserialize, Serialize};

use crate::wire::session::SessionId;

/// Mirrors one answers entry of askUserQuestionAnswerSchema (questions.schema.ts:16-18).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionAnswerItem {
    pub id: String,
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

/// Mirrors askUserQuestionAnswerSchema (questions.schema.ts:14-20), validated
/// strictly against core dsh-user-questions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskUserQuestionAnswer {
    pub answers: Vec<QuestionAnswerItem>,
}

/// Mirrors questionResponsePayloadSchema (questions.schema.ts:23-26): the
/// result.value slot of a client-response answering a question/requested frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionResponsePayload {
    pub session_id: SessionId,
    pub answer: AskUserQuestionAnswer,
}
