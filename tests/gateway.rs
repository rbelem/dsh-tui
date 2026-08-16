//! Gateway lifecycle tests (#34/#35): port-resolution precedence, the
//! invalid-source naming, and spawn/stop against a fake gateway binary
//! (`tests/fixtures/fake-gateway.sh` via the `DSH_TUI_GATEWAY_BIN` seam).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use dsh_tui::gateway::{self, PortError};

/// Serializes env mutations across the gateway tests (the repo's
/// ENV_LOCK pattern).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A unique high port per test (the 18766–18799 range, like live_smoke).
static PORT_COUNTER: Mutex<u16> = Mutex::new(18766);

fn next_port() -> u16 {
    let mut counter = PORT_COUNTER.lock().expect("port counter");
    loop {
        *counter += 1;
        // Skip ports still held by a leaked fake from a failed prior run.
        if !dsh_tui::gateway::port_serving(*counter) {
            return *counter;
        }
    }
}

/// Isolated XDG dirs for one test (config + state), returned as env pairs.
fn isolated_dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("dsh-tui-gw-{tag}-{}", std::process::id()));
    let state = root.join("state");
    let _ = std::fs::create_dir_all(root.join("dsh-tui"));
    let _ = std::fs::create_dir_all(state.join("dsh-tui"));
    (root, state)
}

fn with_env(key: &str, value: Option<&str>, f: impl FnOnce()) {
    let previous = std::env::var_os(key);
    // SAFETY: env mutation is serialized by ENV_LOCK across these tests.
    match value {
        Some(value) => unsafe { std::env::set_var(key, value) },
        None => unsafe { std::env::remove_var(key) },
    }
    f();
    // Restore — a leaked env var (e.g. DSH_PORT from the invalid-value
    // test) would corrupt parallel tests' resolutions.
    match previous {
        Some(previous) => unsafe { std::env::set_var(key, previous) },
        None => unsafe { std::env::remove_var(key) },
    }
}

/// The fixture script (checked in, +x); skips when python3 is unavailable.
fn fake_bin() -> Option<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fake-gateway.sh"
    );
    if std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        Some(path.to_string())
    } else {
        None
    }
}

/// Stops the gateway on drop — panic-safe cleanup for the fake-spawning
/// tests (a leaked fake holds its port and breaks the next run).
struct GatewayGuard {
    active: bool,
}

impl Drop for GatewayGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = gateway::server_stop(&[]);
        }
    }
}

// ---------------------------------------------------------------------------
// #34: resolution precedence
// ---------------------------------------------------------------------------

#[test]
fn resolution_precedence_cli_env_config_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (config_root, _state) = isolated_dirs("precedence");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            std::fs::write(
                config_root.join("dsh-tui/config.toml"),
                "[gateway]\nport = 4003\nauto_start = true\n",
            )
            .expect("config");

            // 5. unset → config value.
            with_env("DSH_PORT", None, || {
                assert_eq!(gateway::resolve_port(&[]).unwrap(), 4003);
            });
            // 4. env beats config.
            with_env("DSH_PORT", Some("4002"), || {
                assert_eq!(gateway::resolve_port(&[]).unwrap(), 4002);
            });
            // 3. CLI `--port <p>` beats env.
            with_env("DSH_PORT", Some("4002"), || {
                assert_eq!(
                    gateway::resolve_port(&["--port".into(), "4001".into()]).unwrap(),
                    4001
                );
            });
            // 2. CLI `--port=<p>` form.
            with_env("DSH_PORT", Some("4002"), || {
                assert_eq!(
                    gateway::resolve_port(&["--port=4000".into()]).unwrap(),
                    4000
                );
            });
        },
    );

    // 1. everything unset → the 3080 default.
    let (config_root, _state) = isolated_dirs("default");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            with_env("DSH_PORT", None, || {
                assert_eq!(gateway::resolve_port(&[]).unwrap(), gateway::DEFAULT_PORT);
            });
        },
    );
}

#[test]
fn invalid_values_name_the_source() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for (value, expected) in [("abc", "--port"), ("0", "--port"), ("70000", "--port")] {
        let error = gateway::resolve_port(&[format!("--port={value}")]).unwrap_err();
        assert_eq!(
            error,
            PortError::Invalid {
                value: value.to_string(),
                source: expected.to_string(),
            },
            "CLI source named for {value}"
        );
    }
    with_env("DSH_PORT", Some("abc"), || {
        let error = gateway::resolve_port(&[]).unwrap_err();
        assert_eq!(
            error,
            PortError::Invalid {
                value: "abc".to_string(),
                source: "DSH_PORT".to_string(),
            },
            "env source named"
        );
    });
    // A dangling --port errors (no silent fall-through).
    assert_eq!(
        gateway::resolve_port(&["--port".into()]).unwrap_err(),
        PortError::MissingValue,
        "dangling --port"
    );
}

