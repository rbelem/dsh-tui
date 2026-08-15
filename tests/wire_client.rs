//! Integration tests: a keyless Rust mock gateway + the WireClient.
//!
//! The mock listens on 127.0.0.1:0, serves scripted POST handlers (echoing
//! the request's rpcId) and scripted WS downlink frame sequences, and captures
//! every raw request head so the tests can assert the loopback fence
//! (`Host: 127.0.0.1:<port>`, no Origin).

use std::time::Duration;

use dsh_tui::client::{ClientError, WireClient};
use dsh_tui::store::SessionStore;
use dsh_tui::wire::approvals::{ApprovalRequestId, ApprovalResponseOutcome};
use dsh_tui::wire::events::{HostFrame, MuxFrame};
use dsh_tui::wire::rpc::{RpcError, RpcId};
use dsh_tui::wire::session::{Origin, PromptMode, SessionId};

mod common;
use common::{MockAction, MockGateway};

/// env-touching tests serialize (DSH_PORT is process-global).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ---------------------------------------------------------------------------
// rpc round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_list_round_trip_and_loopback_fence() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":[
                {"sessionId":"s1","updatedAt":100.0,"running":true,"blank":false,"cwd":"/work"},
                {"sessionId":"s2","updatedAt":200.0,"running":false,"blank":true,"parentSessionId":"s0","origin":"subagent"}
            ]}}}"#,
        ),
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let summaries = client.session_list().await.unwrap();
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].session_id, SessionId("s1".into()));
    assert!(summaries[0].running);
    assert_eq!(summaries[0].cwd.as_deref(), Some("/work"));
    assert_eq!(summaries[1].parent_session_id, Some(SessionId("s0".into())));
    assert_eq!(summaries[1].origin, Some(Origin::Subagent));

    // The loopback fence (api-request-trust.ts:96-123): Host header present,
    // no Origin, no sec-fetch-site.
    let requests = mock.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "session.list");
    assert_eq!(requests[0].path, "/api/session.list");
    let head = &requests[0].head;
    // hyper lowercases header names; the gateway's fence parses them
    // case-insensitively.
    let lower = head.to_ascii_lowercase();
    assert!(
        lower.contains(&format!("host: 127.0.0.1:{}", mock.port())),
        "request must carry the loopback Host header: {head}"
    );
    assert!(
        !head.to_ascii_lowercase().contains("origin"),
        "request must not carry an Origin header: {head}"
    );

    // The POST body is a client-request full form with a generated rpcId.
    let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["type"], "client-request");
    assert_eq!(body["method"], "session.list");
    assert!(body["rpcId"].as_str().unwrap().starts_with("dsh-tui-"));

    mock.stop().await;
}

#[tokio::test]
async fn business_error_maps_to_typed_rpc_error() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.prompt",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":false,"error":{"code":"internal","message":"boom","details":{}}}}"#,
        ),
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let err = client
        .session_prompt(SessionId("s1".into()), PromptMode::Queue, vec![], None)
        .await
        .unwrap_err();
    match err {
        ClientError::Rpc(RpcError::Internal { message, .. }) => assert_eq!(message, "boom"),
        other => panic!("expected Rpc(Internal), got {other:?}"),
    }
    mock.stop().await;
}

#[tokio::test]
async fn unknown_method_maps_to_http_status_404() {
    let mock = MockGateway::start().await;
    // No handler registered → the mock answers 404.
    let client = WireClient::attach(mock.port()).unwrap();
    let err = client
        .call::<_, serde_json::Value>("session.nope", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::HttpStatus(404)), "got {err:?}");
    mock.stop().await;
}

#[tokio::test]
async fn bad_json_body_maps_to_http_status_400() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.list", MockAction::BadJson).await;
    let client = WireClient::attach(mock.port()).unwrap();
    let err = client.session_list().await.unwrap_err();
    assert!(matches!(err, ClientError::HttpStatus(400)), "got {err:?}");
    mock.stop().await;
}

