//! Gateway lifecycle (#34/#35): port resolution, lazy auto-start, and the
//! `server stop` subcommand.
//!
//! Resolution precedence: `--port` CLI > `DSH_PORT` env > `[gateway] port`
//! config > [`DEFAULT_PORT`] (3080 — the dsh web profile's composed
//! default; the README's `DSH_PORT=8765` example is stale).
//!
//! Auto-start (herdr model): when the resolved port isn't serving, dsh-tui
//! spawns `dsh web` itself — detached (own process group, stdin /dev/null),
//! stdout+stderr to `$XDG_STATE_HOME/dsh-tui/gateway.log`, PID to
//! `gateway.pid` — and polls the loopback probe until ready (~30s). The
//! gateway PERSISTS after the TUI exits; only `dsh-tui server stop` (or the
//! user) stops it. `--light` never auto-starts. `DSH_TUI_GATEWAY_BIN`
//! overrides the binary to spawn (the documented test/injection seam;
//! prod defaults to `dsh` on PATH).
//!
//! Concurrency: two racing launches self-heal — the loser's `dsh web`
//! exits on EADDRINUSE and its poll attaches to the winner's gateway; the
//! pid file is last-writer-wins (documented, harmless: the winner's file
//! only matters for `server stop`, and a stale pid with a live port reads
//! as "not ours").

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The dsh web profile's composed default port (harness
/// `examples/web-cordis/cordis.yml` pins the demo off it).
pub const DEFAULT_PORT: u16 = 3080;

/// The poll budget for a spawned gateway to start serving.
const SPAWN_POLL_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// resolution (#34)
// ---------------------------------------------------------------------------

/// The `--port <p>` / `--port=<p>` CLI value, hand-rolled like `--light`
/// (no new deps). A dangling `--port` (no value) is an error — the TUI
/// path no longer falls through silently.
fn cli_port(args: &[String]) -> Result<Option<String>, PortError> {
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--port" => {
                return match args.get(index + 1) {
                    Some(value) => Ok(Some(value.clone())),
                    None => Err(PortError::MissingValue),
                };
            }
            arg if arg.starts_with("--port=") => {
                return Ok(Some(arg["--port=".len()..].to_string()));
            }
            _ => {}
        }
        index += 1;
    }
    Ok(None)
}

/// A bad `--port`/`DSH_PORT` value: invalid (naming the source) or a
/// dangling `--port` with no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    Invalid { value: String, source: String },
    MissingValue,
}

/// Resolve the gateway port: CLI > env > config > default. Invalid values
/// (non-numeric, 0, >65535) error naming the source.
pub fn resolve_port(args: &[String]) -> Result<u16, PortError> {
    if let Some(value) = cli_port(args)? {
        return parse_port(&value, "--port");
    }
    if let Ok(value) = std::env::var("DSH_PORT") {
        return parse_port(&value, "DSH_PORT");
    }
    if let Some(port) = crate::theme::Config::load().gateway.port {
        // The config value is serde-typed u16: out-of-range/non-numeric
        // values fail the config parse and degrade to defaults (the
        // existing corrupt-config contract), so nothing to name here.
        return Ok(port);
    }
    Ok(DEFAULT_PORT)
}

