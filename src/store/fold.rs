//! The event fold: pure derivation of chat nodes from a window of stored
//! events.
//!
//! Mirrors the web's chat-node definitions (v1 subset). The node list is
//! derived state: `fold_events` is pure and idempotent, and the store re-runs
//! it after every window mutation.
//!
//! v1 semantics:
//! - User nodes from append `user/message` (kind by `source.kind`; steering
//!   distinction deferred), context-injection and compaction-checkpoint
//!   messages included.
//! - One assistant node per (turn, step), streamed from chunks and settled by
//!   `assistant/message`; empty-content finalize is skipped.
//! - One tool node per call id, settled by `tool/result` (call backfilled when
//!   in-window).
//! - Compaction nodes keyed by compaction id, formed when the replacement
//!   checkpoint lands; the replacement user/message is ALSO appended as its
//!   own user node (the transcript keeps shadowed messages — the web replaces
//!   on the model surface only).
//! - Boundaries (step/end, turn/end, a new user message) close open nodes;
//!   closed-with-evidence => interrupted.
//! - Required unknown event types degrade to UnknownNode rows; ignorable
//!   unknowns are skipped. `compaction/*` (plugin-owned) are handled by
//!   type-name string + wide data.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use serde::Deserialize;
use serde_json::Value;

use crate::store::event_data::{
    AssistantMessage, CallId, ContentBlock, EventData, StreamChunk, SurfaceOp, TokenUsage,
    ToolErrorIdentity, ToolResultMessage, TurnEndReason, UserMessage, parse_surface_op,
};
use crate::store::node::{
    AssistantBlock, ChatNode, ChatNodeKind, NodeData, NodeKey, RunningToolCall, ToolCallBackfill,
    ToolResultNode, UserNodeKind,
};
use crate::store::session::StoredEvent;
use crate::wire::session::ToolEventView;

/// Fold a window of stored events into the ordered chat-node list.
///
/// Pure and idempotent: the node list is derived state. Nodes are ordered by
/// ascending anchor seq (stable — ties keep fold-insertion order).
pub fn fold_events(events: &[StoredEvent]) -> Vec<ChatNode> {
    let mut fold = Fold::default();
    for stored in events {
        fold.apply(stored);
    }
    fold.finish();
    fold.nodes.sort_by_key(|n| n.anchor_seq);
    fold.nodes
}

/// In-progress fold state.
#[derive(Default)]
struct Fold {
    assistants: HashMap<(i64, i64), AssistantFold>,
    tools: HashMap<String, ToolFold>,
    compactions: HashMap<String, CompactionFold>,
    nodes: Vec<ChatNode>,
}

struct AssistantFold {
    turn: i64,
    step: i64,
    anchor_seq: i64,
    /// Indexed by stream chunk `index`; holes stay `None` while streaming.
    blocks: Vec<Option<AssistantBlock>>,
    finalized: bool,
    usage: Option<TokenUsage>,
}

struct ToolFold {
    call: Option<RunningToolCall>,
    result: Option<ToolResultNode>,
    turn: i64,
    step: i64,
    anchor_seq: i64,
}

/// One compaction lifecycle (compaction.ts): summary metering + checkpoint.
struct CompactionFold {
    summary: Option<CompactionSummaryData>,
    summary_event_seq: Option<i64>,
    /// Whether the replacement checkpoint has landed — the marker node only
    /// appears once it has (web compaction.ts: `checkpoint === undefined`).
    checkpoint: bool,
}

/// The `compaction/summary` fields the marker node reads (compaction types,
/// wide parse — extra fields like provider/model/usage are ignored).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompactionSummaryData {
    compaction_id: String,
    summary: Vec<ContentBlock>,
    shadowed_seqs: Vec<i64>,
    shadowed_token_count: i64,
}