#[tokio::test]
async fn unanswered_rpc_maps_to_timeout() {
    let mock = MockGateway::start().await;
    mock.set_handler("session.models", MockAction::Hang).await;
    let client = WireClient::with_timeout(mock.port(), Duration::from_millis(200)).unwrap();
    let err = client
        .session_models(SessionId("s1".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Timeout), "got {err:?}");
    mock.stop().await;
}

#[tokio::test]
async fn attach_to_a_dead_port_is_a_transport_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // nothing listens on the port anymore
    let err = WireClient::attach(port).err().expect("attach must fail");
    assert!(matches!(err, ClientError::Transport(_)), "got {err:?}");
}

#[tokio::test]
async fn rpc_id_mismatch_maps_to_protocol_error() {
    let mock = MockGateway::start().await;
    // No `{rpcId}` placeholder: the response echoes a WRONG rpcId.
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"wrong-id","result":{"ok":true,"value":{"items":[]}}}"#,
        ),
    )
    .await;
    let client = WireClient::attach(mock.port()).unwrap();
    let err = client.session_list().await.unwrap_err();
    assert!(
        matches!(err, ClientError::Protocol(ref m) if m.contains("rpcId mismatch")),
        "got {err:?}"
    );
    mock.stop().await;
}

#[tokio::test]
async fn unparseable_result_value_maps_to_protocol_error() {
    let mock = MockGateway::start().await;
    // ok:true with a value that cannot deserialize into SessionListValue.
    mock.set_handler(
        "session.list",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"items":"not-an-array"}}}"#,
        ),
    )
    .await;
    let client = WireClient::attach(mock.port()).unwrap();
    let err = client.session_list().await.unwrap_err();
    assert!(matches!(err, ClientError::Protocol(_)), "got {err:?}");
    mock.stop().await;
}

#[tokio::test]
async fn ok_false_without_error_maps_to_protocol_error() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.list",
        MockAction::Ok(r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":false}}"#),
    )
    .await;
    let client = WireClient::attach(mock.port()).unwrap();
    let err = client.session_list().await.unwrap_err();
    assert!(
        matches!(err, ClientError::Protocol(ref m) if m.contains("ok:false without error")),
        "got {err:?}"
    );
    mock.stop().await;
}

#[tokio::test]
async fn session_select_model_round_trip() {
    let mock = MockGateway::start().await;
    mock.set_handler(
        "session.selectModel",
        MockAction::Ok(
            r#"{"type":"server-response","rpcId":"{rpcId}","result":{"ok":true,"value":{"selected":{"provider":"p","model":"m","reasoningEffort":"high"}}}}"#,
        ),
    )
    .await;
    let client = WireClient::attach(mock.port()).unwrap();
    let value = client
        .session_select_model(
            SessionId("s1".into()),
            "p".into(),
            "m".into(),
            Some("high".into()),
        )
        .await
        .unwrap();
    assert_eq!(value.selected.provider, "p");
    assert_eq!(value.selected.model, "m");
    assert_eq!(value.selected.reasoning_effort.as_deref(), Some("high"));

    // The payload rides the full request form.
    let requests = mock.requests().await;
    let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["method"], "session.selectModel");
    assert_eq!(body["payload"]["provider"], "p");
    assert_eq!(body["payload"]["reasoningEffort"], "high");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// mux subscribe → store (closes the spike loop: mock gateway → client → store)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mux_subscribe_feeds_session_store() {
    let mock = MockGateway::start().await;
    mock.set_ws_frames(
        "/api/events.mux",
        vec![
            r#"{"type":"server-request","rpcId":"ws-1","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s1","lastSeq":2}}"#.into(),
            r#"{"type":"server-request","rpcId":"ws-2","method":"events.mux","payload":{"type":"session/event","sessionId":"s1","event":{"type":"user/message","seq":1,"time":1.0,"data":{"id":"m1","role":"user","content":[{"type":"text","text":"hello"}],"source":{"kind":"user"}}}}}"#.into(),
            r#"{"type":"server-request","rpcId":"ws-3","method":"events.mux","payload":{"type":"session/event","sessionId":"s1","event":{"type":"assistant/message","seq":2,"time":2.0,"data":{"turn":1,"step":1,"message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"hi"}],"source":{"kind":"model","provider":"p","model":"m"}}}}}}"#.into(),
            r#"{"type":"server-request","rpcId":"ws-4","method":"events.mux","payload":{"type":"session/projection","sessionId":"s1","key":"session.list","value":{"blank":false},"seq":2}}"#.into(),
        ],
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut frames = client.mux_stream();
    let sid = SessionId("s1".into());
    let drain_sid = sid.clone();
    let drain = tokio::spawn(async move {
        let mut store = SessionStore::new();
        while let Some(downlink) = frames.recv().await {
            store
                .ingest(downlink.frame)
                .expect("store must accept scripted frames");
            let done = store.session(&drain_sid).is_some_and(|state| {
                state.last_seq == 2 && state.projections.contains_key("session.list")
            });
            if done {
                break;
            }
        }
        store
    });
    let store = tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("drain must finish")
        .expect("drain task must not panic");

    let state = store.session(&sid).expect("session state must exist");
    assert_eq!(state.last_seq, 2);
    assert_eq!(state.oldest_seq, 1);
    assert_eq!(state.durable_seq, 2);
    assert_eq!(
        state
            .projections
            .get("session.list")
            .expect("projection")
            .value,
        serde_json::json!({"blank": false})
    );
    assert_eq!(state.nodes.len(), 2, "user + assistant nodes folded");
    assert_eq!(state.nodes[0].key, "m1");
    assert_eq!(state.nodes[1].key, "1:1");

    mock.stop().await;
}

