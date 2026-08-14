//! settings domain wire models (settings.schema.ts).

use serde::{Deserialize, Serialize};

/// Mirrors settingsSecretViewSchema (settings.schema.ts:12-15): one redacted
/// secret slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsSecretView {
    pub path: Vec<String>,
    pub set: bool,
}

/// Mirrors the `applies` union of settingsNamespaceViewSchema (settings.schema.ts:24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppliesMode {
    Live,
    Restart,
}

/// Mirrors settingsNamespaceViewSchema (settings.schema.ts:18-27): the row of
/// settings.describe and the write responses. `schema`/`value`/`base`/`user`
/// stay wide — they are namespace-specific and schema-driven on the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsNamespaceView {
    pub ns: String,
    pub schema: serde_json::Value,
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<serde_json::Value>,
    pub applies: AppliesMode,
    pub secrets: Vec<SettingsSecretView>,
    pub revision: f64,
}

/// Mirrors settingsDescribeRequestSchema (settings.schema.ts:30): empty
/// request payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SettingsDescribeRequest {}

/// Mirrors settingsDescribeValueSchema (settings.schema.ts:33-37).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsDescribeValue {
    pub writable: bool,
    #[serde(rename = "hasDocument")]
    pub has_document: bool,
    pub namespaces: Vec<SettingsNamespaceView>,
}

/// Mirrors settingsOpenDocumentRequestSchema (settings.schema.ts:40): empty
/// request payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SettingsOpenDocumentRequest {}

/// Mirrors settingsOpenDocumentValueSchema (settings.schema.ts:43-45).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsOpenDocumentValue {
    pub opened: bool,
}

/// Mirrors settingsUpdateRequestSchema (settings.schema.ts:48-52).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateRequest {
    pub ns: String,
    pub patch: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<f64>,
}

/// Mirrors settingsReplaceRequestSchema (settings.schema.ts:58-62).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsReplaceRequest {
    pub ns: String,
    pub section: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<f64>,
}

/// Mirrors settingsPathOpSchema (settings.schema.ts:65-68): one path-addressed
/// edit of settings.mutate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum SettingsPathOp {
    Set {
        path: Vec<String>,
        value: serde_json::Value,
    },
    Unset {
        path: Vec<String>,
    },
}

/// Mirrors settingsMutateRequestSchema (settings.schema.ts:71-75).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsMutateRequest {
    pub ns: String,
    pub ops: Vec<SettingsPathOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<f64>,
}

/// settings.update / settings.replace / settings.mutate response value: the
/// namespace's new redacted view (settings.schema.ts:55,78,81) — reuses
/// [`SettingsNamespaceView`].
pub type SettingsWriteValue = SettingsNamespaceView;
