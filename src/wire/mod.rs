//! Wire protocol models for the deepseek-harness gateway.
//!
//! The gateway speaks a custom 4-form RPC (not JSON-RPC): `POST /api/<method>`
//! carries a `ClientRequest` full form and answers with a `ServerResponse`;
//! two downlink-only WebSockets (`/api/events.mux`, `/api/events.host`) carry
//! `ServerRequest` full forms whose payload is a frame union (`MuxFrame`,
//! `HostFrame`). The modules below mirror the zod schemas in
//! `packages/host/apiproxy/src/api/*.schema.ts` of the deepseek-harness
//! reference repo, which remain the single source of truth.

/// Generates a transparent string-brand newtype, mirroring the zod brand
/// schemas (`z.string().min(1)` casts: sessionIdSchema, messageIdSchema,
/// workspaceIdSchema, approvalRequestIdSchema, taskIdSchema). No validation is
/// performed here — ids are opaque tokens validated by the host.
///
/// Usage: `brand!(SessionId, "Mirrors sessionIdSchema (sessions.schema.ts:27).")`
macro_rules! brand {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = std::convert::Infallible;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(s.to_owned()))
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

pub mod approvals;
pub mod events;
pub mod jobs;
pub mod questions;
pub mod rpc;
pub mod session;
pub mod settings;
pub mod skills;
pub mod workspace;

pub use approvals::*;
pub use events::*;
pub use jobs::*;
pub use questions::*;
pub use rpc::*;
pub use session::*;
pub use settings::*;
pub use skills::*;
// Explicit (not a glob): workspace re-exports `WorkspaceId` from session, and
// glob-importing the same item twice is fragile even when it resolves to the
// same definition.
pub use workspace::{
    WorkspaceArchiveSessionRequest, WorkspaceArchiveSessionValue, WorkspaceCreateRequest,
    WorkspaceCreateValue, WorkspaceDeleteRequest, WorkspaceDeleteValue,
    WorkspaceInsertBeforeRequest, WorkspaceInsertBeforeValue, WorkspaceInsertSessionBeforeRequest,
    WorkspaceInsertSessionBeforeValue, WorkspaceListRequest, WorkspaceListValue,
    WorkspaceRenameRequest, WorkspaceRenameValue, WorkspaceView,
};