#[tokio::test]
async fn mux_stream_preserves_envelope_rpc_id() {
    let mock = MockGateway::start().await;
    mock.set_ws_frames(
        "/api/events.mux",
        vec![
            // An answerable frame with a scripted rpcId: the pair must carry
            // it verbatim (the respond echo target, rpc.ts:178).
            r#"{"type":"server-request","rpcId":"rpc-approval-1","method":"events.mux","payload":{"type":"approval/requested","sessionId":"s1","approvalId":"a1","toolName":"read_file","callId":"call-1","reason":"reads /etc"}}"#.into(),
            // A pure push with its own rpcId.
            r#"{"type":"server-request","rpcId":"rpc-push-2","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s1","lastSeq":1}}"#.into(),
        ],
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut frames = client.mux_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), frames.recv())
        .await
        .expect("frame must arrive")
        .expect("stream must not end");
    assert_eq!(first.rpc_id, RpcId("rpc-approval-1".into()));
    assert!(matches!(first.frame, MuxFrame::ApprovalRequested { .. }));
    let second = tokio::time::timeout(Duration::from_secs(5), frames.recv())
        .await
        .expect("frame must arrive")
        .expect("stream must not end");
    assert_eq!(second.rpc_id, RpcId("rpc-push-2".into()));
    assert!(matches!(second.frame, MuxFrame::SessionSubscribed { .. }));
    mock.stop().await;
}

#[tokio::test]
async fn ws_parse_failure_is_recorded_not_fatal() {
    let mock = MockGateway::start().await;
    mock.set_ws_frames("/api/events.mux", vec!["this is not json".into()])
        .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut frames = client.mux_stream();
    // The garbage frame must not reach the consumer, and the error must be
    // recorded on the client.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if client.last_ws_error().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "ws parse error not recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(frames.try_recv().is_err(), "garbage frame must be dropped");

    mock.stop().await;
}

