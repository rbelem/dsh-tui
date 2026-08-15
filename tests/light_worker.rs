//! `--light` worker tests (T1): CLI parsing, task resolution (verbatim
//! file/stdin reads), the mock-gateway turn flow (in-process `run_light` and
//! real-binary subprocess runs), and the sentinel / exit-code contract
//! (0 success · 1 RPC/stream error · 2 usage / no gateway).

mod common;

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use dsh_tui::app::light::{LightError, TaskSource, light_task, parse_light_args, run_light};
use dsh_tui::client::WireClient;

use common::{MockAction, MockGateway};

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

const CREATE_OK: &str = r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"sessionId":"w1"}}}"#;
const PROMPT_OK: &str = r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"accepted":true}}}"#;
const PROMPT_REJECTED: &str = r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":false,"error":{"code":"bad-request","message":"prompt rejected"}}}"#;

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
        "rpcId": "rpc-light",
        "method": "events.mux",
        "payload": payload,
    }))
    .expect("serialize")
}

fn host_frame(payload: serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "type": "server-request",
        "rpcId": "rpc-light-host",
        "method": "events.host",
        "payload": payload,
    }))
    .expect("serialize")
}

/// A complete single-turn script: prompt echo, one text step, turn end.
fn happy_mux_frames() -> Vec<String> {
    vec![
        mux_frame(mux_event(
            "w1",
            1,
            "user/message",
            serde_json::json!({
                "id": "m1",
                "role": "user",
                "content": [{"type": "text", "text": "the task"}],
                "source": {"kind": "user"},
            }),
        )),
        mux_frame(mux_event(
            "w1",
            2,
            "turn/start",
            serde_json::json!({"turn": 1}),
        )),
        mux_frame(mux_event(
            "w1",
            3,
            "assistant/chunk",
            serde_json::json!({"turn": 1, "step": 1, "chunk": {"type": "block-start", "index": 0, "blockType": "text"}}),
        )),
        mux_frame(mux_event(
            "w1",
            4,
            "assistant/chunk",
            serde_json::json!({"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "Hello from the mock"}}),
        )),
        mux_frame(mux_event(
            "w1",
            5,
            "assistant/message",
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": {"id": "m2", "role": "assistant", "content": [{"type": "text", "text": "Hello from the mock"}], "source": {"kind": "model", "provider": "p", "model": "m"}},
            }),
        )),
        mux_frame(mux_event(
            "w1",
            6,
            "turn/end",
            serde_json::json!({"turn": 1, "reason": "completed"}),
        )),
    ]
}

fn happy_host_frames() -> Vec<String> {
    vec![
        host_frame(
            serde_json::json!({"type": "host/session-status", "sessionId": "w1", "running": true}),
        ),
        host_frame(
            serde_json::json!({"type": "host/session-status", "sessionId": "w1", "running": false}),
        ),
    ]
}

/// A mock gateway wired for a successful worker turn.
async fn mock_happy_turn() -> MockGateway {
    let mock = MockGateway::start().await;
    mock.set_handler("session.create", MockAction::Ok(CREATE_OK))
        .await;
    mock.set_handler("session.prompt", MockAction::Ok(PROMPT_OK))
        .await;
    mock.set_ws_frames("/api/events.mux", happy_mux_frames())
        .await;
    mock.set_ws_frames("/api/events.host", happy_host_frames())
        .await;
    mock
}

/// The prompt POST's payload as captured by the mock.
async fn captured_prompt(mock: &MockGateway) -> serde_json::Value {
    let requests = mock.requests().await;
    let prompt = requests
        .iter()
        .find(|request| request.path == "/api/session.prompt")
        .expect("session.prompt POST");
    serde_json::from_str(&prompt.body).expect("json body")
}

// ---------------------------------------------------------------------------
// 1. CLI parsing + task resolution
// ---------------------------------------------------------------------------

