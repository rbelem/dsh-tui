//! The `--light` worker (T1): one gateway session per invocation, plain-text
//! output, sentinel line. No terminal, no raw mode, no event loop — the TUI
//! path is untouched.
//!
//! Transport contract: the task arrives via `--task TEXT`, `--file PATH`, or
//! piped stdin — `--task` wins over `--file`, which wins over stdin
//! ([`parse_light_args`]; the driver transports tasks via `--file`, so
//! quotes and newlines are preserved verbatim). The worker creates a
//! session, submits the task, folds the mux stream into a [`SessionStore`],
//! prints assistant text to stdout as nodes settle, and finishes when the
//! turn completes ([`crate::app::App::session_running`] semantics, the fold
//! half shared via [`SessionState::has_unsettled_tail`]). The sentinel is
//! the last line: `dsh-worker: done` on success, `dsh-worker: error:
//! <reason>` on failure.
//!
//! Exit codes: `0` success · `1` RPC/stream error (also an
//! [`WireClient::attach_from_env`] connection failure — the gateway is
//! unreachable, as opposed to absent) · `2` usage (no task source, empty
//! task, unknown flag) and missing `DSH_PORT` env (with the standard
//! no-DSH_PORT message).
//!
//! herdr lifecycle reporting (T2): when running inside a herdr pane
//! (`HERDR_PANE_ID` set and a herdr binary reachable via `HERDR_BIN_PATH`
//! or `PATH`), the worker reports `working` / `idle` / `blocked` state
//! transitions through `herdr pane report-agent`; otherwise it runs with
//! zero herdr interaction (silent, never a failure).

use std::collections::HashSet;
use std::io::{IsTerminal, Read, Write};

use crate::app::AppError;
use crate::client::{ClientError, WireClient};
use crate::store::event_data::EventData;
use crate::store::node::{AssistantBlock, NodeData};
use crate::store::{SessionState, SessionStore, StoreError};
use crate::wire::events::HostFrame;
use crate::wire::session::{PromptContentPart, PromptMode, SessionId};

/// Worker-level failure. The Display text rides the error sentinel
/// (`dsh-worker: error: <reason>`).
#[derive(Debug, thiserror::Error)]
pub enum LightError {
    #[error("{0}")]
    Rpc(#[from] ClientError),
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("stream closed before the turn completed")]
    StreamClosed,
    #[error("stream error: {0}")]
    StreamError(String),
    #[error("turn failed: {0}")]
    TurnFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// The task source resolved from `--light` CLI args.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskSource {
    /// `--task TEXT`.
    Task(String),
    /// `--file PATH`.
    File(String),
    /// Piped stdin (the caller reports whether stdin is a pipe).
    Stdin,
}

/// Report messages are truncated to ~80 chars (the herdr contract).
const MAX_REPORT_MESSAGE: usize = 80;

/// herdr lifecycle reporter for the `--light` worker. [`HerdrReporter::from_env`]
/// returns `None` when not running inside a herdr pane or herdr is
/// unreachable; the worker then runs without any herdr interaction.
pub struct HerdrReporter {
    pub pane: String,
    pub bin: String,
}

impl HerdrReporter {
    /// Resolve the reporter from the environment: `HERDR_PANE_ID` (the pane
    /// to report into) plus the herdr binary — `HERDR_BIN_PATH` when set,
    /// otherwise `herdr` found by walking the `PATH` entries. Either
    /// missing → `None`.
    pub fn from_env() -> Option<Self> {
        let pane = std::env::var("HERDR_PANE_ID").ok()?;
        let bin = match std::env::var("HERDR_BIN_PATH") {
            Ok(bin) => bin,
            Err(_) => find_in_path("herdr")?,
        };
        Some(HerdrReporter { pane, bin })
    }

    /// Report a state transition via `herdr pane report-agent`. Fire-and-
    /// forget: a failed spawn (dead herdr) is swallowed — reporting must
    /// never break the worker. The message is truncated to ~80 chars.
    pub fn report(&self, state: &str, message: &str) {
        let message: String = message.chars().take(MAX_REPORT_MESSAGE).collect();
        let _ = std::process::Command::new(&self.bin)
            .arg("pane")
            .arg("report-agent")
            .arg(&self.pane)
            .arg("--source")
            .arg("dsh-tui")
            .arg("--agent")
            .arg("dsh-worker")
            .arg("--state")
            .arg(state)
            .arg("--message")
            .arg(message)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Find `name` in the `PATH` entries — an existence walk, so a bare
/// unqualified name is never relied on (the spawn would resolve it, but
/// `from_env` must decide without spawning).
fn find_in_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.to_string_lossy().into_owned())
}

