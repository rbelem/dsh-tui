//! Level-3 e2e harness (ticket 08): the REAL binary in a PTY against the
//! in-process mock gateway. Each scenario maps to a PARITY.md row (see the
//! `## e2e coverage` section in PARITY.md).
//!
//! PTY caveats: the app runs raw mode + alternate screen, so the captured
//! output is escape-laden — assertions search for plain text substrings in
//! the raw bytes, never exact buffer equality. The attach log goes to
//! stderr, which the PTY master merges with stdout. Keys are sent as raw
//! bytes (Ctrl+C = \x03, Ctrl+Q = \x11, Ctrl+T = \x14, Enter = \r).

mod common;
use common::{MockAction, MockGateway};

use serde_json::json;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// One spawned app instance: the PTY master, a reader thread draining the
/// merged stdout+stderr, and the child handle.
struct AppUnderTest {
    #[allow(dead_code)]
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send>,
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
        let xdg = std::env::temp_dir().join(format!("dsh-tui-e2e-xdg-{}", std::process::id()));
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
        // portable-pty 0.8: the child spawns from the slave side.
        let child = pair.slave.spawn_command(cmd).expect("spawn child");
        // The writer is taken once (taking it again panics) and held for the
        // app's lifetime; dropping it sends EOF to the slave.
        let writer = pair.master.take_writer().expect("pty writer");
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let buffer = Arc::clone(&output);
        let reader_thread = std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    // macOS pty masters are nonblocking (BSD) and the dup'd
                    // reader inherits O_NONBLOCK: EAGAIN is a retry, not EOF.
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(ref e) => {
                        // Record the death so failed waits self-diagnose.
                        buffer
                            .lock()
                            .expect("output lock")
                            .extend_from_slice(format!("\n[pty-reader died: {e}]").as_bytes());
                        break;
                    }
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

    /// Send raw key bytes (e.g. `b"hello"`, `b"\r"`, `b"\x03"`, `b"\x11"`).
    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to pty");
        self.writer.flush().expect("flush pty");
    }

    /// The accumulated raw output (escape-laden; search plain substrings).
    fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().expect("output lock")).into_owned()
    }

    /// Wait until `needle` appears in the NORMALIZED output or the deadline
    /// passes (see [`normalize_output`]).
    fn wait_for(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if normalize_output(&self.output.lock().expect("output lock")).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        normalize_output(&self.output.lock().expect("output lock")).contains(needle)
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
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        None
    }
}

/// Normalize the raw PTY stream for substring search: strip ANSI CSI
/// sequences, replacing each with a space. The ratatui diff renderer skips
/// unchanged (space) cells, so contiguous on-screen text is split by
/// cursor-move escapes; this reconstructs the plain text. Styled runs add
/// extra spaces at SGR boundaries — harmless for `contains` on a phrase.
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

/// One e2e scenario: a mock gateway plus a booted app, torn down on drop.
struct Scenario {
    mock: MockGateway,
    app: AppUnderTest,
}

impl Scenario {
    async fn boot(mock: MockGateway) -> Self {
        let mut app = AppUnderTest::spawn(mock.port(), 120, 30);
        // Nudge: the app draws only on events; a no-op key forces the first
        // paint. Ctrl+O (0x0F) is unbound in every surface — inert — while
        // plain letters type into the composer (the app boots focused
        // there), so `x` no longer qualifies as a nudge.
        app.send(b"\x0f");
        Scenario { mock, app }
    }

    fn app(&mut self) -> &mut AppUnderTest {
        &mut self.app
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        // Best-effort teardown: clean quit, else kill.
        let _ = self.app.quit(Duration::from_secs(3));
    }
}

/// Wait for the attach log + session content after boot (shared preamble).
async fn boot_and_attach(history_s_a: &str, history_s_b: &str) -> Scenario {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[
                {"sessionId":"sA","updatedAt":200.0,"running":false,"blank":false},
                {"sessionId":"sB","updatedAt":100.0,"running":false,"blank":false}
            ]}}}"#,
        ),
    )
    .await;
    mock.set_history("sA", history_s_a).await;
    mock.set_history("sB", history_s_b).await;
    Scenario::boot(mock).await
}

