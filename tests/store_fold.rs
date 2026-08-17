//! Scenario tests for the SessionStore: the event fold, seq bookkeeping,
//! window eviction, compaction, projections, and queue snapshots.
//!
//! Plain asserts only — no insta, no network, no file reads. Frames are built
//! by constructing `MuxFrame` values directly; event data via `json!`.

use dsh_tui::store::*;
use dsh_tui::wire::{
    EmptyDetails, MessageId, MessageRole, MuxFrame, QueueItem, QueueMessage, QueueMessageSource,
    QueuePlacement, RpcError, SessionEvent, SessionId, ToolEventView, ToolEventViewCard,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// fixture helpers
// ---------------------------------------------------------------------------

fn ev(seq: i64, r#type: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        r#type: r#type.into(),
        seq,
        time: seq as f64,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn ev_ignorable(seq: i64, r#type: &str, data: serde_json::Value) -> SessionEvent {
    let mut event = ev(seq, r#type, data);
    event.ignorable = Some(true);
    event
}

fn ev_surface(
    seq: i64,
    r#type: &str,
    data: serde_json::Value,
    surface_op: serde_json::Value,
) -> SessionEvent {
    let mut event = ev(seq, r#type, data);
    event.surface_op = Some(surface_op);
    event
}

fn frame(session: &str, event: SessionEvent) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: None,
    }
}

fn frame_with_view(session: &str, event: SessionEvent, view: ToolEventView) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: Some(view),
    }
}

fn ingest_all(store: &mut SessionStore, session: &str, events: Vec<SessionEvent>) {
    for event in events {
        store
            .ingest(frame(session, event))
            .expect("ingest must succeed");
    }
}

fn session_state<'a>(store: &'a SessionStore, session: &str) -> &'a SessionState {
    store
        .session(&SessionId(session.into()))
        .expect("session state must exist")
}

fn nodes<'a>(store: &'a SessionStore, session: &str) -> &'a [ChatNode] {
    &session_state(store, session).nodes
}

fn user_msg(id: &str, text: &str, source: serde_json::Value) -> serde_json::Value {
    json!({"id": id, "role": "user", "content": [{"type": "text", "text": text}], "source": source})
}

fn chunk(turn: i64, step: i64, chunk: serde_json::Value) -> serde_json::Value {
    json!({"turn": turn, "step": step, "chunk": chunk})
}

fn user_source() -> serde_json::Value {
    json!({"kind": "user"})
}

// ---------------------------------------------------------------------------
// 1. scripted happy turn
// ---------------------------------------------------------------------------

#[test]
fn happy_turn_builds_user_and_assistant_nodes() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hello", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 0, "blockType": "text"}),
                ),
            ),
            ev(
                4,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "Hel"}),
                ),
            ),
            ev(
                5,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "lo"}),
                ),
            ),
            ev(
                6,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 1, "blockType": "reasoning"}),
                ),
            ),
            ev(
                7,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "reasoning-delta", "index": 1, "text": "think"}),
                ),
            ),
            ev(
                8,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-end", "index": 1, "block": {"type": "reasoning", "text": "think"}}),
                ),
            ),
            ev(
                9,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "m2", "role": "assistant",
                        "content": [{"type": "text", "text": "Hello"}, {"type": "reasoning", "text": "think"}],
                        "source": {"kind": "model", "provider": "deepseek", "model": "deepseek-chat"},
                    },
                    "usage": {"inputTokens": 10, "outputTokens": 5},
                }),
            ),
            ev(10, "step/end", json!({"turn": 1, "step": 1})),
            ev(
                11,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ],
    );

    let nodes = nodes(&store, s);
    assert_eq!(nodes.len(), 2, "user + assistant, no boundary nodes");
    assert_eq!(session_state(&store, s).last_seq, 11);

    // Node order: user (seq 1) then settled assistant (seq 9).
    assert_eq!(nodes[0].key, "m1");
    assert_eq!(nodes[0].kind, ChatNodeKind::User);
    assert_eq!(nodes[1].key, "1:1");
    let expected = ChatNode {
        key: "1:1".into(),
        kind: ChatNodeKind::Assistant,
        anchor_seq: 9,
        data: NodeData::Assistant {
            turn: 1,
            step: 1,
            blocks: vec![
                AssistantBlock::Text {
                    text: "Hello".into(),
                },
                AssistantBlock::Reasoning {
                    text: "think".into(),
                },
            ],
            usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            }),
            finalized: true,
            interrupted: false,
        },
    };
    assert_eq!(&nodes[1], &expected);
}

// ---------------------------------------------------------------------------
// 2. tool round-trip
// ---------------------------------------------------------------------------

#[test]
fn tool_round_trip_settles_with_call_backfill() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(
                1,
                "user/message",
                user_msg("m1", "list files", user_source()),
            ),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "read_file", "arguments": r#"{"path":"/etc"}"#}),
            ),
            ev(
                4,
                "tool/result",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "r1", "role": "user",
                        "content": [{"type": "tool-result", "toolCallId": "c1", "content": [{"type": "text", "text": "ok"}], "isError": false}],
                        "source": {"kind": "tool", "callId": "c1"},
                    },
                    "error": {"name": "E", "code": "E1"},
                    "meta": {"diff": "x"},
                }),
            ),
            ev(5, "step/end", json!({"turn": 1, "step": 1})),
            ev(
                6,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ],
    );

    let nodes = nodes(&store, s);
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[1].key, "c1");
    assert_eq!(nodes[1].kind, ChatNodeKind::Tool);
    let expected = ChatNode {
        key: "c1".into(),
        kind: ChatNodeKind::Tool,
        anchor_seq: 3,
        data: NodeData::Tool {
            call: Some(RunningToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                args_raw: r#"{"path":"/etc"}"#.into(),
                turn: 1,
                step: 1,
                time: 3.0,
                call_view: None,
            }),
            result: Some(Box::new(ToolResultNode {
                call_id: "c1".into(),
                call: Some(ToolCallBackfill {
                    name: "read_file".into(),
                    args_raw: r#"{"path":"/etc"}"#.into(),
                }),
                call_time: Some(3.0),
                result_time: Some(4.0),
                content: vec![ContentBlock::Text { text: "ok".into() }],
                is_error: false,
                error: Some(ToolErrorIdentity {
                    name: "E".into(),
                    code: "E1".into(),
                }),
                meta: Some(json!({"diff": "x"})),
                call_view: None,
                result_view: None,
            })),
            interrupted: false,
        },
    };
    assert_eq!(&nodes[1], &expected);
}