#[test]
fn config_auto_start_defaults_true() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (config_root, _state) = isolated_dirs("auto");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            let config = dsh_tui::theme::Config::load();
            assert!(config.gateway.auto_start, "default on");
            assert_eq!(config.gateway.port, None);
        },
    );
    std::fs::write(
        config_root.join("dsh-tui/config.toml"),
        "[gateway]\nauto_start = false\n",
    )
    .expect("config");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            assert!(!dsh_tui::theme::Config::load().gateway.auto_start);
        },
    );
}

// ---------------------------------------------------------------------------
// #35: spawn + stop against the fake gateway
// ---------------------------------------------------------------------------

#[test]
fn spawn_serves_the_port_and_stop_cleans_up() {
    let Some(bin) = fake_bin() else {
        eprintln!("skipping: python3 unavailable for the fake gateway");
        return;
    };
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let port = next_port();
    let (config_root, state) = isolated_dirs("spawn");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            with_env("XDG_STATE_HOME", Some(state.to_str().unwrap()), || {
                with_env("DSH_TUI_GATEWAY_BIN", Some(&bin), || {
                    gateway::spawn_gateway(port).expect("spawn");
                    let mut guard = GatewayGuard { active: true };
                    assert!(gateway::port_serving(port), "the fake serves");
                    assert!(gateway::gateway_pid_path().exists(), "pid file written");
                    assert!(gateway::gateway_log_path().exists(), "log written");

                    // The pid file names a live process.
                    let pid: i32 = std::fs::read_to_string(gateway::gateway_pid_path())
                        .expect("pid")
                        .trim()
                        .parse()
                        .expect("pid number");
                    assert!(
                        std::process::Command::new("kill")
                            .arg("-0")
                            .arg(pid.to_string())
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false)
                    );

                    // Stop: SIGTERM + cleanup (the CLI port keeps the
                    // verification deterministic).
                    assert_eq!(
                        gateway::server_stop(&["--port".into(), port.to_string()]),
                        0,
                        "stop exits 0"
                    );
                    guard.active = false;
                    assert!(!gateway::gateway_pid_path().exists(), "pid removed");
                    // The fake dies (its port stops serving within the wait).
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while gateway::port_serving(port) && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    assert!(!gateway::port_serving(port), "gateway stopped");
                });
            });
        },
    );
}

#[test]
fn spawn_race_heals_to_a_serving_winner() {
    // A raw listener on the port: the spawned fake fails to bind and
    // dies (EADDRINUSE), but the probe sees the winner — spawn succeeds.
    let Some(bin) = fake_bin() else {
        eprintln!("skipping: python3 unavailable for the fake gateway");
        return;
    };
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let port = next_port();
    let (config_root, state) = isolated_dirs("serving");
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            with_env("XDG_STATE_HOME", Some(state.to_str().unwrap()), || {
                with_env("DSH_TUI_GATEWAY_BIN", Some(&bin), || {
                    gateway::spawn_gateway(port).expect("race heals to the winner");
                    // The fake is still initializing when the probe succeeds —
                    // it can bind AFTER the listener drops below. Stop it
                    // explicitly (the guard covers the panic path).
                    let mut guard = GatewayGuard { active: true };
                    drop(listener);
                    assert_eq!(
                        gateway::server_stop(&["--port".into(), port.to_string()]),
                        0,
                        "stop the late-binding fake"
                    );
                    assert!(!gateway::port_serving(port), "fake stopped");
                    guard.active = false;
                });
            });
        },
    );
}

#[test]
fn server_stop_stale_pid_reads_as_no_gateway() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (config_root, state) = isolated_dirs("stale");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            with_env("XDG_STATE_HOME", Some(state.to_str().unwrap()), || {
                // A stale pid file (the port is dead): "no gateway", exit
                // 0. The CLI port keeps the verification deterministic —
                // the config/env can race with other test files' writes.
                std::fs::write(gateway::gateway_pid_path(), "999999").expect("pid");
                let stale_port = next_port();
                assert_eq!(
                    gateway::server_stop(&["--port".into(), stale_port.to_string()]),
                    0,
                    "stale pid → no gateway"
                );
                assert!(
                    !gateway::gateway_pid_path().exists(),
                    "stale pid cleaned up"
                );
            });
        },
    );
}