fn history_template(id: &str, text: &str) -> String {
    format!(
        r#"{{"type":"server-response","rpcId":"{{rpcId}}","result":{{"ok":true,"value":{{"events":[
            {{"event":{{"type":"user/message","seq":1,"time":1.0,"data":{{"id":"{id}","role":"user","content":[{{"type":"text","text":"{text}"}}],"source":{{"kind":"user"}}}}}}}}
        ],"hasMore":false}}}}}}"#
    )
}

// ---------------------------------------------------------------------------
// 1. attach + resume (PARITY rows: session list/resume, acceptance: resume
//    seamlessly across surfaces)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn attach_resumes_most_recent_session() {
    let mut scenario = boot_and_attach(
        &history_template("mA", "resumed from session A"),
        &history_template("mB", "session B content"),
    )
    .await;
    let app = scenario.app();

    // The attach log line (stderr, merged into the PTY stream).
    assert!(
        app.wait_for("attached to 127.0.0.1:", Duration::from_secs(10)),
        "attach log: {}",
        app.output()
    );
    // The resumed session's history renders in the chat.
    assert!(
        app.wait_for("resumed from session A", Duration::from_secs(10)),
        "history text: {}",
        app.output()
    );
    // The status line names the resumed session (sA is the most recent).
    assert!(
        app.wait_for("session sA", Duration::from_secs(5)),
        "status line: {}",
        app.output()
    );
    // The sidebar lists both sessions.
    assert!(
        app.wait_for("sB", Duration::from_secs(5)),
        "sidebar shows the second session: {}",
        app.output()
    );
}

// ---------------------------------------------------------------------------
// 2. + 3. prompt submit + streaming render (PARITY rows: composer prompt,
//    streaming chat rows)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn prompt_submit_streams_response() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[
                {"sessionId":"sA","updatedAt":200.0,"running":false,"blank":false}
            ]}}}"#,
        ),
    )
    .await;
    mock.set_history("sA", &history_template("mA", "welcome"))
        .await;
    mock.set_handler(
        "session.prompt",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"accepted":true}}}"#,
        ),
    )
    .await;
    // The streamed response is pushed once the prompt lands.
    mock.set_ws_frames(
        "/api/events.mux",
        vec![mux_frame(mux_event(
            "sA",
            2,
            "assistant/chunk",
            json!({"turn": 1, "step": 1, "chunk": {"type": "block-start", "index": 0, "blockType": "text"}}),
        ))],
    )
    .await;
    let mut scenario = Scenario::boot(mock).await;
    {
        let app = scenario.app();
        assert!(
            app.wait_for("attached to 127.0.0.1:", Duration::from_secs(10)),
            "attach"
        );
        // The app boots in the composer (input area): type and submit.
        app.send(b"hello e2e");
        app.send(b"\r");
    }
    // The streamed assistant text renders (the mock pushes the chunk shortly
    // after connect; the final assistant/message follows via push).
    tokio::time::sleep(Duration::from_millis(200)).await;
    scenario
        .mock
        .push_ws_frame(
            "/api/events.mux",
            mux_frame(mux_event(
                "sA",
                3,
                "assistant/chunk",
                json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "Hello from "}}),
            )),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    scenario
        .mock
        .push_ws_frame(
            "/api/events.mux",
            mux_frame(mux_event(
                "sA",
                4,
                "assistant/chunk",
                json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "the mock"}}),
            )),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    scenario
        .mock
        .push_ws_frame(
            "/api/events.mux",
            mux_frame(mux_event(
                "sA",
                5,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {"id": "m2", "role": "assistant", "content": [{"type": "text", "text": "Hello from the mock"}], "source": {"kind": "model", "provider": "p", "model": "m"}},
                }),
            )),
        )
        .await;

    {
        let app = scenario.app();
        // The diff renderer repaints only changed cells per frame, so a
        // phrase streamed across frames is split in the byte stream — assert
        // the final frame's segment (the tail of the streamed text).
        assert!(
            app.wait_for("the mock", Duration::from_secs(10)),
            "streamed assistant text: {}",
            app.output()
        );
    }
    // The mock captured the prompt: text + queue mode.
    let requests = scenario.mock.requests().await;
    let prompt = requests
        .iter()
        .find(|request| request.path == "/api/session.prompt")
        .expect("session.prompt POST");
    let body: serde_json::Value = serde_json::from_str(&prompt.body).expect("json");
    assert_eq!(body["payload"]["content"][0]["text"], "hello e2e");
    assert_eq!(body["payload"]["mode"], "queue");
}