#[test]
fn tool_result_error_case_and_views() {
    let mut store = SessionStore::new();
    let s = "s1";
    let call = ev(
        3,
        "tool/call",
        json!({"turn": 1, "step": 1, "callId": "c1", "name": "write_file", "arguments": "{}"}),
    );
    store
        .ingest(frame_with_view(
            s,
            call,
            ToolEventView::Call {
                view: ToolEventViewCard { card: "fs".into() },
            },
        ))
        .unwrap();
    let result = ev(
        4,
        "tool/result",
        json!({
            "turn": 1, "step": 1,
            "message": {
                "id": "r1", "role": "user",
                "content": [{"type": "tool-result", "toolCallId": "c1", "content": [{"type": "text", "text": "failed"}], "isError": true}],
                "source": {"kind": "tool", "callId": "c1"},
            },
        }),
    );
    store
        .ingest(frame_with_view(
            s,
            result,
            ToolEventView::Result {
                view: ToolEventViewCard { card: "fsr".into() },
            },
        ))
        .unwrap();

    let node = &nodes(&store, s)[0];
    let NodeData::Tool {
        call,
        result,
        interrupted,
    } = &node.data
    else {
        panic!("expected tool node");
    };
    assert!(!interrupted);
    assert_eq!(
        call.as_ref().unwrap().call_view,
        Some(ToolEventView::Call {
            view: ToolEventViewCard { card: "fs".into() }
        })
    );
    let result = result.as_ref().expect("settled");
    assert!(result.is_error);
    assert_eq!(result.error, None);
    assert_eq!(
        result.result_view,
        Some(ToolEventView::Result {
            view: ToolEventViewCard { card: "fsr".into() }
        })
    );
    // Call-side view rides onto the result node too.
    assert_eq!(
        result.call_view,
        Some(ToolEventView::Call {
            view: ToolEventViewCard { card: "fs".into() }
        })
    );
}

#[test]
fn tool_result_without_call_still_forms_node() {
    let mut store = SessionStore::new();
    let s = "s1";
    // Window cut: only the result is in-window.
    store
        .ingest(frame(s, ev(1, "tool/result", json!({
            "turn": 1, "step": 1,
            "message": {
                "id": "r1", "role": "user",
                "content": [{"type": "tool-result", "toolCallId": "c9", "content": [], "isError": false}],
                "source": {"kind": "tool", "callId": "c9"},
            },
        }))))
        .unwrap();
    let node = &nodes(&store, s)[0];
    assert_eq!(node.key, "c9");
    let NodeData::Tool { call, result, .. } = &node.data else {
        panic!("expected tool node");
    };
    assert_eq!(call.as_ref(), None, "call backfill is None on a window cut");
    assert_eq!(result.as_ref().unwrap().call, None);
    assert_eq!(result.as_ref().unwrap().content, Vec::<ContentBlock>::new());
}

#[test]
fn tool_interrupted_on_turn_end() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "go", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "run", "arguments": "{}"}),
            ),
            ev(
                4,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "aborted", "reason": {"kind": "user"}}}),
            ),
        ],
    );
    let node = &nodes(&store, s)[1];
    let NodeData::Tool {
        call,
        result,
        interrupted,
    } = &node.data
    else {
        panic!("expected tool node");
    };
    assert!(interrupted);
    assert!(call.is_some());
    let result = result
        .as_ref()
        .expect("interrupted tools synthesize an error result");
    assert!(result.is_error);
    assert_eq!(
        result.error,
        Some(ToolErrorIdentity {
            name: "Interrupted".into(),
            code: "interrupted".into()
        })
    );
    assert_eq!(result.content, Vec::<ContentBlock>::new());
}

// ---------------------------------------------------------------------------
// 3. interruption
// ---------------------------------------------------------------------------

#[test]
fn interrupted_assistant_on_aborted_turn_end() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 0, "blockType": "text"}),
                ),
            ),
            ev(
                4,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "He"}),
                ),
            ),
            // turn/end WITHOUT assistant/message finalize.
            ev(
                5,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "aborted", "reason": {"kind": "user"}}}),
            ),
        ],
    );
    let nodes = nodes(&store, s);
    assert_eq!(nodes.len(), 2);
    let NodeData::Assistant {
        blocks,
        interrupted,
        finalized,
        ..
    } = &nodes[1].data
    else {
        panic!("expected assistant node");
    };
    assert_eq!(blocks, &[AssistantBlock::Text { text: "He".into() }]);
    assert!(interrupted);
    assert!(!finalized);
    assert_eq!(
        nodes[1].anchor_seq, 5,
        "interrupted assistant anchors at the boundary seq"
    );
}

#[test]
fn new_user_message_interrupts_open_assistant() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "first", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "partial"}),
                ),
            ),
            // A new human prompt without a finalize closes the open assistant.
            ev(4, "user/message", user_msg("m2", "second", user_source())),
        ],
    );
    let nodes = nodes(&store, s);
    assert_eq!(nodes.len(), 3, "user m1 + interrupted assistant + user m2");
    assert_eq!(nodes[0].key, "m1");
    assert!(matches!(
        &nodes[1].data,
        NodeData::Assistant {
            interrupted: true,
            ..
        }
    ));
    assert_eq!(nodes[1].key, "1:1");
    assert_eq!(nodes[2].key, "m2");
}

// ---------------------------------------------------------------------------
// 4. empty assistant message skipped
// ---------------------------------------------------------------------------

#[test]
fn empty_assistant_message_is_skipped() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {"id": "m2", "role": "assistant", "content": [], "source": {"kind": "model", "provider": "p", "model": "m"}},
                    "usage": {"inputTokens": 1, "outputTokens": 0},
                }),
            ),
            ev(4, "step/end", json!({"turn": 1, "step": 1})),
            ev(
                5,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ],
    );
    let nodes = nodes(&store, s);
    assert_eq!(nodes.len(), 1, "only the user node survives");
    assert_eq!(nodes[0].key, "m1");
}

// ---------------------------------------------------------------------------
// 5. compaction
// ---------------------------------------------------------------------------

