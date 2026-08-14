//! App events and the production event bridges (Q3).
//!
//! One channel carries everything into the single main loop:
//! - keys/resizes from a crossterm reader task (blocking reads on the tokio
//!   blocking pool);
//! - mux frames drained from the wire client's subscriber;
//! - a 16ms `Tick` (the run loop also selects on its own interval; the
//!   channel variant exists so tests can inject ticks deterministically).

use crossterm::event::KeyEvent;
use tokio::sync::mpsc;

use crate::client::DownlinkFrame;
use crate::wire::events::MuxFrame;
use crate::wire::rpc::RpcId;

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
    Resize(u16, u16),
    Tick,
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
                match crossterm::event::read() {
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
                }
            }
        })
        .await;
        let _ = result;
    });
}

/// Spawn the mux frame bridge: drains the wire client's mux subscriber into
/// events. Answerable frames travel as [`AppEvent::Answerable`] (envelope
/// rpcId preserved); everything else as [`AppEvent::Frame`]. The host stream
/// has no store surface yet (v1 TODO).
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