#[test]
fn parse_args_resolves_task_file_stdin_and_usage() {
    // --task takes the next argument verbatim (quotes/whitespace survive).
    assert_eq!(
        parse_light_args(
            &["--light".into(), "--task".into(), "say \"hi\"".into()],
            false
        ),
        Ok(TaskSource::Task("say \"hi\"".into()))
    );
    // --file.
    assert_eq!(
        parse_light_args(
            &["--light".into(), "--file".into(), "task.txt".into()],
            false
        ),
        Ok(TaskSource::File("task.txt".into()))
    );
    // Piped stdin.
    assert_eq!(
        parse_light_args(&["--light".into()], true),
        Ok(TaskSource::Stdin)
    );
    // No task with a terminal stdin → usage error.
    assert!(parse_light_args(&["--light".into()], false).is_err());
    // --task wins over --file.
    assert_eq!(
        parse_light_args(
            &[
                "--light".into(),
                "--task".into(),
                "a".into(),
                "--file".into(),
                "b".into()
            ],
            false
        ),
        Ok(TaskSource::Task("a".into()))
    );
    // Missing values and unknown flags are usage errors.
    assert!(parse_light_args(&["--light".into(), "--task".into()], false).is_err());
    assert!(parse_light_args(&["--light".into(), "--file".into()], false).is_err());
    assert!(parse_light_args(&["--light".into(), "--bogus".into()], false).is_err());
}

