//! Coverage-push fold + event-data tests (the hostile-wire phase): chunk
//! index bounds (negative and i64::MAX — the grow_to cap), ignorable vs
//! required unknown events, tool-call streaming deltas, tool call+result
//! folding, turn-end notice nodes, open-work boundary closes, compaction
//! events, surface-op parse helpers, and the todo/request event types.
//! Tolerance posture throughout: hostile inputs must never panic.

use serde_json::json;

use dsh_tui::store::event_data::parse_surface_op;
use dsh_tui::store::node::{ChatNodeKind, NodeData};
use dsh_tui::store::{SessionStore, SessionStore as Store};
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId};

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
    op: serde_json::Value,
) -> SessionEvent {
    let mut event = ev(seq, r#type, data);
    event.surface_op = Some(op);
    event
}

fn frame(session: &str, event: SessionEvent) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: None,
    }
}

fn chunk(turn: i64, step: i64, data: serde_json::Value) -> serde_json::Value {
    json!({"turn": turn, "step": step, "chunk": data})
}

fn user_msg(id: &str, text: &str) -> serde_json::Value {
    json!({"id": id, "role": "user", "content": [{"type": "text", "text": text}], "source": {"kind": "user"}})
}

struct Harness {
    store: SessionStore,
}

impl Harness {
    fn new() -> Self {
        Harness {
            store: Store::new(),
        }
    }

    fn ingest(&mut self, event: SessionEvent) {
        // Malformed KNOWN event types are rejected at ingest by design —
        // the tolerance contract is "no panic", not "accepted".
        let _ = self.store.ingest(frame("s1", event));
    }

    fn nodes(&self) -> Vec<&dsh_tui::store::node::ChatNode> {
        self.store
            .session(&SessionId("s1".into()))
            .map(|state| state.nodes.iter().collect())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// 1. unknown events: ignorable skipped, required degraded
// ---------------------------------------------------------------------------

#[test]
fn unknown_events_degrade_without_panicking() {
    let mut h = Harness::new();
    h.ingest(ev_ignorable(1, "plugin/sync", json!({"x": 1})));
    assert!(
        h.nodes().is_empty(),
        "ignorable unknown contributes nothing"
    );
    h.ingest(ev(2, "plugin/sync", json!({"x": 1})));
    assert_eq!(h.nodes().len(), 1, "required unknown degrades to a node");
    assert_eq!(h.nodes()[0].kind, ChatNodeKind::Unknown);
}

// ---------------------------------------------------------------------------
// 2. hostile chunk indices: negative + i64::MAX (the block cap)
// ---------------------------------------------------------------------------

#[test]
fn negative_chunk_indices_are_dropped() {
    let mut h = Harness::new();
    h.ingest(ev(1, "user/message", user_msg("m1", "hi")));
    h.ingest(ev(2, "step/start", json!({"turn": 1, "step": 1})));
    for (seq, chunk_data) in [
        (
            3,
            json!({"type": "block-start", "index": -1, "blockType": "text"}),
        ),
        (4, json!({"type": "text-delta", "index": -1, "text": "x"})),
        (
            5,
            json!({"type": "reasoning-delta", "index": -1, "text": "x"}),
        ),
        (
            6,
            json!({"type": "tool-call-delta", "index": -1, "id": "c1"}),
        ),
        (
            7,
            json!({"type": "block-end", "index": -1, "block": {"type": "text", "text": "x"}}),
        ),
    ] {
        h.ingest(ev(seq, "assistant/chunk", chunk(1, 1, chunk_data)));
    }
    h.ingest(ev(8, "step/end", json!({"turn": 1, "step": 1})));
    h.ingest(ev(
        9,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "completed"}}),
    ));
    // Dropped chunks leave an empty assistant: it has no interruption
    // evidence, so close_assistant skips it.
    assert_eq!(h.nodes().len(), 1, "user only");
}

#[test]
fn huge_chunk_index_hits_the_block_cap_without_ooming() {
    // The oracle regression pin: i64::MAX block indices must not allocate
    // (grow_to caps at MAX_ASSISTANT_BLOCKS) — and must not panic.
    let mut h = Harness::new();
    h.ingest(ev(1, "step/start", json!({"turn": 1, "step": 1})));
    h.ingest(ev(
        2,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "block-start", "index": i64::MAX, "blockType": "text"}),
        ),
    ));
    h.ingest(ev(
        3,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "text-delta", "index": i64::MAX, "text": "x"}),
        ),
    ));
    h.ingest(ev(
        4,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "tool-call-delta", "index": i64::MAX, "id": "c1", "name": "bash"}),
        ),
    ));
    h.ingest(ev(
        5,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "block-end", "index": i64::MAX, "block": {"type": "text", "text": "x"}}),
        ),
    ));
    h.ingest(ev(6, "step/end", json!({"turn": 1, "step": 1})));
    h.ingest(ev(
        7,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "completed"}}),
    ));
    // The capped blocks stayed empty; nothing was published.
    assert!(h.nodes().is_empty(), "{:?}", h.nodes());
}

