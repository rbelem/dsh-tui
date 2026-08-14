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
pub use event::{AnswerTag, AppEvent, EventChannel, spawn_frame_bridge, spawn_input_bridge};
pub use run::{TerminalGuard, setup_terminal, teardown_terminal};

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;

use crate::client::{ClientError, WireClient};
use crate::render::row_cache::RowCache;
use crate::store::node::NodeData;
use crate::store::{SessionStore, StoreError};
use crate::ui::composer::Composer;
use crate::ui::sidebar::SidebarState;
use crate::ui::takeover::{ApprovalTakeover, Mode, QuestionTakeover};
use crate::wire::approvals::{ApprovalRequestId, ApprovalResponseOutcome};
use crate::wire::events::{ApprovalOutcome, AskUserQuestionItem, MuxFrame, QuestionOutcome};
use crate::wire::rpc::RpcId;
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
    /// Approval takeover answered with this outcome; the run loop posts the
    /// respond and resolves the takeover.
    AnswerApproval(ApprovalResponseOutcome),
    /// Question takeover submitted; the run loop posts the respond.
    AnswerQuestion,
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

/// One open approval, recorded from its `approval/requested` frame so the
/// answering ClientResponse can echo the frame's rpcId.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalPending {
    /// The envelope rpcId of the `approval/requested` frame — the respond echo
    /// target.
    pub rpc_id: RpcId,
    pub session_id: SessionId,
    pub approval_id: ApprovalRequestId,
    pub tool_name: String,
    pub call_id: Option<String>,
    pub reason: Option<String>,
    /// Insertion order; the newest pending approval takes the takeover.
    pub seq: u64,
}

/// One open question frame, recorded from its `question/requested` payload.
/// The map key is the frame's envelope rpcId as a string (the respond echo
/// target; `question/resolved.questionRpcId` names the same value).
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionPending {
    pub rpc_id: RpcId,
    pub session_id: SessionId,
    pub questions: Vec<AskUserQuestionItem>,
    /// Insertion order (see [`ApprovalPending::seq`]).
    pub seq: u64,
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
    /// Open approvals keyed by approval id (recorded from `approval/requested`,
    /// removed on `approval/resolved`).
    pub pending_approvals: HashMap<ApprovalRequestId, ApprovalPending>,
    /// Open question frames keyed by their envelope rpcId (the respond echo
    /// target; `question/resolved.questionRpcId` names the same value).
    pub pending_questions: HashMap<String, QuestionPending>,
    /// Insertion counter for the pending maps (newest wins the takeover).
    pending_seq: u64,
    /// The app mode: chat, or a full-screen approval/question takeover (Q6).
    pub mode: Mode,
    /// One-line transient notice (answer confirmations, remote resolutions),
    /// shown in the status line and inside takeovers. Cleared on the first
    /// tick at least [`TOAST_TTL`] after it was set; a new toast replaces
    /// the old one.
    pub toast: Option<(String, Instant)>,
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
            pending_approvals: HashMap::new(),
            pending_questions: HashMap::new(),
            pending_seq: 0,
            mode: Mode::Chat,
            toast: None,
            hint: None,
            needs_draw: false,
            last_draw: None,
            draws: 0,
        }
    }
}

impl App {
    /// Record an answerable frame (`approval/requested`, `question/requested`)
    /// with its envelope rpcId, and open its takeover. A new approval takes
    /// the takeover immediately (newest wins); a question takes it only when
    /// no approval is open (approvals win over questions). The resolved
    /// frames are handled by [`App::record_resolved`].
    pub fn record_answerable(&mut self, rpc_id: RpcId, frame: &MuxFrame) {
        self.pending_seq += 1;
        let seq = self.pending_seq;
        match frame {
            MuxFrame::ApprovalRequested {
                session_id,
                approval_id,
                tool_name,
                call_id,
                reason,
            } => {
                let pending = ApprovalPending {
                    rpc_id,
                    session_id: session_id.clone(),
                    approval_id: approval_id.clone(),
                    tool_name: tool_name.clone(),
                    call_id: call_id.clone(),
                    reason: reason.clone(),
                    seq,
                };
                self.mode = Mode::Approval(self.approval_takeover(&pending));
                self.pending_approvals.insert(approval_id.clone(), pending);
            }
            MuxFrame::QuestionRequested {
                session_id,
                questions,
            } => {
                let pending = QuestionPending {
                    rpc_id: rpc_id.clone(),
                    session_id: session_id.clone(),
                    questions: questions.clone(),
                    seq,
                };
                if !matches!(self.mode, Mode::Approval(_)) {
                    self.mode = Mode::Question(QuestionTakeover::new(
                        session_id.clone(),
                        rpc_id.clone(),
                        questions.clone(),
                    ));
                }
                self.pending_questions.insert(rpc_id.to_string(), pending);
            }
            _ => {}
        }
    }

