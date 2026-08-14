//! tasks domain wire models (jobs.schema.ts): the branded job id and the wire
//! view carried by session/jobs frames.

use serde::{Deserialize, Serialize};

brand!(
    TaskId,
    "Mirrors taskIdSchema (jobs.schema.ts:12): `z.string().min(1)`."
);

/// Mirrors the `status` union of taskViewSchema (jobs.schema.ts:23-29).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    Running,
    Stopping,
    Completed,
    Killed,
    Failed,
}

/// Mirrors taskViewSchema (jobs.schema.ts:19-33). `kind` stays an open string:
/// producer plugins extend the registry's kind map by declaration merging, so
/// the closed set is not knowable at this boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: TaskId,
    pub kind: String,
    pub label: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}