// ---------------------------------------------------------------------------
// host stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn host_stream_receives_session_added() {
    let mock = MockGateway::start().await;
    mock.set_ws_frames(
        "/api/events.host",
        vec![
            r#"{"type":"server-request","rpcId":"h-1","method":"events.host","payload":{"type":"host/session-added","sessionId":"s9","blank":true}}"#.into(),
        ],
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut frames = client.host_stream();
    let downlink = tokio::time::timeout(Duration::from_secs(5), frames.recv())
        .await
        .expect("host frame must arrive")
        .expect("stream must not end");
    assert_eq!(
        downlink.rpc_id,
        RpcId("h-1".into()),
        "envelope rpcId preserved"
    );
    match downlink.frame {
        HostFrame::HostSessionAdded {
            session_id, blank, ..
        } => {
            assert_eq!(session_id, SessionId("s9".into()));
            assert!(blank);
        }
        other => panic!("expected host/session-added, got {other:?}"),
    }
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// respond
// ---------------------------------------------------------------------------

#[tokio::test]
async fn respond_approval_echoes_frame_rpc_id_and_returns_receipt() {
    let mock = MockGateway::start().await;
    mock.set_handler("respond", MockAction::Ok(r#"{"accepted":true}"#))
        .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let receipt = client
        .respond_approval(
            RpcId("ws-1".into()),
            SessionId("s1".into()),
            ApprovalRequestId("a1".into()),
            ApprovalResponseOutcome::AllowedOnce,
        )
        .await
        .unwrap();
    assert!(receipt.accepted);

    // The answer is a client-response full form echoing the frame rpcId with
    // the approval payload in result.value ("rpcId echoed, never minted anew").
    let requests = mock.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "respond");
    assert_eq!(requests[0].path, "/api/respond");
    let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["type"], "client-response");
    assert_eq!(body["rpcId"], "ws-1");
    assert_eq!(body["result"]["ok"], true);
    assert_eq!(body["result"]["value"]["sessionId"], "s1");
    assert_eq!(body["result"]["value"]["approvalId"], "a1");
    assert_eq!(body["result"]["value"]["outcome"], "allowed-once");

    mock.stop().await;
}

#[tokio::test]
async fn respond_http_failure_maps_to_status() {
    let mock = MockGateway::start().await;
    mock.set_handler("respond", MockAction::NotFound).await;
    let client = WireClient::attach(mock.port()).unwrap();
    let err = client
        .respond_approval(
            RpcId("r".into()),
            SessionId("s1".into()),
            ApprovalRequestId("a1".into()),
            ApprovalResponseOutcome::Rejected,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::HttpStatus(404)), "got {err:?}");
    mock.stop().await;
}

#[tokio::test]
async fn respond_timeout_when_the_gateway_hangs() {
    let mock = MockGateway::start().await;
    mock.set_handler("respond", MockAction::Hang).await;
    let client = WireClient::with_timeout(mock.port(), Duration::from_millis(200)).unwrap();
    let err = client
        .respond_approval(
            RpcId("r".into()),
            SessionId("s1".into()),
            ApprovalRequestId("a1".into()),
            ApprovalResponseOutcome::Rejected,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Timeout), "got {err:?}");
    mock.stop().await;
}

// ---------------------------------------------------------------------------
// attach_from_env
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn attach_from_env_reads_dsh_port() {
    let mock = MockGateway::start().await;
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: serialized under ENV_LOCK, single-threaded.
    unsafe { std::env::set_var("DSH_PORT", mock.port().to_string()) };
    let attached = WireClient::attach_from_env().unwrap();
    assert!(attached.is_some(), "DSH_PORT set → Some(client)");

    unsafe { std::env::remove_var("DSH_PORT") };
    let detached = WireClient::attach_from_env().unwrap();
    assert!(detached.is_none(), "DSH_PORT unset → None (pure client)");

    drop(_guard); // no awaits while the env lock is held
    mock.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn attach_from_env_rejects_invalid_and_non_unicode_ports() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: serialized under ENV_LOCK; restored before return.
    unsafe { std::env::set_var("DSH_PORT", "not-a-number") };
    let err = WireClient::attach_from_env()
        .err()
        .expect("attach_from_env must fail");
    assert!(
        matches!(err, ClientError::Protocol(ref m) if m.contains("invalid DSH_PORT")),
        "got {err:?}"
    );
    unsafe { std::env::remove_var("DSH_PORT") };

    // A non-unicode value (invalid UTF-8 in the OsStr) hits the
    // VarError::NotUnicode branch.
    use std::os::unix::ffi::OsStringExt;
    let bad = std::ffi::OsString::from_vec(vec![0xFF, 0xFE]);
    unsafe { std::env::set_var("DSH_PORT", &bad) };
    let err = WireClient::attach_from_env()
        .err()
        .expect("attach_from_env must fail");
    assert!(
        matches!(err, ClientError::Protocol(ref m) if m.contains("not valid unicode")),
        "got {err:?}"
    );
    unsafe { std::env::remove_var("DSH_PORT") };
}

