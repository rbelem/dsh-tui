//! skills domain wire models (skills.schema.ts).
//!
//! `skill.list` is the skill domain's only RPC: invocation itself is an
//! ordinary `session.prompt` whose leading `/name` token the host recognizes
//! at the pre-step boundary (apiproxy/src/api/skills.ts:28-32). The response
//! mirrors `SkillEntry` — the provider/source vocabulary stays host-side.

use serde::{Deserialize, Serialize};

use crate::wire::session::SessionId;

/// Mirrors skillEntrySchema (skills.schema.ts:15-20): one user-invocable
/// skill row. `modelInvocable: false` marks a user-only skill
/// (`disable-model-invocation`): invocable from the composer, absent from
/// the model catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    /// Kebab-case identifier the user references as `/name` in the composer.
    pub name: String,
    /// Short routing description.
    pub description: String,
    /// Optional extra routing guidance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    pub model_invocable: bool,
}

/// Mirrors skillListRequestSchema (skills.schema.ts:22-24): addressed by the
/// session whose header cwd resolves to the canonical project root host-side
/// (the client never submits a raw path).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillListRequest {
    pub session_id: SessionId,
}

/// Mirrors skillListValueSchema (skills.schema.ts:26-28).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillListValue {
    pub skills: Vec<SkillEntry>,
}