fn parse_port(value: &str, source: &str) -> Result<u16, PortError> {
    match value.parse::<u16>() {
        Ok(0) => Err(PortError::Invalid {
            value: value.to_string(),
            source: source.to_string(),
        }),
        Ok(port) => Ok(port),
        Err(_) => Err(PortError::Invalid {
            value: value.to_string(),
            source: source.to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// probe + state paths (#35)
// ---------------------------------------------------------------------------

/// Is something listening on `127.0.0.1:port`?
pub fn port_serving(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// `$XDG_STATE_HOME/dsh-tui` (default `~/.local/state/dsh-tui`).
fn state_dir() -> PathBuf {
    let xdg = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty());
    let base = xdg
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"));
    base.join("dsh-tui")
}

/// The gateway's log file (`gateway.log` in the state dir).
pub fn gateway_log_path() -> PathBuf {
    state_dir().join("gateway.log")
}

/// The gateway's pid file (`gateway.pid` in the state dir).
pub fn gateway_pid_path() -> PathBuf {
    state_dir().join("gateway.pid")
}

/// The binary to spawn: `DSH_TUI_GATEWAY_BIN` (test/injection seam), else
/// `dsh` on PATH.
fn gateway_bin() -> String {
    std::env::var("DSH_TUI_GATEWAY_BIN").unwrap_or_else(|_| "dsh".into())
}

/// Spawn `dsh web --host 127.0.0.1 --port <port>` detached, write the pid
/// file, and poll the probe until the port serves (or the spawned process
/// dies, or ~30s elapse). A dead spawn is a race-heal: if the port serves
/// anyway (a concurrent winner), the spawn succeeds.
pub fn spawn_gateway(port: u16) -> Result<(), String> {
    let log = gateway_log_path();
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let log_file =
        std::fs::File::create(&log).map_err(|e| format!("open {}: {e}", log.display()))?;
    let bin = gateway_bin();
    let mut child = Command::new(&bin)
        .arg("web")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            log_file.try_clone().map_err(|e| e.to_string())?,
        ))
        .stderr(Stdio::from(log_file))
        .process_group(0) // detached: own group, survives the TUI's exit
        .spawn()
        .map_err(|e| format!("spawn `{bin}`: {e}"))?;
    let pid = child.id();
    std::fs::write(gateway_pid_path(), pid.to_string())
        .map_err(|e| format!("write pid file: {e}"))?;

    let deadline = Instant::now() + SPAWN_POLL_TIMEOUT;
    while Instant::now() < deadline {
        if port_serving(port) {
            return Ok(());
        }
        // The spawned process died (EADDRINUSE or a bad binary): fail
        // early with the log path — unless the port serves (the race
        // heal above).
        if let Ok(Some(code)) = child.try_wait() {
            return Err(format!(
                "`{bin}` exited with {code} before serving — see {}",
                log.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "gateway did not serve on 127.0.0.1:{port} within {}s — see {}",
        SPAWN_POLL_TIMEOUT.as_secs(),
        log.display()
    ))
}
/// `dsh-tui server stop`: SIGTERM the pid'd gateway, wait for it to die,
/// clean up — and VERIFY the stop: "stopped"/exit 0 only when the port is
/// actually dead. Exit codes: 0 stopped / no gateway, 1 the port serves
/// but wasn't stopped here (a stale pid — e.g. the loser of a spawn race
/// overwrote the winner's — kills nothing, so the winner's gateway reads
/// as "not ours, stop it yourself").
pub fn server_stop(args: &[String]) -> i32 {
    let locale = crate::i18n::Locale::detect(crate::theme::Config::load().locale.as_deref());
    let pid_path = gateway_pid_path();
    // The raw args ride through: `--port 4000 server stop` must probe
    // 4000, not the default.
    let port = resolve_port(args).unwrap_or(DEFAULT_PORT);
    let pid = read_pid(&pid_path);
    if let Some(pid) = pid {
        // SIGTERM via the system kill(1) (no new deps).
        let killed = Command::new("kill")
            .arg(pid.to_string())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if killed {
            // Wait (bounded) for the process to go away — the port dying
            // is the observable, kill -0 the fallback.
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if !pid_alive(pid) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    let _ = std::fs::remove_file(&pid_path);
    if port_serving(port) {
        // #35 review: the stop must be verified — the kill failed (stale
        // pid) or the process ignored SIGTERM, and something still
        // serves. Not ours to stop.
        eprintln!(
            "{}",
            crate::i18n::trf(locale, "gateway.not_ours", &[&port.to_string()])
        );
        return 1;
    }
    if pid.is_some() {
        eprintln!("{}", crate::i18n::tr(locale, "gateway.stopped"));
    } else {
        eprintln!("{}", crate::i18n::tr(locale, "gateway.none"));
    }
    0
}

fn read_pid(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
}

/// Is a process with `pid` alive? (`kill -0`.)
fn pid_alive(pid: i32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