#[test]
fn compaction_checkpoint_keeps_shadowed_and_appends_replacement() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            // The shadowed (append-origin) message stays in the transcript.
            ev(1, "user/message", user_msg("m1", "hello", user_source())),
            ev(
                2,
                "compaction/start",
                json!({"compactionId": "c1", "turn": null}),
            ),
            ev(
                3,
                "compaction/summary",
                json!({
                    "compactionId": "c1",
                    "summary": [{"type": "text", "text": "summarized content"}],
                    "shadowedRange": {"start": 1, "end": 1},
                    "shadowedSeqs": [1],
                    "shadowedTokenCount": 100,
                    "provider": "deepseek",
                    "model": "deepseek-chat",
                }),
            ),
            // The replacement checkpoint: surfaceOp replace + compact plugin source.
            ev_surface(
                4,
                "user/message",
                user_msg(
                    "m2",
                    "summarized content",
                    json!({"kind": "plugin", "plugin": "compact", "compactionId": "c1"}),
                ),
                json!({"op": "replace", "start": 1, "end": 1}),
            ),
            ev(
                5,
                "compaction/end",
                json!({"compactionId": "c1", "turn": null}),
            ),
        ],
    );

    let nodes = nodes(&store, s);
    assert_eq!(
        nodes.len(),
        3,
        "shadowed user + replacement user + compaction marker"
    );
    // Shadowed message still present.
    assert_eq!(nodes[0].key, "m1");
    // Replacement user node appended as its own node (Context kind).
    let replacement = nodes
        .iter()
        .find(|n| n.key == "m2")
        .expect("replacement user node");
    assert!(matches!(
        &replacement.data,
        NodeData::User {
            kind: UserNodeKind::Context,
            ..
        }
    ));
    // Compaction marker with summary fields.
    let marker = nodes
        .iter()
        .find(|n| n.kind == ChatNodeKind::Compaction)
        .expect("compaction node");
    assert_eq!(marker.key, "c1");
    assert_eq!(marker.anchor_seq, 4, "marker anchors at the checkpoint seq");
    assert_eq!(
        marker.data,
        NodeData::Compaction {
            summary: Some("summarized content".into()),
            summary_event_seq: Some(3),
            shadowed_item_count: Some(1),
            shadowed_token_count: Some(100),
        }
    );
}

// ---------------------------------------------------------------------------
// 6. projections
// ---------------------------------------------------------------------------

#[test]
fn projection_higher_seq_wins_and_subscribed_truncates() {
    let mut store = SessionStore::new();
    let s = "s1";
    let sid = SessionId(s.into());

    store
        .ingest(MuxFrame::SessionProjection {
            session_id: sid.clone(),
            key: "k".into(),
            value: json!({"v": 1}),
            seq: 10,
        })
        .unwrap();
    store
        .ingest(MuxFrame::SessionProjection {
            session_id: sid.clone(),
            key: "k".into(),
            value: json!({"v": 2}),
            seq: 12,
        })
        .unwrap();
    store
        .ingest(MuxFrame::SessionProjection {
            session_id: sid.clone(),
            key: "k".into(),
            value: json!({"v": 3}),
            seq: 11,
        })
        .unwrap();
    let row = session_state(&store, s)
        .projections
        .get("k")
        .expect("projection row");
    assert_eq!(row.value, json!({"v": 2}), "higher seq wins");
    assert_eq!(row.seq, 12);

    // Durable baseline truncates rows claiming knowledge beyond it.
    store
        .ingest(MuxFrame::SessionSubscribed {
            session_id: sid.clone(),
            last_seq: 11,
        })
        .unwrap();
    let state = session_state(&store, s);
    assert_eq!(state.durable_seq, 11);
    assert!(
        state.projections.is_empty(),
        "row with seq 12 > 11 truncated"
    );

    // A frame at or below the baseline after a subscribe is the host's
    // attach/replay re-emission, not a stale row: it lands (web-faithful —
    // no durable guard on admission) and the next higher frame wins.
    store
        .ingest(MuxFrame::SessionProjection {
            session_id: sid.clone(),
            key: "k".into(),
            value: json!({"v": 4}),
            seq: 9,
        })
        .unwrap();
    assert_eq!(
        session_state(&store, s)
            .projections
            .get("k")
            .expect("projection row")
            .value,
        json!({"v": 4}),
        "replay frame lands"
    );
    // Live frames above the baseline land.
    store
        .ingest(MuxFrame::SessionProjection {
            session_id: sid,
            key: "k".into(),
            value: json!({"v": 5}),
            seq: 13,
        })
        .unwrap();
    let row = session_state(&store, s)
        .projections
        .get("k")
        .expect("projection row");
    assert_eq!(row.value, json!({"v": 5}));
    assert_eq!(row.seq, 13);
}

// ---------------------------------------------------------------------------
// 7. queue snapshot
// ---------------------------------------------------------------------------

fn queue_item(id: &str, placement: QueuePlacement) -> QueueItem {
    QueueItem {
        id: MessageId(id.into()),
        placement,
        message: QueueMessage {
            id: MessageId(id.into()),
            role: MessageRole::User,
            content: vec![],
            source: QueueMessageSource {
                kind: "composer".into(),
            },
        },
    }
}

#[test]
fn queue_snapshot_is_full_replacement() {
    let mut store = SessionStore::new();
    let s = "s1";
    let sid = SessionId(s.into());

    store
        .ingest(MuxFrame::SessionQueue {
            session_id: sid.clone(),
            items: vec![
                queue_item("m1", QueuePlacement::Queued),
                queue_item("m2", QueuePlacement::Steering),
            ],
        })
        .unwrap();
    let queue = session_state(&store, s)
        .queue
        .as_ref()
        .expect("queue snapshot");
    assert_eq!(queue.items.len(), 2);

    store
        .ingest(MuxFrame::SessionQueue {
            session_id: sid,
            items: vec![queue_item("m2", QueuePlacement::Steering)],
        })
        .unwrap();
    let queue = session_state(&store, s)
        .queue
        .as_ref()
        .expect("queue snapshot");
    assert_eq!(
        queue.items.len(),
        1,
        "second frame fully replaces the first"
    );
    assert_eq!(queue.items[0].id, MessageId("m2".into()));
}

// ---------------------------------------------------------------------------
// 8. duplicates + out-of-order + gaps
// ---------------------------------------------------------------------------