// ---------------------------------------------------------------------------
// 3. tool-call streaming deltas merge into one block
// ---------------------------------------------------------------------------

#[test]
fn tool_call_deltas_merge_and_the_empty_call_id_is_backfilled() {
    let mut h = Harness::new();
    h.ingest(ev(1, "step/start", json!({"turn": 1, "step": 1})));
    // Block-start opens an EMPTY tool-call block.
    h.ingest(ev(
        2,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "block-start", "index": 0, "blockType": "tool-call"}),
        ),
    ));
    // First delta backfills the id/name; the second merges the arguments.
    h.ingest(ev(
        3,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "tool-call-delta", "index": 0, "id": "c1", "name": "bash", "argumentsDelta": "ls "}),
        ),
    ));
    h.ingest(ev(
        4,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "tool-call-delta", "index": 0, "id": "c1", "argumentsDelta": "-la"}),
        ),
    ));
    h.ingest(ev(
        5,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "usage", "usage": {"inputTokens": 1, "outputTokens": 2}}),
        ),
    ));
    h.ingest(ev(
        6,
        "assistant/chunk",
        chunk(1, 1, json!({"type": "finish", "reason": {"kind": "stop"}})),
    ));
    h.ingest(ev(7, "step/end", json!({"turn": 1, "step": 1})));
    h.ingest(ev(
        8,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "completed"}}),
    ));

    let nodes = h.nodes();
    assert_eq!(nodes.len(), 1);
    let NodeData::Assistant { blocks, .. } = &nodes[0].data else {
        panic!("expected assistant");
    };
    assert_eq!(blocks.len(), 1, "one merged tool-call block");
    match &blocks[0] {
        dsh_tui::store::node::AssistantBlock::ToolCall {
            call_id,
            name,
            args_raw,
        } => {
            assert_eq!(call_id, "c1");
            assert_eq!(name, "bash");
            assert_eq!(args_raw, "ls -la");
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 4. tool call + result fold into one node; boundaries close open work
// ---------------------------------------------------------------------------

#[test]
fn tool_call_and_error_result_fold_into_one_node() {
    let mut h = Harness::new();
    h.ingest(ev(1, "step/start", json!({"turn": 1, "step": 1})));
    h.ingest(ev(
        2,
        "tool/call",
        json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": "ls"}),
    ));
    h.ingest(ev(
        3,
        "tool/result",
        json!({
            "turn": 1, "step": 1,
            "message": {
                "id": "r1", "role": "user",
                "content": [{"type": "tool-result", "toolCallId": "c1", "content": [], "isError": true}],
                "source": {"kind": "tool", "callId": "c1"},
            },
            "error": {"name": "bash", "code": "exit-1"},
        }),
    ));
    // step/end closes the open tool (result is set → close_tool publishes).
    h.ingest(ev(4, "step/end", json!({"turn": 1, "step": 1})));
    h.ingest(ev(
        5,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "completed"}}),
    ));

    let nodes = h.nodes();
    assert_eq!(nodes.len(), 1);
    let NodeData::Tool { call, result, .. } = &nodes[0].data else {
        panic!("expected tool node");
    };
    assert!(call.is_some(), "call backfilled");
    let result = result.as_ref().expect("result");
    assert!(result.is_error);
    assert_eq!(
        result.error.as_ref().map(|e| e.code.as_str()),
        Some("exit-1")
    );
}

