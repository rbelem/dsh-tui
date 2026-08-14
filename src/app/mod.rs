//! App shell: boot, event loop, coalesced draw, attach/resume flow, minimal
//! navigation (ticket 05 Q3/Q9/Q15, ticket 06 Q8).
//!
//! One main thread owns the store and the draw (Q3): tokio delivers events
//! (keys from crossterm, mux frames from the wire client, a 16ms tick) over a
//! single mpsc channel; the loop folds frames into the store and coalesces
//! draws at ~16ms. Everything is testable without a real terminal — tests
//! inject [`AppEvent`]s into the same channel and draw into a `TestBackend`.
//!
//! The visual surface lane (composer, sidebar, menus, themes) comes later;
//! this lane is functional plumbing only.

pub mod attach;
pub mod event;
pub mod run;

pub use attach::attach;
pub use event::{AppEvent, spawn_frame_bridge, spawn_input_bridge};
pub use run::{TerminalGuard, setup_terminal, teardown_terminal};

use std::convert::Infallible;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;

use crate::client::ClientError;
use crate::render::row_cache::RowCache;
use crate::store::{SessionStore, StoreError};
use crate::wire::session::SessionId;

/// App-level failure.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("client error: {0}")]
    Client(#[from] ClientError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
}

impl From<Infallible> for AppError {
    fn from(value: Infallible) -> Self {
        match value {}
    }
}

/// One key-handling outcome (table-testable, Q15 subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Signed scroll delta in cached rows. `i64::MIN`/`i64::MAX` encode
    /// "to top" / "to bottom (follow)" (see [`App::handle_key`]).
    Scroll(i64),
    /// Consumed but no-op (Esc in v1).
    None,
}

/// Viewport state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewState {
    /// Viewport offset into the cached row array (scroll position).
    pub offset: usize,
    /// Stick to the bottom: the offset clamps to the bottom on every draw.
    pub follow: bool,
    /// Chat-area height (rows), used for half-page scrolls; updated on
    /// resize and at draw time.
    pub viewport_height: u16,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            offset: 0,
            follow: true,
            viewport_height: 24,
        }
    }
}

/// The application state: store + render cache + viewport.
pub struct App {
    pub store: SessionStore,
    pub row_cache: RowCache,
    pub view: ViewState,
    pub active_session: Option<SessionId>,
    pub running: bool,
    /// Last non-fatal error, shown in the status line.
    pub last_error: Option<String>,
    /// Pending draw flag (coalescing).
    needs_draw: bool,
    last_draw: Option<Instant>,
    /// Draw counter (integration-test observability; plain field because
    /// integration tests link the lib without `cfg(test)`).
    pub draws: usize,
}

impl Default for App {
    fn default() -> Self {
        App {
            store: SessionStore::new(),
            row_cache: RowCache::new(),
            view: ViewState::default(),
            active_session: None,
            running: true,
            last_error: None,
            needs_draw: false,
            last_draw: None,
            draws: 0,
        }
    }
}

impl App {
    /// Handle one key (Q15 subset). Applies the scroll to the view and
    /// returns the resulting [`Action`]; `Quit` is not applied here — the
    /// run loop stops.
    ///
    /// Bindings: `q`/`Ctrl+C` quit; `j`/`Down` +1 row; `k`/`Up` -1 row;
    /// `g`/`Home` top; `G`/`End` bottom (follow on); `Ctrl+d`/`Ctrl+u`
    /// half page; `Esc` no-op.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        use crossterm::event::{KeyCode, KeyModifiers};
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('c') if control => Some(Action::Quit),
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll(1);
                Some(Action::Scroll(1))
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll(-1);
                Some(Action::Scroll(-1))
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.view.offset = 0;
                self.view.follow = false;
                Some(Action::Scroll(i64::MIN))
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.view.follow = true;
                Some(Action::Scroll(i64::MAX))
            }
            KeyCode::Char('d') if control => {
                let half = (self.view.viewport_height / 2) as i64;
                self.scroll(half);
                Some(Action::Scroll(half))
            }
            KeyCode::Char('u') if control => {
                let half = (self.view.viewport_height / 2) as i64;
                self.scroll(-half);
                Some(Action::Scroll(-half))
            }
            KeyCode::Esc => Some(Action::None),
            _ => None,
        }
    }

    /// Apply a signed scroll delta; manual scrolling turns follow off.
    fn scroll(&mut self, delta: i64) {
        self.view.follow = false;
        if delta >= 0 {
            self.view.offset = self.view.offset.saturating_add(delta as usize);
        } else {
            self.view.offset = self
                .view
                .offset
                .saturating_sub(delta.unsigned_abs() as usize);
        }
    }
}

/// Coalesced draw interval (Q3).
pub(crate) const DRAW_INTERVAL: Duration = Duration::from_millis(16);