// ---------------------------------------------------------------------------
// ws downlink death + reconnect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ws_dies_mid_frame_and_the_subscriber_reconnects() {
    let mock = MockGateway::start().await;
    // Serve two frames, then close the socket cleanly: the subscriber must
    // deliver the frames that made it through, then reconnect and re-serve.
    mock.set_ws_frames_and_close(
        "/api/events.mux",
        vec![
            r#"{"type":"server-request","rpcId":"ws-1","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s1","lastSeq":1}}"#.into(),
            r#"{"type":"server-request","rpcId":"ws-2","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s1","lastSeq":2}}"#.into(),
        ],
    )
    .await;

    let client = WireClient::attach(mock.port()).unwrap();
    let mut frames = client.mux_stream();

    // The two scripted frames arrive before the death.
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(5), frames.recv())
            .await
            .expect("frame before the socket dies")
            .expect("stream must not end");
        assert!(matches!(frame.frame, MuxFrame::SessionSubscribed { .. }));
    }
    // The reconnect (capped backoff) re-serves the scripted frames.
    let frame = tokio::time::timeout(Duration::from_secs(10), frames.recv())
        .await
        .expect("reconnected stream must deliver frames again")
        .expect("stream must not end");
    assert!(matches!(frame.frame, MuxFrame::SessionSubscribed { .. }));

    mock.stop().await;
}

#[tokio::test]
async fn gateway_down_records_a_ws_connect_error() {
    let mock = MockGateway::start().await;
    let client = WireClient::attach(mock.port()).unwrap();
    mock.stop().await; // nothing accepts WS upgrades anymore

    let _ = client.mux_stream(); // spawns the subscriber; connect fails
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if client.last_ws_error().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "ws connect error not recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn ws_read_error_is_recorded_not_fatal() {
    let mock = MockGateway::start().await;
    mock.set_ws_raw_garbage("/api/events.mux").await;

    let client = WireClient::attach(mock.port()).unwrap();
    let _ = client.mux_stream();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if client
            .last_ws_error()
            .is_some_and(|error| error.contains("ws read error"))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "ws read error not recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn dropping_the_receiver_stops_the_subscriber() {
    let mock = MockGateway::start().await;
    // No scripted frames: the mock holds the socket open.
    mock.set_ws_frames("/api/events.mux", vec![]).await;

    let client = WireClient::attach(mock.port()).unwrap();
    let frames = client.mux_stream();
    drop(frames); // consumer gone — the next frame kills the subscriber

    // A pushed frame reaches the subscriber, whose tx.send fails; the
    // subscriber stops and the socket closes, which the mock observes by
    // dropping its pusher. Poll until push_ws_frame stops delivering.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let delivered = mock
            .push_ws_frame(
                "/api/events.mux",
                r#"{"type":"server-request","rpcId":"x","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s1","lastSeq":1}}"#.into(),
            )
            .await;
        if !delivered {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "subscriber still alive after the receiver dropped"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn dropping_the_client_stops_the_subscriber() {
    let mock = MockGateway::start().await;
    mock.set_ws_frames("/api/events.mux", vec![]).await;

    let client = WireClient::attach(mock.port()).unwrap();
    let frames = client.mux_stream();
    drop(frames);
    drop(client); // last clone: the stop flag ends the subscriber loop

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let delivered = mock
            .push_ws_frame(
                "/api/events.mux",
                r#"{"type":"server-request","rpcId":"x","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s1","lastSeq":1}}"#.into(),
            )
            .await;
        if !delivered {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "subscriber still alive after the client dropped"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