#[test]
fn duplicate_and_out_of_order_events_are_ignored() {
    let mut store = SessionStore::new();
    let s = "s1";
    // Duplicate seq: the second user/message is ignored.
    ingest_all(
        &mut store,
        s,
        vec![ev(1, "user/message", user_msg("m1", "a", user_source()))],
    );
    store
        .ingest(frame(
            s,
            ev(1, "user/message", user_msg("m2", "b", user_source())),
        ))
        .unwrap();
    let state = session_state(&store, s);
    assert_eq!(state.last_seq, 1);
    assert_eq!(nodes(&store, s).len(), 1, "duplicate seq event ignored");
    assert_eq!(nodes(&store, s)[0].key, "m1");

    // Seq gap (2-4 missing): accepted, watermark reflects max applied.
    store
        .ingest(frame(s, ev(5, "step/start", json!({"turn": 1, "step": 1}))))
        .unwrap();
    assert_eq!(session_state(&store, s).last_seq, 5);
    // Lower seq after higher: ignored.
    store
        .ingest(frame(
            s,
            ev(2, "user/message", user_msg("m3", "c", user_source())),
        ))
        .unwrap();
    assert_eq!(session_state(&store, s).last_seq, 5);
    assert_eq!(nodes(&store, s).len(), 1, "lower seq event ignored");

    store
        .ingest(frame(
            s,
            ev(6, "user/message", user_msg("m4", "d", user_source())),
        ))
        .unwrap();
    assert_eq!(session_state(&store, s).last_seq, 6);
    assert_eq!(nodes(&store, s).len(), 2);
    assert_eq!(nodes(&store, s)[1].key, "m4");
}

// ---------------------------------------------------------------------------
// 9. eviction
// ---------------------------------------------------------------------------

#[test]
fn eviction_truncates_window_and_preserves_fold_state() {
    let mut store = SessionStore::with_max_buffered_events(3);
    let s = "s1";
    let sid = SessionId(s.into());
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 0, "blockType": "text"}),
                ),
            ),
            ev(
                4,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "Hel"}),
                ),
            ),
            ev(
                5,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "lo"}),
                ),
            ),
        ],
    );
    let state = session_state(&store, s);
    assert_eq!(state.oldest_seq, 3, "head evicted down to the cap");
    assert!(state.truncated);
    assert_eq!(state.last_seq, 5);
    assert_eq!(
        nodes(&store, s).len(),
        1,
        "user node evicted; assistant survives"
    );
    assert!(matches!(
        &nodes(&store, s)[0].data,
        NodeData::Assistant { blocks, .. } if blocks == &[AssistantBlock::Text { text: "Hello".into() }]
    ));

    // Fold state for a surviving key survives the rebuild.
    store.set_fold(&sid, "1:1", FoldState::collapsed());
    assert_eq!(store.fold_state(&sid, "1:1"), FoldState::collapsed());

    // More eviction: the assistant keeps streaming, fold state preserved.
    store
        .ingest(frame(
            s,
            ev(
                6,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "text-delta", "index": 0, "text": "!"})),
            ),
        ))
        .unwrap();
    let state = session_state(&store, s);
    assert_eq!(state.oldest_seq, 4);
    assert!(state.truncated);
    assert_eq!(
        store.fold_state(&sid, "1:1"),
        FoldState::collapsed(),
        "fold state survives rebuilds"
    );
    assert!(matches!(
        &nodes(&store, s)[0].data,
        NodeData::Assistant { blocks, .. } if blocks == &[AssistantBlock::Text { text: "Hello!".into() }]
    ));
}

// ---------------------------------------------------------------------------
// 10. unknown events
// ---------------------------------------------------------------------------

#[test]
fn unknown_events_ignorable_skipped_required_kept() {
    let mut store = SessionStore::new();
    let s = "s1";
    // Ignorable unknown type: skipped silently.
    store
        .ingest(frame(s, ev_ignorable(1, "plugin.xyz", json!({"x": 1}))))
        .unwrap();
    assert!(
        nodes(&store, s).is_empty(),
        "ignorable unknown type skipped"
    );

    // Required unknown type: degraded to an UnknownNode row, never dropped.
    store
        .ingest(frame(s, ev(2, "plugin.abc", json!({"y": 2}))))
        .unwrap();
    let node = &nodes(&store, s)[0];
    assert_eq!(node.kind, ChatNodeKind::Unknown);
    assert_eq!(node.key, "unknown:2");
    assert_eq!(
        node.data,
        NodeData::Unknown {
            r#type: "plugin.abc".into(),
            data: json!({"y": 2})
        }
    );
}

// ---------------------------------------------------------------------------
// 11. session/subscribed baseline
// ---------------------------------------------------------------------------

