//! Live-gateway smoke test (ticket 08 Q5): the REAL binary in a PTY against a
//! REAL `dsh web` gateway, auto-started by the test itself on a free port.
//!
//! ## Gate (keeps the default suite green without external infra)
//!
//! The test SKIPS cleanly (prints `[live_smoke] SKIPPED …`, returns ok)
//! unless `DSH_LIVE_SMOKE=1` is set. Run it with:
//!
//! ```text
//! DSH_LIVE_SMOKE=1 devbox run -- cargo test --test live_smoke -- --nocapture
//! ```
//!
//! The gateway binary is resolved as: `$DSH_BIN` → the devbox global profile
//! path → `dsh` on `$PATH`. The gateway is started with an ISOLATED `DSH_HOME`
//! temp dir (no pollution of the user's real store; the provider config is
//! global and survives isolation — verified `session.models.routable` stays
//! true). It is killed by an RAII guard on teardown (`kill` → scoped
//! `pkill -f "dsh web --port …"` → broad `pkill -f 'dsh web'` fallback), and
//! the test asserts the port is free afterwards.
//!
//! ## Provider handling (documented)
//!
//! Provider availability is probed first via `session.models` (`routable`).
//! - routable → the composer round-trip asserts the user echo, then waits
//!   (bounded, 120s) for the model's answer text; if the run is still in
//!   flight at the bound, the in-flight turn is CANCELLED and the cancel
//!   path is asserted instead (both are PASS outcomes).
//! - not routable → the round-trip asserts the user echo, then expects the
//!   graceful turn-error surface (`[turn error: …]` marker) — a PASS, not a
//!   failure. Attach/list/render/settings/theme/catalog always run.
//!
//! Every wait is bounded; no step hangs. PTY caveats mirror tests/e2e.rs:
//! output is escape-laden, assertions search substrings; the ratatui diff
//! renderer emits only CHANGED cells, so "fresh-bytes" searches use a mark
//! (see [`AppUnderTest::mark`]) and styled status labels are matched on raw
//! bytes (normalization strips SGR).
//!
//! Key bytes (crossterm 0.28 parse): Ctrl+P = 0x10, Ctrl+T = 0x14,
//! Ctrl+Q = 0x11, Ctrl+C = 0x03. Raw byte 0x0c decodes as Ctrl+L, NOT
//! Ctrl+, — the settings shortcut is unreachable from a raw PTY, so the
//! launcher's "open settings" action is the exercised path (documented
//! parity gap).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use portable_pty::{Child as PtyChild, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::json;

const GATE_ENV: &str = "DSH_LIVE_SMOKE";
/// Preferred gateway port; falls back to an OS-assigned free port if taken.
const PREFERRED_PORT: u16 = 18765;
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const DEVBOX_DSH: &str =
    "/home/rodrigo/.local/share/devbox/global/default/.devbox/nix/profile/default/bin/dsh";

/// Resolve the `dsh` gateway binary: `$DSH_BIN` wins, then the devbox global
/// profile path, then `dsh` from `$PATH`.
fn dsh_bin() -> String {
    if let Ok(bin) = std::env::var("DSH_BIN") {
        return bin;
    }
    if std::path::Path::new(DEVBOX_DSH).exists() {
        return DEVBOX_DSH.to_string();
    }
    "dsh".to_string()
}