/// Parse the `--light` args: `--task TEXT`, `--file PATH`, or piped stdin
/// (`stdin_piped` — the caller checks `IsTerminal`). `--task` wins over
/// `--file`. Unknown flags and missing values are usage errors (exit 2).
pub fn parse_light_args(args: &[String], stdin_piped: bool) -> Result<TaskSource, String> {
    let mut task: Option<String> = None;
    let mut file: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--light" => {}
            "--task" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or("--task requires a value (quote the task text)")?;
                task = Some(value.clone());
            }
            "--file" => {
                index += 1;
                let value = args.get(index).ok_or("--file requires a path")?;
                file = Some(value.clone());
            }
            // #34: the gateway port resolution applies to --light too (the
            // value is consumed here so the worker's task parse stays
            // clean; the resolution itself reads the raw args).
            "--port" => {
                index += 1;
                if args.get(index).is_none() {
                    return Err("--port requires a value".into());
                }
            }
            arg if arg.starts_with("--port=") => {}
            other => return Err(format!("unknown argument `{other}`")),
        }
        index += 1;
    }
    match (task, file) {
        (Some(task), _) => Ok(TaskSource::Task(task)),
        (None, Some(file)) => Ok(TaskSource::File(file)),
        (None, None) if stdin_piped => Ok(TaskSource::Stdin),
        (None, None) => {
            Err("no task: pass --task TEXT, --file PATH, or pipe the task on stdin".into())
        }
    }
}

/// Resolve the task text from `--light` args: `--file`/stdin are read as-is
/// (verbatim — quotes and newlines survive), so the driver can transport any
/// task. An empty task is a usage error.
pub fn light_task(args: &[String], stdin_piped: bool) -> Result<String, String> {
    let task = match parse_light_args(args, stdin_piped)? {
        TaskSource::Task(text) => text,
        TaskSource::File(path) => std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read task file `{path}`: {error}"))?,
        TaskSource::Stdin => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|error| format!("cannot read task from stdin: {error}"))?;
            text
        }
    };
    if task.is_empty() {
        return Err("task is empty".into());
    }
    Ok(task)
}

/// The `--light` entry point: resolve the task, attach, run the worker, and
/// print the sentinel. Exit 2 for usage problems and a missing gateway (the
/// same no-DSH_PORT message as the TUI path); exit 1 for run failures.
pub fn main_light(args: &[String]) -> Result<(), AppError> {
    let stdin_piped = !std::io::stdin().is_terminal();
    let task = match light_task(args, stdin_piped) {
        Ok(task) => task,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: dsh-tui --light [--task TEXT | --file PATH]");
            std::process::exit(2);
        }
    };
    // #34: the same resolution as the TUI path (CLI > env > config >
    // default); the worker never auto-starts (#35) — a dead port is the
    // no-gateway error.
    let locale = crate::i18n::Locale::detect(crate::theme::Config::load().locale.as_deref());
    let port = match crate::gateway::resolve_port(args) {
        Ok(port) => port,
        Err(crate::gateway::PortError::Invalid { value, source }) => {
            eprintln!(
                "{}",
                crate::i18n::trf(locale, "main.invalid_port", &[&value, &source])
            );
            std::process::exit(2);
        }
        Err(crate::gateway::PortError::MissingValue) => {
            eprintln!("{}", crate::i18n::tr(locale, "main.port_requires_value"));
            std::process::exit(2);
        }
    };
    if !crate::gateway::port_serving(port) {
        eprintln!(
            "{}",
            crate::i18n::trf(locale, "main.no_gateway", &[&port.to_string()])
        );
        std::process::exit(2);
    }
    let client = match WireClient::attach(port) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // The worker is committed (task resolved, gateway attached): report the
    // run to herdr when inside a pane. Reporting is fire-and-forget — a
    // missing/dead herdr never affects the worker.
    let reporter = HerdrReporter::from_env();
    if let Some(reporter) = &reporter {
        reporter.report("working", &task);
    }
    match runtime.block_on(run_light(client, task)) {
        Ok(()) => {
            if let Some(reporter) = &reporter {
                reporter.report("idle", "exit 0");
            }
            println!("dsh-worker: done");
            std::process::exit(0);
        }
        Err(error) => {
            if let Some(reporter) = &reporter {
                reporter.report("blocked", &error.to_string());
            }
            println!("dsh-worker: error: {error}");
            std::process::exit(1);
        }
    }
}