#[test]
fn subscribed_baseline_prunes_events_and_projections() {
    let mut store = SessionStore::new();
    let s = "s1";
    let sid = SessionId(s.into());
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 0, "blockType": "text"}),
                ),
            ),
            ev(
                4,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "text-delta", "index": 0, "text": "H"})),
            ),
            ev(
                5,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "text-delta", "index": 0, "text": "i"})),
            ),
            ev(
                6,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "text-delta", "index": 0, "text": "!"})),
            ),
            ev(8, "user/message", user_msg("m2", "second", user_source())),
        ],
    );
    store
        .ingest(MuxFrame::SessionProjection {
            session_id: sid.clone(),
            key: "k".into(),
            value: json!({"v": 1}),
            seq: 7,
        })
        .unwrap();
    // Pre-baseline: the second user message closed the open assistant.
    assert!(nodes(&store, s).iter().any(|n| matches!(
        &n.data,
        NodeData::Assistant {
            interrupted: true,
            ..
        }
    )));
    assert_eq!(nodes(&store, s).len(), 3);

    store
        .ingest(MuxFrame::SessionSubscribed {
            session_id: sid,
            last_seq: 5,
        })
        .unwrap();
    let state = session_state(&store, s);
    assert_eq!(state.durable_seq, 5);
    assert_eq!(
        state.last_seq, 5,
        "buffered events beyond the baseline dropped"
    );
    assert_eq!(state.oldest_seq, 1);
    assert!(
        state.projections.is_empty(),
        "projection beyond the baseline truncated"
    );

    // Nodes rebuilt from the retained window: the interrupted assistant is
    // running again (its closing user/message was dropped).
    let node_list = nodes(&store, s);
    assert_eq!(node_list.len(), 2);
    assert_eq!(node_list[0].key, "m1");
    assert!(matches!(
        &node_list[1].data,
        NodeData::Assistant {
            interrupted: false,
            finalized: false,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// 12. prepend_events
// ---------------------------------------------------------------------------

#[test]
fn prepend_history_page_grows_window_backward() {
    let mut store = SessionStore::new();
    let s = "s1";
    let sid = SessionId(s.into());
    ingest_all(
        &mut store,
        s,
        vec![
            ev(6, "user/message", user_msg("m2", "second", user_source())),
            ev(7, "step/start", json!({"turn": 2, "step": 1})),
            ev(
                8,
                "assistant/chunk",
                chunk(
                    2,
                    1,
                    json!({"type": "block-start", "index": 0, "blockType": "text"}),
                ),
            ),
            ev(
                9,
                "assistant/chunk",
                chunk(2, 1, json!({"type": "text-delta", "index": 0, "text": "A"})),
            ),
            ev(
                10,
                "assistant/chunk",
                chunk(2, 1, json!({"type": "text-delta", "index": 0, "text": "B"})),
            ),
        ],
    );

    let page = vec![
        StoredEvent::try_new(
            ev(1, "user/message", user_msg("m1", "first", user_source())),
            None,
        )
        .unwrap(),
        StoredEvent::try_new(ev(2, "step/start", json!({"turn": 1, "step": 1})), None).unwrap(),
        StoredEvent::try_new(
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 0, "blockType": "text"}),
                ),
            ),
            None,
        )
        .unwrap(),
        StoredEvent::try_new(
            ev(
                4,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "text-delta", "index": 0, "text": "X"})),
            ),
            None,
        )
        .unwrap(),
        StoredEvent::try_new(
            ev(
                5,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "text-delta", "index": 0, "text": "Y"})),
            ),
            None,
        )
        .unwrap(),
    ];
    store.prepend_events(&sid, page);

    let state = session_state(&store, s);
    assert_eq!(state.oldest_seq, 1);
    assert_eq!(state.last_seq, 10);
    let node_list = nodes(&store, s);
    assert_eq!(node_list.len(), 4);
    let keys: Vec<&str> = node_list.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(
        keys,
        vec!["m1", "1:1", "m2", "2:1"],
        "nodes rebuilt in seq order"
    );
    assert!(matches!(
        &node_list[1].data,
        NodeData::Assistant { blocks, .. } if blocks == &[AssistantBlock::Text { text: "XY".into() }]
    ));

    // Overlap tolerance: seqs >= oldest_seq are dropped, lower seqs accepted.
    let overlap = vec![
        StoredEvent::try_new(
            ev(0, "user/message", user_msg("m0", "zero", user_source())),
            None,
        )
        .unwrap(),
        StoredEvent::try_new(ev(5, "step/start", json!({"turn": 1, "step": 1})), None).unwrap(),
    ];
    store.prepend_events(&sid, overlap);
    let state = session_state(&store, s);
    assert_eq!(state.oldest_seq, 0);
    assert_eq!(
        nodes(&store, s)[0].key,
        "m0",
        "accepted page entry prepended"
    );
    assert!(
        !nodes(&store, s)
            .iter()
            .any(|n| n.key == "1:1" && n.anchor_seq == 5)
    );
}

// ---------------------------------------------------------------------------
// stream/error + malformed data
// ---------------------------------------------------------------------------

#[test]
fn stream_error_recorded_on_store() {
    let mut store = SessionStore::new();
    store
        .ingest(MuxFrame::StreamError {
            error: RpcError::Internal {
                message: "boom".into(),
                details: EmptyDetails {},
            },
        })
        .unwrap();
    assert_eq!(store.last_stream_error.as_deref(), Some("internal: boom"));
}

#[test]
fn malformed_known_event_data_rejected() {
    let mut store = SessionStore::new();
    let err = store.ingest(frame(
        "s1",
        ev(1, "turn/start", json!({"turn": "not-a-number"})),
    ));
    assert!(matches!(err, Err(StoreError::InvalidEventData { .. })));
    // The frame was rejected: no session state was created.
    assert!(store.session(&SessionId("s1".into())).is_none());
    // The store keeps working afterwards.
    store
        .ingest(frame(
            "s1",
            ev(2, "user/message", user_msg("m1", "ok", user_source())),
        ))
        .unwrap();
    assert_eq!(nodes(&store, "s1").len(), 1);
}

#[test]
fn stored_event_try_new_rejects_malformed_payloads() {
    let err = StoredEvent::try_new(
        ev(
            1,
            "tool/call",
            json!({"turn": 1, "step": 1, "callId": "c1"}),
        ),
        None,
    );
    assert!(err.is_err(), "missing name/arguments rejected");
    let ok = StoredEvent::try_new(
        ev(1, "user/message", user_msg("m1", "hi", user_source())),
        None,
    );
    assert!(ok.is_ok());
}

// ---------------------------------------------------------------------------
// fold state + user classification
// ---------------------------------------------------------------------------

#[test]
fn fold_state_defaults_and_override() {
    let mut store = SessionStore::new();
    let s = "s1";
    let sid = SessionId(s.into());
    // A user message + a running tool + a compaction marker.
    store
        .ingest(frame(
            s,
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
        ))
        .unwrap();
    store
        .ingest(frame(
            s,
            ev(
                2,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "c1", "name": "run", "arguments": "{}"}),
            ),
        ))
        .unwrap();
    store
        .ingest(frame(
            s,
            ev_surface(
                4,
                "user/message",
                user_msg(
                    "m2",
                    "s",
                    json!({"kind": "plugin", "plugin": "compact", "compactionId": "comp-1"}),
                ),
                json!({"op": "replace", "start": 1, "end": 1}),
            ),
        ))
        .unwrap();

    assert_eq!(
        store.fold_state(&sid, "m1"),
        FoldState::collapsed(),
        "user messages collapse by default"
    );
    assert_eq!(
        store.fold_state(&sid, "c1"),
        FoldState::collapsed(),
        "tool nodes collapse by default (#39 — the web's ToolRow starts collapsed)"
    );
    // The compaction marker collapses by default.
    let marker = nodes(&store, s)
        .iter()
        .find(|n| n.kind == ChatNodeKind::Compaction)
        .unwrap();
    assert_eq!(store.fold_state(&sid, &marker.key), FoldState::collapsed());

    // Explicit override wins.
    store.set_fold(&sid, "m1", FoldState::expanded());
    assert_eq!(store.fold_state(&sid, "m1"), FoldState::expanded());

    // Unknown session/key: default (expanded).
    assert_eq!(
        store.fold_state(&SessionId("nope".into()), "x"),
        FoldState::expanded()
    );
    assert_eq!(store.fold_state(&sid, "missing-key"), FoldState::expanded());
}

