//! App shell: boot, event loop, coalesced draw, attach/resume flow, and the
//! first UI surface lane — sidebar (session list), composer (multi-line
//! input), and focus cycling between them and the chat (ticket 05 Q3/Q9/
//! Q14/Q15, ticket 06 Q8).
//!
//! One main thread owns the store and the draw (Q3): tokio delivers events
//! (keys from crossterm, mux frames from the wire client, a 16ms tick) over a
//! single mpsc channel; the loop folds frames into the store and coalesces
//! draws at ~16ms. Everything is testable without a real terminal — tests
//! inject [`AppEvent`]s into the same channel and draw into a `TestBackend`.
//!
//! Later lanes: live host-stream session updates, the real `/` command
//! catalog (`command.execute`), the queue strip, workspace grouping, menus,
//! themes.

pub mod attach;
pub mod event;
pub mod run;

pub use attach::attach;
pub use event::{AppEvent, spawn_frame_bridge, spawn_input_bridge};
pub use run::{TerminalGuard, setup_terminal, teardown_terminal};

use std::convert::Infallible;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;

use crate::client::{ClientError, WireClient};
use crate::render::row_cache::RowCache;
use crate::store::node::NodeData;
use crate::store::{SessionStore, StoreError};
use crate::ui::composer::Composer;
use crate::ui::sidebar::SidebarState;
use crate::wire::session::{SessionId, SessionSummary};

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

/// Which surface holds the keyboard focus (Tab cycles chat → composer →
/// sidebar → chat, Q15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Chat,
    Composer,
    Sidebar,
}

impl Focus {
    /// The next surface in the Tab cycle.
    pub fn next(self) -> Self {
        match self {
            Focus::Chat => Focus::Composer,
            Focus::Composer => Focus::Sidebar,
            Focus::Sidebar => Focus::Chat,
        }
    }

    /// Short label for the status line.
    pub fn label(self) -> &'static str {
        match self {
            Focus::Chat => "chat",
            Focus::Composer => "composer",
            Focus::Sidebar => "sidebar",
        }
    }
}

/// One key-handling outcome (table-testable, Q15 subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Signed scroll delta in cached rows. `i64::MIN`/`i64::MAX` encode
    /// "to top" / "to bottom (follow)" (see [`App::handle_key`]).
    Scroll(i64),
    /// Focus moved to another surface (Tab, or Esc back to the chat).
    Focus(Focus),
    /// Composer buffer or caret changed.
    Input,
    /// Composer submitted this text; the run loop dispatches `session.prompt`.
    Submit(String),
    /// Sidebar selection moved.
    Select,
    /// Sidebar Enter switched the active session.
    SwitchSession(SessionId),
    /// Consumed but no-op (Esc in the chat, a blocked submit).
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

/// The application state: store + render cache + viewport + UI surfaces.
pub struct App {
    pub store: SessionStore,
    pub row_cache: RowCache,
    pub view: ViewState,
    pub active_session: Option<SessionId>,
    /// Session list snapshot from the attach flow's `session.list` (live
    /// host-stream updates are a later lane).
    pub sessions: Vec<SessionSummary>,
    /// Which surface holds the keyboard focus.
    pub focus: Focus,
    pub composer: Composer,
    pub sidebar: SidebarState,
    /// The attached gateway client (submit dispatch); `None` in keyless tests.
    pub client: Option<WireClient>,
    pub running: bool,
    /// Last non-fatal error, shown in the status line.
    pub last_error: Option<String>,
    /// One-line transient hint in the status line (e.g. a blocked submit).
    pub hint: Option<String>,
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
            sessions: Vec::new(),
            focus: Focus::default(),
            composer: Composer::new(),
            sidebar: SidebarState::default(),
            client: None,
            running: true,
            last_error: None,
            hint: None,
            needs_draw: false,
            last_draw: None,
            draws: 0,
        }
    }
}

