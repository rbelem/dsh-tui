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
pub mod light;
pub mod run;

pub use attach::attach;
pub use event::{
    AnswerTag, AppEvent, EventChannel, spawn_frame_bridge, spawn_host_bridge, spawn_input_bridge,
};
pub use run::{TerminalGuard, setup_terminal, teardown_terminal};

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;

use crate::client::{ClientError, WireClient};
use crate::render::row_cache::RowCache;
use crate::store::node::NodeData;
use crate::store::{SessionStore, StoreError};
use crate::theme::{Config, terminal_supports_color};
use crate::ui::composer::Composer;
use crate::ui::sidebar::SidebarState;
use crate::ui::takeover::{ApprovalTakeover, Mode, QuestionTakeover};
use crate::wire::approvals::{ApprovalRequestId, ApprovalResponseOutcome};
use crate::wire::events::{
    ApprovalOutcome, AskUserQuestionItem, HostFrame, MuxFrame, QuestionOutcome,
};
use crate::wire::rpc::RpcId;
use crate::wire::session::{AttachmentId, SessionId, SessionSearchItem, SessionSummary};
use unicode_width::UnicodeWidthStr;

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
    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        match self {
            Focus::Chat => crate::i18n::tr(locale, "focus.chat"),
            Focus::Composer => crate::i18n::tr(locale, "focus.composer"),
            Focus::Sidebar => crate::i18n::tr(locale, "focus.sidebar"),
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
    /// The active session's running turn should be cancelled (Q15: Ctrl+C
    /// with a turn in flight); the run loop spawns `session.cancel`.
    CancelTurn,
    /// Queue-popup actions on the focused item (spawned `session.updateQueue`).
    QueueRemove,
    QueueSteer,
    QueueEdit(String),
    /// The `@` popup needs its skill.list catalog (spawned via the
    /// back-channel; result lands as [`AppEvent::CatalogLoaded`]).
    RequestCatalog,
    /// Sidebar `/`: the search popup's query changed — the run loop
    /// spawns `session.search` (the result lands as
    /// [`AppEvent::SessionSearchDone`]).
    SearchSessions(String),
    /// Sidebar selection moved.
    Select,
    /// Sidebar Enter switched the active session.
    SwitchSession(SessionId),
    /// Sidebar `r` committed: rename the session (the run loop spawns
    /// `session.rename`; the result lands as [`AppEvent::RenameDone`]).
    RenameSession {
        session_id: SessionId,
        title: String,
    },
    /// Sidebar `f`: fork the session (spawned `session.fork`; the result
    /// lands as [`AppEvent::ForkDone`]).
    ForkSession(SessionId),
    /// Sidebar `a`: archive the session (spawned `workspace.archiveSession`;
    /// the result lands as [`AppEvent::ArchiveDone`]).
    ArchiveSession(SessionId),
    /// New-session picker Enter: create under this workspace (`None` = no
    /// workspace; the run loop spawns `session.create`, the result lands as
    /// [`AppEvent::SessionCreateDone`]).
    CreateSession {
        workspace_id: Option<crate::wire::session::WorkspaceId>,
    },
    /// Approval takeover answered with this outcome; the run loop posts the
    /// respond and resolves the takeover.
    AnswerApproval(ApprovalResponseOutcome),
    /// Question takeover submitted; the run loop posts the respond.
    AnswerQuestion,
    /// The settings view opened (Ctrl+,); the run loop spawns
    /// `settings.describe`.
    FetchSettings,
    /// The settings form asked to save (Ctrl+S); the run loop spawns
    /// `settings.update` with the form's patch.
    SaveSettings,
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

/// Truncate `text` to `max` display cells with an ASCII ellipsis
/// (CJK-safe: `chars().take` cuts by char, which mis-sizes wide text).
fn truncate_width(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w + 3 > max {
            break;
        }
        out.push(ch);
        width += w;
    }
    format!("{out}...")
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

/// The cached `@` catalog (`skill.list` result). `loading` guards against
/// duplicate fetches; a failed fetch never caches, so the next open retries.
#[derive(Debug, Default, Clone)]
pub struct AtCatalog {
    pub skills: Vec<crate::wire::skills::SkillEntry>,
    pub loading: bool,
}

/// The Ctrl+P launcher (Q17): an overlay fuzzy search over commands,
/// cached skills, and settings actions. `search` reuses the composer's
/// buffer primitives (one line); `selected` indexes the filtered list.
#[derive(Debug, Default)]
pub struct LauncherState {
    pub search: Composer,
    pub selected: usize,
}

/// The new-session picker state (`n`): `selected` indexes the picker
/// entries (workspaces in durable order + the trailing no-workspace
/// entry); `sending` guards the in-flight `session.create` (further
/// Enters inert, mirroring `sidebar_action_sending`).
#[derive(Debug, Default)]
pub struct NewSessionState {
    pub selected: usize,
    pub sending: bool,
}

/// The sidebar search popup state (`/` in the sidebar): the query buffer
/// (a composer's one-line primitives), the live `session.search` result
/// rows (they replace the grouped list while the popup is open), the
/// selection, and the in-flight guard — one `session.search` POST at a
/// time, mirroring `sidebar_action_sending` / `NewSessionState.sending`.
#[derive(Debug, Default)]
pub struct SidebarSearchState {
    pub query: Composer,
    pub results: Vec<SessionSearchItem>,
    pub selected: usize,
    pub sending: bool,
}

/// The application state: store + render cache + viewport + UI surfaces.
pub struct App {
    pub store: SessionStore,
    pub row_cache: RowCache,
    pub view: ViewState,
    pub active_session: Option<SessionId>,
    /// The sidebar's session rows: the attach flow's `session.list` snapshot
    /// plus live host-stream updates (`host/session-added|removed|status`).
    pub sessions: Vec<SessionSummary>,
    /// The sidebar's grouping: `workspace.list` rows (each workspace's
    /// `session_ids` claim its members; a session belongs to at most one
    /// workspace) plus live `host/workspace-changed|removed` updates.
    pub workspaces: Vec<crate::wire::workspace::WorkspaceView>,
    /// The sidebar's workspace display order (`host/workspace-order-changed`;
    /// seeded from the `workspace.list` item order at attach). Ids missing
    /// from this list append after the ordered ones.
    pub workspace_order: Vec<crate::wire::session::WorkspaceId>,
    /// Archived sessions (from `workspace.list` /
    /// `host/archived-sessions-changed`): grouped under a collapsed
    /// "archived" header at the sidebar's foot, out of j/k navigation.
    pub archived_session_ids: Vec<SessionId>,
    /// The sidebar's archived group is expanded (`e` toggles; app-lifetime
    /// state, no persistence) — archived sessions then render as rows and
    /// join j/k navigation.
    pub archived_expanded: bool,
    /// Which surface holds the keyboard focus.
    pub focus: Focus,
    /// Ctrl+W vim pane-prefix armed: the next h/j/k/l key moves focus,
    /// any other key disarms it (no timeout — the prefix survives until
    /// a plain key or Esc).
    pub pane_prefix: bool,
    pub composer: Composer,
    pub sidebar: SidebarState,
    /// The attached gateway client (submit dispatch); `None` in keyless tests.
    pub client: Option<WireClient>,
    /// The active theme (terminal-following default until a config theme or
    /// the picker applies one).
    pub theme: crate::theme::Theme,
    /// The UI locale (config/env-resolved at startup; Ctrl+L cycles).
    pub locale: crate::i18n::Locale,
    /// Available themes: bundled + user dir (loaded at startup).
    pub themes: crate::theme::ThemeRegistry,
    /// The Ctrl+T theme picker state.
    pub theme_picker: crate::theme::ThemePicker,
    /// The persisted config (theme choice).
    pub config: crate::theme::Config,
    /// The session whose history page is loading after a switch (Q9); `None`
    /// when idle. The status line shows "loading history…" while set.
    pub history_loading: Option<SessionId>,
    /// The view-only queue popup (`Alt+q`); scroll offset into the active
    /// session's queue items.
    pub queue_popup_open: bool,
    pub queue_scroll: usize,
    /// A `session.updateQueue` action is in flight (further actions inert).
    pub queue_action_sending: bool,
    /// The inline editor for the focused queue item's text (reuses the
    /// composer's buffer/caret primitives).
    pub queue_editor: Option<Composer>,
    /// The inline rename editor for the selected sidebar session (`r`):
    /// the target session id plus a Composer seeded with the current
    /// title; Enter commits (spawned `session.rename`), Esc cancels, and
    /// all navigation is inert while it's open.
    pub rename_editor: Option<(SessionId, Composer)>,
    /// A sidebar action (rename/fork/archive) is in flight — further
    /// actions are inert (mirrors `queue_action_sending`).
    pub sidebar_action_sending: bool,
    /// The new-session picker (`n` in the chat or sidebar): workspace
    /// choice + in-flight guard. Owns the keyboard while open.
    pub new_session: Option<NewSessionState>,
    /// The sidebar search popup (`/` in the sidebar): query + live
    /// results + in-flight guard. Owns the keyboard while open.
    pub sidebar_search: Option<SidebarSearchState>,
    /// The cached `@`-catalog (skill.list result), fetched once on first
    /// open; a failed fetch is not cached so the next open retries.
    pub at_catalog: Option<AtCatalog>,
    /// The Ctrl+P launcher (None while closed).
    pub launcher: Option<LauncherState>,
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
    /// The detected graphics protocol tier (env-based, resolved once at
    /// startup by [`App::init_images`]; `None` in `Default` so keyless tests
    /// stay terminal-agnostic — protocol paths degrade to the placeholder).
    pub image_protocol: crate::render::image::ImageProtocol,
    /// The ratatui-image picker built from the detected protocol (font size
    /// is the assumed 10×20 — no terminal query in v1).
    pub image_picker: Option<ratatui_image::picker::Picker>,
    /// Decoded image bytes by attachment id (empty in v1: the
    /// `session.attachment` fetch is a later lane — render::image docs).
    pub image_cache: crate::render::image::ImageCache,
    /// Attachment ids with a `session.attachment` fetch in flight (de-dup:
    /// a cached or pending key is never re-requested).
    pub pending_attachments: HashSet<AttachmentId>,
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
    /// The last drawn surface rects, for mouse hit-testing (#12). Zero
    /// until the first draw; a mouse event before that is a no-op.
    pub sidebar_area: Rect,
    pub chat_area: Rect,
    pub composer_area: Rect,
    /// Mouse text selection (#12): the anchor and current cell positions in
    /// CHAT-CONTENT space (rows relative to the content area — the cached
    /// line index is `view.offset + row`). `None` while not selecting.
    pub selection: Option<(CellPos, CellPos)>,
    /// `v` selection mode: drags select, mouse-up copies and exits.
    pub select_mode: bool,
    /// The `copied · N chars` flash: text + when it expires (~2s). Shown
    /// in the status line's right cluster in `success` (no toast system —
    /// this is a status-line flash only).
    pub copied_flash: Option<(String, Instant)>,
    /// #19: the narrow-terminal drawer (<80 cols) overlay is open. The
    /// drawer owns focus while open (reuses [`Focus::Sidebar`]).
    pub drawer_open: bool,
    /// The focus before the drawer opened — Esc/close restores it.
    pub drawer_prior_focus: Focus,
    /// The hint showing before the drawer opened — close restores it (the
    /// drawer hint overwrote it while open).
    pub drawer_prior_hint: Option<String>,
    /// The last drawn terminal width (set per draw; the tier decisions —
    /// too-small screen, drawer tier, status variants — read it).
    pub terminal_width: u16,
    /// #15: the running-spinner animation frame (advanced per tick; the
    /// status line cycles the braille frames while the session runs).
    pub spinner_frame: usize,
    /// #30: the drawer discoverability hint was shown once (per app run) —
    /// the first open only, so it never nags.
    pub drawer_hint_shown: bool,
}

impl Default for App {
    fn default() -> Self {
        App {
            store: SessionStore::new(),
            row_cache: RowCache::new(),
            view: ViewState::default(),
            active_session: None,
            sessions: Vec::new(),
            workspaces: Vec::new(),
            workspace_order: Vec::new(),
            archived_session_ids: Vec::new(),
            archived_expanded: false,
            focus: Focus::Composer,
            pane_prefix: false,
            composer: Composer::new(),
            sidebar: SidebarState::default(),
            client: None,
            theme: crate::theme::Theme::default(),
            locale: crate::i18n::Locale::default(),
            themes: crate::theme::ThemeRegistry::bundled(),
            theme_picker: crate::theme::ThemePicker::default(),
            config: crate::theme::Config::default(),
            history_loading: None,
            queue_popup_open: false,
            queue_scroll: 0,
            queue_action_sending: false,
            queue_editor: None,
            rename_editor: None,
            sidebar_action_sending: false,
            new_session: None,
            sidebar_search: None,
            at_catalog: None,
            launcher: None,
            pending_attachments: HashSet::new(),
            running: true,
            last_error: None,
            pending_approvals: HashMap::new(),
            pending_questions: HashMap::new(),
            pending_seq: 0,
            mode: Mode::Chat,
            image_protocol: crate::render::image::ImageProtocol::None,
            image_picker: None,
            image_cache: crate::render::image::ImageCache::default(),
            toast: None,
            hint: None,
            // Draw the first frame on the first tick: a fresh terminal must
            // not sit blank until an input event (the startup OSC 11 query
            // may also consume the first stray byte, which is harmless).
            needs_draw: true,
            last_draw: None,
            draws: 0,
            sidebar_area: Rect::default(),
            chat_area: Rect::default(),
            composer_area: Rect::default(),
            selection: None,
            select_mode: false,
            copied_flash: None,
            drawer_open: false,
            drawer_prior_focus: Focus::Chat,
            drawer_prior_hint: None,
            // Default to the wide tier: keyless tests (no draw) keep the
            // pre-#19 key semantics; the first draw sets the real width.
            terminal_width: 80,
            spinner_frame: 0,
            drawer_hint_shown: false,
        }
    }
}

/// A cell position in chat-content space (row, column), used by the mouse
/// selection (#12). Row 0 is the content area's first row (below the blank
/// top spacer); column 0 its first cell (after the 2/2 margin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellPos {
    pub row: u16,
    pub col: u16,
}