/// Pick a free loopback port: the preferred one, else an OS-assigned one.
fn pick_port() -> u16 {
    if port_is_free(PREFERRED_PORT) {
        return PREFERRED_PORT;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind port 0");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

fn port_is_free(port: u16) -> bool {
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(300),
    )
    .is_err()
}

/// One spawned app instance: the PTY master, a reader thread draining the
/// merged stdout+stderr, and the child handle (same shape as tests/e2e.rs).
struct AppUnderTest {
    #[allow(dead_code)]
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn PtyChild + Send>,
    output: Arc<Mutex<Vec<u8>>>,
    _reader: std::thread::JoinHandle<()>,
}

impl AppUnderTest {
    /// Spawn the real binary with `DSH_PORT` + a deterministic env (Locale En,
    /// isolated XDG config, truecolor) in a `cols`×`rows` PTY.
    fn spawn(port: u16, cols: u16, rows: u16) -> Self {
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_dsh-tui"));
        cmd.env("DSH_PORT", port.to_string());
        cmd.env("DSH_TUI_LOCALE", "en");
        cmd.env("LANG", "en_US.UTF-8");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM", "xterm-256color");
        // Isolate from the host config (~/.config/dsh-tui) for determinism.
        let xdg = std::env::temp_dir().join(format!("dsh-tui-live-xdg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&xdg);
        cmd.env("XDG_CONFIG_HOME", &xdg);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let child = pair.slave.spawn_command(cmd).expect("spawn child");
        let writer = pair.master.take_writer().expect("pty writer");
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let buffer = Arc::clone(&output);
        let reader_thread = std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buffer
                        .lock()
                        .expect("output lock")
                        .extend_from_slice(&chunk[..n]),
                }
            }
        });
        AppUnderTest {
            master: pair.master,
            writer,
            child,
            output,
            _reader: reader_thread,
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to pty");
        self.writer.flush().expect("flush pty");
    }

    fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().expect("output lock")).into_owned()
    }

    /// Mark the current output length; [`AppUnderTest::since`] searches only
    /// bytes after the mark (the diff renderer never rewrites unchanged
    /// cells, so old frames would otherwise satisfy searches forever).
    fn mark(&self) -> usize {
        self.output.lock().expect("output lock").len()
    }

    /// The output bytes after `mark`.
    fn since(&self, mark: usize) -> Vec<u8> {
        let output = self.output.lock().expect("output lock");
        output.get(mark..).unwrap_or(&[]).to_vec()
    }

    /// Wait until `needle` appears in the NORMALIZED bytes after `mark`.
    fn wait_since(&self, mark: usize, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if normalize_output(&self.since(mark)).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        normalize_output(&self.since(mark)).contains(needle)
    }

    /// Wait until the RAW bytes after `mark` contain `needle` (for styled
    /// status labels, whose SGR sequences normalization would strip).
    fn wait_since_raw(&self, mark: usize, needle: &[u8], timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.since(mark).windows(needle.len()).any(|w| w == needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.since(mark).windows(needle.len()).any(|w| w == needle)
    }

    /// Wait until the NORMALIZED bytes after `mark` contain `needle` as a
    /// SUBSEQUENCE (chars in order, gaps allowed). The diff renderer emits
    /// changed runs of the status line fragmented by cursor moves and skips
    /// cells that align with the previous frame (e.g. the dashes of a uuid),
    /// so a long id never appears contiguously — but its characters always
    /// arrive in order.
    fn wait_since_subsequence(&self, mark: usize, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if contains_subsequence(&normalize_output(&self.since(mark)), needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        contains_subsequence(&normalize_output(&self.since(mark)), needle)
    }

    /// Wait until `needle` appears anywhere in the NORMALIZED output.
    fn wait_for(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if normalize_output(&self.output.lock().expect("output lock")).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        normalize_output(&self.output.lock().expect("output lock")).contains(needle)
    }

    /// The child's exit status if it already exited.
    fn try_exit(&mut self) -> Option<i32> {
        self.child
            .try_wait()
            .ok()
            .flatten()
            .map(|s| s.exit_code() as i32)
    }

    /// Clean shutdown: Ctrl+Q, then kill + reap on timeout. Returns the exit
    /// status when the child exited.
    fn quit(&mut self, timeout: Duration) -> Option<i32> {
        self.send(b"\x11");
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status.exit_code() as i32);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        None
    }
}

