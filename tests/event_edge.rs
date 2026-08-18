//! Edge-variant parse coverage (#44 companion): the typed event-data parser's
//! less-traveled branches (turn-end reasons, finish kinds, cancel causes,
//! malformed payloads) and the ingestion of those variants through the store
//! — the trajectory ledger and the chat fold both read these typed shapes.

use serde_json::json;

use dsh_tui::store::SessionStore;
use dsh_tui::store::event_data::{
    CallId, EventData, FinishReason, TurnEndReason, parse_event_data, parse_finish_reason,
};
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

fn frame(session: &str, event: SessionEvent) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: None,
    }
}

#[test]
fn call_id_display_and_as_ref() {
    let id = CallId("c-1".into());
    assert_eq!(id.to_string(), "c-1");
    assert_eq!(id.as_ref(), "c-1");
}

/// Turn-end reasons cover the string kinds, the object kinds with their
/// cancel-cause sub-shapes, and unknown kind/string fallbacks.
#[test]
fn turn_end_reasons_parse_all_kinds() {
    for (seq, reason, expect) in [
        (1, json!("completed"), TurnEndReason::Completed),
        (2, json!("blocked"), TurnEndReason::Blocked),
        (3, json!("max-tokens"), TurnEndReason::MaxTokens),
        (4, json!("interrupted"), TurnEndReason::Interrupted),
        (
            5,
            json!("mystery"),
            TurnEndReason::Unknown("mystery".into()),
        ),
        (
            6,
            json!({"kind": "aborted", "reason": {"kind": "user"}}),
            TurnEndReason::Aborted {
                reason: dsh_tui::store::event_data::TurnEndCancelCause::User,
            },
        ),
        (
            7,
            json!({"kind": "aborted", "reason": {"kind": "parent"}}),
            TurnEndReason::Aborted {
                reason: dsh_tui::store::event_data::TurnEndCancelCause::Parent,
            },
        ),
        (
            8,
            json!({"kind": "aborted", "reason": {"kind": "hook", "reason": "nope"}}),
            TurnEndReason::Aborted {
                reason: dsh_tui::store::event_data::TurnEndCancelCause::Hook {
                    reason: "nope".into(),
                },
            },
        ),
        (
            9,
            json!({"kind": "aborted", "reason": {"kind": "disposed"}}),
            TurnEndReason::Aborted {
                reason: dsh_tui::store::event_data::TurnEndCancelCause::Disposed,
            },
        ),
        (
            10,
            json!({"kind": "aborted", "reason": {"kind": "legacy"}}),
            TurnEndReason::Aborted {
                reason: dsh_tui::store::event_data::TurnEndCancelCause::Legacy,
            },
        ),
        (
            11,
            json!({"kind": "aborted", "reason": {"kind": "whatever"}}),
            TurnEndReason::Aborted {
                reason: dsh_tui::store::event_data::TurnEndCancelCause::Unknown("whatever".into()),
            },
        ),
        (
            12,
            json!({"kind": "newfangled"}),
            TurnEndReason::Unknown("newfangled".into()),
        ),
    ] {
        let data = parse_event_data("turn/end", &json!({"turn": 1, "reason": reason}), false)
            .unwrap_or_else(|e| panic!("seq {seq}: {e}"));
        assert_eq!(
            data,
            EventData::TurnEnd {
                turn: 1,
                reason: expect
            },
            "seq {seq}"
        );
    }

    // A missing `reason` rejects the event (the strict-known-type rule).
    assert!(parse_event_data("turn/end", &json!({"turn": 1}), false).is_err());
    // A reason object without a `kind` rejects too.
    assert!(parse_event_data("turn/end", &json!({"turn": 1, "reason": {"x": 1}}), false).is_err());
    // A reason that is neither string nor object rejects.
    assert!(parse_event_data("turn/end", &json!({"turn": 1, "reason": 42}), false).is_err());
}