impl App {
    /// Startup image-pipeline resolution: detect the protocol tier from the
    /// environment (cached once here — see render::image docs) and build the
    /// picker. Called from `main` only; tests keep the `None` default.
    pub fn init_images(&mut self) {
        self.image_protocol = crate::render::image::detect_protocol();
        self.image_picker = crate::render::image::picker_for(self.image_protocol);
    }

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
                self.set_toast(remote_approval_text(*outcome, self.locale));
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
                self.set_toast(remote_question_text(*outcome, self.locale));
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

    /// Expire the `copied · N chars` status flash (the tick drives this).
    pub fn expire_copied_flash(&mut self) {
        if self
            .copied_flash
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= COPY_FLASH_TTL)
        {
            self.copied_flash = None;
            self.needs_draw = true;
        }
    }

    /// #15: advance the running-spinner frame and — only while the chat is
    /// actually busy — schedule the repaint that animates it. Idle draws
    /// nothing (the needs_draw gating stays untouched), so the animation
    /// causes no redraw churn when there is nothing to animate.
    pub fn advance_spinner(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
        if matches!(self.mode, Mode::Chat) && self.session_running() {
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
                // Width-aware (CJK-safe) preview of the raw arguments.
                let args: String = truncate_width(&call.args_raw, 60);
                Some(format!("{} {}", call.name, args))
            }
            _ => None,
        })
    }

    /// Handle one key (Q15 subset). Global keys first: `Ctrl+C` cancels the
    /// active session's running turn (spawned by the run loop) or quits when
    /// idle — in a takeover it stays the quit panic-button (blocking frames
    /// must not be cancelled silently); `Ctrl+Q` quits in every mode. Then a
    /// takeover swallows ALL keys, `Ctrl+T` toggles the theme picker, an
    /// open picker swallows keys until Enter/Esc, `Ctrl+W` arms the vim pane
    /// prefix (h/j/k/l move focus, any other key disarms), `Tab` cycles
    /// focus, and the focused surface gets the key. Global bindings are
    /// rebindable via `[keymap]` in config.toml (see
    /// [`crate::theme::Keymap`]); fixed: `Ctrl+W` (pane prefix), `Tab`, and
    /// the popup/picker internals (Enter/Esc and the picker's own keys).
    /// Returns the resulting [`Action`]; `Quit` is not applied here — the
    /// run loop stops.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        use crossterm::event::{KeyCode, KeyModifiers};
        if self.config.keymap.matches("cancel", key) {
            if !matches!(self.mode, Mode::Chat) {
                return Some(Action::Quit);
            }
            return if self.session_running() {
                Some(Action::CancelTurn)
            } else {
                Some(Action::Quit)
            };
        }
        if self.config.keymap.matches("quit", key) {
            return Some(Action::Quit);
        }
        // #19: below 32 cols only `q` (and the global ctrl+q/ctrl+c above)
        // works — the too-small screen owns the terminal.
        if self.terminal_width < crate::app::TOO_SMALL_WIDTH {
            if key.code == KeyCode::Char('q') && key.modifiers.is_empty() {
                return Some(Action::Quit);
            }
            return Some(Action::None);
        }
        if !matches!(self.mode, Mode::Chat) {
            return Some(self.handle_takeover_key(key));
        }
        if self.config.keymap.matches("settings", key) {
            self.mode = Mode::Settings(crate::ui::settings::SettingsState::new());
            self.hint = None;
            return Some(Action::FetchSettings);
        }
        if self.config.keymap.matches("locale", key) {
            // Ctrl+L cycles the UI locale (inert in takeovers and the
            // settings view — both swallow all keys above this point).
            self.cycle_locale();
            return Some(Action::None);
        }
        if self.config.keymap.matches("theme-picker", key) {
            if self.theme_picker.open {
                self.theme_picker.open = false;
            } else {
                self.theme_picker.selected = self
                    .themes
                    .themes
                    .iter()
                    .position(|theme| theme.name == self.theme.name)
                    .unwrap_or(0);
                self.theme_picker.open = true;
            }
            return Some(Action::None);
        }
        if self.theme_picker.open {
            return Some(self.handle_picker_key(key));
        }
        if self.config.keymap.matches("queue", key) {
            return Some(self.toggle_queue_popup());
        }
        if self.queue_popup_open {
            return Some(self.handle_queue_popup_key(key));
        }
        // Ctrl+P toggles the global launcher (rebindable via `[keymap]`).
        // Inert in the seed popup (it owns the composer's keys); Ctrl+Q/
        // Ctrl+C above stay untouched.
        if self.config.keymap.matches("launcher", key) {
            if self.launcher.is_some() {
                self.launcher = None; // toggle closed
                return Some(Action::None);
            }
            if self.composer.popup().is_some() {
                return Some(Action::None);
            }
            let needs_fetch = self.at_catalog_stale();
            self.launcher = Some(LauncherState::default());
            // First open with no cached skills: the run loop fetches
            // skill.list through the back-channel (loading line, like the
            // `@` menu).
            return Some(if needs_fetch {
                Action::RequestCatalog
            } else {
                Action::None
            });
        }
        if self.launcher.is_some() {
            return Some(self.handle_launcher_key(key));
        }
        // The new-session picker owns the keyboard while open (`n` opens it
        // from the chat/sidebar focus; Ctrl+Q/Ctrl+C above stay global).
        if self.new_session.is_some() {
            return Some(self.handle_new_session_key(key));
        }
        // The sidebar search popup owns the keyboard while open (`/` opens
        // it from the sidebar focus).
        if self.sidebar_search.is_some() {
            return Some(self.handle_sidebar_search_key(key));
        }
        // #19: the narrow-terminal drawer owns the keys while open — ↑/↓
        // navigate, Enter selects (and closes), Esc/s close.
        if self.drawer_open {
            return Some(self.handle_drawer_key(key));
        }
        // #19: `s` (rebindable via `[keymap] drawer-toggle`) toggles the
        // drawer in the drawer tier (<80 cols). Focus-gated so the
        // composer keeps typing `s`.
        if self.config.keymap.matches("drawer-toggle", key)
            && self.focus != Focus::Composer
            && self.terminal_width < 80
        {
            return Some(self.toggle_drawer());
        }
        // Ctrl+W arms the vim pane prefix; the next h/j/k/l moves focus
        // (Sidebar/Chat/Composer), any other key disarms it.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('w') {
            self.pane_prefix = true;
            return Some(Action::None);
        }
        if self.pane_prefix {
            self.pane_prefix = false;
            return match key.code {
                KeyCode::Char('h') => Some(self.move_focus(Focus::Sidebar)),
                KeyCode::Char('l') => Some(self.move_focus(Focus::Chat)),
                KeyCode::Char('j') => Some(self.move_focus(Focus::Composer)),
                KeyCode::Char('k') => Some(self.move_focus(Focus::Chat)),
                _ => Some(Action::None),
            };
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

    /// Move keyboard focus to a surface (Ctrl+W h/j/k/l pane navigation);
    /// a no-op when it's already there.
    fn move_focus(&mut self, focus: Focus) -> Action {
        if self.focus == focus {
            return Action::None;
        }
        self.focus = focus;
        Action::Focus(focus)
    }

    // -------------------------------------------------------------------
    // #19: the narrow-terminal drawer
    // -------------------------------------------------------------------

    /// `s`: toggle the drawer overlay (only meaningful in the drawer tier,
    /// <80 cols; the keymap check is focus-gated). Opening moves focus into
    /// the drawer; closing restores the prior focus.
    fn toggle_drawer(&mut self) -> Action {
        if self.drawer_open {
            self.close_drawer();
        } else {
            self.drawer_open = true;
            self.drawer_prior_focus = self.focus;
            // Save whatever hint is showing (an armed select-mode hint,
            // a queue hint, …) so closing the drawer restores it.
            self.drawer_prior_hint = self.hint.take();
            self.focus = Focus::Sidebar;
            // #30: the discoverability hint (`s sessions · esc close`)
            // shows on the FIRST open of the run, in the status line's
            // left cluster while the drawer is open — but only when it can
            // actually render: below 40 cols the tier rules hide the left
            // cluster, so the once-per-run flag must not burn invisibly at
            // 32–39 (a later open at ≥40 still shows it).
            if !self.drawer_hint_shown && self.terminal_width >= 40 {
                self.hint = Some(crate::i18n::tr(self.locale, "status.drawer_hint").into());
                self.drawer_hint_shown = true;
            }
        }
        Action::None
    }

    /// Close the drawer, restore the focus it took, and restore the hint
    /// that was showing before it opened (the select-mode hint survives an
    /// open/close round-trip).
    fn close_drawer(&mut self) {
        if self.drawer_open {
            self.drawer_open = false;
            self.focus = self.drawer_prior_focus;
            self.hint = self.drawer_prior_hint.take();
        }
    }

    /// The drawer's key handling while open: ↑/↓ navigate the session
    /// list, Enter selects (and closes), Esc/s close, everything else is
    /// inert (the global quit/cancel keys are handled before this).
    fn handle_drawer_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::KeyCode;
        let len = crate::ui::sidebar::SidebarGroup::visible_len(&self.sidebar_groups());
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.sidebar.move_by(1, len);
                Action::Select
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.sidebar.move_by(-1, len);
                Action::Select
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.sidebar.first();
                Action::Select
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.sidebar.last(len);
                Action::Select
            }
            KeyCode::Enter => {
                let action = self.switch_to_selected();
                self.close_drawer();
                action
            }
            KeyCode::Esc | KeyCode::Char('s') => {
                self.close_drawer();
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Ctrl+L: cycle the UI locale, persist it to the config, and force a
    /// full re-render (every cached row is localized).
    pub fn cycle_locale(&mut self) {
        self.locale = self.locale.next();
        self.config.locale = Some(match self.locale {
            crate::i18n::Locale::En => "en".into(),
            crate::i18n::Locale::Zh => "zh".into(),
        });
        if let Err(error) = self.config.save() {
            self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.config_save_failed",
                &[&error.to_string()],
            ));
        }
        self.row_cache.invalidate_all();
        self.set_toast(crate::i18n::trf(
            self.locale,
            "toast.locale",
            &[self.locale.native_name()],
        ));
    }

    /// Theme picker bindings: `Up`/`Down` (or `j`/`k`) move the selection,
    /// `Enter` applies live and persists the choice, `Esc` closes without
    /// applying. Everything else is inert while the picker is open.
    fn handle_picker_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.theme_picker.selected = self.theme_picker.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.theme_picker.selected + 1 < self.themes.themes.len() {
                    self.theme_picker.selected += 1;
                }
            }
            KeyCode::Enter => self.apply_picked_theme(),
            KeyCode::Esc => self.theme_picker.open = false,
            _ => {}
        }
        Action::None
    }

    /// Apply the picked theme live, persist it to the config file, and
    /// invalidate the row cache so the next draw re-renders with the new
    /// colors. A failed save only toasts — the applied theme stays.
    fn apply_picked_theme(&mut self) {
        let Some(theme) = self.themes.themes.get(self.theme_picker.selected) else {
            return;
        };
        self.theme = theme.clone();
        self.config.theme = Some(theme.name.clone());
        if let Err(error) = self.config.save() {
            self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.config_save_failed",
                &[&error.to_string()],
            ));
        }
        self.row_cache.invalidate_all();
        self.theme_picker.open = false;
    }

    /// Startup theme resolution: load user themes, then apply the persisted
    /// config theme when the terminal can render palettes (truecolor
    /// `COLORTERM`). `DSH_THEME=<name>` beats the persisted config (the
    /// tmux/herdr + testing escape hatch; an unknown name applies nothing —
    /// same contract as a bad config theme).
    ///
    /// With no explicit theme, the detected terminal/system light/dark
    /// scheme picks the default — `dsh-dark` (dark or detection failure)
    /// or `dsh-light`. The all-`Reset` terminal-following default only
    /// remains for non-truecolor terminals or an explicit `default` name
    /// (the pre-#11 look, opt-in).
    pub fn load_theme_config(&mut self) {
        self.themes.load_user_dir();
        self.config = Config::load();
        // `DSH_THEME` (when set and non-empty) beats config.toml; naming
        // the unregistered `default` keeps the Reset-based neutral.
        let explicit = std::env::var("DSH_THEME")
            .ok()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| self.config.theme.clone());
        if let Some(name) = &explicit
            && let Some(theme) = self.themes.find(name)
            && terminal_supports_color()
        {
            self.theme = theme.clone();
        } else if explicit.is_none() && terminal_supports_color() {
            // No explicit theme: pick by the detected scheme. A failed
            // detection on a truecolor terminal defaults to dsh-dark —
            // never the all-Reset theme (issue #11: OSC 11 unanswered
            // left the app monochrome).
            let name = match crate::theme::detect::detect_color_mode() {
                Some(crate::theme::detect::ColorMode::Light) => "dsh-light",
                Some(crate::theme::detect::ColorMode::Dark) | None => "dsh-dark",
            };
            if let Some(theme) = self.themes.find(name) {
                self.theme = theme.clone();
            }
        }
    }

    /// `Alt+q`: toggle the queue popup. Opening requires a non-empty queue
    /// on the active session (the strip is gone then anyway); otherwise a
    /// hint. The popup is view-only v1 (updateQueue actions are a later
    /// lane).
    fn toggle_queue_popup(&mut self) -> Action {
        if self.queue_popup_open {
            self.queue_popup_open = false;
            return Action::None;
        }
        if self.active_queue().is_empty() {
            self.hint = Some(crate::i18n::tr(self.locale, "hint.queue_empty").into());
            return Action::None;
        }
        self.queue_scroll = 0;
        self.queue_popup_open = true;
        Action::None
    }

    /// Queue popup bindings: `Up`/`Down` (or `j`/`k`) scroll, `Esc` closes
    /// (`Alt+q` toggles — handled before this). While the inline editor is
    /// open, nav keys are inert and typing edits the focused item's text
    /// (`Enter` commits, `Esc` cancels). On a `queued` item: `x` removes,
    /// `s` steers, `e` edits — all spawned via the back-channel; steering/
    /// context items are host-owned and inert. Further actions are inert
    /// while one is in flight (mirror the takeover's sending guard).
    fn handle_queue_popup_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::KeyCode;
        // Inline editor mode: typing edits, Enter commits, Esc cancels.
        if self.queue_editor.is_some() {
            match key.code {
                KeyCode::Char(c) => {
                    if let Some(editor) = &mut self.queue_editor {
                        editor.insert_char(c);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(editor) = &mut self.queue_editor {
                        editor.backspace();
                    }
                }
                KeyCode::Enter => {
                    let text = self
                        .queue_editor
                        .take()
                        .map(|mut e| e.take())
                        .unwrap_or_default();
                    if text.trim().is_empty() {
                        return Action::None; // empty edit: cancel (composer parity)
                    }
                    return Action::QueueEdit(text);
                }
                KeyCode::Esc => self.queue_editor = None,
                _ => {}
            }
            return Action::None;
        }
        let len = self.active_queue().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.queue_scroll = self.queue_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = len.saturating_sub(crate::ui::queue::QUEUE_POPUP_MAX_ROWS.min(len));
                self.queue_scroll = (self.queue_scroll + 1).min(max);
            }
            KeyCode::Char('x') if self.queue_focused_is_queued() && !self.queue_action_sending => {
                return Action::QueueRemove;
            }
            KeyCode::Char('s') if self.queue_focused_is_queued() && !self.queue_action_sending => {
                return Action::QueueSteer;
            }
            KeyCode::Char('e') if self.queue_focused_is_queued() && !self.queue_action_sending => {
                self.open_queue_editor();
            }
            KeyCode::Esc => {
                self.queue_popup_open = false;
                self.queue_editor = None;
            }
            _ => {}
        }
        Action::None
    }

    /// The composer-popup key handling: while the popup is open,
    /// `Up`/`Down` navigate it (clamped to the app's filtered entry list),
    /// `Enter` inserts the selected entry's text, `Esc` closes it. An `@`
    /// popup whose catalog is not cached yet returns
    /// [`Action::RequestCatalog`] so the run loop fetches `skill.list`.
    fn handle_popup_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::KeyCode;
        let entries = self.popup_entries();
        match key.code {
            KeyCode::Up => {
                self.composer.popup_move(-1, entries.len());
                Action::Input
            }
            KeyCode::Down => {
                self.composer.popup_move(1, entries.len());
                Action::Input
            }
            KeyCode::Enter => {
                let insert = entries
                    .get(self.composer.popup_selected())
                    .map(|entry| entry.insert.clone())
                    .unwrap_or_default();
                self.composer.popup_accept(&insert);
                Action::Input
            }
            KeyCode::Esc => {
                self.composer.popup_dismiss();
                Action::None
            }
            _ => Action::None,
        }
    }

    /// The filtered entry list for the open popup: `/` mirrors the core
    /// commands (substring-filtered by the typed suffix), `@` serves the
    /// cached skill.list result (empty while loading or failed).
    pub fn popup_entries(&self) -> Vec<crate::ui::catalog::CatalogEntry> {
        let Some(kind) = self.composer.popup() else {
            return Vec::new();
        };
        let suffix = &self.composer.buffer()[1..]; // after the trigger char
        let entries = match kind {
            crate::ui::composer::PopupKind::Slash => crate::ui::catalog::slash_entries(self.locale),
            crate::ui::composer::PopupKind::At => self
                .at_catalog
                .as_ref()
                .map(|catalog| crate::ui::catalog::skill_entries(&catalog.skills))
                .unwrap_or_default(),
        };
        crate::ui::catalog::filter_entries(&entries, suffix)
    }

    /// The skill catalog is not cached and no fetch is in flight.
    fn at_catalog_stale(&self) -> bool {
        match &self.at_catalog {
            None => true,
            Some(AtCatalog { loading, skills }) => !loading && skills.is_empty(),
        }
    }

    /// The `@` catalog needs fetching when the popup is open, the catalog
    /// is not cached, and no fetch is in flight.
    fn at_catalog_needs_fetch(&self) -> bool {
        matches!(
            self.composer.popup(),
            Some(crate::ui::composer::PopupKind::At)
        ) && self.at_catalog_stale()
    }

    /// The launcher's filtered entries — mirrored commands, cached skills,
    /// and settings actions — subsequence-ranked by the search text (pub
    /// for tests).
    pub fn launcher_entries_filtered(&self) -> Vec<crate::ui::launcher::LauncherEntry> {
        let Some(launcher) = &self.launcher else {
            return Vec::new();
        };
        let skills = self
            .at_catalog
            .as_ref()
            .map(|catalog| catalog.skills.as_slice())
            .unwrap_or(&[]);
        let entries = crate::ui::launcher::launcher_entries(self.locale, skills);
        crate::ui::launcher::fuzzy_filter(&entries, launcher.search.buffer())
    }

    /// Launcher keys: typing filters, Up/Down/j/k move, Enter picks, Esc
    /// closes. Picking a command/skill inserts it into the composer and
    /// submits through the prompt path — dispatches immediately, no
    /// leading-input state (the web's launcher semantics). Settings
    /// actions execute in place, mirroring their shortcuts.
    fn handle_launcher_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::KeyCode;
        let len = self.launcher_entries_filtered().len();
        match key.code {
            KeyCode::Esc => {
                self.launcher = None;
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(launcher) = &mut self.launcher {
                    launcher.selected = launcher.selected.saturating_sub(1);
                }
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(launcher) = &mut self.launcher {
                    launcher.selected = (launcher.selected + 1).min(len.saturating_sub(1));
                }
                Action::None
            }
            KeyCode::Enter => {
                let Some(launcher) = &self.launcher else {
                    return Action::None;
                };
                let Some(entry) = self
                    .launcher_entries_filtered()
                    .get(launcher.selected)
                    .cloned()
                else {
                    return Action::None;
                };
                self.launcher = None;
                match entry.action {
                    crate::ui::launcher::LauncherAction::Dispatch { text } => {
                        // Insert into the composer buffer, then submit
                        // through the prompt path (the gateway dispatches
                        // slash text via session.prompt).
                        self.composer.set_text(&text);
                        Action::Submit(self.composer.take())
                    }
                    crate::ui::launcher::LauncherAction::OpenSettings => {
                        self.mode = Mode::Settings(crate::ui::settings::SettingsState::new());
                        self.hint = None;
                        Action::FetchSettings
                    }
                    crate::ui::launcher::LauncherAction::OpenThemePicker => {
                        if self.theme_picker.open {
                            self.theme_picker.open = false;
                        } else {
                            self.theme_picker.selected = self
                                .themes
                                .themes
                                .iter()
                                .position(|theme| theme.name == self.theme.name)
                                .unwrap_or(0);
                            self.theme_picker.open = true;
                        }
                        Action::None
                    }
                    crate::ui::launcher::LauncherAction::CycleLocale => {
                        self.cycle_locale();
                        Action::None
                    }
                    crate::ui::launcher::LauncherAction::Quit => Action::Quit,
                }
            }
            KeyCode::Backspace => {
                if let Some(launcher) = &mut self.launcher {
                    launcher.search.backspace();
                    launcher.selected = 0;
                }
                Action::None
            }
            KeyCode::Char(c) => {
                if let Some(launcher) = &mut self.launcher {
                    launcher.search.insert_char(c);
                    launcher.selected = 0;
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Whether the focused popup item is queue-owned (actionable).
    fn queue_focused_is_queued(&self) -> bool {
        matches!(
            self.focused_queue_item().map(|item| item.placement),
            Some(crate::wire::events::QueuePlacement::Queued)
        )
    }

    /// The popup's focused item (the scroll-top row).
    pub fn focused_queue_item(&self) -> Option<&crate::wire::events::QueueItem> {
        self.active_queue().get(self.queue_scroll)
    }

    /// Open the inline editor seeded with the focused item's text content.
    fn open_queue_editor(&mut self) {
        let text = self
            .focused_queue_item()
            .map(|item| {
                item.message
                    .content
                    .iter()
                    .filter_map(|block| block.text())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let mut editor = Composer::new();
        for c in text.chars() {
            editor.insert_char(c);
        }
        self.queue_editor = Some(editor);
    }

    /// The active session's queue snapshot items (empty when no session, no
    /// snapshot, or an empty queue).
    pub fn active_queue(&self) -> &[crate::wire::events::QueueItem] {
        self.active_session
            .as_ref()
            .and_then(|session_id| self.store.session(session_id))
            .and_then(|state| state.queue.as_ref())
            .map(|queue| queue.items.as_slice())
            .unwrap_or(&[])
    }

    /// The sidebar's group model: workspace groups (in durable order) →
    /// ungrouped → archived (collapsed unless `archived_expanded` — the
    /// `e` toggle, app-lifetime state).
    pub fn sidebar_groups(&self) -> Vec<crate::ui::sidebar::SidebarGroup> {
        crate::ui::sidebar::build_groups(
            &self.sessions,
            &self.workspaces,
            &self.workspace_order,
            &self.archived_session_ids,
            self.locale,
            self.archived_expanded,
        )
    }

    /// Live sidebar updates from the host stream (Q2). Handled: session
    /// added (lands at the top — the list stays updatedAt-desc), removed
    /// (an active removal clears to the empty chat; no auto-switch v1),
    /// status (the running flag); workspace changed (upsert — membership
    /// derives from `workspace.session_ids`), removed (its sessions reflow
    /// to ungrouped), order-changed (the durable display order), and
    /// archived-sessions-changed (the archived set — those sessions drop
    /// out of navigation into the collapsed footer group). Ignored with a
    /// TODO: remote-event, agent-error (no v1 surface). The selection is
    /// re-clamped against the new group model.
    pub fn handle_host_frame(&mut self, frame: HostFrame) {
        match frame {
            HostFrame::HostSessionAdded {
                session_id,
                blank,
                parent_session_id,
                origin,
                cwd,
                agent_preset,
            } => {
                if self
                    .sessions
                    .iter()
                    .any(|summary| summary.session_id == session_id)
                {
                    return;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                self.sessions.insert(
                    0,
                    SessionSummary {
                        session_id,
                        updated_at: now,
                        running: false,
                        blank,
                        parent_session_id,
                        origin,
                        cwd,
                        agent_preset,
                        projections: None,
                    },
                );
            }
            HostFrame::HostSessionRemoved { session_id } => {
                self.sessions
                    .retain(|summary| summary.session_id != session_id);
                if self.active_session.as_ref() == Some(&session_id) {
                    // v1: no auto-switch — the chat goes empty.
                    self.active_session = None;
                    self.row_cache.invalidate_all();
                }
            }
            HostFrame::HostSessionStatus {
                session_id,
                running,
            } => {
                if let Some(summary) = self
                    .sessions
                    .iter_mut()
                    .find(|summary| summary.session_id == session_id)
                {
                    summary.running = running;
                }
            }
            HostFrame::HostWorkspaceChanged { workspace } => {
                if let Some(existing) = self
                    .workspaces
                    .iter_mut()
                    .find(|ws| ws.workspace_id == workspace.workspace_id)
                {
                    *existing = workspace;
                } else {
                    self.workspaces.push(workspace);
                }
            }
            HostFrame::HostWorkspaceRemoved { workspace_id } => {
                self.workspaces.retain(|ws| ws.workspace_id != workspace_id);
                self.workspace_order.retain(|id| id != &workspace_id);
                // The removed workspace's sessions reflow to ungrouped —
                // membership derives from workspace.session_ids, nothing
                // else to do.
            }
            HostFrame::HostWorkspaceOrderChanged { workspace_ids } => {
                self.workspace_order = workspace_ids;
            }
            HostFrame::HostArchivedSessionsChanged {
                archived_session_ids,
            } => {
                self.archived_session_ids = archived_session_ids;
            }
            // TODO(later lanes): remote events, agent errors.
            _ => {}
        }
        self.sidebar
            .clamp(crate::ui::sidebar::SidebarGroup::visible_len(
                &self.sidebar_groups(),
            ));
    }

    /// Takeover bindings (Q6/Q13). Approval: `y` allow once, `n`/`Esc`
    /// reject (blocking — there is no dismiss, the server waits for a
    /// response). Question: `Tab` cycles questions, `Up`/`Down`/`j`/`k`
    /// move the cursor, `Space` toggles (multi-select), `Enter` submits all
    /// answers; `Esc` is a no-op with a hint (no cancel in v1 — the server
    /// resolves eventually). Image viewer: `n`/`p` cycle, `t` fit/actual,
    /// `Esc`/`q` close (see ui::image_viewer).
    fn handle_takeover_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::KeyCode;
        // The viewer's close keys end the mode (needs `self.mode`, so this
        // arm can't bind the viewer like the arms below).
        if matches!(&self.mode, Mode::Image(_))
            && matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        {
            self.mode = Mode::Chat;
            return Action::None;
        }
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
                    self.hint = Some(crate::i18n::tr(self.locale, "hint.no_cancel").into());
                    Action::None
                }
                _ => Action::None,
            },
            Mode::Settings(_) => self.handle_settings_key(key),
            Mode::Image(viewer) => match key.code {
                KeyCode::Char('n') => {
                    viewer.next();
                    Action::None
                }
                KeyCode::Char('p') => {
                    viewer.prev();
                    Action::None
                }
                KeyCode::Char('t') => {
                    viewer.toggle_fit();
                    Action::None
                }
                _ => Action::None,
            },
            Mode::Chat => Action::None,
        }
    }

    /// Settings view bindings. Global: `Esc` closes (unsaved edits only
    /// toast a warning — the edits go with the view), `Ctrl+T` keeps the
    /// theme picker available (themes live there, not in this view),
    /// `Ctrl+S` saves the selected form's patch. Nav focus: `Up`/`Down`/
    /// `j`/`k` move the section; `Tab`/`Right` enter the form. Form focus:
    /// `Up`/`Down` move the field, `Enter`/`Space` edit (booleans toggle,
    /// enums cycle, strings/numbers open the inline editor — `Enter`
    /// commits, `Esc` cancels), `Tab`/`Left` return to the nav. Everything
    /// else is inert in v1.
    fn handle_settings_key(&mut self, key: KeyEvent) -> Action {
        use crate::ui::settings::{FieldKind, LineEditor, SettingsFocus, SettingsForm};
        use crossterm::event::{KeyCode, KeyModifiers};
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('t') {
            if self.theme_picker.open {
                self.theme_picker.open = false;
            } else {
                self.theme_picker.selected = self
                    .themes
                    .themes
                    .iter()
                    .position(|theme| theme.name == self.theme.name)
                    .unwrap_or(0);
                self.theme_picker.open = true;
            }
            return Action::None;
        }
        if self.theme_picker.open {
            return self.handle_picker_key(key);
        }
        let Mode::Settings(state) = &mut self.mode else {
            return Action::None;
        };
        // The inline editor swallows keys until Enter commits or Esc cancels.
        if let Some(form) = state.selected_form_mut()
            && form.editing.is_some()
        {
            let editor = form.editing.as_mut().expect("checked above");
            match key.code {
                KeyCode::Enter => {
                    if let Err(hint) = form.commit_edit() {
                        self.hint = Some(hint.into());
                    }
                }
                KeyCode::Esc => form.editing = None,
                KeyCode::Backspace => editor.backspace(),
                KeyCode::Left => editor.move_left(),
                KeyCode::Right => editor.move_right(),
                KeyCode::Home => editor.caret = 0,
                KeyCode::End => editor.caret = editor.buffer.len(),
                KeyCode::Char(c) if !control => editor.insert_char(c),
                _ => {}
            }
            return Action::Input;
        }
        match key.code {
            KeyCode::Esc => {
                let dirty = state.dirty();
                self.mode = Mode::Chat;
                if dirty {
                    self.set_toast(crate::i18n::tr(self.locale, "toast.settings_discarded"));
                }
                Action::None
            }
            KeyCode::Char('s') if control => {
                if state.saving {
                    return Action::None;
                }
                let dirty = state.selected_form().is_some_and(SettingsForm::dirty);
                if !dirty {
                    self.hint = Some(crate::i18n::tr(self.locale, "hint.nothing_to_save").into());
                    return Action::None;
                }
                state.saving = true;
                self.hint = Some(crate::i18n::tr(self.locale, "hint.saving").into());
                Action::SaveSettings
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Left => {
                state.focus = match state.focus {
                    SettingsFocus::Nav => SettingsFocus::Form,
                    SettingsFocus::Form => SettingsFocus::Nav,
                };
                Action::Input
            }
            KeyCode::Up | KeyCode::Char('k') => {
                match state.focus {
                    SettingsFocus::Nav => state.move_selection(-1),
                    SettingsFocus::Form => {
                        if let Some(form) = state.selected_form_mut() {
                            form.cursor = form.cursor.saturating_sub(1);
                        }
                    }
                }
                Action::Input
            }
            KeyCode::Down | KeyCode::Char('j') => {
                match state.focus {
                    SettingsFocus::Nav => state.move_selection(1),
                    SettingsFocus::Form => {
                        if let Some(form) = state.selected_form_mut() {
                            let last = form.fields.len().saturating_sub(1);
                            form.cursor = (form.cursor + 1).min(last);
                        }
                    }
                }
                Action::Input
            }
            KeyCode::Enter | KeyCode::Char(' ') if state.focus == SettingsFocus::Form => {
                let Some(form) = state.selected_form_mut() else {
                    return Action::None;
                };
                let Some(field) = form.fields.get_mut(form.cursor) else {
                    return Action::None;
                };
                match &field.kind {
                    FieldKind::Boolean => {
                        let on = field.value.as_bool().unwrap_or(false);
                        field.value = serde_json::Value::Bool(!on);
                    }
                    FieldKind::Choice(options) => {
                        let current = field.value.as_str().unwrap_or_default();
                        let next = options
                            .iter()
                            .position(|option| option == current)
                            .map(|index| (index + 1) % options.len())
                            .unwrap_or(0);
                        field.value = serde_json::Value::String(options[next].clone());
                    }
                    FieldKind::Text => {
                        let text = field.value.as_str().unwrap_or_default().to_string();
                        form.editing = Some(LineEditor::new(text));
                    }
                    FieldKind::Number => {
                        let text = match &field.value {
                            serde_json::Value::Null => String::new(),
                            other => other.to_string(),
                        };
                        form.editing = Some(LineEditor::new(text));
                    }
                    FieldKind::Raw => {
                        self.hint =
                            Some(crate::i18n::tr(self.locale, "hint.read_only_field").into())
                    }
                }
                Action::Input
            }
            _ => Action::None,
        }
    }

    /// Chat bindings: `q` quits; `j`/`Down` +1 row; `k`/`Up` -1 row;
    /// `g`/`Home` top; `G`/`End` bottom (follow on); `Ctrl+d`/`Ctrl+u`
    /// half page; `v` opens the image viewer on the session's images;
    /// `Esc` no-op.
    fn handle_chat_key(&mut self, key: KeyEvent) -> Option<Action> {
        use crossterm::event::{KeyCode, KeyModifiers};
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        // #12: `v` (rebindable via `[keymap] selection-toggle`) arms the
        // mouse selection mode; `i` (rebindable via `[keymap]
        // image-viewer`) opens the image viewer. Both are keymap-routed
        // with no hardcoded fallback: rebinding (or moving) the action
        // truly moves the key, and the popup/mode gates above already
        // route around the overlays.
        if self.config.keymap.matches("selection-toggle", key) {
            return Some(self.toggle_selection_mode());
        }
        if self.config.keymap.matches("image-viewer", key) {
            return Some(self.open_image_viewer());
        }
        match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('n') => {
                self.open_new_session_picker();
                Some(Action::None)
            }
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
            // #12: Esc cancels an armed selection (the status hint says
            // `v select · esc cancel`).
            KeyCode::Esc => {
                if self.select_mode {
                    self.cancel_selection();
                }
                Some(Action::None)
            }
            _ => None,
        }
    }

    // -------------------------------------------------------------------
    // #12: mouse support
    // -------------------------------------------------------------------

    /// `v`: arm (or disarm) the mouse selection mode. Arming shows the
    /// `v select · esc cancel` hint in the status line; a drag then
    /// selects, and mouse-up copies and exits.
    fn toggle_selection_mode(&mut self) -> Action {
        self.select_mode = !self.select_mode;
        if self.select_mode {
            self.hint = Some(crate::i18n::tr(self.locale, "status.select_hint").into());
        } else {
            self.selection = None;
            self.hint = None;
        }
        Action::None
    }

    /// Esc / a non-chat click: drop the selection state and the mode.
    fn cancel_selection(&mut self) {
        self.selection = None;
        self.select_mode = false;
        self.hint = None;
    }

    /// A mouse event (capture enabled at terminal setup). Popup-open and
    /// non-chat modes route everything to the popup: chat/sidebar/composer
    /// mouse and `v` are no-ops there.
    pub fn handle_mouse(&mut self, event: crossterm::event::MouseEvent) -> Action {
        use crossterm::event::{MouseButton, MouseEventKind};
        // #19: below 32 cols the too-small screen owns the terminal — the
        // stale wide-draw hit-test rects must not select or scroll.
        if self.terminal_width < crate::app::TOO_SMALL_WIDTH {
            return Action::None;
        }
        // Any overlay owns the surface: the theme picker, queue popup,
        // launcher, new-session/search popups, the composer's seed popup,
        // and the takeover/settings/image modes.
        let popup_open = self.theme_picker.open
            || self.queue_popup_open
            || self.launcher.is_some()
            || self.new_session.is_some()
            || self.sidebar_search.is_some()
            || self.composer.popup().is_some()
            || !matches!(self.mode, Mode::Chat);
        if popup_open {
            return Action::None;
        }
        let column = event.column;
        let row = event.row;
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(column, row),
            MouseEventKind::Drag(MouseButton::Left) => self.mouse_drag(column, row),
            MouseEventKind::Up(MouseButton::Left) => self.mouse_up(),
            MouseEventKind::ScrollUp => self.mouse_wheel(-1, column, row),
            MouseEventKind::ScrollDown => self.mouse_wheel(1, column, row),
            // Right/middle clicks, plain moves, and scroll-drag variants
            // are no-ops in v1 (Shift+wheel / Shift+drag stay the
            // terminal-level escape hatch).
            _ => Action::None,
        }
    }

    /// Whether `(column, row)` is inside `rect`.
    fn in_rect(rect: Rect, column: u16, row: u16) -> bool {
        column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
    }

    /// The chat content area (the 2/2 margin + the 1 blank top row the
    /// ChatView applies internally) — selection coordinates are relative
    /// to this rect.
    fn chat_content_rect(&self) -> Rect {
        Rect {
            x: self.chat_area.x + 2,
            y: self.chat_area.y + 1,
            width: self.chat_area.width.saturating_sub(4),
            height: self.chat_area.height.saturating_sub(1),
        }
    }

    /// Left-button down: sidebar row click / group-header toggle, composer
    /// click-to-cursor, or a selection anchor on the chat (select mode).
    /// #19: the drawer's `≡` affordance toggles it, and a click outside an
    /// open drawer closes it.
    fn mouse_down(&mut self, column: u16, row: u16) -> Action {
        // The `≡` affordance cell (the chat's top-left) toggles the drawer.
        if self.terminal_width < 80
            && self.terminal_width >= crate::app::TOO_SMALL_WIDTH
            && column == self.chat_area.x
            && row == self.chat_area.y
        {
            return self.toggle_drawer();
        }
        // Click-outside an open drawer closes it; the click then proceeds
        // normally (the drawer owns nothing else while open).
        if self.drawer_open && !Self::in_rect(self.sidebar_area, column, row) {
            self.close_drawer();
        }
        if Self::in_rect(self.sidebar_area, column, row) {
            self.cancel_selection();
            let action = self.sidebar_click(row);
            // A drawer session click selects AND closes (#19); the
            // permanent sidebar has no drawer to close.
            if self.drawer_open {
                self.close_drawer();
            }
            return action;
        }
        if Self::in_rect(self.composer_area, column, row) {
            self.cancel_selection();
            self.focus = Focus::Composer;
            self.composer_click(column, row);
            return Action::Focus(Focus::Composer);
        }
        // Chat: selection only while `v` mode is armed; otherwise a chat
        // click is a no-op (no click-to-position in v1).
        if self.select_mode {
            let content = self.chat_content_rect();
            if Self::in_rect(content, column, row) {
                let pos = CellPos {
                    row: row - content.y,
                    col: column - content.x,
                };
                self.selection = Some((pos, pos));
                self.hint = None;
                return Action::None;
            }
        }
        Action::None
    }

    /// Left-button drag: extend the selection, clamped to the chat rect
    /// (the queue/composer are never selected).
    fn mouse_drag(&mut self, column: u16, row: u16) -> Action {
        if self.selection.is_none() {
            return Action::None;
        }
        let content = self.chat_content_rect();
        let pos = CellPos {
            row: row
                .saturating_sub(content.y)
                .min(content.height.saturating_sub(1)),
            col: column
                .saturating_sub(content.x)
                .min(content.width.saturating_sub(1)),
        };
        if let Some((anchor, _)) = &mut self.selection {
            self.selection = Some((*anchor, pos));
        }
        Action::None
    }

    /// Left-button up: finish the selection — copy the selected text via
    /// OSC 52, flash `copied · N chars`, and exit select mode.
    fn mouse_up(&mut self) -> Action {
        let Some((anchor, current)) = self.selection else {
            return Action::None;
        };
        self.selection = None;
        let text = self.selected_text(anchor, current);
        if text.is_empty() {
            self.select_mode = false;
            self.hint = None;
            return Action::None;
        }
        let count = crate::clipboard::copy_text(&text);
        self.copied_flash = Some((
            crate::i18n::trf(self.locale, "status.copied", &[&count.to_string()]),
            Instant::now(),
        ));
        self.select_mode = false;
        self.hint = None;
        self.needs_draw = true;
        Action::None
    }

    /// Wheel: 3 lines per event. Over the chat it scrolls the viewport
    /// (and the selection follows the content); over the sidebar it scrolls
    /// the session list when it overflows; the status line and the hero
    /// are no-ops.
    fn mouse_wheel(&mut self, direction: i64, column: u16, row: u16) -> Action {
        // The sidebar/drawer is checked FIRST (like the click path): the
        // open drawer's inner rect sits inside `chat_area`, so the chat
        // branch must never win over it — wheel over the open drawer
        // scrolls the drawer's list, not the chat underneath.
        if Self::in_rect(self.sidebar_area, column, row) {
            self.sidebar_wheel(direction);
            return Action::None;
        }
        if Self::in_rect(self.chat_area, column, row) {
            // `scroll` clamps at the top/bottom bounds and disables follow.
            self.scroll(direction * 3);
            return Action::Scroll(direction * 3);
        }
        Action::None
    }

    /// Wheel over the sidebar: the list scrolls via the selection-driven
    /// window — moving the selection by 3 (clamped), only when the list
    /// actually overflows its window.
    fn sidebar_wheel(&mut self, direction: i64) {
        let groups = self.sidebar_groups();
        let inner_height = self.sidebar_area.height;
        if inner_height == 0 {
            return;
        }
        let (rows, _) =
            crate::ui::sidebar::display_layout(&groups, self.sidebar.selected, inner_height);
        // The visible window is `inner_height - 3` rows; scroll only when
        // the list is taller than it.
        if rows.len() <= inner_height.saturating_sub(3) as usize {
            return;
        }
        let len = crate::ui::sidebar::SidebarGroup::visible_len(&groups);
        self.sidebar.move_by((direction * 3) as isize, len);
    }

    /// A click on a sidebar row: select that session (switching when it is
    /// not already active). Clicking the active row is a no-op — a click
    /// never steals the composer's focus. A group header click toggles the
    /// group's collapse.
    fn sidebar_click(&mut self, row: u16) -> Action {
        let area = self.sidebar_area;
        if area.height == 0 || area.width == 0 {
            return Action::None;
        }
        let inner = area.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 0,
        });
        let line_index = row.saturating_sub(inner.y);
        // Header + blank row above; the footer line below — no rows there.
        if line_index < 2 || line_index >= inner.height.saturating_sub(1) {
            return Action::None;
        }
        let groups = self.sidebar_groups();
        let (rows, start) =
            crate::ui::sidebar::display_layout(&groups, self.sidebar.selected, inner.height);
        let Some(display) = rows.get(start + (line_index - 2) as usize) else {
            return Action::None;
        };
        match display {
            crate::ui::sidebar::DisplayRow::Header(group_index) => {
                // Only the archived group is collapsible in the v1 model —
                // its header click toggles the expansion (workspace and
                // ungrouped headers are inert).
                if groups[*group_index].is_archived {
                    self.archived_expanded = !self.archived_expanded;
                    self.sidebar
                        .clamp(crate::ui::sidebar::SidebarGroup::visible_len(&groups));
                }
                Action::None
            }
            crate::ui::sidebar::DisplayRow::Session { index, ordinal } => {
                self.sidebar.selected = *ordinal;
                self.switch_to_session(self.sessions[*index].session_id.clone())
            }
        }
    }

    /// A click in the composer's content area: place the caret at the
    /// clicked cell (clamped to the line end, honoring the horizontal
    /// scroll). The click already moved focus.
    fn composer_click(&mut self, column: u16, row: u16) {
        let area = self.composer_area;
        let inner = Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width.saturating_sub(4),
            height: area.height.saturating_sub(1),
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let content_row = row.saturating_sub(inner.y);
        let col_cells = column.saturating_sub(inner.x);
        // The caret's rendered column includes the current scroll; a click
        // at a visible cell maps to line space via it.
        let scroll = self.composer.caret_layout(inner.width).2;
        self.composer
            .click_to_cell(content_row as usize, col_cells + scroll);
    }

    /// The text inside the selected range: the cached (rendered) lines'
    /// content, per-line cell-clamped to the range (CJK-safe), trailing
    /// whitespace trimmed, lines joined with newlines. Content row `r`
    /// maps to cache line `view.offset + r` (ChatView renders 1:1).
    fn selected_text(&self, anchor: CellPos, current: CellPos) -> String {
        let (start, end) = if (anchor.row, anchor.col) <= (current.row, current.col) {
            (anchor, current)
        } else {
            (current, anchor)
        };
        let flat: Vec<&ratatui::text::Line> = self
            .row_cache
            .lines()
            .iter()
            .flat_map(|row| row.lines.iter())
            .collect();
        let mut out = Vec::new();
        for row in start.row..=end.row {
            let Some(line) = flat.get(self.view.offset + row as usize) else {
                continue; // past the conversation's tail
            };
            let (col_start, col_end) = if row == start.row && row == end.row {
                (start.col.min(end.col), start.col.max(end.col))
            } else if row == start.row {
                (start.col, u16::MAX)
            } else if row == end.row {
                (0, end.col)
            } else {
                (0, u16::MAX)
            };
            // Collect (char, start-cell) pairs, then slice by cell range.
            let mut cells = 0u16;
            let mut text = String::new();
            'chars: for span in &line.spans {
                for ch in span.content.chars() {
                    let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                    if cells >= col_end {
                        break 'chars;
                    }
                    if cells >= col_start {
                        text.push(ch);
                    }
                    cells += w;
                }
            }
            out.push(text.trim_end().to_string());
        }
        out.join("\n")
    }

    /// Paste: insert into the composer only while it is focused (and no
    /// popup owns the keyboard); everywhere else the payload is dropped.
    pub fn handle_paste(&mut self, text: String) -> Action {
        // #19: below 32 cols the invisible composer must not receive
        // paste either.
        if self.terminal_width < crate::app::TOO_SMALL_WIDTH {
            return Action::None;
        }
        if self.focus != Focus::Composer
            || self.theme_picker.open
            || self.queue_popup_open
            || self.launcher.is_some()
            || self.new_session.is_some()
            || self.sidebar_search.is_some()
            || !matches!(self.mode, Mode::Chat)
        {
            return Action::None;
        }
        for ch in text.chars() {
            self.composer.insert_char(ch);
        }
        Action::None
    }

    /// Composer bindings: chars edit the buffer; `Enter` submits,
    /// `Shift+Enter` inserts a newline (web parity, Q14); arrows/Home/End
    /// move the caret; `Ctrl+D` quits (shell EOF convention — in chat focus
    /// it keeps the vim-style scroll-half-page binding); `Esc` returns
    /// focus to the chat. Every binding is rebindable via `[keymap]` in
    /// config.toml (see [`crate::theme::Keymap`]). While a seed popup is
    /// open, `Up`/`Down` navigate it, `Enter` accepts, `Esc` closes it.
    fn handle_composer_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::{KeyCode, KeyModifiers};
        let control = key.modifiers.contains(KeyModifiers::CONTROL);

        // `quit-eof` is checked first so it works even with the seed popup
        // open, mirroring the global quit bindings.
        if self.config.keymap.matches("composer.quit-eof", key) {
            return Action::Quit;
        }

        if self.composer.popup().is_some() {
            // Esc always dismisses the popup — never hijacked by the
            // catalog fetch (a user closing the popup must not RPC).
            if key.code == KeyCode::Esc {
                self.composer.popup_dismiss();
                return Action::None;
            }
            if self.at_catalog_needs_fetch() {
                return Action::RequestCatalog;
            }
            return self.handle_popup_key(key);
        }

        if self.config.keymap.matches("composer.focus-chat", key) {
            self.focus = Focus::Chat;
            return Action::Focus(Focus::Chat);
        }
        if self.config.keymap.matches("composer.newline", key) {
            self.composer.newline();
            return Action::Input;
        }
        if self.config.keymap.matches("composer.submit", key) {
            return self.submit();
        }
        if self.config.keymap.matches("composer.backspace", key) {
            self.composer.backspace();
            return Action::Input;
        }
        if self.config.keymap.matches("composer.delete", key) {
            self.composer.delete();
            return Action::Input;
        }
        if self.config.keymap.matches("composer.left", key) {
            self.composer.move_left();
            return Action::Input;
        }
        if self.config.keymap.matches("composer.right", key) {
            self.composer.move_right();
            return Action::Input;
        }
        if self.config.keymap.matches("composer.home", key) {
            self.composer.move_home();
            return Action::Input;
        }
        if self.config.keymap.matches("composer.end", key) {
            self.composer.move_end();
            return Action::Input;
        }
        if self.config.keymap.matches("composer.up", key) {
            self.composer.move_up();
            return Action::Input;
        }
        if self.config.keymap.matches("composer.down", key) {
            self.composer.move_down();
            return Action::Input;
        }
        match key.code {
            KeyCode::Char(c) if !control => {
                self.composer.insert_char(c);
                Action::Input
            }
            _ => Action::None,
        }
    }

    /// Sidebar bindings: `j`/`k`/arrows move the selection, `g`/`G` (and
    /// Home/End) jump to the ends, `Enter` switches the active session,
    /// `/` opens the search popup, `e` toggles the archived group's
    /// expansion (app-lifetime state), `Esc` returns focus to the chat,
    /// `q` quits. The selection moves in session-space: group headers are
    /// skipped and collapsed (archived) sessions are unreachable — the
    /// expanded archived group's rows are reachable like any other.
    fn handle_sidebar_key(&mut self, key: KeyEvent) -> Option<Action> {
        use crossterm::event::KeyCode;
        // Inline rename editor (`r`): typing edits, Enter commits, Esc
        // cancels, and every other key is inert while it's open (mirrors
        // the queue editor).
        if self.rename_editor.is_some() {
            match key.code {
                KeyCode::Char(c) => {
                    if let Some((_, editor)) = &mut self.rename_editor {
                        editor.insert_char(c);
                    }
                }
                KeyCode::Backspace => {
                    if let Some((_, editor)) = &mut self.rename_editor {
                        editor.backspace();
                    }
                }
                KeyCode::Enter => {
                    let Some((session_id, mut editor)) = self.rename_editor.take() else {
                        return Some(Action::None);
                    };
                    let title = editor.take();
                    if title.trim().is_empty() {
                        return Some(Action::None); // empty rename: cancel (queue parity)
                    }
                    return Some(Action::RenameSession { session_id, title });
                }
                KeyCode::Esc => self.rename_editor = None,
                _ => {}
            }
            return Some(Action::None);
        }
        let len = crate::ui::sidebar::SidebarGroup::visible_len(&self.sidebar_groups());
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
            KeyCode::Char('n') => {
                self.open_new_session_picker();
                Some(Action::None)
            }
            // `/` opens the sidebar search popup (read-only — no action
            // guard; the search POSTs spawn through the back-channel).
            KeyCode::Char('/') => {
                self.sidebar_search = Some(SidebarSearchState::default());
                Some(Action::None)
            }
            // `e` toggles the archived group's expansion; the selection
            // re-clamps against the new visible row count (expanding makes
            // the archived rows reachable, collapsing drops them again).
            KeyCode::Char('e') => {
                self.archived_expanded = !self.archived_expanded;
                self.sidebar
                    .clamp(crate::ui::sidebar::SidebarGroup::visible_len(
                        &self.sidebar_groups(),
                    ));
                Some(Action::None)
            }
            // Sidebar session actions (rename/fork/archive): require a
            // visible selection and one in-flight action at a time.
            KeyCode::Char('r') => {
                if self.sidebar_action_sending {
                    return Some(Action::None);
                }
                self.open_rename_editor();
                Some(Action::None)
            }
            KeyCode::Char('f') => {
                if self.sidebar_action_sending {
                    return Some(Action::None);
                }
                let Some(index) = crate::ui::sidebar::SidebarGroup::visible_session(
                    &self.sidebar_groups(),
                    self.sidebar.selected,
                ) else {
                    return Some(Action::None);
                };
                Some(Action::ForkSession(self.sessions[index].session_id.clone()))
            }
            KeyCode::Char('a') => {
                if self.sidebar_action_sending {
                    return Some(Action::None);
                }
                let Some(index) = crate::ui::sidebar::SidebarGroup::visible_session(
                    &self.sidebar_groups(),
                    self.sidebar.selected,
                ) else {
                    return Some(Action::None);
                };
                Some(Action::ArchiveSession(
                    self.sessions[index].session_id.clone(),
                ))
            }
            KeyCode::Esc => {
                self.focus = Focus::Chat;
                Some(Action::Focus(Focus::Chat))
            }
            _ => None,
        }
    }

    /// Open the inline rename editor for the selected sidebar session,
    /// seeded with its displayed title (the `title` projection, falling
    /// back to the session id — the sidebar's label rule).
    fn open_rename_editor(&mut self) {
        let Some(index) = crate::ui::sidebar::SidebarGroup::visible_session(
            &self.sidebar_groups(),
            self.sidebar.selected,
        ) else {
            return;
        };
        let summary = &self.sessions[index];
        let title = summary
            .projections
            .as_ref()
            .and_then(|block| block.values.get("title"))
            .and_then(|value| {
                value
                    .as_str()
                    .or_else(|| value.get("title").and_then(|v| v.as_str()))
            })
            .unwrap_or(&summary.session_id.0);
        let mut editor = Composer::new();
        for c in title.chars() {
            editor.insert_char(c);
        }
        self.rename_editor = Some((summary.session_id.clone(), editor));
    }

    /// The picker's entries: workspaces in display order (the durable
    /// `workspace_order` first, unlisted workspaces appended — the
    /// sidebar's rule) plus the trailing "no workspace" entry.
    pub fn new_session_entries(&self) -> Vec<crate::ui::new_session::NewSessionEntry> {
        let mut ordered: Vec<&crate::wire::workspace::WorkspaceView> = self
            .workspace_order
            .iter()
            .filter_map(|id| self.workspaces.iter().find(|ws| &ws.workspace_id == id))
            .collect();
        ordered.extend(
            self.workspaces
                .iter()
                .filter(|ws| !self.workspace_order.contains(&ws.workspace_id)),
        );
        let mut entries: Vec<_> = ordered
            .into_iter()
            .map(|ws| crate::ui::new_session::NewSessionEntry {
                workspace_id: Some(ws.workspace_id.clone()),
                label: ws.title.clone(),
            })
            .collect();
        entries.push(crate::ui::new_session::NewSessionEntry {
            workspace_id: None,
            label: crate::i18n::tr(self.locale, "create.no_workspace").into(),
        });
        entries
    }

    /// `n` in the chat or sidebar: open the new-session picker.
    fn open_new_session_picker(&mut self) {
        self.new_session = Some(NewSessionState::default());
    }

    /// Picker bindings: `Up`/`Down` (or `j`/`k`) move the workspace
    /// selection, `Enter` creates with the highlighted entry (inert while
    /// a create is in flight), `Esc` closes; everything else is inert.
    fn handle_new_session_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::KeyCode;
        let entries = self.new_session_entries();
        let Some(state) = &mut self.new_session else {
            return Action::None;
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                state.selected = state.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.selected = (state.selected + 1).min(entries.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                if state.sending {
                    return Action::None;
                }
                let Some(entry) = entries.get(state.selected) else {
                    return Action::None;
                };
                return Action::CreateSession {
                    workspace_id: entry.workspace_id.clone(),
                };
            }
            KeyCode::Esc => self.new_session = None,
            _ => {}
        }
        Action::None
    }

    /// Search popup bindings: typing edits the query (and searches — the
    /// action returns [`Action::SearchSessions`] for the run loop to
    /// spawn, or `None` once the query is empty); `j`/`k`/arrows move the
    /// result selection (clamped); `Enter` switches to the highlighted
    /// result and closes the popup; `Esc` closes and restores the full
    /// grouped list. Everything else is inert while the popup is open.
    fn handle_sidebar_search_key(&mut self, key: KeyEvent) -> Action {
        use crossterm::event::KeyCode;
        let Some(state) = &mut self.sidebar_search else {
            return Action::None;
        };
        match key.code {
            KeyCode::Backspace => {
                state.query.backspace();
                state.results.clear();
                state.selected = 0;
                if state.query.buffer().is_empty() {
                    return Action::None;
                }
                Action::SearchSessions(state.query.buffer().to_string())
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.selected = state.selected.saturating_sub(1);
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let last = state.results.len().saturating_sub(1);
                state.selected = (state.selected + 1).min(last);
                Action::None
            }
            KeyCode::Enter => {
                let Some(result) = state.results.get(state.selected) else {
                    return Action::None;
                };
                let session_id = result.session_id.clone();
                self.sidebar_search = None; // switching leaves the popup
                self.switch_to_session(session_id)
            }
            KeyCode::Esc => {
                self.sidebar_search = None;
                Action::None
            }
            KeyCode::Char(c) => {
                state.query.insert_char(c);
                state.results.clear();
                state.selected = 0;
                if state.query.buffer().is_empty() {
                    return Action::None;
                }
                Action::SearchSessions(state.query.buffer().to_string())
            }
            _ => Action::None,
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
            self.hint = Some(crate::i18n::tr(self.locale, "hint.turn_running").into());
            return Action::None;
        }
        self.hint = None;
        Action::Submit(self.composer.take())
    }

    /// Enter in the sidebar: switch the active session to the selected row —
    /// open its store state, drop the row cache (node keys collide across
    /// sessions), and reset the viewport to the bottom (follow). The run
    /// loop spawns the history fetch (Q9) via [`Action::SwitchSession`].
    /// Pending approvals/questions stay global across switches (blocking
    /// frames are not session-scoped in v1).
    fn switch_to_selected(&mut self) -> Action {
        let groups = self.sidebar_groups();
        let Some(index) =
            crate::ui::sidebar::SidebarGroup::visible_session(&groups, self.sidebar.selected)
        else {
            return Action::None;
        };
        self.switch_to_session(self.sessions[index].session_id.clone())
    }

    /// The shared switch path (sidebar Enter and the search popup's Enter):
    /// same store/cache/viewport semantics as [`App::switch_to_selected`].
    fn switch_to_session(&mut self, session_id: SessionId) -> Action {
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

    /// `v` in the chat: open the image viewer on the active session's image
    /// blocks (display order; `n`/`p` cycle with wrap-around). The chat has
    /// no per-row focus, so the viewer starts at the first image-bearing row
    /// at/after the viewport top, else the last image in the session. No
    /// images (or no session): a status hint, no mode change.
    /// The `session.attachment` fetch finished: on success decode the
    /// base64 payload into [`ImageCache`] (the inline placeholder upgrades
    /// to a real image on the next draw — the row cache must be
    /// invalidated so the caption+filler rows re-render); on failure toast
    /// and stay uncached (the next render encounter retries). The pending
    /// guard is cleared either way.
    fn on_attachment_done(
        &mut self,
        attachment_id: AttachmentId,
        result: Result<crate::wire::session::SessionAttachmentValue, ClientError>,
    ) {
        self.pending_attachments.remove(&attachment_id);
        let bytes = match result {
            Ok(value) => {
                use base64::Engine;
                match base64::engine::general_purpose::STANDARD.decode(&value.data) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        self.set_toast(crate::i18n::trf(
                            self.locale,
                            "toast.attachment_failed",
                            &[&error.to_string()],
                        ));
                        return;
                    }
                }
            }
            Err(error) => {
                self.set_toast(crate::i18n::trf(
                    self.locale,
                    "toast.attachment_failed",
                    &[&error.to_string()],
                ));
                return;
            }
        };
        let Some(picker) = &self.image_picker else {
            // No graphics tier (keyless/tests, TERM unset): captions are the
            // tier; the bytes are dropped rather than cached.
            return;
        };
        match self.image_cache.insert(picker, attachment_id, &bytes) {
            Ok(()) => {
                // The inline filler rows now exist: every cached row that
                // touches this attachment must re-render.
                self.row_cache.invalidate_all();
            }
            Err(error) => {
                self.set_toast(crate::i18n::trf(
                    self.locale,
                    "toast.attachment_failed",
                    &[&error.to_string()],
                ));
            }
        }
    }

    /// The attachment ids the render would show as caption-only placeholders
    /// right now: every image block in the active session's store whose
    /// cache key has no bytes and whose fetch is not already in flight.
    /// Empty without a graphics tier (nothing would render anyway) or
    /// without an active session.
    pub fn attachment_needs(&self) -> Vec<AttachmentId> {
        if self.image_picker.is_none() {
            return Vec::new();
        }
        let Some(session_id) = &self.active_session else {
            return Vec::new();
        };
        let mut needed = Vec::new();
        for (_, images) in session_image_blocks(&self.store, session_id) {
            for attachment in images {
                if self.image_cache.get(&attachment.attachment_id).is_some() {
                    continue; // cached: the inline tier already renders
                }
                if self.pending_attachments.contains(&attachment.attachment_id) {
                    continue; // in flight: the done-event will populate it
                }
                needed.push(attachment.attachment_id);
            }
        }
        needed
    }

    fn open_image_viewer(&mut self) -> Action {
        let Some(session_id) = self.active_session.clone() else {
            self.hint = Some(crate::i18n::tr(self.locale, "hint.no_images").into());
            return Action::None;
        };
        let by_node = session_image_blocks(&self.store, &session_id);
        let total: usize = by_node.iter().map(|(_, images)| images.len()).sum();
        if total == 0 {
            self.hint = Some(crate::i18n::tr(self.locale, "hint.no_images").into());
            return Action::None;
        }
        // Node key → the ordinal of its first image in the flat cycle list.
        let mut base = 0usize;
        let mut starts: HashMap<&str, usize> = HashMap::new();
        for (key, images) in &by_node {
            starts.insert(key.as_str(), base);
            base += images.len();
        }
        let rows = self.row_cache.lines();
        let mut start = if rows.is_empty() { 0 } else { total - 1 };
        // The viewport offset is line-space; map it to the row at the
        // viewport top so the viewer starts at the first image at/after it.
        let (row_at_top, _) = self.row_cache.line_to_row(self.view.offset);
        for row in rows.iter().skip(row_at_top) {
            if let Some(first) = starts.get(row.node_key.as_str()) {
                start = *first;
                break;
            }
        }
        let images = by_node.into_iter().flat_map(|(_, images)| images).collect();
        self.mode = Mode::Image(crate::ui::image_viewer::ImageViewer::new(
            session_id, images, start,
        ));
        Action::None
    }

    /// Whether the active session has a turn in flight: the summary's
    /// `running` flag, or the node fold — an unsettled tail (see
    /// [`crate::store::SessionState::has_unsettled_tail`]).
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
        state.has_unsettled_tail()
    }

    /// Apply a signed scroll delta; manual scrolling turns follow off.
    ///
    /// Scrolling down is BOTTOM-LOCKED in line space: the offset never
    /// passes `total - viewport_height`, the same anchor follow-mode uses
    /// (run.rs draw: `viewport_height` is the chat pane height). Without
    /// the clamp, ↓ could run the offset past the last content line —
    /// `line_to_row` then pins the START at the final line, rendering the
    /// tail at the TOP of the chat with a blank void below (v1 blocker).
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
        let total: usize = self
            .row_cache
            .lines()
            .iter()
            .map(|row| row.lines.len())
            .sum();
        let max = total.saturating_sub(self.view.viewport_height as usize);
        self.view.offset = self.view.offset.min(max);
    }
}