#[test]
fn light_task_reads_file_verbatim_and_rejects_empty() {
    let dir = std::env::temp_dir().join(format!("dsh-tui-light-unit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("task.txt");
    let task = "say \"hello\"\nsecond line\n```\ncode\n```";
    std::fs::write(&path, task).expect("write task");

    // File content arrives verbatim — the driver transports tasks via
    // --file, so quotes and newlines must survive.
    assert_eq!(
        light_task(
            &[
                "--light".into(),
                "--file".into(),
                path.to_str().unwrap().into()
            ],
            false
        ),
        Ok(task.into())
    );
    // An empty task is a usage error.
    assert!(light_task(&["--light".into(), "--task".into(), "".into()], false).is_err());
    // An unreadable file is a usage error.
    assert!(
        light_task(
            &[
                "--light".into(),
                "--file".into(),
                "/nonexistent/nope.txt".into()
            ],
            false
        )
        .is_err()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 2. in-process turn flow (mock gateway)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn run_light_streams_assistant_text_and_completes() {
    let mock = mock_happy_turn().await;
    let client = WireClient::attach(mock.port()).expect("attach");

    let result = run_light(client, "the task".into()).await;
    assert!(result.is_ok(), "turn completes cleanly: {result:?}");

    // The prompt went out with the task text, queue mode, and the new
    // session id.
    let body = captured_prompt(&mock).await;
    assert_eq!(body["payload"]["content"][0]["text"], "the task");
    assert_eq!(body["payload"]["mode"], "queue");
    assert_eq!(body["payload"]["sessionId"], "w1");
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_light_reports_prompt_rejection() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.create", MockAction::Ok(CREATE_OK))
        .await;
    mock.set_handler("session.prompt", MockAction::Ok(PROMPT_REJECTED))
        .await;
    let client = WireClient::attach(mock.port()).expect("attach");

    let result = run_light(client, "task".into()).await;
    assert!(
        matches!(result, Err(LightError::Rpc(_))),
        "a rejected prompt is an RPC error: {result:?}"
    );
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_light_reports_stream_error() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.create", MockAction::Ok(CREATE_OK))
        .await;
    mock.set_handler("session.prompt", MockAction::Ok(PROMPT_OK))
        .await;
    mock.set_ws_frames(
        "/api/events.mux",
        vec![mux_frame(serde_json::json!({
            "type": "stream/error",
            "error": {"code": "internal", "message": "boom", "details": {}},
        }))],
    )
    .await;
    let client = WireClient::attach(mock.port()).expect("attach");

    let result = run_light(client, "task".into()).await;
    assert!(
        matches!(result, Err(LightError::StreamError(ref message)) if message.contains("internal: boom")),
        "stream/error surfaces as a stream error: {result:?}"
    );
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_light_reports_turn_error() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.create", MockAction::Ok(CREATE_OK))
        .await;
    mock.set_handler("session.prompt", MockAction::Ok(PROMPT_OK))
        .await;
    mock.set_ws_frames(
        "/api/events.mux",
        vec![
            mux_frame(mux_event(
                "w1",
                1,
                "user/message",
                serde_json::json!({
                    "id": "m1",
                    "role": "user",
                    "content": [{"type": "text", "text": "task"}],
                    "source": {"kind": "user"},
                }),
            )),
            mux_frame(mux_event("w1", 2, "turn/start", serde_json::json!({"turn": 1}))),
            mux_frame(mux_event(
                "w1",
                3,
                "turn/end",
                serde_json::json!({"turn": 1, "reason": {"kind": "error", "error": {"message": "model exploded", "code": "internal"}}}),
            )),
        ],
    )
    .await;
    let client = WireClient::attach(mock.port()).expect("attach");

    let result = run_light(client, "task".into()).await;
    assert!(
        matches!(result, Err(LightError::TurnFailed(ref message)) if message == "model exploded"),
        "a turn/end error is a failed turn: {result:?}"
    );
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// 3. real-binary subprocess runs: sentinel + exit codes
// ---------------------------------------------------------------------------

/// Run the real binary as a worker: `port` None = no DSH_PORT env. Returns
/// (exit code, stdout, stderr). The child is killed if it outlives the
/// deadline (a hung worker must fail the test, not hang it).
fn run_worker(port: Option<u16>, args: &[&str]) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dsh-tui"));
    match port {
        Some(port) => {
            cmd.env("DSH_PORT", port.to_string());
        }
        None => {
            cmd.env_remove("DSH_PORT");
        }
    }
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn worker");
    let mut stdout = child.stdout.take().expect("worker stdout");
    let mut stderr = child.stderr.take().expect("worker stderr");
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status.code();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = child.wait();
    (
        status,
        out.join().expect("stdout thread"),
        err.join().expect("stderr thread"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_task_flag_streams_text_and_sentinel() {
    let mock = mock_happy_turn().await;
    let (status, stdout, stderr) =
        run_worker(Some(mock.port()), &["--light", "--task", "hello worker"]);
    assert_eq!(status, Some(0), "exit 0 on success; stderr: {stderr}");
    assert!(
        stdout.contains("Hello from the mock"),
        "assistant text on stdout: {stdout}"
    );
    assert!(
        stdout.trim_end().ends_with("dsh-worker: done"),
        "sentinel is the last line: {stdout}"
    );
    // The task text reached the gateway verbatim.
    let body = captured_prompt(&mock).await;
    assert_eq!(body["payload"]["content"][0]["text"], "hello worker");
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_task_file_streams_text_and_sentinel() {
    let dir = std::env::temp_dir().join(format!("dsh-tui-light-sub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("task.txt");
    let task = "say \"hello\"\nsecond line";
    std::fs::write(&path, task).expect("write task");

    let mock = mock_happy_turn().await;
    let (status, stdout, stderr) = run_worker(
        Some(mock.port()),
        &["--light", "--file", path.to_str().unwrap()],
    );
    assert_eq!(status, Some(0), "exit 0 on success; stderr: {stderr}");
    assert!(
        stdout.trim_end().ends_with("dsh-worker: done"),
        "sentinel is the last line: {stdout}"
    );
    // The file's content (quotes + newlines) reached the gateway verbatim.
    let body = captured_prompt(&mock).await;
    assert_eq!(body["payload"]["content"][0]["text"], task);
    let _ = std::fs::remove_dir_all(&dir);
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_prompt_rejection_prints_error_sentinel() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.create", MockAction::Ok(CREATE_OK))
        .await;
    mock.set_handler("session.prompt", MockAction::Ok(PROMPT_REJECTED))
        .await;
    let (status, stdout, _stderr) = run_worker(Some(mock.port()), &["--light", "--task", "task"]);
    assert_eq!(status, Some(1), "exit 1 on an RPC failure");
    assert!(
        stdout.trim_end().starts_with("dsh-worker: error:"),
        "error sentinel: {stdout}"
    );
    mock.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_usage_and_no_gateway_exit_2() {
    // No gateway: the existing no-DSH_PORT message + exit 2.
    let (status, _stdout, stderr) = run_worker(None, &["--light", "--task", "task"]);
    assert_eq!(status, Some(2), "exit 2 without DSH_PORT");
    assert!(
        stderr.contains("no DSH_PORT set"),
        "hint on stderr: {stderr}"
    );

    // No task source (stdin is null in the subprocess): usage + exit 2.
    let (status, _stdout, stderr) = run_worker(None, &["--light"]);
    assert_eq!(status, Some(2), "exit 2 with no task");
    assert!(
        stderr.contains("task is empty"),
        "usage on stderr: {stderr}"
    );

    // Unknown flag: usage + exit 2.
    let (status, _stdout, stderr) = run_worker(None, &["--light", "--bogus"]);
    assert_eq!(status, Some(2), "exit 2 on an unknown flag");
    assert!(
        stderr.contains("unknown argument `--bogus`"),
        "usage on stderr: {stderr}"
    );
}
