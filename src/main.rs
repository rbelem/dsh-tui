//! dsh-tui: terminal UI client for the deepseek-harness gateway.
//!
//! Pure client (ticket 06 Q8): attach via `DSH_PORT` to a RUNNING gateway —
//! never boots anything.

use tokio::sync::mpsc;

use dsh_tui::app::{App, AppError, attach, event, run};
use dsh_tui::client::WireClient;

fn main() -> Result<(), AppError> {
    let client = match WireClient::attach_from_env()? {
        Some(client) => client,
        None => {
            eprintln!(
                "dsh-tui: no DSH_PORT set — attach to a running gateway (dsh web) or set DSH_PORT=<port>"
            );
            std::process::exit(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_app(client))
}

async fn run_app(client: WireClient) -> Result<(), AppError> {
    let mut app = App::default();
    let (session_id, sessions) = attach(&client, &mut app.store).await?;
    app.active_session = session_id.clone();
    app.sessions = sessions;
    app.client = Some(client.clone());
    if session_id.is_none() {
        app.last_error = Some("gateway has no sessions — start one from the web UI".into());
    }

    let mut terminal = run::setup_terminal()?;
    let _guard = run::TerminalGuard;

    let (tx, mut events) = mpsc::unbounded_channel();
    event::spawn_input_bridge(tx.clone());
    event::spawn_frame_bridge(client.mux_stream(), tx);
    app.run(&mut terminal, &mut events).await
}