/// The session's image blocks in display order, grouped by node: user
/// message content and tool-result content carry images (nested tool results
/// recurse). Drives the viewer's `n`/`p` cycle list.
fn session_image_blocks(
    store: &SessionStore,
    session_id: &SessionId,
) -> Vec<(String, Vec<crate::wire::session::ImageAttachmentRef>)> {
    fn collect(
        blocks: &[crate::store::event_data::ContentBlock],
        out: &mut Vec<crate::wire::session::ImageAttachmentRef>,
    ) {
        for block in blocks {
            match block {
                crate::store::event_data::ContentBlock::Image { attachment } => {
                    out.push(attachment.clone());
                }
                crate::store::event_data::ContentBlock::ToolResult { content, .. } => {
                    collect(content, out);
                }
                _ => {}
            }
        }
    }
    let Some(state) = store.session(session_id) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for node in &state.nodes {
        let mut images = Vec::new();
        match &node.data {
            NodeData::User { content, .. } => collect(content, &mut images),
            NodeData::Tool {
                result: Some(result_node),
                ..
            } => collect(&result_node.content, &mut images),
            _ => {}
        }
        if !images.is_empty() {
            result.push((node.key.clone(), images));
        }
    }
    result
}

/// Coalesced draw interval (Q3).
pub(crate) const DRAW_INTERVAL: Duration = Duration::from_millis(16);