// ---------------------------------------------------------------------------
// 4. approval takeover (PARITY row: approvals)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn approval_takeover_answers_with_echoed_rpc_id() {
    let mut scenario = boot_and_attach(
        &history_template("mA", "welcome"),
        &history_template("mB", "other"),
    )
    .await;
    mock_respond_ok(&scenario.mock).await;
    {
        let app = scenario.app();
        assert!(
            app.wait_for("attached to 127.0.0.1:", Duration::from_secs(10)),
            "attach"
        );
    }

    // The gateway triggers an approval after boot. The mock's pusher
    // registers when the app's mux subscriber connects — poll the push
    // under parallel load instead of asserting a single delivery.
    let frame: String = r#"{"type":"server-request","rpcId":"rpc-e2e-1","method":"events.mux","payload":{"type":"approval/requested","sessionId":"sA","approvalId":"a-e2e","toolName":"read_file","callId":"call-1","reason":"reads /etc"}}"#.into();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if scenario
            .mock
            .push_ws_frame("/api/events.mux", frame.clone())
            .await
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "approval frame never delivered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The takeover shows the tool name and the action hints.
    {
        let app = scenario.app();
        assert!(
            app.wait_for("read_file", Duration::from_secs(10)),
            "tool name: {}",
            app.output()
        );
        assert!(
            app.wait_for("allow once", Duration::from_secs(5)),
            "y hint: {}",
            app.output()
        );
        assert!(
            app.wait_for("reject", Duration::from_secs(5)),
            "n hint: {}",
            app.output()
        );
        // Answer: y.
        app.send(b"y");
    }
    // The mock captures /api/respond with the ECHOED rpcId + allowed-once.
    let echoed = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let ids = scenario.mock.respond_rpc_ids().await;
            if !ids.is_empty() {
                break ids;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "respond POST never arrived"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    assert_eq!(
        echoed,
        vec!["rpc-e2e-1".to_string()],
        "respond echoes the frame rpcId"
    );
    let requests = scenario.mock.requests().await;
    let respond = requests
        .iter()
        .find(|request| request.path == "/api/respond")
        .expect("respond POST");
    let body: serde_json::Value = serde_json::from_str(&respond.body).expect("json");
    assert_eq!(body["result"]["value"]["approvalId"], "a-e2e");
    assert_eq!(body["result"]["value"]["outcome"], "allowed-once");

    // The resolved frame closes the takeover and toasts.
    assert!(
        scenario
            .mock
            .push_ws_frame(
                "/api/events.mux",
                r#"{"type":"server-request","rpcId":"rpc-push-9","method":"events.mux","payload":{"type":"approval/resolved","sessionId":"sA","approvalId":"a-e2e","outcome":"allowed-once"}}"#.into(),
            )
            .await,
        "resolved delivered"
    );
    {
        let app = scenario.app();
        assert!(
            app.wait_for("allowed once", Duration::from_secs(10)),
            "toast: {}",
            app.output()
        );
        // Chat returns: the composer placeholder is visible again.
        assert!(
            app.wait_for("type a message — enter to send", Duration::from_secs(5)),
            "chat restored: {}",
            app.output()
        );
    }
}

// ---------------------------------------------------------------------------
// 5. theme picker (PARITY row: theme)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn theme_picker_opens_and_closes() {
    let mut scenario = boot_and_attach(
        &history_template("mA", "welcome"),
        &history_template("mB", "other"),
    )
    .await;
    let app = scenario.app();
    assert!(
        app.wait_for("attached to 127.0.0.1:", Duration::from_secs(10)),
        "attach"
    );

    // Ctrl+T opens the picker; the popup lists the bundled themes.
    app.send(b"\x14");
    assert!(
        app.wait_for("themes", Duration::from_secs(10)),
        "popup title: {}",
        app.output()
    );
    assert!(
        app.wait_for("catppuccin-mocha", Duration::from_secs(5)),
        "theme row: {}",
        app.output()
    );

    // Esc closes the picker: keys reach the composer again (the app boots
    // focused there; the output is cumulative, so assert interaction
    // rather than absence).
    app.send(b"\x1b");
    app.send(b"hi");
    assert!(
        app.wait_for("hi", Duration::from_secs(5)),
        "composer accepts keys after the picker closed: {}",
        app.output()
    );
}