impl App {
    /// Handle one key (Q15 subset). Global keys first (`Ctrl+C` quits, `Tab`
    /// cycles focus), then dispatch to the focused surface. Returns the
    /// resulting [`Action`]; `Quit` is not applied here — the run loop stops.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        use crossterm::event::{KeyCode, KeyModifiers};
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        if key.code == KeyCode::Tab {
            let next = self.focus.next();
            self.focus = next;
            return Some(Action::Focus(next));
        }
        match self.focus {
            Focus::Chat => self.handle_chat_key(key),
            Focus::Composer => Some(self.handle_composer_key(key)),
            Focus::Sidebar => self.handle_sidebar_key(key),
        }
    }

    /// Chat bindings: `q` quits; `j`/`Down` +1 row; `k`/`Up` -1 row;
    /// `g`/`Home` top; `G`/`End` bottom (follow on); `Ctrl+d`/`Ctrl+u`
    /// half page; `Esc` no-op.
    fn handle_chat_key(&mut self, key: KeyEvent) -> Option<Action> {
        use crossterm::event::{KeyCode, KeyModifiers};
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
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

    /// Composer bindings: chars edit the buffer; `Enter` submits,
    /// `Shift+Enter` inserts a newline (web parity, Q14); arrows/Home/End
    /// move the caret; `Esc` returns focus to the chat. While a seed popup
    /// is open, `Up`/`Down` navigate it, `Enter` accepts, `Esc` closes it.
    fn handle_composer_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::{KeyCode, KeyModifiers};
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);

        if self.composer.popup().is_some() {
            match key.code {
                KeyCode::Up => {
                    self.composer.popup_move(-1);
                    return Action::Input;
                }
                KeyCode::Down => {
                    self.composer.popup_move(1);
                    return Action::Input;
                }
                KeyCode::Enter => {
                    self.composer.popup_accept();
                    return Action::Input;
                }
                KeyCode::Esc => {
                    self.composer.popup_dismiss();
                    return Action::None;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => {
                self.focus = Focus::Chat;
                Action::Focus(Focus::Chat)
            }
            KeyCode::Enter if shift => {
                self.composer.newline();
                Action::Input
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.composer.backspace();
                Action::Input
            }
            KeyCode::Delete => {
                self.composer.delete();
                Action::Input
            }
            KeyCode::Left => {
                self.composer.move_left();
                Action::Input
            }
            KeyCode::Right => {
                self.composer.move_right();
                Action::Input
            }
            KeyCode::Home => {
                self.composer.move_home();
                Action::Input
            }
            KeyCode::End => {
                self.composer.move_end();
                Action::Input
            }
            KeyCode::Up => {
                self.composer.move_up();
                Action::Input
            }
            KeyCode::Down => {
                self.composer.move_down();
                Action::Input
            }
            KeyCode::Char(c) if !control => {
                self.composer.insert_char(c);
                Action::Input
            }
            _ => Action::None,
        }
    }

    /// Sidebar bindings: `j`/`k`/arrows move the selection, `g`/`G` (and
    /// Home/End) jump to the ends, `Enter` switches the active session,
    /// `Esc` returns focus to the chat, `q` quits.
    fn handle_sidebar_key(&mut self, key: KeyEvent) -> Option<Action> {
        use crossterm::event::KeyCode;
        let len = self.sessions.len();
        match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('j') | KeyCode::Down => {
                self.sidebar.move_by(1, len);
                Some(Action::Select)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.sidebar.move_by(-1, len);
                Some(Action::Select)
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.sidebar.first();
                Some(Action::Select)
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.sidebar.last(len);
                Some(Action::Select)
            }
            KeyCode::Enter => Some(self.switch_to_selected()),
            KeyCode::Esc => {
                self.focus = Focus::Chat;
                Some(Action::Focus(Focus::Chat))
            }
            _ => None,
        }
    }

    /// Enter in the composer: no-op on an empty buffer; a blocked no-op with
    /// a status hint while the session is running; otherwise take the buffer
    /// and ask the run loop to dispatch `session.prompt`.
    fn submit(&mut self) -> Action {
        if self.composer.buffer().trim().is_empty() {
            return Action::None;
        }
        if self.session_running() {
            self.hint = Some("turn running — wait for it to finish".into());
            return Action::None;
        }
        self.hint = None;
        Action::Submit(self.composer.take())
    }

    /// Enter in the sidebar: switch the active session to the selected row —
    /// open its store state, drop the row cache (node keys collide across
    /// sessions), and reset the viewport. History fetch on switch is a later
    /// lane (Q9); the chat shows what the mux stream has already delivered.
    fn switch_to_selected(&mut self) -> Action {
        let Some(summary) = self.sessions.get(self.sidebar.selected) else {
            return Action::None;
        };
        let session_id = summary.session_id.clone();
        if self.active_session.as_ref() == Some(&session_id) {
            return Action::None;
        }
        self.store.open_session(session_id.clone());
        self.row_cache.invalidate_all();
        self.active_session = Some(session_id.clone());
        self.view.offset = 0;
        self.view.follow = true;
        self.hint = None;
        Action::SwitchSession(session_id)
    }

    /// Whether the active session has a turn in flight: the summary's
    /// `running` flag, or the node fold — the last node is an assistant or
    /// tool node that has neither finalized nor been interrupted.
    pub fn session_running(&self) -> bool {
        let Some(session_id) = &self.active_session else {
            return false;
        };
        if self
            .sessions
            .iter()
            .any(|summary| &summary.session_id == session_id && summary.running)
        {
            return true;
        }
        let Some(state) = self.store.session(session_id) else {
            return false;
        };
        match state.nodes.last().map(|node| &node.data) {
            Some(NodeData::Assistant {
                finalized,
                interrupted,
                ..
            }) => !finalized && !interrupted,
            Some(NodeData::Tool {
                result,
                interrupted,
                ..
            }) => result.is_none() && !interrupted,
            _ => false,
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