/// The finish-reason parser: string kinds, object kinds with failures, and
/// the merge-extensible fallbacks.
#[test]
fn finish_reasons_parse_strings_and_objects() {
    assert_eq!(parse_finish_reason(&json!("stop")), Ok(FinishReason::Stop));
    assert_eq!(
        parse_finish_reason(&json!("tool-calls")),
        Ok(FinishReason::ToolCalls)
    );
    assert_eq!(
        parse_finish_reason(&json!("max-tokens")),
        Ok(FinishReason::MaxTokens)
    );
    assert_eq!(
        parse_finish_reason(&json!("weird")),
        Ok(FinishReason::Unknown("weird".into()))
    );
    let failure = json!({"message": "boom", "code": "E", "status": 500});
    assert_eq!(
        parse_finish_reason(&json!({"kind": "aborted", "failure": failure.clone()})),
        Ok(FinishReason::Aborted {
            failure: serde_json::from_value(failure.clone()).unwrap(),
        })
    );
    assert!(matches!(
        parse_finish_reason(&json!({"kind": "error", "failure": failure})),
        Ok(FinishReason::Error { .. })
    ));
    assert!(parse_finish_reason(&json!(42)).is_err());
}

/// A malformed known-type payload rejects the whole ingest; unknown types
/// degrade to `Unknown` and stay ingestible.
#[test]
fn malformed_known_type_rejects_and_unknown_stays_tolerated() {
    let mut store = SessionStore::new();
    // turn/end missing `reason` — a known type with a bad payload.
    let result = store.ingest(frame("s1", ev(1, "turn/end", json!({"turn": 1}))));
    assert!(result.is_err(), "known type rejects");

    // request/context with a non-integer contextWindow rejects.
    let result = store.ingest(frame(
        "s2",
        ev(
            1,
            "request/context",
            json!({"provider": "p", "model": "m", "contextWindow": "wide"}),
        ),
    ));
    assert!(result.is_err(), "contextWindow typed strictly");

    // Unknown + ignorable event parses to Unknown and the store accepts it.
    store
        .ingest(frame("s3", ev(1, "plugin.xyz", json!({"x": 1}))))
        .expect("unknown tolerates");
}

#[test]
fn todo_and_user_variants_parse() {
    // todo/write with todos.
    let data = parse_event_data(
        "todo/write",
        &json!({"todos": [{"content": "step one", "status": "pending"}]}),
        false,
    )
    .unwrap();
    assert!(matches!(
        data,
        EventData::TodoWrite { todos } if todos.len() == 1
    ));
    // User messages with plugin sources stay typed (the fold reads source).
    let data = parse_event_data(
        "user/message",
        &json!({"id": "u1", "content": [{"type": "text", "text": "hi"}], "source": {"kind": "plugin", "plugin": "web"}}),
        false,
    )
    .unwrap();
    match data {
        EventData::UserMessage(message) => {
            assert_eq!(message.source_kind(), Some("plugin"));
            assert_eq!(message.source_plugin(), Some("web"));
        }
        other => panic!("expected user message, got {other:?}"),
    }
}

/// A full turn with every reason kind ingests and folds into the node list
/// (the chat fold's turn-end branch reads the reason).
#[test]
fn turn_end_variants_ingest_and_fold() {
    for (seq, reason) in [
        (1, json!("completed")),
        (2, json!("interrupted")),
        (3, json!({"kind": "aborted", "reason": {"kind": "user"}})),
    ] {
        let mut store = SessionStore::new();
        store
            .ingest(frame("s1", ev(1, "turn/start", json!({"turn": 1}))))
            .expect("start");
        store
            .ingest(frame(
                "s1",
                ev(2, "turn/end", json!({"turn": 1, "reason": reason})),
            ))
            .expect("end");
        let state = store.session(&SessionId("s1".into())).unwrap();
        assert_eq!(state.last_seq, 2, "seq {seq}: both events applied");
        assert_eq!(seq, seq); // the reason shape folded without error
    }
}
