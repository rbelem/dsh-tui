//! Attach flow (Q9): resume the most recently updated session on the
//! gateway — the client attaches to whatever serves the resolved port
//! (the gateway may have been auto-started by `main`, #35).

use crate::app::AppError;
use crate::client::WireClient;
use crate::store::SessionStore;
use crate::wire::session::{ModelSelection, SessionId, SessionSummary};
use crate::wire::workspace::WorkspaceListValue;

/// History page size for the attach resume.
const HISTORY_PAGE: usize = 200;

/// Attach to the gateway's sessions: `session.list`, then load the most
/// recently updated non-blank session's history tail into the store.
///
/// `workspace.list` rides along (in parallel) to seed the sidebar's
/// grouping. It is TOLERANT: a failure degrades to the flat sidebar
/// (empty groups, no archived set) with a stderr note — the session list
/// is the only hard requirement for boot.
///
/// Returns the opened session id (`None` when the gateway has no sessions —
/// the app stays on an empty chat and the caller sets a hint), the full
/// summary list for the sidebar, the workspace snapshot, and the resumed
/// session's model selection (#43: `session.models` is TOLERANT too — an
/// unavailable gateway just leaves the Model/Effort status segments hidden).
pub async fn attach(
    client: &WireClient,
    store: &mut SessionStore,
    locale: crate::i18n::Locale,
) -> Result<
    (
        Option<SessionId>,
        Vec<SessionSummary>,
        WorkspaceListValue,
        Option<ModelSelection>,
    ),
    AppError,
> {
    let (summaries, workspaces) = {
        let summaries = client.session_list();
        let workspaces = client.workspace_list();
        let (summaries, workspaces) = tokio::join!(summaries, workspaces);
        let workspaces = match workspaces {
            Ok(value) => value,
            Err(error) => {
                eprintln!(
                    "{}",
                    crate::i18n::trf(locale, "main.workspace_list_failed", &[&error.to_string()],)
                );
                WorkspaceListValue::default()
            }
        };
        (summaries?, workspaces)
    };
    // Most recently updated non-blank session; fall back to any session.
    let chosen = summaries
        .iter()
        .filter(|summary| !summary.blank)
        .max_by(|a, b| a.updated_at.total_cmp(&b.updated_at))
        .or_else(|| {
            summaries
                .iter()
                .max_by(|a, b| a.updated_at.total_cmp(&b.updated_at))
        });
    let Some(summary) = chosen else {
        return Ok((None, summaries, workspaces, None));
    };

    let history = client
        .session_history(summary.session_id.clone(), None, Some(HISTORY_PAGE as i64))
        .await?;
    let entries = history
        .events
        .into_iter()
        .map(|entry| (entry.event, entry.view))
        .collect();
    store.ingest_history(&summary.session_id, entries)?;

    // #43: the resumed session's model selection — tolerant (a gateway
    // without models leaves the model/effort status segments hidden).
    let model = client
        .session_models(summary.session_id.clone())
        .await
        .ok()
        .map(|value| value.current);

    eprintln!(
        "{}",
        crate::i18n::trf(
            locale,
            "main.attached",
            &[&client.port().to_string(), summary.session_id.as_ref()],
        )
    );
    let opened = summary.session_id.clone();
    Ok((Some(opened), summaries, workspaces, model))
}