#[test]
fn context_injection_renders_as_context_user_node() {
    let mut store = SessionStore::new();
    let s = "s1";
    store
        .ingest(frame(
            s,
            ev(
                1,
                "user/message",
                user_msg(
                    "ctx-1",
                    "file changed",
                    json!({"kind": "plugin", "plugin": "agent-instructions"}),
                ),
            ),
        ))
        .unwrap();
    let node = &nodes(&store, s)[0];
    assert!(matches!(
        &node.data,
        NodeData::User { kind: UserNodeKind::Context, message_id, .. } if message_id == "ctx-1"
    ));
}

#[test]
fn multiple_sessions_are_isolated() {
    let mut store = SessionStore::new();
    let s1 = "s1";
    let s2 = "s2";
    ingest_all(
        &mut store,
        s1,
        vec![ev(1, "user/message", user_msg("m1", "a", user_source()))],
    );
    ingest_all(
        &mut store,
        s2,
        vec![ev(1, "user/message", user_msg("m2", "b", user_source()))],
    );
    assert_eq!(store.sessions().count(), 2);
    assert_eq!(nodes(&store, s1)[0].key, "m1");
    assert_eq!(nodes(&store, s2)[0].key, "m2");
    assert_eq!(session_state(&store, s1).last_seq, 1);
    assert_eq!(session_state(&store, s2).last_seq, 1);
}

// ---------------------------------------------------------------------------
// fold_events is pure and idempotent
// ---------------------------------------------------------------------------

#[test]
fn fold_events_is_pure_and_idempotent() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "Hi"}),
                ),
            ),
        ],
    );
    let first = session_state(&store, s).nodes.clone();
    // Duplicate seq ingest: rejected, nodes identical.
    store
        .ingest(frame(
            s,
            ev(
                3,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "text-delta", "index": 0, "text": "X"})),
            ),
        ))
        .unwrap();
    assert_eq!(session_state(&store, s).nodes, first);
}

// ---------------------------------------------------------------------------
// live-gateway finish shapes (ticket 08 Q5 smoke regression)
// ---------------------------------------------------------------------------

/// The real gateway emits finish chunks with the object reason shape
/// `{"kind":"stop"}` (FinishReasonMap, llm/src/types.ts:116-122) — `failure`
/// rides ONLY on aborted/error finishes. A stop finish without `failure`
/// must ingest cleanly (this crashed attach on the live smoke).
#[test]
fn finish_chunk_object_reason_shapes_ingest() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 0, "blockType": "text"}),
                ),
            ),
            ev(
                4,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 0, "text": "Hi"}),
                ),
            ),
            // stop finish, OBJECT reason, no `failure` (live gateway shape).
            ev(
                5,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "finish", "reason": {"kind": "stop"}})),
            ),
            ev(
                6,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {"id": "m2", "role": "assistant", "content": [{"type": "text", "text": "Hi"}], "source": {"kind": "model", "provider": "p", "model": "m"}},
                }),
            ),
            ev(7, "step/end", json!({"turn": 1, "step": 1})),
            ev(
                8,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ],
    );
    let state = session_state(&store, s);
    assert_eq!(state.last_seq, 8);
    // The folded assistant node carries the completed text.
    let assistant = nodes(&store, s)
        .iter()
        .find(|node| matches!(&node.data, NodeData::Assistant { blocks, .. } if blocks.iter().any(|b| matches!(b, AssistantBlock::Text { text } if text == "Hi"))));
    assert!(
        assistant.is_some(),
        "assistant node with text missing: {:?}",
        nodes(&store, s)
    );
}

/// Aborted/error finishes DO require `failure` (unchanged behavior), and
/// the tool-calls/max-tokens object shapes parse without it.
#[test]
fn finish_chunk_object_reason_failure_and_other_shapes() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "finish", "reason": {"kind": "tool-calls"}}),
                ),
            ),
            ev(
                4,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "finish", "reason": {"kind": "max-tokens"}}),
                ),
            ),
            ev(
                5,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({
                        "type": "finish",
                        "reason": {
                            "kind": "aborted",
                            "failure": {"code": "provider-error", "message": "boom"},
                        },
                    }),
                ),
            ),
            ev(
                6,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({
                        "type": "finish",
                        "reason": {
                            "kind": "error",
                            "failure": {"code": "provider-error", "message": "kaput"},
                        },
                    }),
                ),
            ),
        ],
    );
    assert_eq!(session_state(&store, s).last_seq, 6);

    // An aborted finish WITHOUT `failure` is still rejected (tolerant shape
    // is stop/tool-calls/max-tokens only).
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
        ],
    );
    let rejected = store.ingest(frame(
        s,
        ev(
            3,
            "assistant/chunk",
            chunk(
                1,
                1,
                json!({"type": "finish", "reason": {"kind": "aborted"}}),
            ),
        ),
    ));
    assert!(
        rejected.is_err(),
        "aborted finish without failure must be rejected"
    );
}

#[test]
fn hostile_chunk_index_is_dropped_not_allocated() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
            ev(2, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                3,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "text-delta", "index": 999_999_999, "text": "x"}),
                ),
            ),
            ev(
                4,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "block-start", "index": 2_147_483_647, "blockType": "text"}),
                ),
            ),
            ev(
                5,
                "assistant/chunk",
                chunk(
                    1,
                    1,
                    json!({"type": "reasoning-delta", "index": 1_000_000_000, "text": "y"}),
                ),
            ),
        ],
    );
    let nodes = nodes(&store, s);
    assert_eq!(
        nodes.len(),
        1,
        "hostile chunks must not materialize an assistant node"
    );
    assert_eq!(nodes[0].key, "m1", "the user message survives untouched");
}

// ---------------------------------------------------------------------------
// #38/#39: session stats aggregation (turns · steps · tokens · context)
// ---------------------------------------------------------------------------