#[test]
fn user_message_closes_open_assistant_and_tool_work() {
    let mut h = Harness::new();
    h.ingest(ev(1, "step/start", json!({"turn": 1, "step": 1})));
    h.ingest(ev(
        2,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "block-start", "index": 0, "blockType": "text"}),
        ),
    ));
    h.ingest(ev(
        3,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "text-delta", "index": 0, "text": "partial"}),
        ),
    ));
    h.ingest(ev(
        4,
        "tool/call",
        json!({"turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": "ls"}),
    ));
    // A new human prompt closes everything open: the assistant degrades to
    // an interrupted node, the tool to an interrupted node.
    h.ingest(ev(5, "user/message", user_msg("m2", "stop")));

    let nodes = h.nodes();
    let tool = nodes
        .iter()
        .find(|n| n.kind == ChatNodeKind::Tool)
        .expect("interrupted tool node");
    let NodeData::Tool {
        result,
        interrupted,
        ..
    } = &tool.data
    else {
        panic!("expected tool");
    };
    assert!(*interrupted);
    assert!(
        result.as_ref().is_some_and(|r| r.is_error),
        "synthesized error result"
    );
    let assistant = nodes
        .iter()
        .find(|n| n.kind == ChatNodeKind::Assistant)
        .expect("interrupted assistant node");
    let NodeData::Assistant { interrupted, .. } = &assistant.data else {
        panic!("expected assistant");
    };
    assert!(*interrupted);
}

// ---------------------------------------------------------------------------
// 5. turn-end notice nodes
// ---------------------------------------------------------------------------

#[test]
fn turn_end_error_and_max_tokens_push_notice_nodes() {
    let mut h = Harness::new();
    // Turn 1: an open assistant then an error turn/end → TurnError node.
    h.ingest(ev(1, "step/start", json!({"turn": 1, "step": 1})));
    h.ingest(ev(
        2,
        "assistant/chunk",
        chunk(
            1,
            1,
            json!({"type": "block-start", "index": 0, "blockType": "text"}),
        ),
    ));
    h.ingest(ev(
        3,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "error", "error": {"message": "nope", "code": "bad-input"}}}),
    ));
    // Turn 2: max-tokens (string form) → TurnMaxTokens node.
    h.ingest(ev(4, "step/start", json!({"turn": 2, "step": 1})));
    h.ingest(ev(
        5,
        "turn/end",
        json!({"turn": 2, "reason": "max-tokens"}),
    ));

    let nodes = h.nodes();
    assert!(
        nodes.iter().any(|n| n.kind == ChatNodeKind::TurnError),
        "{nodes:?}"
    );
    assert!(
        nodes.iter().any(|n| n.kind == ChatNodeKind::TurnMaxTokens),
        "{nodes:?}"
    );
}

// ---------------------------------------------------------------------------
// 6. compaction events
// ---------------------------------------------------------------------------

#[test]
fn compaction_start_summary_and_checkpoint_fold() {
    let mut h = Harness::new();
    h.ingest(ev(1, "user/message", user_msg("m1", "one")));
    // A start without a compactionId contributes nothing (lenient).
    h.ingest(ev(2, "compaction/start", json!({"seq": 2})));
    h.ingest(ev(
        3,
        "compaction/start",
        json!({"compactionId": "cmp-1", "seq": 3}),
    ));
    h.ingest(ev(
        4,
        "compaction/summary",
        json!({
            "compactionId": "cmp-1",
            "summary": [{"type": "text", "text": "the summary"}],
            "shadowedSeqs": [1, 2],
            "shadowedTokenCount": 50,
        }),
    ));
    // A malformed summary is lenient.
    h.ingest(ev(5, "compaction/summary", json!({"nope": true})));
    // The checkpoint (a plugin compact replace message) materializes the node.
    h.ingest(ev_surface(
        6,
        "user/message",
        json!({
            "id": "m-cmp", "role": "user",
            "content": [{"type": "text", "text": "compacted"}],
            "source": {"kind": "plugin", "plugin": "compact", "compactionId": "cmp-1"},
        }),
        json!({"op": "replace", "start": 0, "end": 2}),
    ));
    h.ingest(ev(
        7,
        "compaction/end",
        json!({"compactionId": "cmp-1", "seq": 7}),
    ));

    let nodes = h.nodes();
    let compaction = nodes
        .iter()
        .find(|n| n.kind == ChatNodeKind::Compaction)
        .expect("compaction node");
    match &compaction.data {
        NodeData::Compaction {
            summary,
            shadowed_item_count,
            shadowed_token_count,
            ..
        } => {
            assert_eq!(summary.as_deref(), Some("the summary"));
            assert_eq!(*shadowed_item_count, Some(2));
            assert_eq!(*shadowed_token_count, Some(50));
        }
        other => panic!("expected Compaction, got {other:?}"),
    }
}