/// #35 review: a stale pid with a LIVE port (the loser of a spawn race
/// overwrote the winner's pid) — the kill kills nothing, the port still
/// serves → "not ours", exit 1, and the pid file still cleans up.
#[test]
fn server_stop_stale_pid_with_a_serving_port_reads_as_not_ours() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let port = next_port();
    let (config_root, state) = isolated_dirs("stale-serving");
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            with_env("XDG_STATE_HOME", Some(state.to_str().unwrap()), || {
                // A stale pid (an impossible pid that could never be
                // alive) plus a serving port the pid file doesn't own.
                std::fs::write(gateway::gateway_pid_path(), "999999").expect("pid");
                assert_eq!(
                    gateway::server_stop(&["--port".into(), port.to_string()]),
                    1,
                    "stale pid + serving port → not ours"
                );
                assert!(
                    !gateway::gateway_pid_path().exists(),
                    "the stale pid still cleans up"
                );
            });
        },
    );
    drop(listener);
}

/// #35 review: `--port 4000 server stop` probes 4000, not the default.
#[test]
fn server_stop_honors_cli_port_args() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let port = next_port();
    let (config_root, state) = isolated_dirs("stop-cli");
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            with_env("XDG_STATE_HOME", Some(state.to_str().unwrap()), || {
                // No pid file; the port serves. `server stop --port <p>`
                // must probe <p> (exit 1 "not ours") — with the pre-fix
                // `&[]` it probed 3080 and misreported "no gateway" (0).
                assert_eq!(
                    gateway::server_stop(&[
                        "server".into(),
                        "stop".into(),
                        "--port".into(),
                        port.to_string(),
                    ]),
                    1,
                    "the stop probes the CLI port"
                );
            });
        },
    );
    drop(listener);
}

#[test]
fn server_stop_unknown_gateway_message_path() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let port = next_port();
    let (config_root, state) = isolated_dirs("unknown");
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            with_env("XDG_STATE_HOME", Some(state.to_str().unwrap()), || {
                // The port serves but no pid file: exit 1 (the user stops it).
                let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind");
                assert_eq!(
                    gateway::server_stop(&["--port".into(), port.to_string()]),
                    1,
                    "not ours → exit 1"
                );
                drop(listener);
            });
        },
    );
}

/// #35 acceptance 1 (binary level): the real binary spawns the gateway via
/// the injection seam — "starting gateway…" on stderr, pid+log files
/// created, the fake serves. The fake speaks no HTTP, so the app's
/// session.list RPC fails after attach (exit 1) — the spawn + probe +
/// attach path is what's exercised; `server stop` then cleans up.
#[test]
fn real_binary_spawns_the_gateway_via_the_injection_seam() {
    let Some(bin) = fake_bin() else {
        eprintln!("skipping: python3 unavailable for the fake gateway");
        return;
    };
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let port = next_port();
    let (config_root, state) = isolated_dirs("bin");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_dsh-tui"));
    cmd.env("DSH_PORT", port.to_string())
        .env("DSH_TUI_GATEWAY_BIN", &bin)
        .env("XDG_CONFIG_HOME", config_root.to_str().unwrap())
        .env("XDG_STATE_HOME", state.to_str().unwrap())
        .env("DSH_TUI_LOCALE", "en")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn binary");
    let mut stderr = child.stderr.take().expect("stderr");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        use std::io::Read;
        let _ = stderr.read_to_string(&mut text);
        text
    });
    // The binary exits on its own (the attach RPC fails against the fake).
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the binary hung (spawn/poll/attach path)");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stderr = reader.join().expect("reader");
    // All state-path reads happen under the same isolated env the binary
    // wrote into.
    with_env(
        "XDG_CONFIG_HOME",
        Some(config_root.to_str().unwrap()),
        || {
            with_env("XDG_STATE_HOME", Some(state.to_str().unwrap()), || {
                let mut guard = GatewayGuard { active: true };
                assert!(
                    stderr.contains(&format!("starting gateway on 127.0.0.1:{port}")),
                    "starting message on stderr: {stderr}"
                );
                assert!(
                    gateway::gateway_pid_path().exists(),
                    "pid file created by the binary"
                );
                assert!(
                    gateway::gateway_log_path().exists(),
                    "log file created by the binary"
                );
                // The spawned fake serves (the binary attached to it).
                assert!(gateway::port_serving(port), "the spawned gateway serves");
                assert!(
                    status.code() == Some(1) || status.code() == Some(101),
                    "the app exits with an error against the HTTP-less fake: {status:?}"
                );
                // Cleanup: the gateway persists after the TUI exit — stop
                // it (the CLI port keeps the verification deterministic).
                assert_eq!(
                    gateway::server_stop(&["--port".into(), port.to_string()]),
                    0,
                    "stop cleans up"
                );
                assert!(!gateway::port_serving(port), "gateway stopped");
                guard.active = false;
            });
        },
    );
}