impl Fold {
    fn apply(&mut self, stored: &StoredEvent) {
        match &stored.data {
            EventData::Unknown { ignorable: true } => {}
            EventData::Unknown { ignorable: false } => match stored.event.r#type.as_str() {
                // Plugin-owned compaction events: handle by type-name string +
                // wide data, and never degrade them to UnknownNode rows.
                "compaction/start" => self.apply_compaction_start(stored),
                "compaction/summary" => self.apply_compaction_summary(stored),
                "compaction/end" => {}
                _ => self.push_unknown(stored),
            },
            EventData::TurnStart { .. } | EventData::EndSeed => {}
            EventData::TurnEnd { turn, reason } => {
                self.apply_turn_end(stored, *turn, reason);
            }
            EventData::StepStart { turn, step } => {
                self.assistants
                    .entry((*turn, *step))
                    .or_insert_with(|| AssistantFold {
                        turn: *turn,
                        step: *step,
                        anchor_seq: stored.event.seq,
                        blocks: Vec::new(),
                        finalized: false,
                        usage: None,
                    });
            }
            EventData::StepEnd { turn, step } => self.close_step(*turn, *step, stored.event.seq),
            EventData::UserMessage(message) => self.apply_user_message(stored, message),
            EventData::AssistantChunk { turn, step, chunk } => {
                self.apply_chunk(stored.event.seq, *turn, *step, chunk);
            }
            EventData::AssistantMessage {
                turn,
                step,
                message,
                usage,
            } => {
                self.apply_assistant_message(stored, *turn, *step, message, usage.as_ref());
            }
            EventData::ToolCall {
                turn,
                step,
                call_id,
                name,
                arguments,
            } => {
                self.apply_tool_call(stored, *turn, *step, call_id, name, arguments);
            }
            EventData::ToolResult {
                turn,
                step,
                message,
                error,
                meta,
            } => {
                self.apply_tool_result(
                    stored,
                    *turn,
                    *step,
                    message,
                    error.as_ref(),
                    meta.as_ref(),
                );
            }
            EventData::TodoWrite { .. }
            | EventData::RequestHeader { .. }
            | EventData::RequestContext { .. } => {}
        }
    }

    // ---- user messages ---------------------------------------------------

    fn apply_user_message(&mut self, stored: &StoredEvent, message: &UserMessage) {
        let surface =
            parse_surface_op(stored.event.surface_op.as_ref()).unwrap_or(SurfaceOp::Append);
        let compact_checkpoint = matches!(surface, SurfaceOp::Replace { .. })
            && message.source_kind() == Some("plugin")
            && message.source_plugin() == Some("compact");
        if compact_checkpoint {
            self.apply_compaction_checkpoint(stored, message);
        } else if matches!(surface, SurfaceOp::Replace { .. }) {
            // A non-compact replacement has no v1 claim — degrade, never drop.
            self.push_unknown(stored);
            return;
        } else if message.source_kind() == Some("user") {
            // A real human prompt closes whatever the previous turn left open.
            self.close_all_open(stored.event.seq);
        }
        let kind = if message.source_kind() == Some("user") {
            UserNodeKind::User
        } else {
            // Injected context / plugin messages (steering is a v1 TODO).
            UserNodeKind::Context
        };
        self.push_node(ChatNode {
            key: message.id.clone(),
            kind: ChatNodeKind::User,
            anchor_seq: stored.event.seq,
            data: NodeData::User {
                kind,
                message_id: message.id.clone(),
                content: message.content.clone(),
                source: message.source.clone(),
            },
        });
    }

    // ---- assistant streaming ---------------------------------------------

    fn apply_chunk(&mut self, seq: i64, turn: i64, step: i64, chunk: &StreamChunk) {
        let assistant = match self.assistants.entry((turn, step)) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(AssistantFold {
                turn,
                step,
                anchor_seq: seq,
                blocks: Vec::new(),
                finalized: false,
                usage: None,
            }),
        };
        match chunk {
            StreamChunk::BlockStart { index, block_type } => {
                let Some(index) = non_negative(*index) else {
                    return;
                };
                let block = match block_type.as_str() {
                    "text" => Some(AssistantBlock::Text {
                        text: String::new(),
                    }),
                    "reasoning" => Some(AssistantBlock::Reasoning {
                        text: String::new(),
                    }),
                    "tool-call" => Some(AssistantBlock::ToolCall {
                        call_id: String::new(),
                        name: String::new(),
                        args_raw: String::new(),
                    }),
                    _ => None,
                };
                set_block(&mut assistant.blocks, index, block);
            }
            StreamChunk::TextDelta { index, text } => {
                let Some(index) = non_negative(*index) else {
                    return;
                };
                if !grow_to(&mut assistant.blocks, index) {
                    return;
                }
                let block = match assistant.blocks[index].take() {
                    Some(AssistantBlock::Text { text: previous }) => AssistantBlock::Text {
                        text: format!("{previous}{text}"),
                    },
                    _ => AssistantBlock::Text { text: text.clone() },
                };
                assistant.blocks[index] = Some(block);
            }
            StreamChunk::ReasoningDelta { index, text } => {
                let Some(index) = non_negative(*index) else {
                    return;
                };
                if !grow_to(&mut assistant.blocks, index) {
                    return;
                }
                let block = match assistant.blocks[index].take() {
                    Some(AssistantBlock::Reasoning { text: previous }) => {
                        AssistantBlock::Reasoning {
                            text: format!("{previous}{text}"),
                        }
                    }
                    _ => AssistantBlock::Reasoning { text: text.clone() },
                };
                assistant.blocks[index] = Some(block);
            }
            StreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let Some(index) = non_negative(*index) else {
                    return;
                };
                if !grow_to(&mut assistant.blocks, index) {
                    return;
                }
                let block = match assistant.blocks[index].take() {
                    Some(AssistantBlock::ToolCall {
                        call_id,
                        name: previous_name,
                        args_raw,
                    }) => AssistantBlock::ToolCall {
                        call_id: if call_id.is_empty() {
                            id.to_string()
                        } else {
                            call_id
                        },
                        name: name.clone().unwrap_or(previous_name),
                        args_raw: format!("{args_raw}{arguments_delta}"),
                    },
                    _ => AssistantBlock::ToolCall {
                        call_id: id.to_string(),
                        name: name.clone().unwrap_or_default(),
                        args_raw: arguments_delta.clone(),
                    },
                };
                assistant.blocks[index] = Some(block);
            }
            StreamChunk::BlockEnd { index, block } => {
                let Some(index) = non_negative(*index) else {
                    return;
                };
                set_block(&mut assistant.blocks, index, to_assistant_block(block));
            }
            StreamChunk::Usage { usage } => assistant.usage = Some(usage.clone()),
            // finish is stream-level metadata; turn/end carries the failure.
            StreamChunk::Finish { .. } => {}
        }
    }

    fn apply_assistant_message(
        &mut self,
        stored: &StoredEvent,
        turn: i64,
        step: i64,
        message: &AssistantMessage,
        usage: Option<&TokenUsage>,
    ) {
        if matches!(
            parse_surface_op(stored.event.surface_op.as_ref()),
            Some(SurfaceOp::Replace { .. })
        ) {
            self.push_unknown(stored);
            return;
        }
        let blocks = to_assistant_blocks(&message.content);
        // An empty-content assistant/message is skipped: it exists only to
        // host a max-tokens step's usage (deriveEventMessage, surface.ts:103).
        if blocks.is_empty() {
            return;
        }
        self.assistants.remove(&(turn, step));
        self.push_node(ChatNode {
            key: assistant_key(turn, step),
            kind: ChatNodeKind::Assistant,
            anchor_seq: stored.event.seq,
            data: NodeData::Assistant {
                turn,
                step,
                blocks,
                usage: usage.cloned(),
                finalized: true,
                interrupted: false,
            },
        });
    }

    // ---- tool calls ------------------------------------------------------

    fn apply_tool_call(
        &mut self,
        stored: &StoredEvent,
        turn: i64,
        step: i64,
        call_id: &CallId,
        name: &str,
        arguments: &str,
    ) {
        let running = RunningToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            args_raw: arguments.to_string(),
            turn,
            step,
            time: stored.event.time,
            call_view: call_view_of(stored.view.as_ref()),
        };
        match self.tools.entry(call_id.to_string()) {
            Entry::Occupied(mut entry) => {
                // Result-first window cut: backfill the call onto the node.
                if entry.get().call.is_none() {
                    entry.get_mut().call = Some(running);
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(ToolFold {
                    call: Some(running),
                    result: None,
                    turn,
                    step,
                    anchor_seq: stored.event.seq,
                });
            }
        }
    }

    fn apply_tool_result(
        &mut self,
        stored: &StoredEvent,
        turn: i64,
        step: i64,
        message: &ToolResultMessage,
        error: Option<&ToolErrorIdentity>,
        meta: Option<&Value>,
    ) {
        let call_id = message.source_call_id().unwrap_or_default().to_string();
        let result_view = result_view_of(stored.view.as_ref());
        let content = message
            .tool_result_block()
            .map(|block| match block {
                ContentBlock::ToolResult { content, .. } => content.clone(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        let (call, call_time, call_view) = match self.tools.get(&call_id) {
            Some(existing) => (
                existing.call.as_ref().map(|call| ToolCallBackfill {
                    name: call.name.clone(),
                    args_raw: call.args_raw.clone(),
                }),
                existing.call.as_ref().map(|call| call.time),
                existing
                    .call
                    .as_ref()
                    .and_then(|call| call.call_view.clone()),
            ),
            None => (None, None, None),
        };
        let result = ToolResultNode {
            call_id: call_id.clone(),
            call,
            call_time,
            result_time: Some(stored.event.time),
            content,
            is_error: message.is_error(),
            error: error.cloned(),
            meta: meta.cloned(),
            call_view,
            result_view,
        };
        match self.tools.entry(call_id.clone()) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().result = Some(result);
            }
            Entry::Vacant(entry) => {
                // Result without call (window cut): the node still forms.
                entry.insert(ToolFold {
                    call: None,
                    result: Some(result),
                    turn,
                    step,
                    anchor_seq: stored.event.seq,
                });
            }
        }
        self.push_settled_tool(&call_id);
    }

    /// Publish the settled tool node for `call_id` (deduped by key).
    fn push_settled_tool(&mut self, call_id: &str) {
        let (call, result, anchor_seq) = {
            let Some(fold) = self.tools.get(call_id) else {
                return;
            };
            let Some(result) = fold.result.clone() else {
                return;
            };
            (fold.call.clone(), result, fold.anchor_seq)
        };
        self.push_node(ChatNode {
            key: call_id.to_string(),
            kind: ChatNodeKind::Tool,
            anchor_seq,
            data: NodeData::Tool {
                call,
                result: Some(Box::new(result)),
                interrupted: false,
            },
        });
    }

    // ---- boundaries ------------------------------------------------------

    fn apply_turn_end(&mut self, stored: &StoredEvent, turn: i64, reason: &TurnEndReason) {
        // Close whatever this turn left open (interrupted).
        let assistant_keys: Vec<(i64, i64)> = self
            .assistants
            .keys()
            .copied()
            .filter(|(t, _)| *t == turn)
            .collect();
        for key in assistant_keys {
            self.close_assistant(key.0, key.1, stored.event.seq);
        }
        let tool_keys: Vec<String> = self
            .tools
            .iter()
            .filter(|(_, t)| t.turn == turn && t.result.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        for key in tool_keys {
            self.close_tool(&key, stored.event.seq);
        }
        match reason {
            TurnEndReason::Error { error } => {
                self.push_node(ChatNode {
                    key: format!("turn-error:{turn}"),
                    kind: ChatNodeKind::TurnError,
                    anchor_seq: stored.event.seq,
                    data: NodeData::TurnError {
                        turn,
                        message: error.message.clone(),
                        code: Some(error.code.clone()),
                    },
                });
            }
            TurnEndReason::MaxTokens => {
                self.push_node(ChatNode {
                    key: format!("turn-max-tokens:{turn}"),
                    kind: ChatNodeKind::TurnMaxTokens,
                    anchor_seq: stored.event.seq,
                    data: NodeData::TurnMaxTokens { turn },
                });
            }
            _ => {}
        }
    }

    fn close_step(&mut self, turn: i64, step: i64, boundary_seq: i64) {
        self.close_assistant(turn, step, boundary_seq);
        let tool_keys: Vec<String> = self
            .tools
            .iter()
            .filter(|(_, t)| t.turn == turn && t.step == step && t.result.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        for key in tool_keys {
            self.close_tool(&key, boundary_seq);
        }
    }

    fn close_all_open(&mut self, boundary_seq: i64) {
        let assistant_keys: Vec<(i64, i64)> = self.assistants.keys().copied().collect();
        for key in assistant_keys {
            self.close_assistant(key.0, key.1, boundary_seq);
        }
        let tool_keys: Vec<String> = self
            .tools
            .iter()
            .filter(|(_, t)| t.result.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        for key in tool_keys {
            self.close_tool(&key, boundary_seq);
        }
    }

    fn close_assistant(&mut self, turn: i64, step: i64, boundary_seq: i64) {
        let Some(assistant) = self.assistants.remove(&(turn, step)) else {
            return;
        };
        if assistant.finalized {
            return; // settled node already published
        }
        let blocks = compact_blocks(&assistant.blocks);
        if !has_interruption_evidence(&blocks) {
            return;
        }
        self.push_node(ChatNode {
            key: assistant_key(turn, step),
            kind: ChatNodeKind::Assistant,
            anchor_seq: boundary_seq,
            data: NodeData::Assistant {
                turn,
                step,
                blocks,
                usage: assistant.usage,
                finalized: false,
                interrupted: true,
            },
        });
    }

    fn close_tool(&mut self, call_id: &str, _boundary_seq: i64) {
        let Some(fold) = self.tools.remove(call_id) else {
            return;
        };
        if fold.result.is_some() {
            return; // settled node already published
        }
        let Some(call) = fold.call else {
            return;
        };
        // Synthesize the interrupted result (web projectBlock: name
        // "Interrupted", code "interrupted").
        let anchor_seq = fold.anchor_seq;
        let result = ToolResultNode {
            call_id: call_id.to_string(),
            call: Some(ToolCallBackfill {
                name: call.name.clone(),
                args_raw: call.args_raw.clone(),
            }),
            call_time: Some(call.time),
            result_time: None, // interrupted: no real result event
            content: Vec::new(),
            is_error: true,
            error: Some(ToolErrorIdentity {
                name: "Interrupted".into(),
                code: "interrupted".into(),
            }),
            meta: None,
            call_view: call.call_view.clone(),
            result_view: None,
        };
        self.push_node(ChatNode {
            key: call_id.to_string(),
            kind: ChatNodeKind::Tool,
            anchor_seq,
            data: NodeData::Tool {
                call: Some(call),
                result: Some(Box::new(result)),
                interrupted: true,
            },
        });
    }

    // ---- compaction ------------------------------------------------------

    fn apply_compaction_start(&mut self, stored: &StoredEvent) {
        let Some(compaction_id) = compaction_id_of(stored) else {
            return;
        };
        self.compactions
            .entry(compaction_id)
            .or_insert_with(|| CompactionFold {
                summary: None,
                summary_event_seq: None,
                checkpoint: false,
            });
    }

    fn apply_compaction_summary(&mut self, stored: &StoredEvent) {
        let Ok(summary) =
            serde_json::from_value::<CompactionSummaryData>(stored.event.data.clone())
        else {
            return; // lenient: a malformed summary contributes nothing
        };
        let fold = self
            .compactions
            .entry(summary.compaction_id.clone())
            .or_insert_with(|| CompactionFold {
                summary: None,
                summary_event_seq: None,
                checkpoint: false,
            });
        fold.summary = Some(summary);
        fold.summary_event_seq = Some(stored.event.seq);
    }

    fn apply_compaction_checkpoint(&mut self, stored: &StoredEvent, message: &UserMessage) {
        let Some(compaction_id) = message
            .source
            .get("compactionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return;
        };
        let (summary, summary_event_seq) = {
            let fold = self
                .compactions
                .entry(compaction_id.clone())
                .or_insert_with(|| CompactionFold {
                    summary: None,
                    summary_event_seq: None,
                    checkpoint: false,
                });
            fold.checkpoint = true;
            (fold.summary.clone(), fold.summary_event_seq)
        };
        // The checkpoint is what makes the marker node appear; anchor it at
        // the checkpoint seq (web compactSummary).
        self.push_node(ChatNode {
            key: compaction_id,
            kind: ChatNodeKind::Compaction,
            anchor_seq: stored.event.seq,
            data: NodeData::Compaction {
                summary: summary.as_ref().and_then(|s| summary_text(&s.summary)),
                summary_event_seq,
                shadowed_item_count: summary.as_ref().map(|s| s.shadowed_seqs.len()),
                shadowed_token_count: summary.as_ref().map(|s| s.shadowed_token_count),
            },
        });
    }

    // ---- unknown ---------------------------------------------------------

    fn push_unknown(&mut self, stored: &StoredEvent) {
        self.push_node(ChatNode {
            key: format!("unknown:{}", stored.event.seq),
            kind: ChatNodeKind::Unknown,
            anchor_seq: stored.event.seq,
            data: NodeData::Unknown {
                r#type: stored.event.r#type.clone(),
                data: stored.event.data.clone(),
            },
        });
    }

    // ---- emission --------------------------------------------------------

    /// Publish a node, replacing any previous node with the same key (a late
    /// result re-settles an interrupted tool; a rewrite replaces its node).
    fn push_node(&mut self, node: ChatNode) {
        if let Some(position) = self.nodes.iter().position(|n| n.key == node.key) {
            self.nodes.remove(position);
        }
        self.nodes.push(node);
    }

    /// Emit still-open nodes at window end: running assistants with visible
    /// content and running tools.
    fn finish(&mut self) {
        let running: Vec<ChatNode> = self
            .assistants
            .values()
            .filter_map(|assistant| {
                if assistant.finalized {
                    return None;
                }
                let blocks = compact_blocks(&assistant.blocks);
                if !has_visible_content(&blocks) {
                    return None;
                }
                Some(ChatNode {
                    key: assistant_key(assistant.turn, assistant.step),
                    kind: ChatNodeKind::Assistant,
                    anchor_seq: assistant.anchor_seq,
                    data: NodeData::Assistant {
                        turn: assistant.turn,
                        step: assistant.step,
                        blocks,
                        usage: assistant.usage.clone(),
                        finalized: false,
                        interrupted: false,
                    },
                })
            })
            .collect();
        let running_tools: Vec<ChatNode> = self
            .tools
            .values()
            .filter_map(|fold| {
                if fold.result.is_some() {
                    return None;
                }
                let call = fold.call.clone()?;
                Some(ChatNode {
                    key: call.call_id.clone(),
                    kind: ChatNodeKind::Tool,
                    anchor_seq: fold.anchor_seq,
                    data: NodeData::Tool {
                        call: Some(call),
                        result: None,
                        interrupted: false,
                    },
                })
            })
            .collect();
        for node in running {
            self.push_node(node);
        }
        for node in running_tools {
            self.push_node(node);
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn assistant_key(turn: i64, step: i64) -> NodeKey {
    format!("{turn}:{step}")
}

fn non_negative(index: i64) -> Option<usize> {
    usize::try_from(index).ok()
}

/// Upper bound on assistant blocks per turn: guards the grow_to allocation
/// against hostile stream chunk indices (i64::MAX would otherwise OOM).
const MAX_ASSISTANT_BLOCKS: usize = 1024;

fn grow_to(blocks: &mut Vec<Option<AssistantBlock>>, index: usize) -> bool {
    if index >= MAX_ASSISTANT_BLOCKS {
        return false;
    }
    if blocks.len() <= index {
        blocks.resize_with(index + 1, || None);
    }
    true
}

fn set_block(
    blocks: &mut Vec<Option<AssistantBlock>>,
    index: usize,
    block: Option<AssistantBlock>,
) {
    if grow_to(blocks, index) {
        blocks[index] = block;
    }
}

fn compact_blocks(blocks: &[Option<AssistantBlock>]) -> Vec<AssistantBlock> {
    blocks.iter().flatten().cloned().collect()
}

/// The web's `hasVisibleContent`: text/reasoning with non-empty trimmed text;
/// tool-call scaffolding alone does not count (assistant.ts:57-63).
fn has_visible_content(blocks: &[AssistantBlock]) -> bool {
    blocks.iter().any(AssistantBlock::has_visible_text)
}

/// The web's `hasInterruptionEvidence`: visible text/reasoning, or any other
/// block (tool-call) (assistant.ts:65-70).
fn has_interruption_evidence(blocks: &[AssistantBlock]) -> bool {
    blocks
        .iter()
        .any(|block| matches!(block, AssistantBlock::ToolCall { .. }) || block.has_visible_text())
}

/// Map a wire content block to an assistant display block (web
/// toAssistantBlock): text/reasoning/tool-call; other block types are not
/// part of the assistant display in v1.
fn to_assistant_block(block: &ContentBlock) -> Option<AssistantBlock> {
    match block {
        ContentBlock::Text { text } => Some(AssistantBlock::Text { text: text.clone() }),
        ContentBlock::Reasoning { text } => Some(AssistantBlock::Reasoning { text: text.clone() }),
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } => Some(AssistantBlock::ToolCall {
            call_id: id.clone(),
            name: name.clone(),
            args_raw: arguments.clone(),
        }),
        _ => None,
    }
}

fn to_assistant_blocks(content: &[ContentBlock]) -> Vec<AssistantBlock> {
    content.iter().filter_map(to_assistant_block).collect()
}

/// The frame's view when it targets the call side.
fn call_view_of(view: Option<&ToolEventView>) -> Option<ToolEventView> {
    view.filter(|v| matches!(v, ToolEventView::Call { .. }))
        .cloned()
}

/// The frame's view when it targets the result side.
fn result_view_of(view: Option<&ToolEventView>) -> Option<ToolEventView> {
    view.filter(|v| matches!(v, ToolEventView::Result { .. }))
        .cloned()
}

fn compaction_id_of(stored: &StoredEvent) -> Option<String> {
    stored
        .event
        .data
        .get("compactionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Join the summary's text blocks; None when empty (web compactSummary).
fn summary_text(summary: &[ContentBlock]) -> Option<String> {
    let text: String = summary
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}