/// Toast lifetime: cleared on the first tick at least this long after it
/// was set.
pub(crate) const TOAST_TTL: Duration = Duration::from_secs(3);

/// The `copied · N chars` status flash lifetime (#12).
pub(crate) const COPY_FLASH_TTL: Duration = Duration::from_secs(2);

/// #19: below this width the full-screen "terminal too small" screen takes
/// over (only `q` quits; a resize back restores the prior screen live).
pub(crate) const TOO_SMALL_WIDTH: u16 = 32;

/// Toast text for a remotely resolved approval (no exclusivity, Q10).
fn remote_approval_text(outcome: ApprovalOutcome, locale: crate::i18n::Locale) -> String {
    match outcome {
        ApprovalOutcome::AllowedOnce => crate::i18n::tr(locale, "toast.approved_remote").into(),
        ApprovalOutcome::Rejected => crate::i18n::tr(locale, "toast.rejected_remote").into(),
        ApprovalOutcome::Cancelled => crate::i18n::tr(locale, "toast.approval_cancelled").into(),
        ApprovalOutcome::Unavailable => {
            crate::i18n::tr(locale, "toast.approval_unavailable").into()
        }
    }
}

/// Toast text for a remotely resolved question.
fn remote_question_text(outcome: QuestionOutcome, locale: crate::i18n::Locale) -> String {
    match outcome {
        QuestionOutcome::Answered => crate::i18n::tr(locale, "toast.answered_remote").into(),
        QuestionOutcome::Cancelled => crate::i18n::tr(locale, "toast.question_cancelled").into(),
    }
}