#[test]
fn session_stats_aggregate_usage_turns_steps_and_context_window() {
    let mut store = SessionStore::new();
    let s = "s1";
    ingest_all(
        &mut store,
        s,
        vec![
            ev(
                1,
                "request/context",
                json!({"provider": "p", "model": "m", "contextWindow": 100}),
            ),
            ev(2, "user/message", user_msg("m1", "hi", user_source())),
            // Turn 1, step 1: usage with cached input.
            ev(3, "step/start", json!({"turn": 1, "step": 1})),
            ev(
                4,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "a1", "role": "assistant",
                        "content": [{"type": "text", "text": "ok"}],
                        "source": {"kind": "model", "provider": "p", "model": "m"},
                    },
                    "usage": {"inputTokens": 10, "outputTokens": 5, "cacheReadTokens": 3},
                }),
            ),
            // Turn 1, step 2: a tool step (no usage — tool steps don't
            // bill the model) — distinct step, no token change.
            ev(5, "step/start", json!({"turn": 1, "step": 2})),
            ev(
                6,
                "tool/call",
                json!({"turn": 1, "step": 2, "callId": "c1", "name": "bash", "arguments": "ls"}),
            ),
            ev(7, "step/end", json!({"turn": 1, "step": 2})),
            // Turn 2, step 1: a second model call.
            ev(8, "step/start", json!({"turn": 2, "step": 1})),
            ev(
                9,
                "assistant/message",
                json!({
                    "turn": 2, "step": 1,
                    "message": {
                        "id": "a2", "role": "assistant",
                        "content": [{"type": "text", "text": "again"}],
                        "source": {"kind": "model", "provider": "p", "model": "m"},
                    },
                    "usage": {"inputTokens": 2, "outputTokens": 1},
                }),
            ),
            // A newer context window wins over the earlier one.
            ev(
                10,
                "request/context",
                json!({"provider": "p", "model": "m", "contextWindow": 500}),
            ),
        ],
    );
    let stats = dsh_tui::store::session_stats(session_state(&store, s));
    assert_eq!(stats.turns, 2, "two turns with model activity");
    assert_eq!(stats.steps, 3, "two assistant steps + one tool step");
    assert_eq!(stats.input_tokens, 12, "10 + 2");
    assert_eq!(stats.output_tokens, 6, "5 + 1");
    assert_eq!(stats.cache_read_tokens, 3);
    assert_eq!(stats.cache_write_tokens, 0, "absent counts as zero");
    assert_eq!(stats.context_window, Some(500), "the LAST context wins");
}

#[test]
fn session_stats_hide_gracefully_without_usage_or_context() {
    let mut store = SessionStore::new();
    let s = "s1";
    // A prompt-only session: a user message, no turn/step nodes at all.
    store
        .ingest(frame(
            s,
            ev(1, "user/message", user_msg("m1", "hi", user_source())),
        ))
        .unwrap();
    let stats = dsh_tui::store::session_stats(session_state(&store, s));
    assert_eq!(stats.turns, 0, "no model activity — no turn nodes");
    assert_eq!(stats.steps, 0);
    assert_eq!(stats.input_tokens, 0);
    assert_eq!(stats.context_window, None, "never reported — hidden");
    assert_eq!(stats.llm_seconds, 0.0, "no in-window turn timing");
    assert_eq!(stats.ttft_seconds, None, "no measurable turn — TTFT hidden");
    assert_eq!(
        dsh_tui::store::tokens_per_second(&stats),
        None,
        "no output or duration — tok/s hidden"
    );
}

/// #39: the per-turn timing metrics — LLM duration from in-window
/// TurnStart→TurnEnd pairs, TTFT from the first chunk, tool duration from
/// call→result, tok/s from output / LLM duration. Events with envelope
/// times (the `ev_at` helper) — the window's `time` is the source.
#[test]
fn session_stats_derive_timing_from_the_event_window() {
    let mut store = SessionStore::new();
    let s = "s1";
    let ev_at = |seq: i64, time: f64, r#type: &str, data: serde_json::Value| {
        let mut event = ev(seq, r#type, data);
        event.time = time;
        event
    };
    ingest_all(
        &mut store,
        s,
        vec![
            ev_at(1, 1.0, "user/message", user_msg("m1", "hi", user_source())),
            // Turn 1: start 10 → first chunk 10.3 → end 110 (100s LLM).
            ev_at(2, 10.0, "turn/start", json!({"turn": 1})),
            ev_at(
                3,
                10.3,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "text-delta", "index": 0, "text": "H"})),
            ),
            ev_at(
                4,
                10.4,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "a1", "role": "assistant",
                        "content": [{"type": "text", "text": "Hi"}],
                        "source": {"kind": "model", "provider": "p", "model": "m"},
                    },
                    "usage": {"inputTokens": 10, "outputTokens": 100, "cacheReadTokens": 5},
                }),
            ),
            // A settled tool: 20 → 25 (5s).
            ev_at(
                5,
                20.0,
                "tool/call",
                json!({"turn": 1, "step": 2, "callId": "c1", "name": "bash", "arguments": "ls"}),
            ),
            ev_at(
                6,
                25.0,
                "tool/result",
                json!({
                    "turn": 1, "step": 2,
                    "message": {
                        "id": "r1", "role": "user",
                        "content": [{"type": "tool-result", "toolCallId": "c1", "content": [{"type": "text", "text": "out"}], "isError": false}],
                        "source": {"kind": "tool", "callId": "c1"},
                    },
                }),
            ),
            ev_at(
                7,
                110.0,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
            // Turn 2: start 210 → first chunk 210.4 → end 260 (50s LLM).
            ev_at(8, 210.0, "turn/start", json!({"turn": 2})),
            ev_at(
                9,
                210.4,
                "assistant/chunk",
                chunk(2, 1, json!({"type": "text-delta", "index": 0, "text": "A"})),
            ),
            ev_at(
                10,
                210.5,
                "assistant/message",
                json!({
                    "turn": 2, "step": 1,
                    "message": {
                        "id": "a2", "role": "assistant",
                        "content": [{"type": "text", "text": "Again"}],
                        "source": {"kind": "model", "provider": "p", "model": "m"},
                    },
                    "usage": {"inputTokens": 2, "outputTokens": 50},
                }),
            ),
            ev_at(
                11,
                260.0,
                "turn/end",
                json!({"turn": 2, "reason": {"kind": "completed"}}),
            ),
        ],
    );
    let stats = dsh_tui::store::session_stats(session_state(&store, s));
    assert_eq!(stats.turns, 2);
    assert_eq!(stats.steps, 3, "two assistant steps + one tool step");
    assert_eq!(stats.llm_seconds, 150.0, "100s + 50s");
    assert_eq!(stats.measured_turns, 2);
    // TTFT = mean(0.3, 0.4) = 0.35 (f64 mean — compare approximately).
    let ttft = stats.ttft_seconds.expect("ttft");
    assert!((ttft - 0.35).abs() < 1e-9, "ttft: {ttft}");
    assert_eq!(stats.tool_seconds, 5.0, "25 − 20");
    assert_eq!(stats.measured_tools, 1);
    // 150 output tokens / 150s = 1 tok/s.
    assert_eq!(dsh_tui::store::tokens_per_second(&stats), Some(1.0));
}