/// RAII guard for the gateway process: kills it on teardown (direct kill,
/// then scoped/broad `pkill` fallbacks), verifies the port is free, and
/// removes the isolated `DSH_HOME` and log files.
struct GatewayGuard {
    child: Option<Child>,
    port: u16,
    home: std::path::PathBuf,
    log: std::path::PathBuf,
}

impl GatewayGuard {
    fn start(port: u16) -> Self {
        let bin = dsh_bin();
        let home = std::env::temp_dir().join(format!("dsh-live-home-{}", std::process::id()));
        let log = std::env::temp_dir().join(format!("dsh-live-{}.log", std::process::id()));
        let _ = std::fs::create_dir_all(&home);
        let log_file = std::fs::File::create(&log).expect("create gateway log");
        let child = Command::new(&bin)
            .args(["web", "--port", &port.to_string()])
            .env("DSH_HOME", &home)
            .stdout(Stdio::from(log_file.try_clone().expect("clone log")))
            .stderr(Stdio::from(log_file))
            .spawn()
            .unwrap_or_else(|e| panic!("failed to start `{bin} web --port {port}`: {e}"));
        let guard = GatewayGuard {
            child: Some(child),
            port,
            home,
            log,
        };
        guard.wait_ready();
        guard
    }

    /// Poll until the gateway serves HTTP on the base URL (bounded; on
    /// timeout, dump the gateway log and panic).
    fn wait_ready(&self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            // The web server accepts connections before the API proxy
            // mounts its routes, so readiness = a valid `session.list`
            // server-response, not just an HTTP 200 on `/`.
            if api_ok(self.port, "session.list") {
                println!("[live_smoke] gateway ready on 127.0.0.1:{}", self.port);
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        panic!(
            "gateway did not become ready within {READY_TIMEOUT:?}; log tail:\n{}",
            log.chars()
                .rev()
                .take(1500)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        );
    }

    /// Kill the gateway and verify the port is free (bounded). Also used as
    /// the Drop teardown; panics only when called explicitly.
    fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.wait();
        }
        // Fallbacks for any orphaned listener (scoped first, broad last).
        for pattern in [
            format!("dsh web --port {}", self.port),
            "dsh web".to_string(),
        ] {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline && !port_is_free(self.port) {
                let _ = Command::new("pkill").args(["-f", &pattern]).status();
                std::thread::sleep(Duration::from_millis(200));
            }
            if port_is_free(self.port) {
                break;
            }
        }
        assert!(
            port_is_free(self.port),
            "gateway port {} still in use after teardown",
            self.port
        );
        let _ = std::fs::remove_dir_all(&self.home);
        let _ = std::fs::remove_file(&self.log);
    }
}

impl Drop for GatewayGuard {
    fn drop(&mut self) {
        // Best-effort teardown; `shutdown` is also called explicitly at the
        // end of the test so teardown failures surface as assertions.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new("pkill").args(["-f", "dsh web"]).status();
    }
}

/// Whether one RPC method answers with a `server-response` full form (the
/// API-proxy readiness signal; the web server serves `/` before the API
/// routes mount).
fn api_ok(port: u16, method: &str) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(500),
    ) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let body = format!(
        r#"{{"type":"client-request","rpcId":"probe-1","method":"{method}","payload":{{}}}}"#
    );
    let _ = write!(
        stream,
        "POST /api/{method} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut buf = [0u8; 512];
    let Ok(n) = stream.read(&mut buf) else {
        return false;
    };
    String::from_utf8_lossy(&buf[..n]).contains("server-response")
}