// ---------------------------------------------------------------------------
// 6. clean quit
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_q_exits_cleanly() {
    let mut scenario = boot_and_attach(
        &history_template("mA", "welcome"),
        &history_template("mB", "other"),
    )
    .await;
    let app = scenario.app();
    assert!(
        app.wait_for("attached to 127.0.0.1:", Duration::from_secs(10)),
        "attach"
    );

    let status = app.quit(Duration::from_secs(10));
    assert_eq!(status, Some(0), "clean exit status");
    assert!(
        !app.output().contains("panicked"),
        "no panic text: {}",
        app.output()
    );
}

// ---------------------------------------------------------------------------
// 7. process lifecycle: no DSH_PORT exits 2; an empty gateway warns
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn missing_dsh_port_exits_with_a_hint() {
    // The binary WITHOUT DSH_PORT: the pure-client contract says exit(2)
    // with a hint on stderr (main.rs's no-port branch).
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_dsh-tui"));
    cmd.env("DSH_TUI_LOCALE", "en");
    let xdg = std::env::temp_dir().join(format!("dsh-tui-e2e-xdg-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&xdg);
    cmd.env("XDG_CONFIG_HOME", &xdg);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn child");
    drop(pair.master.take_writer().expect("pty writer"));

    // Drain the merged output on a thread (never joined — the pty read may
    // not EOF, so the AppUnderTest pattern applies).
    let output = Arc::new(Mutex::new(Vec::new()));
    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let buffer = Arc::clone(&output);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                // macOS pty masters are nonblocking (BSD) and the dup'd
                // reader inherits O_NONBLOCK: EAGAIN is a retry, not EOF.
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(ref e) => {
                    // Record the death so failed waits self-diagnose.
                    buffer
                        .lock()
                        .expect("output lock")
                        .extend_from_slice(format!("\n[pty-reader died: {e}]").as_bytes());
                    break;
                }
                Ok(n) => buffer
                    .lock()
                    .expect("output lock")
                    .extend_from_slice(&chunk[..n]),
            }
        }
    });

    // Poll the exit status with a deadline (the child exits on its own).
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break Some(status.exit_code() as i32);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // The reader thread drains asynchronously — poll the accumulated output
    // (a single read can race the thread under parallel load and see an
    // empty buffer even though the child printed instantly).
    let text = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = String::from_utf8_lossy(&output.lock().expect("output lock")).into_owned();
            if text.contains("no DSH_PORT set") || Instant::now() > deadline {
                break text;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(text.contains("no DSH_PORT set"), "hint on stderr: {text}");
    assert_eq!(status, Some(2), "exit code 2 for the no-port contract");
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_gateway_shows_the_no_sessions_notice() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[]}}}"#,
        ),
    )
    .await;
    mock.set_handler(
        "workspace.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[],"archivedSessionIds":[]}}}"#,
        ),
    )
    .await;
    let mut scenario = Scenario::boot(mock).await;
    let app = scenario.app();
    assert!(
        app.wait_for(
            "gateway has no sessions — start one from the web UI",
            Duration::from_secs(10)
        ),
        "no-sessions status: {}",
        app.output()
    );
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn mux_event(session: &str, seq: i64, r#type: &str, data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "session/event",
        "sessionId": session,
        "event": {"type": r#type, "seq": seq, "time": seq as f64, "data": data},
    })
}

fn mux_frame(payload: serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "type": "server-request",
        "rpcId": "rpc-e2e",
        "method": "events.mux",
        "payload": payload,
    }))
    .expect("serialize")
}

async fn mock_respond_ok(mock: &MockGateway) {
    mock.set_handler("respond", MockAction::Ok(r#"{"accepted":true}"#))
        .await;
}
