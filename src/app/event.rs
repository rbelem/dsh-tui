//! App events and the production event bridges (Q3).
//!
//! One channel carries everything into the single main loop:
//! - keys/resizes from a crossterm reader task (blocking reads on the tokio
//!   blocking pool);
//! - mux frames drained from the wire client's subscriber;
//! - a 16ms `Tick` (the run loop also selects on its own interval; the
//!   channel variant exists so tests can inject ticks deterministically).

use std::time::Duration;

use crossterm::event::KeyEvent;
use tokio::sync::mpsc;

use crate::client::{ClientError, DownlinkFrame};
use crate::wire::approvals::ApprovalRequestId;
use crate::wire::events::{HostFrame, MuxFrame};
use crate::wire::rpc::{RpcId, RpcReceipt};
use crate::wire::session::{
    AttachmentId, SessionAttachmentValue, SessionCancelValue, SessionCreateValue, SessionForkValue,
    SessionHistoryValue, SessionId, SessionPromptValue, SessionRenameValue,
    SessionUpdateQueueValue,
};
use crate::wire::settings::{SettingsDescribeValue, SettingsWriteValue};
use crate::wire::skills::SkillListValue;
use crate::wire::workspace::WorkspaceArchiveSessionValue;

/// One event for the main loop.
#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Frame(MuxFrame),
    /// An answerable frame (`approval/requested`, `question/requested`) with
    /// its envelope rpcId — the echo target for the answering ClientResponse
    /// ("rpcId echoed, never minted anew", rpc.ts:178).
    Answerable {
        rpc_id: RpcId,
        frame: MuxFrame,
    },
    /// The spawned answer task finished: correlate back to the open takeover
    /// via the tag.
    AnswerDone {
        tag: AnswerTag,
        result: Result<RpcReceipt, ClientError>,
    },
    /// The spawned `session.prompt` task finished.
    PromptDone {
        result: Result<SessionPromptValue, ClientError>,
    },
    /// The spawned `session.cancel` task finished (Q15).
    CancelDone {
        result: Result<SessionCancelValue, ClientError>,
    },
    /// A lazy `session.attachment` fetch finished (the image-cache producer
    /// lane): the base64 payload lands in `ImageCache` on success.
    AttachmentDone {
        attachment_id: AttachmentId,
        result: Result<SessionAttachmentValue, ClientError>,
    },
    /// A history page for a switched-to session arrived (Q9); the app folds
    /// it only when the session is still active (stale guard).
    HistoryLoaded {
        session_id: SessionId,
        result: Result<SessionHistoryValue, ClientError>,
    },
    /// A spawned `session.updateQueue` action finished.
    QueueActionDone {
        kind: QueueActionKind,
        result: Result<SessionUpdateQueueValue, ClientError>,
    },
    /// A spawned sidebar `session.rename` finished (`r`).
    RenameDone {
        session_id: SessionId,
        result: Result<SessionRenameValue, ClientError>,
    },
    /// A spawned sidebar `session.fork` finished (`f`).
    ForkDone {
        result: Result<SessionForkValue, ClientError>,
    },
    /// A spawned new-session picker `session.create` finished (`n`).
    SessionCreateDone {
        result: Result<SessionCreateValue, ClientError>,
    },
    /// A spawned sidebar `workspace.archiveSession` finished (`a`).
    ArchiveDone {
        session_id: SessionId,
        result: Result<WorkspaceArchiveSessionValue, ClientError>,
    },
    /// The `@` catalog fetch (`skill.list`) finished.
    CatalogLoaded {
        result: Result<SkillListValue, ClientError>,
    },
    /// The spawned `settings.describe` task finished (opening the settings
    /// view, or the conflict refresh). The app folds it only while the
    /// settings view is open.
    SettingsDescribeDone {
        result: Result<SettingsDescribeValue, ClientError>,
    },
    /// The spawned `settings.update` task finished; `ns` correlates the
    /// result back to the form that spawned it.
    SettingsSaveDone {
        ns: String,
        result: Result<SettingsWriteValue, ClientError>,
    },
    /// A host-stream frame (session list liveness). Host frames are pure
    /// pushes — the downlink rpcId is ignored.
    HostFrame(HostFrame),
    Resize(u16, u16),
    Tick,
}

