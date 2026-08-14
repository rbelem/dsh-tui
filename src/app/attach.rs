//! Attach flow (Q9): resume the most recently updated session on a running
//! gateway (ticket 06 Q8 — pure client, never boots anything).

use crate::app::AppError;
use crate::client::WireClient;
use crate::store::SessionStore;
use crate::wire::session::{SessionId, SessionSummary};

/// History page size for the attach resume.
const HISTORY_PAGE: usize = 200;

/// Attach to the gateway's sessions: `session.list`, then load the most
/// recently updated non-blank session's history tail into the store.
///
/// Returns the opened session id (`None` when the gateway has no sessions —
/// the app stays on an empty chat and the caller sets a hint) plus the full
/// summary list for the sidebar.
pub async fn attach(
    client: &WireClient,
    store: &mut SessionStore,
) -> Result<(Option<SessionId>, Vec<SessionSummary>), AppError> {
    let summaries = client.session_list().await?;
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
        return Ok((None, summaries));
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

    eprintln!(
        "dsh-tui: attached to 127.0.0.1:{}, session {}",
        client.port(),
        summary.session_id
    );
    let opened = summary.session_id.clone();
    Ok((Some(opened), summaries))
}
