//! dsh-tui: terminal UI client for the deepseek-harness gateway.
//!
//! Gateway lifecycle (#34/#35): the port resolves CLI > `DSH_PORT` env >
//! `[gateway] port` config > 3080; when the resolved port isn't serving
//! and `[gateway] auto_start` (default true) is on, dsh-tui spawns the
//! gateway itself (see [`dsh_tui::gateway`]) and keeps it running after
//! the TUI exits. `dsh-tui server stop` stops it explicitly.

use dsh_tui::app::{App, AppError, EventChannel, attach, event, run};
use dsh_tui::client::WireClient;

fn main() -> Result<(), AppError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The `server stop` subcommand routes before everything.
    if args.first().map(String::as_str) == Some("server")
        && args.get(1).map(String::as_str) == Some("stop")
    {
        std::process::exit(dsh_tui::gateway::server_stop(&args));
    }
    // The `--light` worker lane dispatches before the TUI path (which
    // stays byte-identical below).
    if args.iter().any(|arg| arg == "--light") {
        return dsh_tui::app::light::main_light(&args);
    }
    let locale = dsh_tui::i18n::Locale::detect(dsh_tui::theme::Config::load().locale.as_deref());
    let port = match dsh_tui::gateway::resolve_port(&args) {
        Ok(port) => port,
        Err(dsh_tui::gateway::PortError::Invalid { value, source }) => {
            eprintln!(
                "{}",
                dsh_tui::i18n::trf(locale, "main.invalid_port", &[&value, &source])
            );
            std::process::exit(2);
        }
        Err(dsh_tui::gateway::PortError::MissingValue) => {
            // #35 review: a dangling `--port` errors like the light path,
            // never a silent fall-through.
            eprintln!("{}", dsh_tui::i18n::tr(locale, "main.port_requires_value"));
            std::process::exit(2);
        }
    };
    // #35: probe; a dead port auto-starts the gateway (herdr model) unless
    // the config opts out.
    if !dsh_tui::gateway::port_serving(port) {
        let auto_start = dsh_tui::theme::Config::load().gateway.auto_start;
        if !auto_start {
            eprintln!(
                "{}",
                dsh_tui::i18n::trf(locale, "main.no_gateway", &[&port.to_string()])
            );
            std::process::exit(2);
        }
        eprintln!(
            "{}",
            dsh_tui::i18n::trf(locale, "main.starting_gateway", &[&port.to_string()])
        );
        if let Err(message) = dsh_tui::gateway::spawn_gateway(port) {
            eprintln!(
                "{}",
                dsh_tui::i18n::trf(locale, "main.gateway_start_failed", &[&message])
            );
            std::process::exit(2);
        }
    }
    let client = WireClient::attach(port)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_app(client))
}

async fn run_app(client: WireClient) -> Result<(), AppError> {
    let mut app = App::default();
    app.load_theme_config();
    // Image protocol tier: env-detected once at startup (render::image docs).
    app.init_images();
    // Locale resolution (increment 3): config wins, then DSH_TUI_LOCALE,
    // then LANG/LC_ALL (Locale::detect); persisted on Ctrl+L.
    app.locale = dsh_tui::i18n::Locale::detect(app.config.locale.as_deref());
    let (session_id, sessions, workspace_list) =
        attach(&client, &mut app.store, app.locale).await?;
    app.active_session = session_id.clone();
    app.sessions = sessions;
    app.workspace_order = workspace_list
        .items
        .iter()
        .map(|workspace| workspace.workspace_id.clone())
        .collect();
    app.workspaces = workspace_list.items;
    app.archived_session_ids = workspace_list.archived_session_ids;
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
