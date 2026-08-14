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

use crate::wire::events::MuxFrame;

/// One event for the main loop.
#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Frame(MuxFrame),
    Resize(u16, u16),
    Tick,
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
/// Frame events. The host stream has no store surface yet (v1 TODO).
pub fn spawn_frame_bridge(
    mut mux: mpsc::UnboundedReceiver<MuxFrame>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        while let Some(frame) = mux.recv().await {
            if tx.send(AppEvent::Frame(frame)).is_err() {
                break;
            }
        }
    });
}