#[test]
fn compaction_checkpoint_without_a_summary_renders_an_empty_marker() {
    let mut h = Harness::new();
    h.ingest(ev_surface(
        1,
        "user/message",
        json!({
            "id": "m-cmp", "role": "user",
            "content": [{"type": "text", "text": "compacted"}],
            "source": {"kind": "plugin", "plugin": "compact", "compactionId": "cmp-2"},
        }),
        json!({"op": "replace", "start": 0, "end": 2}),
    ));
    let nodes = h.nodes();
    let compaction = nodes
        .iter()
        .find(|n| n.kind == ChatNodeKind::Compaction)
        .expect("compaction node");
    match &compaction.data {
        NodeData::Compaction {
            summary,
            shadowed_item_count,
            ..
        } => {
            assert_eq!(summary, &None);
            assert_eq!(*shadowed_item_count, None);
        }
        other => panic!("expected Compaction, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 7. surface-op parse + todo/request events
// ---------------------------------------------------------------------------

#[test]
fn surface_op_parse_accepts_strings_objects_and_garbage() {
    assert_eq!(
        parse_surface_op(Some(&json!("append"))),
        Some(dsh_tui::store::event_data::SurfaceOp::Append)
    );
    assert_eq!(
        parse_surface_op(Some(&json!({"op": "replace", "start": 1, "end": 5}))),
        Some(dsh_tui::store::event_data::SurfaceOp::Replace { start: 1, end: 5 })
    );
    assert_eq!(parse_surface_op(Some(&json!({"op": "replace"}))), None);
    assert_eq!(parse_surface_op(Some(&json!("other"))), None);
    assert_eq!(parse_surface_op(Some(&json!({"op": "append"}))), None);
    assert_eq!(parse_surface_op(Some(&json!(42))), None);
    assert_eq!(parse_surface_op(None), None);
}

#[test]
fn todo_request_and_context_events_are_tolerated() {
    let mut h = Harness::new();
    h.ingest(ev(
        1,
        "todo/write",
        json!({"todos": [{"id": "t1", "content": "fix", "status": "pending"}]}),
    ));
    h.ingest(ev(2, "request/header", json!({"header": {"x": 1}})));
    h.ingest(ev(
        3,
        "request/context",
        json!({"provider": "p", "model": "m", "contextWindow": 100}),
    ));
    h.ingest(ev(4, "session/end-seed", json!({})));
    h.ingest(ev(5, "turn/start", json!({"turn": 9})));
    // A bare-string turn/end reason (completed) is tolerated.
    h.ingest(ev(6, "turn/end", json!({"turn": 9, "reason": "completed"})));
    h.ingest(ev(7, "turn/end", json!({"turn": 10, "reason": "blocked"})));
    h.ingest(ev(
        8,
        "turn/end",
        json!({"turn": 11, "reason": "interrupted"}),
    ));
    h.ingest(ev(
        9,
        "turn/end",
        json!({"turn": 12, "reason": "something-new"}),
    ));
    // Object-form aborted/blocked reasons parse too.
    h.ingest(ev(
        10,
        "turn/end",
        json!({"turn": 13, "reason": {"kind": "aborted", "reason": {"kind": "user"}}}),
    ));
    h.ingest(ev(
        11,
        "turn/end",
        json!({"turn": 14, "reason": {"kind": "blocked"}}),
    ));
    h.ingest(ev(
        12,
        "turn/end",
        json!({"turn": 15, "reason": {"kind": "interrupted"}}),
    ));
    h.ingest(ev(
        13,
        "turn/end",
        json!({"turn": 16, "reason": {"kind": "custom"}}),
    ));
    // No nodes: todo/request events and notice-less turn ends contribute
    // nothing to the chat; the store stayed coherent.
    assert!(h.nodes().is_empty(), "{:?}", h.nodes());
}

// ---------------------------------------------------------------------------
// 8. wrong-typed hostile fixtures: tolerance, no panic
// ---------------------------------------------------------------------------

#[test]
fn wrong_typed_fields_degrade_like_malformed_frames() {
    let mut h = Harness::new();
    // tool/call with a non-string arguments field.
    h.ingest(ev(
        1,
        "tool/call",
        json!({"turn": 1, "step": 1, "callId": "c1", "name": 42, "arguments": []}),
    ));
    // A step/start with a non-integer turn.
    h.ingest(ev(2, "step/start", json!({"turn": "x", "step": 1})));
    // A user/message whose content is a string instead of an array.
    h.ingest(ev(
        3,
        "user/message",
        json!({"id": "m1", "role": "user", "content": "nope", "source": {"kind": "user"}}),
    ));
    // A deep-nesting bomb in an unknown event's data (parsed as Raw).
    h.ingest(ev_ignorable(
        4,
        "plugin/deep",
        json!({"nested": {"nested": {"nested": {"deep": true}}}}),
    ));
    // The store survived; nothing panicked.
    assert!(h.nodes().is_empty(), "{:?}", h.nodes());
}