/// Normalize the raw PTY stream for substring search: strip ANSI CSI
/// sequences, replacing each with a space (same as tests/e2e.rs).
fn normalize_output(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(' ');
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() || n == '~' {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn check(step: &str, ok: bool, extra: &str) {
    println!("[{}] {step} — {}", if ok { "PASS" } else { "FAIL" }, extra);
    assert!(ok, "{step}: {extra}");
}

fn tail(app: &AppUnderTest, n: usize) -> String {
    app.output()
        .chars()
        .rev()
        .take(n)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

/// Whether `needle`'s characters appear in `haystack` in order (gaps
/// allowed). Used for diff-fragmented status-line ids (see
/// [`AppUnderTest::wait_since_subsequence`]).
fn contains_subsequence(haystack: &str, needle: &str) -> bool {
    let mut rest = haystack.chars();
    needle.chars().all(|c| rest.by_ref().any(|h| h == c))
}

/// Seed the gateway: one workspace session + one ungrouped session. Returns
/// (workspace session id, ungrouped session id, provider routable).
async fn seed_gateway(port: u16) -> (String, String, bool) {
    let client = dsh_tui::client::WireClient::attach(port).expect("attach wire client");
    let repo = std::env::current_dir()
        .expect("cwd")
        .to_string_lossy()
        .to_string();
    // The sidebar's workspace group: adopt the repo dir (tolerated if the
    // gateway already knows it — re-runs return the same workspace). Bounded
    // retry: the API proxy may still be mounting routes right after the
    // readiness probe.
    let mut ws_value: serde_json::Value = serde_json::Value::Null;
    for attempt in 0..10 {
        match client
            .call("workspace.create", json!({ "path": repo }))
            .await
        {
            Ok(value) => {
                ws_value = value;
                break;
            }
            Err(error) if attempt < 9 => {
                println!("[live_smoke] workspace.create retry {attempt}: {error}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(error) => panic!("workspace.create: {error}"),
        }
    }
    let ws_id = ws_value["workspace"]["workspaceId"]
        .as_str()
        .expect("workspaceId")
        .to_string();
    let ws_session = client
        .session_create(Some(ws_id.parse().unwrap()), None, None, None)
        .await
        .expect("session.create in workspace");
    let cwd_session = client
        .session_create(None, Some(repo), None, None)
        .await
        .expect("session.create with cwd");
    // Provider probe: `routable` tells whether a model route exists.
    let routable = client
        .session_models(cwd_session.session_id.clone())
        .await
        .map(|models| models.routable)
        .unwrap_or(false);
    println!(
        "[live_smoke] seeded ws-session {} cwd-session {} provider_routable={routable}",
        ws_session.session_id, cwd_session.session_id
    );
    (ws_session.session_id.0, cwd_session.session_id.0, routable)
}

#[tokio::test(flavor = "multi_thread")]
async fn live_gateway_smoke() {
    if std::env::var(GATE_ENV).is_err() {
        println!("[live_smoke] SKIPPED — set {GATE_ENV}=1 to run against a live `dsh web` gateway");
        return;
    }

    // ------------------------------------------------------------------
    // 0. gateway lifecycle: start on a free port, seed, RAII teardown.
    // ------------------------------------------------------------------
    let port = pick_port();
    let mut gateway = GatewayGuard::start(port);
    let (ws_session, cwd_session, provider_routable) = seed_gateway(port).await;

    let mut app = AppUnderTest::spawn(port, 140, 36);
    app.send(b"x"); // force first paint

    // ------------------------------------------------------------------
    // 1. attach handshake completes; status line names the resumed session
    //    (the most recently updated = the cwd session, seeded last).
    // ------------------------------------------------------------------
    check(
        "attach handshake",
        app.wait_for(
            &format!("attached to 127.0.0.1:{port}"),
            Duration::from_secs(20),
        ),
        &tail(&app, 400),
    );
    let resumed = cwd_session.clone();
    check(
        "resumed session in status",
        app.wait_for(&resumed, Duration::from_secs(10)),
        &tail(&app, 400),
    );

    // ------------------------------------------------------------------
    // 2. sidebar renders REAL workspace + session names. The sidebar
    //    truncates long ids to its ~20-col width, so assert id PREFIXES
    //    here (the status line below carries the full ids).
    // ------------------------------------------------------------------
    let ws_prefix: String = ws_session.chars().take(12).collect();
    let cwd_prefix: String = cwd_session.chars().take(12).collect();
    check(
        "sidebar workspace title",
        app.wait_for("dsh-tui", Duration::from_secs(10)),
        &tail(&app, 400),
    );
    check(
        "sidebar ws-session row",
        app.wait_for(&ws_prefix, Duration::from_secs(10)),
        &tail(&app, 400),
    );
    check(
        "sidebar cwd-session row",
        app.wait_for(&cwd_prefix, Duration::from_secs(10)),
        &tail(&app, 400),
    );

    // ------------------------------------------------------------------
    // 3. sidebar nav + Enter-switch: selection starts at row 0 (the
    //    workspace session); j moves to the cwd session (1), k back to 0,
    //    Enter switches the active session. The status line diff-renders
    //    only CHANGED cells: aligned unchanged chars (uuid dashes AND
    //    coincident hex digits) are skipped, so the emitted fragment is
    //    exactly the chars where the two ids differ — in order. Assert
    //    THAT string as a subsequence of the fresh bytes.
    // ------------------------------------------------------------------
    let ws_hex: String = ws_session
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    let cwd_hex: String = cwd_session
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    // Each direction emits the changed chars from ITS side of the diff.
    let ws_fragment: String = ws_hex
        .chars()
        .zip(cwd_hex.chars())
        .filter(|(w, c)| w != c)
        .map(|(w, _)| w)
        .collect();
    let cwd_fragment: String = cwd_hex
        .chars()
        .zip(ws_hex.chars())
        .filter(|(c, w)| c != w)
        .map(|(c, _)| c)
        .collect();
    let m = app.mark();
    app.send(b"\t"); // Chat -> Composer
    app.send(b"\t"); // Composer -> Sidebar
    app.send(b"j"); // 0 -> 1: cwd session
    app.send(b"k"); // 1 -> 0: workspace session
    app.send(b"\r"); // Enter: switch to the workspace session
    check(
        "Enter-switch to ws session",
        app.wait_since_subsequence(m, &ws_fragment, Duration::from_secs(10)),
        &tail(&app, 400),
    );
    let m = app.mark();
    app.send(b"j"); // 0 -> 1: cwd session
    app.send(b"\r"); // Enter: switch back to the cwd session
    check(
        "Enter-switch back to cwd session",
        app.wait_since_subsequence(m, &cwd_fragment, Duration::from_secs(10)),
        &tail(&app, 400),
    );

    // ------------------------------------------------------------------
    // 4. settings view via the launcher (Ctrl+, unreachable from a raw PTY
    //    — crossterm maps 0x0c to Ctrl+L; parity gap, documented).
    // ------------------------------------------------------------------
    let m = app.mark();
    app.send(b"\x10"); // Ctrl+P launcher (global key; works in sidebar focus)
    app.send(b"settings"); // filter: the action entry is below the fold
    check(
        "launcher filter",
        app.wait_since(m, "open settings", Duration::from_secs(10)),
        &tail(&app, 300),
    );
    app.send(b"\r"); // Enter: OpenSettings action
    check(
        "settings view live",
        app.wait_since(m, "ui-theme", Duration::from_secs(15)),
        &tail(&app, 500),
    );
    check(
        "settings live namespace",
        app.wait_since(m, "locale", Duration::from_secs(5)),
        &tail(&app, 300),
    );
    app.send(b"\x1b"); // Esc closes (no dirty edits)
    // Pacing: the settings fetch is async; let the draw settle before the
    // next key so the theme picker opens over the chat, not stale settings.
    std::thread::sleep(Duration::from_millis(600));

    // ------------------------------------------------------------------
    // 5. theme picker live.
    // ------------------------------------------------------------------
    let m = app.mark();
    app.send(b"\x14"); // Ctrl+T
    check(
        "theme picker open",
        app.wait_since(m, "catppuccin-mocha", Duration::from_secs(10)),
        &tail(&app, 300),
    );
    app.send(b"\r"); // Enter applies live + persists
    std::thread::sleep(Duration::from_millis(300));
    app.send(b"\x1b"); // Esc closes

    // ------------------------------------------------------------------
    // 6. Ctrl+P catalog with real skills; cancel flow (Esc, no run spawned).
    // ------------------------------------------------------------------
    let m = app.mark();
    app.send(b"\x10");
    check(
        "launcher real skill",
        app.wait_since(m, "agent-browser", Duration::from_secs(15)),
        &tail(&app, 500),
    );
    app.send(b"\x1b"); // Esc: close launcher without picking
    std::thread::sleep(Duration::from_millis(300)); // let the ESC decode alone
    let m = app.mark();
    app.send(b"\t"); // Sidebar -> Chat: only possible when the launcher closed
    // The status focus token is styled, so the raw stream is
    // `[36;95H<ESC...SGR>chat` — match raw bytes (normalization strips SGR).
    check(
        "launcher cancel flow",
        app.wait_since_raw(m, b"49mchat", Duration::from_secs(5)),
        &tail(&app, 300),
    );

    // ------------------------------------------------------------------
    // 7. composer input -> prompt round-trip.
    // ------------------------------------------------------------------
    let m = app.mark();
    app.send(b"\t"); // Chat -> Composer
    app.send(b"reply with exactly the word smokenumber42");
    // The composer echoes each char at its own cursor position (diff
    // renderer), so the phrase is never contiguous — check the raw stream
    // for the final typed char `2` at its positioned write.
    check(
        "composer echo",
        app.wait_since_raw(m, b"H2", Duration::from_secs(10)),
        &tail(&app, 400),
    );
    app.send(b"\r"); // submit
    check(
        "user message echo",
        app.wait_since(
            m,
            "reply with exactly the word smokenumber42",
            Duration::from_secs(15),
        ),
        &tail(&app, 400),
    );

    if provider_routable {
        // Real run: the model streams the answer. Mark AFTER the echo so the
        // answer needle can't match the prompt text itself. The diff renderer
        // fragments streamed text across frames, so match the answer as a
        // SUBSEQUENCE of the fresh bytes.
        let m = app.mark();
        let answered = app.wait_since_subsequence(m, "smokenumber42", Duration::from_secs(120));
        println!(
            "[{}] model answer within 120s — {}",
            if answered { "PASS" } else { "TIMEOUT" },
            tail(&app, 300)
        );
        if !answered {
            // Run still in flight at the bound: cancel it (the cancel path
            // is the asserted outcome). Ctrl+C cancels a running turn; if
            // the turn already ended it quits — accept either, then re-quit.
            app.send(b"\x03");
            std::thread::sleep(Duration::from_millis(700));
            app.send(b"x"); // repaint
            if app.try_exit().is_none() {
                check(
                    "cancel flow no crash",
                    app.wait_for("cancelled", Duration::from_secs(10))
                        || app.wait_for("type a message", Duration::from_secs(5)),
                    &tail(&app, 400),
                );
            } else {
                println!("[live_smoke] turn had ended; Ctrl+C quit cleanly (exit 0 path)");
            }
        }
    } else {
        // No provider: the gateway accepts the prompt but the run fails —
        // the graceful turn-error surface is the PASS condition.
        let m = app.mark();
        check(
            "graceful turn error (no provider)",
            app.wait_since(m, "[turn error", Duration::from_secs(60)),
            &tail(&app, 400),
        );
    }

    // ------------------------------------------------------------------
    // 8. clean quit.
    // ------------------------------------------------------------------
    let status = app.quit(Duration::from_secs(10));
    check("clean quit", status == Some(0), &format!("exit={status:?}"));
    check(
        "no panic",
        !app.output().contains("panicked"),
        "output must not contain panicked",
    );

    gateway.shutdown();
    println!("LIVE SMOKE COMPLETE");
}