/// Which queue action a finished task applied (the success toast echoes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueActionKind {
    Remove,
    Steer,
    Edit,
}

/// Correlates a finished answer task back to the takeover that spawned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerTag {
    Approval {
        approval_id: ApprovalRequestId,
        /// The outcome the user chose — the success toast echoes it
        /// ("allowed once" / "rejected").
        outcome: crate::wire::approvals::ApprovalResponseOutcome,
    },
    /// The question frame's envelope rpcId (the echo target).
    Question(RpcId),
}

/// The app event channel: the run loop owns the `rx` end; spawned
/// back-channel tasks (answers, prompts) hold `tx` clones.
pub struct EventChannel {
    pub tx: mpsc::UnboundedSender<AppEvent>,
    pub rx: mpsc::UnboundedReceiver<AppEvent>,
}

impl EventChannel {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        EventChannel { tx, rx }
    }
}

impl Default for EventChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a mux frame is answerable (its envelope rpcId is the respond echo
/// target). Only approval/question requested frames — `session/event` etc.
/// are pure pushes.
pub fn is_answerable(frame: &MuxFrame) -> bool {
    matches!(
        frame,
        MuxFrame::ApprovalRequested { .. } | MuxFrame::QuestionRequested { .. }
    )
}

/// Spawn the crossterm input bridge: reads terminal events and forwards
/// Key/Resize (mouse/focus/paste have no v1 surface). `crossterm::event::read`
/// blocks, so the loop runs on the tokio blocking pool. Stops when the
/// channel closes or the terminal errors.
pub fn spawn_input_bridge(tx: mpsc::UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            loop {
                // Poll with a timeout instead of blocking forever: the loop
                // must be able to notice the channel closing so the app can
                // exit cleanly (an unbounded `read()` would hang the runtime
                // shutdown after Ctrl+Q).
                match crossterm::event::poll(Duration::from_millis(100)) {
                    Ok(true) => match crossterm::event::read() {
                        Ok(crossterm::event::Event::Key(key)) => {
                            if tx.send(AppEvent::Key(key)).is_err() {
                                break;
                            }
                        }
                        Ok(crossterm::event::Event::Resize(width, height)) => {
                            if tx.send(AppEvent::Resize(width, height)).is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    },
                    Ok(false) => {
                        // The run loop dropped the receiver (app quitting).
                        if tx.is_closed() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .await;
        let _ = result;
    });
}

/// Spawn the mux frame bridge: drains the wire client's mux subscriber into
/// events. Answerable frames travel as [`AppEvent::Answerable`] (envelope
/// rpcId preserved); everything else as [`AppEvent::Frame`].
pub fn spawn_frame_bridge(
    mut mux: mpsc::UnboundedReceiver<DownlinkFrame<MuxFrame>>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        while let Some(downlink) = mux.recv().await {
            let event = if is_answerable(&downlink.frame) {
                AppEvent::Answerable {
                    rpc_id: downlink.rpc_id,
                    frame: downlink.frame,
                }
            } else {
                AppEvent::Frame(downlink.frame)
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    });
}

/// Spawn the host frame bridge: drains the wire client's host subscriber
/// into [`AppEvent::HostFrame`]s. Host frames are pure pushes (the downlink
/// rpcId is ignored); the app handles the session-liveness subset and
/// ignores the rest (workspace grouping, archived filtering — later lanes).
pub fn spawn_host_bridge(
    mut host: mpsc::UnboundedReceiver<DownlinkFrame<HostFrame>>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        while let Some(downlink) = host.recv().await {
            if tx.send(AppEvent::HostFrame(downlink.frame)).is_err() {
                break;
            }
        }
    });
}