/// #39: a turn whose start fell outside the retained window contributes
/// nothing — the LLM/TTFT segments hide rather than fabricate. An
/// interrupted turn (no TurnEnd) is ignored too.
#[test]
fn session_stats_skip_window_cut_and_open_turns() {
    let mut store = SessionStore::new();
    let s = "s1";
    let ev_at = |seq: i64, time: f64, r#type: &str, data: serde_json::Value| {
        let mut event = ev(seq, r#type, data);
        event.time = time;
        event
    };
    ingest_all(
        &mut store,
        s,
        vec![
            // TurnEnd WITHOUT its TurnStart (window cut at seq 1).
            ev_at(
                1,
                50.0,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
            // Turn 2: start in-window but NO end (still open).
            ev_at(2, 10.0, "turn/start", json!({"turn": 2})),
            ev_at(
                3,
                10.2,
                "assistant/chunk",
                chunk(2, 1, json!({"type": "text-delta", "index": 0, "text": "H"})),
            ),
        ],
    );
    let stats = dsh_tui::store::session_stats(session_state(&store, s));
    assert_eq!(stats.measured_turns, 0, "no complete turn in-window");
    assert_eq!(stats.llm_seconds, 0.0);
    assert_eq!(stats.ttft_seconds, None);
    assert_eq!(
        dsh_tui::store::tokens_per_second(&stats),
        None,
        "no duration → no tok/s"
    );
}

/// #39: a closed turn WITHOUT a first chunk (immediate turn-error) must not
/// dilute or fabricate TTFT — the mean divides by chunked turns only, and an
/// all-chunkless window hides TTFT entirely.
#[test]
fn session_stats_ttft_excludes_chunkless_turns() {
    let mut store = SessionStore::new();
    let s = "s1";
    let ev_at = |seq: i64, time: f64, r#type: &str, data: serde_json::Value| {
        let mut event = ev(seq, r#type, data);
        event.time = time;
        event
    };
    ingest_all(
        &mut store,
        s,
        vec![
            // Turn 1: chunked — TTFT 0.3.
            ev_at(1, 10.0, "turn/start", json!({"turn": 1})),
            ev_at(
                2,
                10.3,
                "assistant/chunk",
                chunk(1, 1, json!({"type": "text-delta", "index": 0, "text": "H"})),
            ),
            ev_at(
                3,
                110.0,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
            // Turn 2: closed WITHOUT any chunk (immediate turn-error).
            ev_at(4, 200.0, "turn/start", json!({"turn": 2})),
            ev_at(
                5,
                201.0,
                "turn/end",
                json!({"turn": 2, "reason": {"kind": "error", "error": {"code": "provider-error", "message": "boom"}}}),
            ),
        ],
    );
    let stats = dsh_tui::store::session_stats(session_state(&store, s));
    assert_eq!(stats.measured_turns, 2, "both closed turns counted");
    assert_eq!(stats.llm_seconds, 101.0, "100s + 1s");
    let ttft = stats
        .ttft_seconds
        .expect("ttft measured over chunked turns only");
    assert!(
        (ttft - 0.3).abs() < 1e-9,
        "ttft: {ttft} — chunkless turn excluded"
    );

    // All-chunkless window: TTFT hidden, not 0.0.
    let mut store = SessionStore::new();
    ingest_all(
        &mut store,
        s,
        vec![
            ev_at(1, 10.0, "turn/start", json!({"turn": 1})),
            ev_at(
                2,
                11.0,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "error", "error": {"code": "provider-error", "message": "boom"}}}),
            ),
        ],
    );
    let stats = dsh_tui::store::session_stats(session_state(&store, s));
    assert_eq!(stats.measured_turns, 1);
    assert_eq!(
        stats.ttft_seconds, None,
        "no chunked turn — TTFT hidden, never 0.0"
    );
}

/// #41: `session/jobs` frames update the running-task count live —
/// Running/Stopping count, other statuses don't; a later frame replaces
/// the count wholesale; no frame yet → `None` (the header's jobs segment
/// stays hidden).
#[test]
fn session_jobs_frame_tracks_running_tasks() {
    let mut store = SessionStore::new();
    let s = "s1";
    let jobs_frame = |jobs: Vec<serde_json::Value>| dsh_tui::wire::events::MuxFrame::SessionJobs {
        session_id: SessionId(s.into()),
        jobs: jobs
            .into_iter()
            .map(|value| serde_json::from_value(value).expect("task view"))
            .collect(),
    };
    store
        .ingest(jobs_frame(vec![
            json!({"id": "t1", "kind": "file-write", "label": "w", "status": "running", "startedAt": 1}),
            json!({"id": "t2", "kind": "file-write", "label": "s", "status": "stopping", "startedAt": 1}),
            json!({"id": "t3", "kind": "file-write", "label": "c", "status": "completed", "startedAt": 1, "finishedAt": 2}),
        ]))
        .unwrap();
    assert_eq!(
        session_state(&store, s).running_jobs,
        Some(2),
        "running + stopping count"
    );
    // A later frame replaces the count (all done → 0, still Some(0)).
    store
        .ingest(jobs_frame(vec![json!({"id": "t1", "kind": "file-write", "label": "w", "status": "killed", "startedAt": 1, "finishedAt": 2})]))
        .unwrap();
    assert_eq!(session_state(&store, s).running_jobs, Some(0));
    // No jobs frame yet → None (the header's segment stays hidden).
    let mut fresh = SessionStore::new();
    fresh.open_session(SessionId(s.into()));
    assert_eq!(
        session_state(&fresh, s).running_jobs,
        None,
        "never reported — hidden"
    );
}
