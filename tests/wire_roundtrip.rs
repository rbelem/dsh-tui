//! Hand-built JSON fixture round-trip tests for the wire models.
//!
//! Fixtures are inlined JSON built from the zod schemas (keyless, no network,
//! no file reads). Every test asserts (1) parse success + field spot-checks or
//! full struct equality, and (2) parse → serialize → parse stability.

use dsh_tui::wire::*;
use serde::{Serialize, de::DeserializeOwned};
use std::fmt::Debug;

/// Parse `json`, assert that re-serializing and re-parsing yields the same
/// struct, and return the parsed value.
fn round_trip<T>(json: &str) -> T
where
    T: DeserializeOwned + Serialize + PartialEq + Debug,
{
    let parsed: T = serde_json::from_str(json).expect("fixture must parse");
    let re = serde_json::to_string(&parsed).expect("re-serialization must succeed");
    let reparsed: T = serde_json::from_str(&re).expect("re-serialized JSON must parse");
    assert_eq!(parsed, reparsed, "round-trip must be stable");
    parsed
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).expect("inline JSON must be valid")
}

// ---------------------------------------------------------------------------
// Full forms (rpc.schema.ts)
// ---------------------------------------------------------------------------

mod full_forms {
    use super::*;

    #[test]
    fn client_request_round_trip() {
        let fixture = r#"{"type":"client-request","rpcId":"rpc-100","method":"session.list","payload":{"cursor":null}}"#;
        let req: ClientRequest = round_trip(fixture);
        assert_eq!(req.r#type, ClientRequestType::ClientRequest);
        assert_eq!(req.rpc_id, RpcId("rpc-100".into()));
        assert_eq!(req.method, "session.list");
        assert_eq!(req.payload, json(r#"{"cursor":null}"#));
        let expected = ClientRequest {
            r#type: ClientRequestType::ClientRequest,
            rpc_id: RpcId("rpc-100".into()),
            method: "session.list".into(),
            payload: json(r#"{"cursor":null}"#),
        };
        assert_eq!(req, expected);
    }

    #[test]
    fn server_response_ok_round_trip() {
        let fixture = r#"{"type":"server-response","rpcId":"rpc-100","result":{"ok":true,"value":{"items":[]}}}"#;
        let resp: ServerResponse = round_trip(fixture);
        assert_eq!(resp.r#type, ServerResponseType::ServerResponse);
        assert!(resp.result.ok);
        assert_eq!(resp.result.value, Some(json(r#"{"items":[]}"#)));
        assert_eq!(resp.result.error, None);
    }

    #[test]
    fn server_response_error_round_trip() {
        let fixture = r#"{"type":"server-response","rpcId":"rpc-101","result":{"ok":false,"error":{"code":"internal","message":"boom","details":{}}}}"#;
        let resp: ServerResponse = round_trip(fixture);
        assert!(!resp.result.ok);
        assert_eq!(resp.result.value, None);
        assert_eq!(
            resp.result.error,
            Some(RpcError::Internal {
                message: "boom".into(),
                details: EmptyDetails {},
            })
        );
    }

    #[test]
    fn server_response_ok_without_value_round_trips() {
        // A void business result serializes with no `value` field at all.
        let fixture = r#"{"type":"server-response","rpcId":"rpc-102","result":{"ok":true}}"#;
        let resp: ServerResponse = round_trip(fixture);
        assert!(resp.result.ok);
        assert_eq!(resp.result.value, None);
        let re = serde_json::to_string(&resp).unwrap();
        assert!(
            !re.contains("value"),
            "void result must not serialize value: {re}"
        );
    }

    #[test]
    fn client_response_round_trip() {
        let fixture = r#"{"type":"client-response","rpcId":"rpc-103","result":{"ok":true,"value":{"sessionId":"s1","approvalId":"a1","outcome":"allowed-once"}}}"#;
        let resp: ClientResponse = round_trip(fixture);
        assert_eq!(resp.r#type, ClientResponseType::ClientResponse);
        assert_eq!(resp.rpc_id, RpcId("rpc-103".into()));
        let expected = ClientResponse {
            r#type: ClientResponseType::ClientResponse,
            rpc_id: RpcId("rpc-103".into()),
            result: RpcResult {
                ok: true,
                value: Some(json(
                    r#"{"sessionId":"s1","approvalId":"a1","outcome":"allowed-once"}"#,
                )),
                error: None,
            },
        };
        assert_eq!(resp, expected);
    }

    #[test]
    fn server_request_round_trip() {
        let fixture = r#"{"type":"server-request","rpcId":"rpc-104","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s1","lastSeq":5}}"#;
        let req: ServerRequest = round_trip(fixture);
        assert_eq!(req.r#type, ServerRequestType::ServerRequest);
        assert_eq!(req.method, "events.mux");
        assert_eq!(req.rpc_id, RpcId("rpc-104".into()));
    }

    #[test]
    fn rpc_receipt_accepted() {
        let receipt: RpcReceipt = round_trip(r#"{"accepted":true}"#);
        assert_eq!(
            receipt,
            RpcReceipt {
                accepted: true,
                reason: None,
            }
        );
    }

    #[test]
    fn rpc_receipt_rejected_not_pending() {
        let receipt: RpcReceipt = round_trip(r#"{"accepted":false,"reason":"not-pending"}"#);
        assert_eq!(
            receipt,
            RpcReceipt {
                accepted: false,
                reason: Some(RpcReceiptReason::NotPending),
            }
        );
    }

    #[test]
    fn rpc_receipt_rejected_bad_response() {
        let receipt: RpcReceipt = round_trip(r#"{"accepted":false,"reason":"bad-response"}"#);
        assert_eq!(
            receipt,
            RpcReceipt {
                accepted: false,
                reason: Some(RpcReceiptReason::BadResponse),
            }
        );
    }
}

// ---------------------------------------------------------------------------
// RpcError: all 40 code branches (rpc.schema.ts:34-79)
// ---------------------------------------------------------------------------

mod rpc_error {
    use super::*;

    fn error_fixture(code: &str, details: &str) -> String {
        format!(r#"{{"code":"{code}","message":"m","details":{details}}}"#)
    }

    fn code_of(e: &RpcError) -> &'static str {
        match e {
            RpcError::BadRequest { .. } => "bad-request",
            RpcError::Cancelled { .. } => "cancelled",
            RpcError::SessionNotFound { .. } => "session-not-found",
            RpcError::ModelUnavailable { .. } => "model-unavailable",
            RpcError::SessionConflict { .. } => "session-conflict",
            RpcError::InvalidTimeZone { .. } => "invalid-time-zone",
            RpcError::WorkspaceAttachFailed { .. } => "workspace-attach-failed",
            RpcError::WorkspaceNotFound { .. } => "workspace-not-found",
            RpcError::WorkspaceInvalidPath { .. } => "workspace-invalid-path",
            RpcError::WorkspaceNameConflict { .. } => "workspace-name-conflict",
            RpcError::WorkspaceMoveInvalid { .. } => "workspace-move-invalid",
            RpcError::DirectoryUnreadable { .. } => "directory-unreadable",
            RpcError::DirectoryExists { .. } => "directory-exists",
            RpcError::DirectoryCreateFailed { .. } => "directory-create-failed",
            RpcError::DirectoryPickerUnavailable { .. } => "directory-picker-unavailable",
            RpcError::AgentPresetReadOnly { .. } => "agent-preset-read-only",
            RpcError::AgentPresetLocked { .. } => "agent-preset-locked",
            RpcError::AgentPresetConflict { .. } => "agent-preset-conflict",
            RpcError::AgentPresetNotFound { .. } => "agent-preset-not-found",
            RpcError::AgentPresetInvalid { .. } => "agent-preset-invalid",
            RpcError::AgentBusy { .. } => "agent-busy",
            RpcError::AttachmentError { .. } => "attachment-error",
            RpcError::QueueItemNotFound { .. } => "queue-item-not-found",
            RpcError::SteerUnavailable { .. } => "steer-unavailable",
            RpcError::CommandError { .. } => "command-error",
            RpcError::UnknownCommand { .. } => "unknown-command",
            RpcError::SettingsRejected { .. } => "settings-rejected",
            RpcError::SettingsNotExposed { .. } => "settings-not-exposed",
            RpcError::SettingsConflict { .. } => "settings-conflict",
            RpcError::CredentialRejected { .. } => "credential-rejected",
            RpcError::ModelDiscoveryFailed { .. } => "model-discovery-failed",
            RpcError::TitleInvalid { .. } => "title-invalid",
            RpcError::ForkUnavailable { .. } => "fork-unavailable",
            RpcError::SubagentParentUnavailable { .. } => "subagent-parent-unavailable",
            RpcError::SubagentNotFound { .. } => "subagent-not-found",
            RpcError::SubagentCatalogDiagnostic { .. } => "subagent-catalog-diagnostic",
            RpcError::SubagentNotResumable { .. } => "subagent-not-resumable",
            RpcError::SubagentUnauthorized { .. } => "subagent-unauthorized",
            RpcError::SubagentDeliveryUnavailable { .. } => "subagent-delivery-unavailable",
            RpcError::Internal { .. } => "internal",
        }
    }

    /// One fixture per code branch (code, details object).
    const ERROR_TABLE: &[(&str, &str)] = &[
        (
            "bad-request",
            r#"{"issues":[{"code":"invalid_type","path":["a"],"expected":"string","received":"number"}]}"#,
        ),
        ("cancelled", "{}"),
        ("session-not-found", r#"{"sessionId":"s1"}"#),
        (
            "model-unavailable",
            r#"{"provider":"deepseek","model":"deepseek-chat"}"#,
        ),
        (
            "session-conflict",
            r#"{"sessionId":"s1","requestedCwd":"/a","existingCwd":"/b"}"#,
        ),
        ("invalid-time-zone", r#"{"value":"UTC"}"#),
        (
            "workspace-attach-failed",
            r#"{"sessionId":"s1","workspaceId":"w1"}"#,
        ),
        ("workspace-not-found", r#"{"workspaceId":"w1"}"#),
        ("workspace-invalid-path", r#"{"path":"/x"}"#),
        ("workspace-name-conflict", r#"{"name":"proj"}"#),
        (
            "workspace-move-invalid",
            r#"{"workspaceId":"w1","sessionId":"s1","beforeSessionId":"s0"}"#,
        ),
        ("directory-unreadable", r#"{"path":"/x"}"#),
        ("directory-exists", r#"{"path":"/x"}"#),
        ("directory-create-failed", r#"{"path":"/x"}"#),
        ("directory-picker-unavailable", r#"{"capability":"dialog"}"#),
        (
            "agent-preset-read-only",
            r#"{"agentPreset":"code","reason":"read-only"}"#,
        ),
        (
            "agent-preset-locked",
            r#"{"sessionId":"s1","agentPreset":"code"}"#,
        ),
        (
            "agent-preset-conflict",
            r#"{"sessionId":"s1","requestedPreset":"a","existingPreset":"b"}"#,
        ),
        (
            "agent-preset-not-found",
            r#"{"agentPreset":"code","available":["code","plan"]}"#,
        ),
        (
            "agent-preset-invalid",
            r#"{"agentPreset":"code","reason":"bad"}"#,
        ),
        ("agent-busy", r#"{"reason":"running"}"#),
        ("attachment-error", r#"{"reason":"too large"}"#),
        ("queue-item-not-found", r#"{"itemId":"m1"}"#),
        ("steer-unavailable", r#"{"itemId":"m1"}"#),
        ("command-error", "{}"),
        ("unknown-command", "{}"),
        ("settings-rejected", r#"{"ns":"general"}"#),
        ("settings-not-exposed", r#"{"ns":"general"}"#),
        (
            "settings-conflict",
            r#"{"ns":"general","expected":1,"actual":2}"#,
        ),
        ("credential-rejected", r#"{"ref":"deepseek"}"#),
        (
            "model-discovery-failed",
            r#"{"settingsNs":"general","baseURL":"http://localhost:11434"}"#,
        ),
        ("title-invalid", r#"{"sessionId":"s1"}"#),
        ("fork-unavailable", r#"{"sessionId":"s1"}"#),
        ("subagent-parent-unavailable", r#"{"parentSessionId":"s1"}"#),
        (
            "subagent-not-found",
            r#"{"parentSessionId":"s1","childSessionId":"s2"}"#,
        ),
        (
            "subagent-catalog-diagnostic",
            r#"{"parentSessionId":"s1","childSessionId":"s2","reason":"unsupported"}"#,
        ),
        ("subagent-not-resumable", r#"{"childSessionId":"s2"}"#),
        ("subagent-unauthorized", r#"{"childSessionId":"s2"}"#),
        (
            "subagent-delivery-unavailable",
            r#"{"childSessionId":"s2"}"#,
        ),
        ("internal", "{}"),
    ];

    #[test]
    fn all_rpc_error_codes_parse() {
        assert_eq!(ERROR_TABLE.len(), 40, "schema has 40 code branches");
        for (code, details) in ERROR_TABLE {
            let fixture = error_fixture(code, details);
            let parsed: RpcError =
                serde_json::from_str(&fixture).unwrap_or_else(|e| panic!("code {code}: {e}"));
            assert_eq!(code_of(&parsed), *code, "code mismatch for {code}");
            // Every fixture also round-trips stably.
            let re = serde_json::to_string(&parsed).unwrap();
            let reparsed: RpcError = serde_json::from_str(&re).unwrap();
            assert_eq!(parsed, reparsed, "round-trip failed for {code}");
        }
    }

    #[test]
    fn bad_request_details_issues_round_trip() {
        let fixture = error_fixture(
            "bad-request",
            r#"{"issues":[{"code":"invalid_type","path":["content",0],"expected":"string","received":"number"},{"code":"unrecognized_keys","keys":["x"]}]}"#,
        );
        let parsed: RpcError = round_trip(&fixture);
        match &parsed {
            RpcError::BadRequest { message, details } => {
                assert_eq!(message, "m");
                assert_eq!(details.issues.len(), 2);
                assert_eq!(details.issues[0]["code"], "invalid_type");
                assert_eq!(details.issues[1]["keys"][0], "x");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn session_conflict_details_round_trip_with_and_without_existing_cwd() {
        let with_cwd = error_fixture(
            "session-conflict",
            r#"{"sessionId":"s1","requestedCwd":"/new","existingCwd":"/old"}"#,
        );
        let parsed: RpcError = round_trip(&with_cwd);
        assert_eq!(
            parsed,
            RpcError::SessionConflict {
                message: "m".into(),
                details: SessionConflictDetails {
                    session_id: "s1".into(),
                    requested_cwd: "/new".into(),
                    existing_cwd: Some("/old".into()),
                },
            }
        );

        // `existingCwd` is optional: absent on the wire → None.
        let without_cwd = error_fixture(
            "session-conflict",
            r#"{"sessionId":"s1","requestedCwd":"/new"}"#,
        );
        let parsed: RpcError = round_trip(&without_cwd);
        match parsed {
            RpcError::SessionConflict { details, .. } => assert_eq!(details.existing_cwd, None),
            other => panic!("expected SessionConflict, got {other:?}"),
        }
    }

    #[test]
    fn model_discovery_failed_details_round_trip() {
        let with_base = error_fixture(
            "model-discovery-failed",
            r#"{"settingsNs":"general","baseURL":"http://localhost:11434"}"#,
        );
        let parsed: RpcError = round_trip(&with_base);
        assert_eq!(
            parsed,
            RpcError::ModelDiscoveryFailed {
                message: "m".into(),
                details: ModelDiscoveryFailedDetails {
                    settings_ns: "general".into(),
                    base_url: Some("http://localhost:11434".into()),
                },
            }
        );

        let without_base = error_fixture("model-discovery-failed", r#"{"settingsNs":"general"}"#);
        let parsed: RpcError = round_trip(&without_base);
        match parsed {
            RpcError::ModelDiscoveryFailed { details, .. } => assert_eq!(details.base_url, None),
            other => panic!("expected ModelDiscoveryFailed, got {other:?}"),
        }
    }

    #[test]
    fn subagent_catalog_diagnostic_reason_round_trip() {
        for (reason, variant) in [
            ("corrupt", SubagentCatalogDiagnosticReason::Corrupt),
            ("unsupported", SubagentCatalogDiagnosticReason::Unsupported),
            ("unavailable", SubagentCatalogDiagnosticReason::Unavailable),
        ] {
            let fixture = error_fixture(
                "subagent-catalog-diagnostic",
                &format!(r#"{{"parentSessionId":"s1","childSessionId":"s2","reason":"{reason}"}}"#),
            );
            let parsed: RpcError = round_trip(&fixture);
            assert_eq!(
                parsed,
                RpcError::SubagentCatalogDiagnostic {
                    message: "m".into(),
                    details: SubagentCatalogDiagnosticDetails {
                        parent_session_id: "s1".into(),
                        child_session_id: "s2".into(),
                        reason: variant,
                    },
                }
            );
        }
    }

    #[test]
    fn empty_details_branches_share_shape() {
        for (code, expected) in [
            (
                "cancelled",
                RpcError::Cancelled {
                    message: "m".into(),
                    details: EmptyDetails {},
                },
            ),
            (
                "command-error",
                RpcError::CommandError {
                    message: "m".into(),
                    details: EmptyDetails {},
                },
            ),
            (
                "unknown-command",
                RpcError::UnknownCommand {
                    message: "m".into(),
                    details: EmptyDetails {},
                },
            ),
            (
                "internal",
                RpcError::Internal {
                    message: "m".into(),
                    details: EmptyDetails {},
                },
            ),
        ] {
            let fixture = error_fixture(code, "{}");
            let parsed: RpcError = round_trip(&fixture);
            assert_eq!(parsed, expected, "code {code}");
        }
    }
}

// ---------------------------------------------------------------------------
// MuxFrame: one fixture per variant (events.schema.ts:43-67)
// ---------------------------------------------------------------------------

mod mux_frames {
    use super::*;

    fn event(seq: i64, r#type: &str, data: &str) -> SessionEvent {
        SessionEvent {
            r#type: r#type.into(),
            seq,
            time: 100.0,
            data: json(data),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn session_event_full_envelope() {
        let fixture = r#"{"type":"session/event","sessionId":"s1","event":{"type":"message.chunk","seq":7,"time":1234.5,"data":{"text":"hi"},"sourceEventSeqs":[1,2,3],"surfaceOp":{"type":"composer-update"},"ignorable":true},"view":{"for":"call","view":{"card":"read_file","width":80}}}"#;
        let frame: MuxFrame = round_trip(fixture);
        match &frame {
            MuxFrame::SessionEvent {
                session_id,
                event,
                view,
            } => {
                assert_eq!(session_id, &SessionId("s1".into()));
                assert_eq!(event.r#type, "message.chunk");
                assert_eq!(event.seq, 7);
                assert_eq!(event.time, 1234.5);
                assert_eq!(event.data, json(r#"{"text":"hi"}"#));
                assert_eq!(event.source_event_seqs, Some(vec![1.0, 2.0, 3.0]));
                assert_eq!(
                    event.surface_op,
                    Some(json(r#"{"type":"composer-update"}"#))
                );
                assert_eq!(event.ignorable, Some(true));
                assert_eq!(
                    view,
                    &Some(ToolEventView::Call {
                        view: ToolEventViewCard {
                            card: "read_file".into()
                        },
                    })
                );
            }
            other => panic!("expected session/event, got {other:?}"),
        }
        // Full equality against a hand-built expected value.
        let expected = MuxFrame::SessionEvent {
            session_id: SessionId("s1".into()),
            event: SessionEvent {
                r#type: "message.chunk".into(),
                seq: 7,
                time: 1234.5,
                data: json(r#"{"text":"hi"}"#),
                source_event_seqs: Some(vec![1.0, 2.0, 3.0]),
                surface_op: Some(json(r#"{"type":"composer-update"}"#)),
                ignorable: Some(true),
            },
            view: Some(ToolEventView::Call {
                view: ToolEventViewCard {
                    card: "read_file".into(),
                },
            }),
        };
        assert_eq!(frame, expected);
    }

    #[test]
    fn session_event_minimal() {
        let fixture = r#"{"type":"session/event","sessionId":"s1","event":{"type":"session.state","seq":3,"time":100.0,"data":{"running":true}}}"#;
        let frame: MuxFrame = round_trip(fixture);
        assert_eq!(
            frame,
            MuxFrame::SessionEvent {
                session_id: SessionId("s1".into()),
                event: event(3, "session.state", r#"{"running":true}"#),
                view: None,
            }
        );
    }

    #[test]
    fn session_subscribed() {
        let fixture = r#"{"type":"session/subscribed","sessionId":"s1","lastSeq":5}"#;
        let frame: MuxFrame = round_trip(fixture);
        assert_eq!(
            frame,
            MuxFrame::SessionSubscribed {
                session_id: SessionId("s1".into()),
                last_seq: 5,
            }
        );
    }

    #[test]
    fn approval_requested_full_and_minimal() {
        let full = r#"{"type":"approval/requested","sessionId":"s1","approvalId":"a1","toolName":"read_file","callId":"call-1","reason":"reads /etc/passwd"}"#;
        let frame: MuxFrame = round_trip(full);
        assert_eq!(
            frame,
            MuxFrame::ApprovalRequested {
                session_id: SessionId("s1".into()),
                approval_id: ApprovalRequestId("a1".into()),
                tool_name: "read_file".into(),
                call_id: Some("call-1".into()),
                reason: Some("reads /etc/passwd".into()),
            }
        );

        let minimal = r#"{"type":"approval/requested","sessionId":"s1","approvalId":"a1","toolName":"write_file"}"#;
        let frame: MuxFrame = round_trip(minimal);
        match frame {
            MuxFrame::ApprovalRequested {
                call_id, reason, ..
            } => {
                assert_eq!(call_id, None);
                assert_eq!(reason, None);
            }
            other => panic!("expected approval/requested, got {other:?}"),
        }
    }

    #[test]
    fn approval_resolved_every_outcome() {
        let outcomes: &[(&str, ApprovalOutcome)] = &[
            ("allowed-once", ApprovalOutcome::AllowedOnce),
            ("rejected", ApprovalOutcome::Rejected),
            ("cancelled", ApprovalOutcome::Cancelled),
            ("unavailable", ApprovalOutcome::Unavailable),
        ];
        for (code, variant) in outcomes {
            let fixture = format!(
                r#"{{"type":"approval/resolved","sessionId":"s1","approvalId":"a1","outcome":"{code}"}}"#
            );
            let frame: MuxFrame = round_trip(&fixture);
            assert_eq!(
                frame,
                MuxFrame::ApprovalResolved {
                    session_id: SessionId("s1".into()),
                    approval_id: ApprovalRequestId("a1".into()),
                    outcome: *variant,
                },
                "outcome {code}"
            );
        }
    }

    #[test]
    fn question_requested_with_intent() {
        let fixture = r#"{"type":"question/requested","sessionId":"s1","questions":[{"id":"q1","question":"Approve this plan?","header":"Plan review","detail":"2 steps","options":[{"label":"Yes","description":"run it"},{"label":"No"}],"multiSelect":true,"intent":{"kind":"plan-review","approve":"plan/1"}}]}"#;
        let frame: MuxFrame = round_trip(fixture);
        assert_eq!(
            frame,
            MuxFrame::QuestionRequested {
                session_id: SessionId("s1".into()),
                questions: vec![AskUserQuestionItem {
                    id: "q1".into(),
                    question: "Approve this plan?".into(),
                    header: Some("Plan review".into()),
                    detail: Some("2 steps".into()),
                    options: Some(vec![
                        QuestionOption {
                            label: "Yes".into(),
                            description: Some("run it".into()),
                        },
                        QuestionOption {
                            label: "No".into(),
                            description: None
                        },
                    ]),
                    multi_select: Some(true),
                    intent: Some(QuestionIntent::PlanReview {
                        approve: "plan/1".into()
                    }),
                }],
            }
        );
    }

    #[test]
    fn question_requested_plain() {
        let fixture = r#"{"type":"question/requested","sessionId":"s1","questions":[{"id":"q1","question":"OK?"}]}"#;
        let frame: MuxFrame = round_trip(fixture);
        match frame {
            MuxFrame::QuestionRequested { questions, .. } => {
                assert_eq!(questions.len(), 1);
                assert_eq!(questions[0].id, "q1");
                assert_eq!(questions[0].header, None);
                assert_eq!(questions[0].options, None);
                assert_eq!(questions[0].multi_select, None);
                assert_eq!(questions[0].intent, None);
            }
            other => panic!("expected question/requested, got {other:?}"),
        }
    }

    #[test]
    fn question_resolved_every_outcome() {
        let fixture = r#"{"type":"question/resolved","sessionId":"s1","questionRpcId":"rpc-9","outcome":"answered"}"#;
        let frame: MuxFrame = round_trip(fixture);
        assert_eq!(
            frame,
            MuxFrame::QuestionResolved {
                session_id: SessionId("s1".into()),
                question_rpc_id: RpcId("rpc-9".into()),
                outcome: QuestionOutcome::Answered,
            }
        );

        let fixture = r#"{"type":"question/resolved","sessionId":"s1","questionRpcId":"rpc-9","outcome":"cancelled"}"#;
        let frame: MuxFrame = round_trip(fixture);
        assert_eq!(
            frame,
            MuxFrame::QuestionResolved {
                session_id: SessionId("s1".into()),
                question_rpc_id: RpcId("rpc-9".into()),
                outcome: QuestionOutcome::Cancelled,
            }
        );
    }

    #[test]
    fn session_queue_with_content_blocks_and_source() {
        let fixture = r#"{"type":"session/queue","sessionId":"s1","items":[
            {"id":"m1","placement":"queued","message":{"id":"m1","role":"user","content":[{"type":"text","text":"hello"},{"type":"tool-call","name":"read_file","args":{}}],"source":{"kind":"composer","origin":"user"}}},
            {"id":"m2","placement":"steering","message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"hi"}],"source":{"kind":"queue"}}},
            {"id":"m3","placement":"context","message":{"id":"m3","role":"system","content":[],"source":{"kind":"ctx"}}}
        ]}"#;
        let frame: MuxFrame = round_trip(fixture);
        match &frame {
            MuxFrame::SessionQueue { session_id, items } => {
                assert_eq!(session_id, &SessionId("s1".into()));
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].placement, QueuePlacement::Queued);
                assert_eq!(items[1].placement, QueuePlacement::Steering);
                assert_eq!(items[2].placement, QueuePlacement::Context);
                assert_eq!(items[0].message.role, MessageRole::User);
                assert_eq!(items[0].message.source.kind, "composer");
                assert_eq!(items[0].message.content.len(), 2);
                assert_eq!(items[0].message.content[0].r#type, "text");
                // Extra block keys ride the ContentBlock passthrough
                // verbatim (`text`, `name`, `args`); `type` is the tag.
                assert_eq!(items[0].message.content[0].text(), Some("hello"));
                assert_eq!(items[0].message.content[1].r#type, "tool-call");
            }
            other => panic!("expected session/queue, got {other:?}"),
        }
        // Full equality for the first item's message.
        let expected = MuxFrame::SessionQueue {
            session_id: SessionId("s1".into()),
            items: vec![
                QueueItem {
                    id: MessageId("m1".into()),
                    placement: QueuePlacement::Queued,
                    message: QueueMessage {
                        id: MessageId("m1".into()),
                        role: MessageRole::User,
                        content: vec![
                            ContentBlock {
                                r#type: "text".into(),
                                extra: serde_json::Map::from_iter([(
                                    "text".to_string(),
                                    serde_json::json!("hello"),
                                )]),
                            },
                            ContentBlock {
                                r#type: "tool-call".into(),
                                extra: serde_json::Map::from_iter([
                                    ("name".to_string(), serde_json::json!("read_file")),
                                    ("args".to_string(), serde_json::json!({})),
                                ]),
                            },
                        ],
                        source: QueueMessageSource {
                            kind: "composer".into(),
                        },
                    },
                },
                QueueItem {
                    id: MessageId("m2".into()),
                    placement: QueuePlacement::Steering,
                    message: QueueMessage {
                        id: MessageId("m2".into()),
                        role: MessageRole::Assistant,
                        content: vec![ContentBlock {
                            r#type: "text".into(),
                            extra: serde_json::Map::from_iter([(
                                "text".to_string(),
                                serde_json::json!("hi"),
                            )]),
                        }],
                        source: QueueMessageSource {
                            kind: "queue".into(),
                        },
                    },
                },
                QueueItem {
                    id: MessageId("m3".into()),
                    placement: QueuePlacement::Context,
                    message: QueueMessage {
                        id: MessageId("m3".into()),
                        role: MessageRole::System,
                        content: vec![],
                        source: QueueMessageSource { kind: "ctx".into() },
                    },
                },
            ],
        };
        assert_eq!(frame, expected);
    }

    #[test]
    fn session_jobs_with_task_view() {
        let fixture = r#"{"type":"session/jobs","sessionId":"s1","jobs":[{"id":"t1","kind":"file-write","label":"Write notes.md","status":"running","detail":"writing 3/10","startedAt":100,"finishedAt":null}]}"#;
        let frame: MuxFrame = round_trip(fixture);
        assert_eq!(
            frame,
            MuxFrame::SessionJobs {
                session_id: SessionId("s1".into()),
                jobs: vec![TaskView {
                    id: TaskId("t1".into()),
                    kind: "file-write".into(),
                    label: "Write notes.md".into(),
                    status: TaskStatus::Running,
                    detail: Some("writing 3/10".into()),
                    started_at: 100,
                    // `finishedAt: null` deserializes to None (tolerant).
                    finished_at: None,
                }],
            }
        );

        // Every status variant parses.
        for (code, variant) in [
            ("running", TaskStatus::Running),
            ("stopping", TaskStatus::Stopping),
            ("completed", TaskStatus::Completed),
            ("killed", TaskStatus::Killed),
            ("failed", TaskStatus::Failed),
        ] {
            let fixture = format!(
                r#"{{"type":"session/jobs","sessionId":"s1","jobs":[{{"id":"t1","kind":"k","label":"l","status":"{code}","startedAt":1}}]}}"#
            );
            let frame: MuxFrame = round_trip(&fixture);
            match frame {
                MuxFrame::SessionJobs { jobs, .. } => assert_eq!(jobs[0].status, variant),
                other => panic!("expected session/jobs, got {other:?}"),
            }
        }
    }

    #[test]
    fn session_projection_wide_value() {
        let fixture = r#"{"type":"session/projection","sessionId":"s1","key":"session.list","value":{"items":[{"sessionId":"s1","updatedAt":1,"running":true,"blank":false}]},"seq":9}"#;
        let frame: MuxFrame = round_trip(fixture);
        assert_eq!(
            frame,
            MuxFrame::SessionProjection {
                session_id: SessionId("s1".into()),
                key: "session.list".into(),
                value: json(
                    r#"{"items":[{"sessionId":"s1","updatedAt":1,"running":true,"blank":false}]}"#
                ),
                seq: 9,
            }
        );
    }

    #[test]
    fn stream_error_frame() {
        let fixture = r#"{"type":"stream/error","error":{"code":"internal","message":"stream broke","details":{}}}"#;
        let frame: MuxFrame = round_trip(fixture);
        assert_eq!(
            frame,
            MuxFrame::StreamError {
                error: RpcError::Internal {
                    message: "stream broke".into(),
                    details: EmptyDetails {},
                },
            }
        );
    }
}

// ---------------------------------------------------------------------------
// HostFrame: one fixture per variant (events.schema.ts:70-93)
// ---------------------------------------------------------------------------

mod host_frames {
    use super::*;

    fn workspace_view() -> WorkspaceView {
        WorkspaceView {
            workspace_id: WorkspaceId("w1".into()),
            path: "/home/u/proj".into(),
            title: "Proj".into(),
            session_ids: vec![SessionId("s1".into()), SessionId("s2".into())],
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-02T00:00:00Z".into(),
        }
    }

    const WORKSPACE_JSON: &str = r#"{"workspaceId":"w1","path":"/home/u/proj","title":"Proj","sessionIds":["s1","s2"],"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-02T00:00:00Z"}"#;

    #[test]
    fn session_added_full_and_minimal() {
        let full = r#"{"type":"host/session-added","sessionId":"s1","blank":false,"parentSessionId":"s0","origin":"subagent","cwd":"/work","agentPreset":"code"}"#;
        let frame: HostFrame = round_trip(full);
        assert_eq!(
            frame,
            HostFrame::HostSessionAdded {
                session_id: SessionId("s1".into()),
                blank: false,
                parent_session_id: Some(SessionId("s0".into())),
                origin: Some(Origin::Subagent),
                cwd: Some("/work".into()),
                agent_preset: Some("code".into()),
            }
        );

        let minimal = r#"{"type":"host/session-added","sessionId":"s2","blank":true}"#;
        let frame: HostFrame = round_trip(minimal);
        assert_eq!(
            frame,
            HostFrame::HostSessionAdded {
                session_id: SessionId("s2".into()),
                blank: true,
                parent_session_id: None,
                origin: None,
                cwd: None,
                agent_preset: None,
            }
        );
    }

    #[test]
    fn session_removed() {
        let fixture = r#"{"type":"host/session-removed","sessionId":"s1"}"#;
        let frame: HostFrame = round_trip(fixture);
        assert_eq!(
            frame,
            HostFrame::HostSessionRemoved {
                session_id: SessionId("s1".into())
            }
        );
    }

    #[test]
    fn session_status() {
        let fixture = r#"{"type":"host/session-status","sessionId":"s1","running":true}"#;
        let frame: HostFrame = round_trip(fixture);
        assert_eq!(
            frame,
            HostFrame::HostSessionStatus {
                session_id: SessionId("s1".into()),
                running: true,
            }
        );
    }

    #[test]
    fn agent_error() {
        let fixture = r#"{"type":"host/agent-error","sessionId":"s1","message":"agent crashed"}"#;
        let frame: HostFrame = round_trip(fixture);
        assert_eq!(
            frame,
            HostFrame::HostAgentError {
                session_id: SessionId("s1".into()),
                message: "agent crashed".into(),
            }
        );
    }

    #[test]
    fn workspace_changed() {
        let fixture =
            format!(r#"{{"type":"host/workspace-changed","workspace":{WORKSPACE_JSON}}}"#);
        let frame: HostFrame = round_trip(&fixture);
        assert_eq!(
            frame,
            HostFrame::HostWorkspaceChanged {
                workspace: workspace_view()
            }
        );
    }

    #[test]
    fn workspace_removed() {
        let fixture = r#"{"type":"host/workspace-removed","workspaceId":"w1"}"#;
        let frame: HostFrame = round_trip(fixture);
        assert_eq!(
            frame,
            HostFrame::HostWorkspaceRemoved {
                workspace_id: WorkspaceId("w1".into())
            }
        );
    }

    #[test]
    fn workspace_order_changed() {
        let fixture = r#"{"type":"host/workspace-order-changed","workspaceIds":["w1","w2","w3"]}"#;
        let frame: HostFrame = round_trip(fixture);
        assert_eq!(
            frame,
            HostFrame::HostWorkspaceOrderChanged {
                workspace_ids: vec![
                    WorkspaceId("w1".into()),
                    WorkspaceId("w2".into()),
                    WorkspaceId("w3".into()),
                ],
            }
        );
    }

    #[test]
    fn archived_sessions_changed() {
        let fixture =
            r#"{"type":"host/archived-sessions-changed","archivedSessionIds":["s1","s2"]}"#;
        let frame: HostFrame = round_trip(fixture);
        assert_eq!(
            frame,
            HostFrame::HostArchivedSessionsChanged {
                archived_session_ids: vec![SessionId("s1".into()), SessionId("s2".into())],
            }
        );
    }

    #[test]
    fn remote_event_with_args() {
        let fixture = r#"{"type":"host/remote-event","event":"settings.changed","args":[{"ns":"general"},42,"str",null,true,[1,2]]}"#;
        let frame: HostFrame = round_trip(fixture);
        assert_eq!(
            frame,
            HostFrame::HostRemoteEvent {
                event: "settings.changed".into(),
                args: vec![
                    json(r#"{"ns":"general"}"#),
                    json("42"),
                    json(r#""str""#),
                    json("null"),
                    json("true"),
                    json("[1,2]"),
                ],
            }
        );
    }

    #[test]
    fn host_stream_error() {
        let fixture = r#"{"type":"stream/error","error":{"code":"workspace-not-found","message":"no w1","details":{"workspaceId":"w1"}}}"#;
        let frame: HostFrame = round_trip(fixture);
        assert_eq!(
            frame,
            HostFrame::StreamError {
                error: RpcError::WorkspaceNotFound {
                    message: "no w1".into(),
                    details: WorkspaceNotFoundDetails {
                        workspace_id: "w1".into()
                    },
                },
            }
        );
    }
}

// ---------------------------------------------------------------------------
// ServerRequest payload → frame second-level parse
// ---------------------------------------------------------------------------

mod server_request_frames {
    use super::*;

    #[test]
    fn server_request_payload_into_mux_frame() {
        let fixture = r#"{"type":"server-request","rpcId":"r1","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s1","lastSeq":5}}"#;
        let req: ServerRequest = round_trip(fixture);
        assert_eq!(req.method, "events.mux");
        let frame = req
            .into_mux_frame()
            .expect("payload must parse as MuxFrame");
        assert_eq!(
            frame,
            MuxFrame::SessionSubscribed {
                session_id: SessionId("s1".into()),
                last_seq: 5,
            }
        );
    }

    #[test]
    fn server_request_payload_into_host_frame() {
        let fixture = r#"{"type":"server-request","rpcId":"r2","method":"events.host","payload":{"type":"host/workspace-removed","workspaceId":"w1"}}"#;
        let req: ServerRequest = round_trip(fixture);
        let frame = req
            .into_host_frame()
            .expect("payload must parse as HostFrame");
        assert_eq!(
            frame,
            HostFrame::HostWorkspaceRemoved {
                workspace_id: WorkspaceId("w1".into())
            }
        );
    }

    #[test]
    fn mux_frame_from_value() {
        let value = json(
            r#"{"type":"session/event","sessionId":"s1","event":{"type":"x","seq":1,"time":1.0,"data":{}}}"#,
        );
        let frame = MuxFrame::from_value(value).expect("must parse");
        match frame {
            MuxFrame::SessionEvent { session_id, .. } => {
                assert_eq!(session_id, SessionId("s1".into()))
            }
            other => panic!("expected session/event, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Session domain (sessions.schema.ts)
// ---------------------------------------------------------------------------

mod session_domain {
    use super::*;

    #[test]
    fn list_request_and_value() {
        let req: SessionListRequest = round_trip("{}");
        assert_eq!(req, SessionListRequest { cursor: None });

        let fixture = r#"{"items":[
            {"sessionId":"s1","updatedAt":1234.5,"running":true,"blank":false,"cwd":"/work","agentPreset":"code","projections":{"asOfSeq":12,"values":{"session.list":{"blank":false,"lastPromptAt":null}}}},
            {"sessionId":"s2","updatedAt":100.0,"running":false,"blank":true,"parentSessionId":"s0","origin":"subagent"}
        ]}"#;
        let value: SessionListValue = round_trip(fixture);
        let expected = SessionListValue {
            items: vec![
                SessionSummary {
                    session_id: SessionId("s1".into()),
                    updated_at: 1234.5,
                    running: true,
                    blank: false,
                    parent_session_id: None,
                    origin: None,
                    cwd: Some("/work".into()),
                    agent_preset: Some("code".into()),
                    projections: Some(SessionProjectionsBlock {
                        as_of_seq: 12,
                        values: serde_json::Map::from_iter([(
                            "session.list".into(),
                            json(r#"{"blank":false,"lastPromptAt":null}"#),
                        )]),
                    }),
                },
                SessionSummary {
                    session_id: SessionId("s2".into()),
                    updated_at: 100.0,
                    running: false,
                    blank: true,
                    parent_session_id: Some(SessionId("s0".into())),
                    origin: Some(Origin::Subagent),
                    cwd: None,
                    agent_preset: None,
                    projections: None,
                },
            ],
        };
        assert_eq!(value, expected);
        // Spot-check field access on the parsed value too.
        assert_eq!(value.items[0].projections.as_ref().unwrap().as_of_seq, 12);
        assert_eq!(
            value.items[1].parent_session_id,
            Some(SessionId("s0".into()))
        );
    }

    #[test]
    fn search_request_and_value() {
        let req: SessionSearchRequest = round_trip(r#"{"query":"hello"}"#);
        assert_eq!(
            req,
            SessionSearchRequest {
                query: "hello".into()
            }
        );

        let fixture = r#"{"items":[{"sessionId":"s1","snippet":"hello world"}],"hasMore":true}"#;
        let value: SessionSearchValue = round_trip(fixture);
        assert_eq!(
            value,
            SessionSearchValue {
                items: vec![SessionSearchItem {
                    session_id: SessionId("s1".into()),
                    snippet: "hello world".into(),
                }],
                has_more: true,
            }
        );
    }

    #[test]
    fn create_request_both_variants() {
        let via_workspace: SessionCreateRequest = round_trip(r#"{"workspaceId":"w1"}"#);
        assert_eq!(
            via_workspace,
            SessionCreateRequest {
                workspace_id: Some(WorkspaceId("w1".into())),
                cwd: None,
                session_id: None,
                agent_preset: None,
            }
        );

        let via_cwd: SessionCreateRequest =
            round_trip(r#"{"cwd":"/work","sessionId":"s1","agentPreset":"code"}"#);
        assert_eq!(
            via_cwd,
            SessionCreateRequest {
                workspace_id: None,
                cwd: Some("/work".into()),
                session_id: Some(SessionId("s1".into())),
                agent_preset: Some("code".into()),
            }
        );

        let value: SessionCreateValue = round_trip(r#"{"sessionId":"s1","agentPreset":"code"}"#);
        assert_eq!(
            value,
            SessionCreateValue {
                session_id: SessionId("s1".into()),
                agent_preset: Some("code".into()),
            }
        );
    }

    #[test]
    fn history_request_and_value() {
        let req: SessionHistoryRequest =
            round_trip(r#"{"sessionId":"s1","beforeSeq":50,"maxMessages":20}"#);
        assert_eq!(
            req,
            SessionHistoryRequest {
                session_id: SessionId("s1".into()),
                before_seq: Some(50),
                max_messages: Some(20),
            }
        );

        let fixture = r#"{"events":[
            {"event":{"type":"message.chunk","seq":5,"time":1.0,"data":{"text":"a"}},"view":{"for":"call","view":{"card":"read_file"}}},
            {"event":{"type":"message.result","seq":6,"time":2.0,"data":{"text":"b"}},"view":{"for":"result","view":{"card":"write_file"}}}
        ],"hasMore":false,"projections":{"asOfSeq":-1,"values":{}}}"#;
        let value: SessionHistoryValue = round_trip(fixture);
        let expected = SessionHistoryValue {
            events: vec![
                HistoryEntry {
                    event: SessionEvent {
                        r#type: "message.chunk".into(),
                        seq: 5,
                        time: 1.0,
                        data: json(r#"{"text":"a"}"#),
                        source_event_seqs: None,
                        surface_op: None,
                        ignorable: None,
                    },
                    view: Some(ToolEventView::Call {
                        view: ToolEventViewCard {
                            card: "read_file".into(),
                        },
                    }),
                },
                HistoryEntry {
                    event: SessionEvent {
                        r#type: "message.result".into(),
                        seq: 6,
                        time: 2.0,
                        data: json(r#"{"text":"b"}"#),
                        source_event_seqs: None,
                        surface_op: None,
                        ignorable: None,
                    },
                    view: Some(ToolEventView::Result {
                        view: ToolEventViewCard {
                            card: "write_file".into(),
                        },
                    }),
                },
            ],
            has_more: false,
            projections: Some(SessionProjectionsBlock {
                as_of_seq: -1,
                values: serde_json::Map::new(),
            }),
        };
        assert_eq!(value, expected);
    }

    #[test]
    fn prompt_request_with_text_and_image_parts() {
        let fixture = r#"{"sessionId":"s1","mode":"queue","content":[
            {"type":"text","text":"hello"},
            {"type":"image","mediaType":"image/png","data":"iVBORw0KGgo=","name":"shot.png"}
        ],"clientTimeZone":"America/Sao_Paulo"}"#;
        let req: SessionPromptRequest = round_trip(fixture);
        assert_eq!(
            req,
            SessionPromptRequest {
                session_id: SessionId("s1".into()),
                mode: PromptMode::Queue,
                content: vec![
                    PromptContentPart::Text {
                        text: "hello".into()
                    },
                    PromptContentPart::Image {
                        media_type: ImageMediaType::ImagePng,
                        data: "iVBORw0KGgo=".into(),
                        name: Some("shot.png".into()),
                    },
                ],
                client_time_zone: Some("America/Sao_Paulo".into()),
            }
        );

        // steer mode + no clientTimeZone.
        let req: SessionPromptRequest = round_trip(
            r#"{"sessionId":"s1","mode":"steer","content":[{"type":"text","text":"go"}]}"#,
        );
        assert_eq!(req.mode, PromptMode::Steer);
        assert_eq!(req.client_time_zone, None);
    }

    #[test]
    fn prompt_value_with_and_without_command() {
        let with_command: SessionPromptValue =
            round_trip(r#"{"accepted":true,"command":{"kind":"success","text":"/plan hello"}}"#);
        assert_eq!(
            with_command,
            SessionPromptValue {
                accepted: true,
                command: Some(PromptCommand {
                    kind: PromptCommandKind::Success,
                    text: Some("/plan hello".into()),
                }),
            }
        );

        let without_command: SessionPromptValue = round_trip(r#"{"accepted":true}"#);
        assert_eq!(
            without_command,
            SessionPromptValue {
                accepted: true,
                command: None
            }
        );
    }

    #[test]
    fn attachment_request_and_value() {
        let req: SessionAttachmentRequest =
            round_trip(r#"{"sessionId":"s1","attachmentId":"att1"}"#);
        assert_eq!(
            req,
            SessionAttachmentRequest {
                session_id: SessionId("s1".into()),
                attachment_id: AttachmentId("att1".into()),
            }
        );

        let fixture = r#"{"attachment":{"attachmentId":"att1","mediaType":"image/webp","bytes":1024,"width":64,"height":48,"name":"a.webp"},"data":"base64data"}"#;
        let value: SessionAttachmentValue = round_trip(fixture);
        assert_eq!(
            value,
            SessionAttachmentValue {
                attachment: ImageAttachmentRef {
                    attachment_id: AttachmentId("att1".into()),
                    media_type: ImageMediaType::ImageWebp,
                    bytes: 1024,
                    width: 64,
                    height: 48,
                    name: Some("a.webp".into()),
                },
                data: "base64data".into(),
            }
        );
    }

    #[test]
    fn update_queue_edit_remove_steer() {
        let edit: SessionUpdateQueueRequest = round_trip(
            r#"{"sessionId":"s1","itemId":"m1","action":{"kind":"edit","content":[{"type":"text","text":"edited"},{"type":"tool-call","id":"x","args":{}}]}}"#,
        );
        assert_eq!(
            edit,
            SessionUpdateQueueRequest {
                session_id: SessionId("s1".into()),
                item_id: MessageId("m1".into()),
                action: UpdateQueueAction::Edit {
                    content: vec![
                        ContentBlock {
                            r#type: "text".into(),
                            extra: serde_json::Map::from_iter([(
                                "text".to_string(),
                                serde_json::json!("edited"),
                            )]),
                        },
                        ContentBlock {
                            r#type: "tool-call".into(),
                            extra: serde_json::Map::from_iter([
                                ("id".to_string(), serde_json::json!("x")),
                                ("args".to_string(), serde_json::json!({})),
                            ]),
                        },
                    ],
                },
            }
        );

        let remove: SessionUpdateQueueRequest =
            round_trip(r#"{"sessionId":"s1","itemId":"m1","action":{"kind":"remove"}}"#);
        assert_eq!(remove.action, UpdateQueueAction::Remove);

        let steer: SessionUpdateQueueRequest =
            round_trip(r#"{"sessionId":"s1","itemId":"m1","action":{"kind":"steer"}}"#);
        assert_eq!(steer.action, UpdateQueueAction::Steer);

        let value: SessionUpdateQueueValue = round_trip(r#"{"accepted":true}"#);
        assert_eq!(value, SessionUpdateQueueValue { accepted: true });
    }

    #[test]
    fn models_request_and_value() {
        let req: SessionModelsRequest = round_trip(r#"{"sessionId":"s1"}"#);
        assert_eq!(
            req,
            SessionModelsRequest {
                session_id: SessionId("s1".into())
            }
        );

        let fixture = r#"{"current":{"provider":"deepseek","model":"deepseek-chat","reasoningEffort":"high"},"routable":true,"groups":[
            {"id":"deepseek","name":"DeepSeek","models":[{"id":"deepseek-chat","name":"DeepSeek Chat","description":"chat","reasoning":{"efforts":[{"id":"low","name":"Low","description":"fast"}],"defaultEffort":"low"}}]}
        ],"failures":[{"id":"ollama","name":"Ollama","message":"connection refused"}]}"#;
        let value: SessionModelsValue = round_trip(fixture);
        let expected = SessionModelsValue {
            current: ModelSelection {
                provider: "deepseek".into(),
                model: "deepseek-chat".into(),
                reasoning_effort: Some("high".into()),
            },
            routable: true,
            groups: vec![ModelProviderGroup {
                id: "deepseek".into(),
                name: "DeepSeek".into(),
                models: vec![ModelCatalogModel {
                    id: "deepseek-chat".into(),
                    name: "DeepSeek Chat".into(),
                    description: Some("chat".into()),
                    reasoning: Some(ModelReasoning {
                        efforts: vec![ModelReasoningEffort {
                            id: "low".into(),
                            name: "Low".into(),
                            description: Some("fast".into()),
                        }],
                        default_effort: Some("low".into()),
                    }),
                }],
            }],
            failures: vec![ModelCatalogFailure {
                id: "ollama".into(),
                name: "Ollama".into(),
                message: "connection refused".into(),
            }],
        };
        assert_eq!(value, expected);
    }

    #[test]
    fn select_model_request_and_value() {
        let req: SessionSelectModelRequest = round_trip(
            r#"{"sessionId":"s1","provider":"deepseek","model":"deepseek-reasoner","reasoningEffort":"high"}"#,
        );
        assert_eq!(
            req,
            SessionSelectModelRequest {
                session_id: SessionId("s1".into()),
                provider: "deepseek".into(),
                model: "deepseek-reasoner".into(),
                reasoning_effort: Some("high".into()),
            }
        );

        let value: SessionSelectModelValue =
            round_trip(r#"{"selected":{"provider":"deepseek","model":"deepseek-reasoner"}}"#);
        assert_eq!(
            value,
            SessionSelectModelValue {
                selected: ModelSelection {
                    provider: "deepseek".into(),
                    model: "deepseek-reasoner".into(),
                    reasoning_effort: None,
                },
            }
        );
    }

    #[test]
    fn rename_request_and_value() {
        let req: SessionRenameRequest = round_trip(r#"{"sessionId":"s1","title":"New title"}"#);
        assert_eq!(
            req,
            SessionRenameRequest {
                session_id: SessionId("s1".into()),
                title: "New title".into(),
            }
        );

        let value: SessionRenameValue = round_trip(r#"{"title":"New title","seq":8}"#);
        assert_eq!(
            value,
            SessionRenameValue {
                title: "New title".into(),
                seq: 8
            }
        );
    }

    #[test]
    fn fork_request_and_value() {
        let req: SessionForkRequest = round_trip(r#"{"sessionId":"s1","atSeq":42}"#);
        assert_eq!(
            req,
            SessionForkRequest {
                session_id: SessionId("s1".into()),
                at_seq: Some(42),
            }
        );

        let req: SessionForkRequest = round_trip(r#"{"sessionId":"s1"}"#);
        assert_eq!(req.at_seq, None);

        let value: SessionForkValue = round_trip(r#"{"sessionId":"s2"}"#);
        assert_eq!(
            value,
            SessionForkValue {
                session_id: SessionId("s2".into())
            }
        );
    }

    #[test]
    fn cancel_request_and_value() {
        let req: SessionCancelRequest = round_trip(r#"{"sessionId":"s1"}"#);
        assert_eq!(
            req,
            SessionCancelRequest {
                session_id: SessionId("s1".into())
            }
        );

        let value: SessionCancelValue = round_trip(r#"{"accepted":true}"#);
        assert_eq!(value, SessionCancelValue { accepted: true });
    }

    #[test]
    fn every_image_media_type_parses() {
        for (code, variant) in [
            ("image/png", ImageMediaType::ImagePng),
            ("image/jpeg", ImageMediaType::ImageJpeg),
            ("image/webp", ImageMediaType::ImageWebp),
            ("image/gif", ImageMediaType::ImageGif),
        ] {
            let fixture = format!(
                r#"{{"sessionId":"s1","mode":"queue","content":[{{"type":"image","mediaType":"{code}","data":"d"}}]}}"#
            );
            let req: SessionPromptRequest = round_trip(&fixture);
            match &req.content[0] {
                PromptContentPart::Image { media_type, .. } => assert_eq!(*media_type, variant),
                other => panic!("expected image part, got {other:?}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Settings domain (settings.schema.ts)
// ---------------------------------------------------------------------------

mod settings {
    use super::*;

    fn namespace_view() -> SettingsNamespaceView {
        SettingsNamespaceView {
            ns: "general".into(),
            schema: json(r#"{"type":"object"}"#),
            value: json(r#"{"theme":"dark"}"#),
            base: Some(json(r#"{"theme":"light"}"#)),
            user: Some(json(r#"{"theme":"dark"}"#)),
            applies: AppliesMode::Live,
            secrets: vec![SettingsSecretView {
                path: vec!["apiKey".into()],
                set: true,
            }],
            revision: 3.0,
        }
    }

    const NAMESPACE_JSON: &str = r#"{"ns":"general","schema":{"type":"object"},"value":{"theme":"dark"},"base":{"theme":"light"},"user":{"theme":"dark"},"applies":"live","secrets":[{"path":["apiKey"],"set":true}],"revision":3}"#;

    #[test]
    fn describe_request_and_value() {
        let req: SettingsDescribeRequest = round_trip("{}");
        assert_eq!(req, SettingsDescribeRequest {});

        let fixture = format!(
            r#"{{"writable":true,"hasDocument":true,"namespaces":[{NAMESPACE_JSON},{{"ns":"models","schema":{{}},"value":{{}},"applies":"restart","secrets":[],"revision":1}}]}}"#
        );
        let value: SettingsDescribeValue = round_trip(&fixture);
        assert!(value.writable);
        assert!(value.has_document);
        assert_eq!(value.namespaces.len(), 2);
        assert_eq!(value.namespaces[0], namespace_view());
        assert_eq!(value.namespaces[1].applies, AppliesMode::Restart);
        assert_eq!(value.namespaces[1].base, None);
        assert_eq!(value.namespaces[1].user, None);
    }

    #[test]
    fn open_document_request_and_value() {
        let req: SettingsOpenDocumentRequest = round_trip("{}");
        assert_eq!(req, SettingsOpenDocumentRequest {});

        let value: SettingsOpenDocumentValue = round_trip(r#"{"opened":true}"#);
        assert_eq!(value, SettingsOpenDocumentValue { opened: true });
    }

    #[test]
    fn update_request_and_value() {
        let req: SettingsUpdateRequest =
            round_trip(r#"{"ns":"general","patch":{"theme":"dark"},"expectedRevision":2}"#);
        assert_eq!(
            req,
            SettingsUpdateRequest {
                ns: "general".into(),
                patch: serde_json::Map::from_iter([("theme".into(), json(r#""dark""#))]),
                expected_revision: Some(2.0),
            }
        );

        // expectedRevision is optional.
        let req: SettingsUpdateRequest = round_trip(r#"{"ns":"general","patch":{}}"#);
        assert_eq!(req.expected_revision, None);

        let value: SettingsNamespaceView = round_trip(NAMESPACE_JSON);
        assert_eq!(value, namespace_view());
    }

    #[test]
    fn replace_request() {
        let req: SettingsReplaceRequest =
            round_trip(r#"{"ns":"general","section":{"theme":"dark"}}"#);
        assert_eq!(
            req,
            SettingsReplaceRequest {
                ns: "general".into(),
                section: serde_json::Map::from_iter([("theme".into(), json(r#""dark""#))]),
                expected_revision: None,
            }
        );
    }

    #[test]
    fn mutate_request_both_ops() {
        let fixture = r#"{"ns":"general","ops":[{"op":"set","path":["theme"],"value":"dark"},{"op":"unset","path":["apiKey"]}],"expectedRevision":2}"#;
        let req: SettingsMutateRequest = round_trip(fixture);
        assert_eq!(
            req,
            SettingsMutateRequest {
                ns: "general".into(),
                ops: vec![
                    SettingsPathOp::Set {
                        path: vec!["theme".into()],
                        value: json(r#""dark""#),
                    },
                    SettingsPathOp::Unset {
                        path: vec!["apiKey".into()]
                    },
                ],
                expected_revision: Some(2.0),
            }
        );
    }
}

// ---------------------------------------------------------------------------
// Workspace domain (workspace.schema.ts)
// ---------------------------------------------------------------------------

mod workspace {
    use super::*;

    fn view() -> WorkspaceView {
        WorkspaceView {
            workspace_id: WorkspaceId("w1".into()),
            path: "/home/u/proj".into(),
            title: "Proj".into(),
            session_ids: vec![SessionId("s1".into()), SessionId("s2".into())],
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-02T00:00:00Z".into(),
        }
    }

    const VIEW_JSON: &str = r#"{"workspaceId":"w1","path":"/home/u/proj","title":"Proj","sessionIds":["s1","s2"],"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-02T00:00:00Z"}"#;

    #[test]
    fn list_request_and_value() {
        let req: WorkspaceListRequest = round_trip("{}");
        assert_eq!(req, WorkspaceListRequest {});

        let fixture = format!(r#"{{"items":[{VIEW_JSON}],"archivedSessionIds":["s9"]}}"#);
        let value: WorkspaceListValue = round_trip(&fixture);
        assert_eq!(
            value,
            WorkspaceListValue {
                items: vec![view()],
                archived_session_ids: vec![SessionId("s9".into())],
            }
        );
    }

    #[test]
    fn create_request_and_value() {
        let req: WorkspaceCreateRequest = round_trip(r#"{"path":"/home/u/proj"}"#);
        assert_eq!(
            req,
            WorkspaceCreateRequest {
                path: "/home/u/proj".into()
            }
        );

        let fixture = format!(r#"{{"workspace":{VIEW_JSON},"created":true}}"#);
        let value: WorkspaceCreateValue = round_trip(&fixture);
        assert_eq!(
            value,
            WorkspaceCreateValue {
                workspace: view(),
                created: true
            }
        );
    }

    #[test]
    fn rename_request_and_value() {
        let req: WorkspaceRenameRequest = round_trip(r#"{"workspaceId":"w1","title":"Renamed"}"#);
        assert_eq!(
            req,
            WorkspaceRenameRequest {
                workspace_id: WorkspaceId("w1".into()),
                title: "Renamed".into(),
            }
        );

        let fixture = format!(r#"{{"workspace":{VIEW_JSON}}}"#);
        let value: WorkspaceRenameValue = round_trip(&fixture);
        assert_eq!(value, WorkspaceRenameValue { workspace: view() });
    }

    #[test]
    fn delete_request_and_value() {
        let req: WorkspaceDeleteRequest = round_trip(r#"{"workspaceId":"w1"}"#);
        assert_eq!(
            req,
            WorkspaceDeleteRequest {
                workspace_id: WorkspaceId("w1".into())
            }
        );

        let value: WorkspaceDeleteValue = round_trip(r#"{"deleted":true}"#);
        assert_eq!(value, WorkspaceDeleteValue { deleted: true });
    }

    #[test]
    fn insert_before_request_and_value() {
        let req: WorkspaceInsertBeforeRequest =
            round_trip(r#"{"workspaceId":"w2","beforeWorkspaceId":"w1"}"#);
        assert_eq!(
            req,
            WorkspaceInsertBeforeRequest {
                workspace_id: WorkspaceId("w2".into()),
                before_workspace_id: Some(WorkspaceId("w1".into())),
            }
        );

        let req: WorkspaceInsertBeforeRequest = round_trip(r#"{"workspaceId":"w2"}"#);
        assert_eq!(req.before_workspace_id, None);

        let value: WorkspaceInsertBeforeValue = round_trip(r#"{"workspaceIds":["w2","w1","w3"]}"#);
        assert_eq!(
            value,
            WorkspaceInsertBeforeValue {
                workspace_ids: vec![
                    WorkspaceId("w2".into()),
                    WorkspaceId("w1".into()),
                    WorkspaceId("w3".into()),
                ],
            }
        );
    }

    #[test]
    fn insert_session_before_request_and_value() {
        let req: WorkspaceInsertSessionBeforeRequest =
            round_trip(r#"{"workspaceId":"w1","sessionId":"s5","beforeSessionId":"s2"}"#);
        assert_eq!(
            req,
            WorkspaceInsertSessionBeforeRequest {
                workspace_id: WorkspaceId("w1".into()),
                session_id: SessionId("s5".into()),
                before_session_id: Some(SessionId("s2".into())),
            }
        );

        let fixture = format!(r#"{{"workspace":{VIEW_JSON}}}"#);
        let value: WorkspaceInsertSessionBeforeValue = round_trip(&fixture);
        assert_eq!(
            value,
            WorkspaceInsertSessionBeforeValue { workspace: view() }
        );
    }

    #[test]
    fn archive_session_request_and_value() {
        let req: WorkspaceArchiveSessionRequest = round_trip(r#"{"sessionId":"s1"}"#);
        assert_eq!(
            req,
            WorkspaceArchiveSessionRequest {
                session_id: SessionId("s1".into())
            }
        );

        let value: WorkspaceArchiveSessionValue =
            round_trip(r#"{"archivedSessionIds":["s1","s9"]}"#);
        assert_eq!(
            value,
            WorkspaceArchiveSessionValue {
                archived_session_ids: vec![SessionId("s1".into()), SessionId("s9".into())],
            }
        );
    }
}

// ---------------------------------------------------------------------------
// Skill domain (skills.schema.ts) — the `@` catalog's `skill.list`
// ---------------------------------------------------------------------------

mod skill_domain {
    use super::*;

    #[test]
    fn skill_list_request_round_trips() {
        let req: SkillListRequest = round_trip(r#"{"sessionId":"s1"}"#);
        assert_eq!(
            req,
            SkillListRequest {
                session_id: SessionId("s1".into())
            }
        );
    }

    #[test]
    fn skill_list_value_round_trips_all_fields() {
        let fixture = r#"{"skills":[
            {"name":"commit","description":"write a commit message","whenToUse":null,"modelInvocable":true},
            {"name":"triage","description":"sort the inbox","whenToUse":"mail piles up","modelInvocable":false}
        ]}"#;
        let value: SkillListValue = round_trip(fixture);
        assert_eq!(value.skills.len(), 2);
        assert_eq!(value.skills[0].name, "commit");
        assert!(value.skills[0].model_invocable);
        assert_eq!(value.skills[0].when_to_use, None);
        assert_eq!(
            value.skills[1].when_to_use.as_deref(),
            Some("mail piles up")
        );
        assert!(!value.skills[1].model_invocable);
    }
}

// ---------------------------------------------------------------------------
// Approvals + questions response payloads
// ---------------------------------------------------------------------------

mod approvals_questions {
    use super::*;

    #[test]
    fn approval_response_payload_every_outcome() {
        for (code, variant) in [
            ("allowed-once", ApprovalResponseOutcome::AllowedOnce),
            ("rejected", ApprovalResponseOutcome::Rejected),
        ] {
            let fixture = format!(r#"{{"sessionId":"s1","approvalId":"a1","outcome":"{code}"}}"#);
            let payload: ApprovalResponsePayload = round_trip(&fixture);
            assert_eq!(
                payload,
                ApprovalResponsePayload {
                    session_id: SessionId("s1".into()),
                    approval_id: ApprovalRequestId("a1".into()),
                    outcome: variant,
                },
                "outcome {code}"
            );
        }
    }

    #[test]
    fn question_response_payload_with_answers() {
        let fixture = r#"{"sessionId":"s1","answer":{"answers":[{"id":"q1","selected":["a","b"],"custom":"other"},{"id":"q2","selected":[]}]}}"#;
        let payload: QuestionResponsePayload = round_trip(fixture);
        assert_eq!(
            payload,
            QuestionResponsePayload {
                session_id: SessionId("s1".into()),
                answer: AskUserQuestionAnswer {
                    answers: vec![
                        QuestionAnswerItem {
                            id: "q1".into(),
                            selected: vec!["a".into(), "b".into()],
                            custom: Some("other".into()),
                        },
                        QuestionAnswerItem {
                            id: "q2".into(),
                            selected: vec![],
                            custom: None
                        },
                    ],
                },
            }
        );
    }

    #[test]
    fn brand_newtypes_behave_like_strings() {
        let id = SessionId("abc".into());
        assert_eq!(id.as_ref(), "abc");
        assert_eq!(id.to_string(), "abc");
        assert_eq!("abc".parse::<SessionId>().unwrap(), id);
        // Transparent serialization.
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""abc""#);
        let back: SessionId = serde_json::from_str(r#""abc""#).unwrap();
        assert_eq!(back, id);
    }
}

// ---------------------------------------------------------------------------
// Tolerance: extra unknown keys must parse and must NOT survive round-trip
// ---------------------------------------------------------------------------

mod tolerance {
    use super::*;

    #[test]
    fn extra_keys_on_session_summary_are_ignored() {
        let fixture = r#"{"sessionId":"s1","updatedAt":100.0,"running":true,"blank":false,"extraTop":"x","extraNested":{"deep":[1,2]},"projections":{"asOfSeq":1,"values":{},"extraProj":true}}"#;
        let parsed: SessionSummary = round_trip(fixture);
        assert_eq!(parsed.session_id, SessionId("s1".into()));
        assert!(parsed.running);
        let re = serde_json::to_string(&parsed).unwrap();
        assert!(!re.contains("extraTop"), "extra keys must be dropped: {re}");
        assert!(!re.contains("extraProj"));
        let reparsed: SessionSummary = serde_json::from_str(&re).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn extra_keys_on_frame_and_envelope_are_ignored() {
        let fixture = r#"{"type":"session/event","sessionId":"s1","futureFrameField":1,"event":{"type":"x","seq":1,"time":1.0,"data":{},"futureEnvelopeField":{"a":1}},"view":{"for":"result","view":{"card":"c"},"extraViewField":2}}"#;
        let frame: MuxFrame = round_trip(fixture);
        match &frame {
            MuxFrame::SessionEvent { event, view, .. } => {
                assert_eq!(event.r#type, "x");
                assert_eq!(
                    view,
                    &Some(ToolEventView::Result {
                        view: ToolEventViewCard { card: "c".into() },
                    })
                );
            }
            other => panic!("expected session/event, got {other:?}"),
        }
        let re = serde_json::to_string(&frame).unwrap();
        assert!(!re.contains("futureFrameField"));
        assert!(!re.contains("futureEnvelopeField"));
        assert!(!re.contains("extraViewField"));
    }

    #[test]
    fn extra_keys_inside_error_details_are_ignored() {
        let fixture = r#"{"code":"internal","message":"m","details":{"futureDetailField":42}}"#;
        let parsed: RpcError = round_trip(fixture);
        assert_eq!(
            parsed,
            RpcError::Internal {
                message: "m".into(),
                details: EmptyDetails {},
            }
        );
    }

    #[test]
    fn null_optionals_parse_as_absent() {
        // Tolerant null handling for optional slots (host never sends these,
        // but a lenient client model accepts them).
        let fixture = r#"{"type":"session/event","sessionId":"s1","event":{"type":"x","seq":1,"time":1.0,"data":{},"sourceEventSeqs":null,"surfaceOp":null,"ignorable":null}}"#;
        let frame: MuxFrame = round_trip(fixture);
        match frame {
            MuxFrame::SessionEvent { event, .. } => {
                assert_eq!(event.source_event_seqs, None);
                assert_eq!(event.surface_op, None);
                assert_eq!(event.ignorable, None);
            }
            other => panic!("expected session/event, got {other:?}"),
        }
    }
}
