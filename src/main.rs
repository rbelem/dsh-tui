//! dsh-tui: terminal UI client for the deepseek-harness gateway.
//!
//! Pure client (ticket 06 Q8): attach via `DSH_PORT` to a RUNNING gateway —
//! never boots anything.

use dsh_tui::app::{App, AppError, EventChannel, attach, event, run};
use dsh_tui::client::WireClient;

fn main() -> Result<(), AppError> {
    let client = match WireClient::attach_from_env()? {
        Some(client) => client,
        None => {
            let locale =
                dsh_tui::i18n::Locale::detect(dsh_tui::theme::Config::load().locale.as_deref());
            eprintln!("{}", dsh_tui::i18n::tr(locale, "main.no_dsh_port"));
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
    app.load_theme_config();
    // Locale resolution (increment 3): config wins, then DSH_TUI_LOCALE,
    // then LANG/LC_ALL (Locale::detect); persisted on Ctrl+L.
    app.locale = dsh_tui::i18n::Locale::detect(app.config.locale.as_deref());
    let (session_id, sessions) = attach(&client, &mut app.store, app.locale).await?;
    app.active_session = session_id.clone();
    app.sessions = sessions;
    app.client = Some(client.clone());
    if session_id.is_none() {
        app.last_error = Some(dsh_tui::i18n::tr(app.locale, "main.no_sessions").into());
    }

    let mut terminal = run::setup_terminal()?;
    let _guard = run::TerminalGuard;

    let mut events = EventChannel::new();
    event::spawn_input_bridge(events.tx.clone());
    event::spawn_frame_bridge(client.mux_stream(), events.tx.clone());
    event::spawn_host_bridge(client.host_stream(), events.tx.clone());
    app.run(&mut terminal, &mut events).await
}