    /// Drop resolved answerable frames. The resolved frames' OWN envelope
    /// rpcIds are fresh push ids — correlation is payload-driven:
    /// `approval/resolved.approvalId` names the requested frame's approval,
    /// `question/resolved.questionRpcId` echoes the requested frame's rpcId.
    ///
    /// A resolution that still has a pending entry is a REMOTE resolution
    /// (another client answered — no exclusivity, Q10): toast the outcome,
    /// and if it names the displayed takeover, promote the next pending (or
    /// return to the chat). A resolution with no pending entry is the echo
    /// of a local answer — already toasted optimistically, nothing to do.
    pub fn record_resolved(&mut self, frame: &MuxFrame) {
        match frame {
            MuxFrame::ApprovalResolved {
                approval_id,
                outcome,
                ..
            } => {
                if self.pending_approvals.remove(approval_id).is_none() {
                    return;
                }
                self.set_toast(remote_approval_text(*outcome));
                if matches!(&self.mode, Mode::Approval(takeover) if takeover.approval_id == *approval_id)
                {
                    self.mode = self.next_takeover().unwrap_or(Mode::Chat);
                }
            }
            MuxFrame::QuestionResolved {
                question_rpc_id,
                outcome,
                ..
            } => {
                if self
                    .pending_questions
                    .remove(&question_rpc_id.to_string())
                    .is_none()
                {
                    return;
                }
                self.set_toast(remote_question_text(*outcome));
                if matches!(&self.mode, Mode::Question(takeover) if takeover.rpc_id == *question_rpc_id)
                {
                    self.mode = self.next_takeover().unwrap_or(Mode::Chat);
                }
            }
            _ => {}
        }
    }

    /// Set the transient status toast (see [`App::toast`]).
    pub fn set_toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    /// The current toast text (tests and the status line).
    pub fn toast_text(&self) -> Option<&str> {
        self.toast.as_ref().map(|(text, _)| text.as_str())
    }