/// Run one worker turn: create a session, submit the task, fold the mux
/// stream into a store, print assistant text as nodes settle, and finish
/// when the turn completes ([`turn_complete`]).
pub async fn run_light(client: WireClient, task: String) -> Result<(), LightError> {
    // Subscribe BEFORE the prompt: the mux stream delivers the turn's
    // frames live (a late connection replays the durable baseline via
    // `session/subscribed`).
    let mut mux = client.mux_stream();
    let mut host = client.host_stream();
    let created = client.session_create(None, None, None, None).await?;
    let session_id = created.session_id;
    client
        .session_prompt(
            session_id.clone(),
            PromptMode::Queue,
            vec![PromptContentPart::Text { text: task }],
            None,
        )
        .await?;

    let mut store = SessionStore::new();
    // The summary running flag: a freshly created session is not running;
    // `host/session-status` keeps it live (the `App::session_running`
    // mirror — the summary half).
    let mut running = false;
    let mut printed: HashSet<String> = HashSet::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    loop {
        tokio::select! {
            maybe = mux.recv() => {
                let Some(downlink) = maybe else {
                    return Err(LightError::StreamClosed);
                };
                store.ingest(downlink.frame)?;
                print_settled_assistant_text(&store, &session_id, &mut printed, &mut out)?;
                if let Some(error) = store.last_stream_error.clone() {
                    return Err(LightError::StreamError(error));
                }
                if turn_complete(running, &store, &session_id) {
                    return finish(&store, &session_id);
                }
            }
            maybe = host.recv() => {
                let Some(downlink) = maybe else {
                    return Err(LightError::StreamClosed);
                };
                if let HostFrame::HostSessionStatus {
                    session_id: host_session,
                    running: is_running,
                } = downlink.frame
                    && host_session == session_id
                {
                    running = is_running;
                }
            }
        }
    }
}

/// Print the Text blocks of assistant nodes that have settled (finalized or
/// interrupted) and not been printed yet — one node per message, flushed
/// immediately so the driver sees progressive output.
fn print_settled_assistant_text(
    store: &SessionStore,
    session_id: &SessionId,
    printed: &mut HashSet<String>,
    out: &mut impl Write,
) -> Result<(), LightError> {
    let Some(state) = store.session(session_id) else {
        return Ok(());
    };
    for node in &state.nodes {
        let NodeData::Assistant {
            finalized,
            interrupted,
            blocks,
            ..
        } = &node.data
        else {
            continue;
        };
        if !(*finalized || *interrupted) || !printed.insert(node.key.clone()) {
            continue;
        }
        let text: String = blocks
            .iter()
            .filter_map(|block| match block {
                AssistantBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if !text.is_empty() {
            out.write_all(text.as_bytes())?;
            if !text.ends_with('\n') {
                out.write_all(b"\n")?;
            }
            out.flush()?;
        }
    }
    Ok(())
}

/// Whether the turn is complete — the `App::session_running` rule: the
/// summary running flag is false AND the node fold has no unsettled tail
/// ([`SessionState::has_unsettled_tail`], the shared fold half). The bare
/// prompt user node does not complete (the host running flag may lag the
/// user/message frame) unless the turn/end boundary closed an
/// otherwise-empty turn.
fn turn_complete(running: bool, store: &SessionStore, session_id: &SessionId) -> bool {
    if running {
        return false;
    }
    let Some(state) = store.session(session_id) else {
        return false;
    };
    if state.has_unsettled_tail() {
        return false;
    }
    match state.nodes.last().map(|node| &node.data) {
        Some(NodeData::User { .. }) => turn_ended(state),
        Some(_) => true,
        None => false,
    }
}

/// Whether the turn/end boundary is in the store's window — an empty
/// completed turn (no model content) still completes.
fn turn_ended(state: &SessionState) -> bool {
    state
        .events()
        .iter()
        .any(|stored| matches!(stored.data, EventData::TurnEnd { .. }))
}

/// Final verdict: a turn/end error is a failed turn, not a done one.
fn finish(store: &SessionStore, session_id: &SessionId) -> Result<(), LightError> {
    if let Some(NodeData::TurnError { message, .. }) = store
        .session(session_id)
        .and_then(|state| state.nodes.last())
        .map(|node| &node.data)
    {
        return Err(LightError::TurnFailed(message.clone()));
    }
    Ok(())
}