    /// Clear an expired toast (called from the run loop's ticks so the line
    /// visibly clears without waiting for input).
    pub fn expire_toast(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= TOAST_TTL)
        {
            self.toast = None;
            self.needs_draw = true;
        }
    }

    /// The notice line inside a takeover: a fresh toast, else the hint.
    pub fn current_notice(&self) -> Option<&str> {
        self.toast
            .as_ref()
            .map(|(text, _)| text.as_str())
            .or(self.hint.as_deref())
    }

    /// Build the approval takeover for a pending entry, enriching it with
    /// the matching tool call's one-line summary from the store.
    fn approval_takeover(&self, pending: &ApprovalPending) -> ApprovalTakeover {
        ApprovalTakeover {
            session_id: pending.session_id.clone(),
            approval_id: pending.approval_id.clone(),
            rpc_id: pending.rpc_id.clone(),
            tool_name: pending.tool_name.clone(),
            call_id: pending.call_id.clone(),
            reason: pending.reason.clone(),
            tool_summary: self.tool_summary(&pending.session_id, pending.call_id.as_deref()),
            sending: false,
        }
    }

    /// The next takeover after the displayed one resolves: the newest
    /// pending approval, else the newest pending question.
    fn next_takeover(&self) -> Option<Mode> {
        if let Some(pending) = self.pending_approvals.values().max_by_key(|p| p.seq) {
            return Some(Mode::Approval(self.approval_takeover(pending)));
        }
        self.pending_questions
            .values()
            .max_by_key(|p| p.seq)
            .map(|pending| {
                Mode::Question(QuestionTakeover::new(
                    pending.session_id.clone(),
                    pending.rpc_id.clone(),
                    pending.questions.clone(),
                ))
            })
    }

    /// `name args…` for the tool node whose call id matches, when the store
    /// has it (the approval frame itself carries no arguments).
    fn tool_summary(&self, session_id: &SessionId, call_id: Option<&str>) -> Option<String> {
        let call_id = call_id?;
        let state = self.store.session(session_id)?;
        state.nodes.iter().find_map(|node| match &node.data {
            NodeData::Tool {
                call: Some(call), ..
            } if call.call_id == call_id => {
                let args: String = call.args_raw.chars().take(60).collect();
                Some(format!("{} {}", call.name, args))
            }
            _ => None,
        })
    }

    /// Handle one key (Q15 subset). Global keys first (`Ctrl+C` quits —
    /// also during a takeover, the one documented exception), then a
    /// takeover swallows ALL keys (chat/composer/sidebar keys are inert,
    /// including `q`), then `Tab` cycles focus and the focused surface gets
    /// the key. Returns the resulting [`Action`]; `Quit` is not applied
    /// here — the run loop stops.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        use crossterm::event::{KeyCode, KeyModifiers};
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        if !matches!(self.mode, Mode::Chat) {
            return Some(self.handle_takeover_key(key));
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

    /// Takeover bindings (Q6/Q13). Approval: `y` allow once, `n`/`Esc`
    /// reject (blocking — there is no dismiss, the server waits for a
    /// response). Question: `Tab` cycles questions, `Up`/`Down`/`j`/`k`
    /// move the cursor, `Space` toggles (multi-select), `Enter` submits all
    /// answers; `Esc` is a no-op with a hint (no cancel in v1 — the server
    /// resolves eventually).
    fn handle_takeover_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::KeyCode;
        match &mut self.mode {
            Mode::Approval(takeover) => {
                if takeover.sending {
                    // Answer in flight: further answer keys are ignored.
                    return Action::None;
                }
                match key.code {
                    KeyCode::Char('y') => {
                        Action::AnswerApproval(ApprovalResponseOutcome::AllowedOnce)
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        Action::AnswerApproval(ApprovalResponseOutcome::Rejected)
                    }
                    _ => Action::None,
                }
            }
            Mode::Question(takeover) => match key.code {
                KeyCode::Tab => {
                    takeover.focus_next();
                    Action::Input
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(question) = takeover.questions.get_mut(takeover.focused) {
                        question.move_cursor(-1);
                    }
                    Action::Input
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(question) = takeover.questions.get_mut(takeover.focused) {
                        question.move_cursor(1);
                    }
                    Action::Input
                }
                KeyCode::Char(' ') => {
                    if let Some(question) = takeover.questions.get_mut(takeover.focused) {
                        question.toggle();
                    }
                    Action::Input
                }
                KeyCode::Enter if takeover.sending => Action::None,
                KeyCode::Enter => Action::AnswerQuestion,
                KeyCode::Esc => {
                    self.hint = Some("can't cancel yet — answer or wait".into());
                    Action::None
                }
                _ => Action::None,
            },
            Mode::Chat => Action::None,
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

/// Toast lifetime: cleared on the first tick at least this long after it
/// was set.
pub(crate) const TOAST_TTL: Duration = Duration::from_secs(3);

/// Toast text for a remotely resolved approval (no exclusivity, Q10).
fn remote_approval_text(outcome: ApprovalOutcome) -> String {
    match outcome {
        ApprovalOutcome::AllowedOnce => "approved by another client".into(),
        ApprovalOutcome::Rejected => "rejected by another client".into(),
        ApprovalOutcome::Cancelled => "approval cancelled".into(),
        ApprovalOutcome::Unavailable => "approval unavailable".into(),
    }
}

/// Toast text for a remotely resolved question.
fn remote_question_text(outcome: QuestionOutcome) -> String {
    match outcome {
        QuestionOutcome::Answered => "answered by another client".into(),
        QuestionOutcome::Cancelled => "question cancelled".into(),
    }
}
