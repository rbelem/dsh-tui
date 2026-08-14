//! The main loop and draw (Q3): single-threaded store + draw, coalesced at
//! ~16ms, plus the raw-mode/alternate-screen lifecycle.
//!
//! Layout (the first surface lane): a full-height sidebar on the left
//! (hidden below 60 columns), and a right column stacking the chat (fill),
//! the composer (one top rule; height tracks the buffer, capped at 8 rows),
//! and the one-line status. Each seam is a single divider — no boxed panes.
//! An approval/question takeover (Q6) replaces the whole layout.

use std::time::Instant;

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use crate::app::event::{AnswerTag, AppEvent, EventChannel};
use crate::app::{Action, App, AppError, DRAW_INTERVAL, Focus};
use crate::client::ClientError;
use crate::render::chat_view::ChatView;
use crate::theme::Theme;
use crate::theme::ThemePopup;
use crate::ui::composer::{ComposerView, SeedPopup};
use crate::ui::sidebar::{SidebarView, sidebar_width};
use crate::ui::style;
use crate::ui::takeover::{ApprovalView, Mode, QuestionView};
use crate::wire::approvals::ApprovalResponseOutcome;
use crate::wire::questions::{AskUserQuestionAnswer, QuestionAnswerItem};
use crate::wire::rpc::{RpcReceipt, RpcReceiptReason};
use crate::wire::session::{PromptContentPart, PromptMode};

impl App {
    /// The main loop. Events arrive over one channel; a 16ms interval drives
    /// coalesced draws (Q3). Returns when a quit key is handled or every
    /// bridge closes.
    ///
    /// Draw policy: terminal events (Key/Resize) draw immediately; frame
    /// changes draw at most once per [`DRAW_INTERVAL`] (the tick drives the
    /// next draw); the first draw happens as soon as anything changes.
    pub async fn run<B>(
        &mut self,
        term: &mut Terminal<B>,
        events: &mut EventChannel,
    ) -> Result<(), AppError>
    where
        B: Backend,
        B::Error: Into<AppError>,
    {
        // Spawned back-channel tasks (answers, prompts) send their results
        // through this sender; the loop reads them from `events.rx`.
        let event_tx = events.tx.clone();
        let mut tick = tokio::time::interval(DRAW_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                maybe = events.rx.recv() => {
                    match maybe {
                        Some(AppEvent::Key(key)) => {
                            match self.handle_key(key) {
                                Some(Action::Quit) => {
                                    self.running = false;
                                    break;
                                }
                                Some(Action::Submit(text)) => {
                                    self.dispatch_prompt(text, event_tx.clone())
                                }
                                // Spawned: the loop keeps pumping while the
                                // respond POST is in flight.
                                Some(Action::AnswerApproval(outcome)) => {
                                    self.answer_approval(outcome, event_tx.clone())
                                }
                                Some(Action::AnswerQuestion) => {
                                    self.answer_question(event_tx.clone())
                                }
                                _ => {}
                            }
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::AnswerDone { tag, result }) => {
                            self.on_answer_done(tag, result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::PromptDone { result }) => {
                            if let Err(error) = result {
                                self.set_toast(format!("prompt failed: {error}"));
                            }
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::Frame(frame)) => {
                            self.record_resolved(&frame);
                            match self.store.ingest(frame) {
                                Ok(()) => {}
                                Err(error) => self.last_error = Some(error.to_string()),
                            }
                            self.needs_draw = true;
                            self.draw_if_due(term, false)?;
                        }
                        Some(AppEvent::Answerable { rpc_id, frame }) => {
                            self.record_answerable(rpc_id, &frame);
                            // The store ignores answerable frames; the
                            // takeover they open draws immediately.
                            match self.store.ingest(frame) {
                                Ok(()) => {}
                                Err(error) => self.last_error = Some(error.to_string()),
                            }
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::Resize(_width, height)) => {
                            // Q10: width change → full re-render.
                            self.view.viewport_height = height;
                            self.row_cache.invalidate_all();
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::Tick) => {
                            self.expire_toast();
                            self.draw_if_due(term, false)?;
                        }
                        None => break,
                    }
                }
                _ = tick.tick() => {
                    self.expire_toast();
                    self.draw_if_due(term, false)?;
                }
            }
        }
        Ok(())
    }

    /// Draw when due: terminal events immediately; otherwise at most once per
    /// [`DRAW_INTERVAL`] since the last draw.
    fn draw_if_due<B>(&mut self, term: &mut Terminal<B>, immediate: bool) -> Result<(), AppError>
    where
        B: Backend,
        B::Error: Into<AppError>,
    {
        if !self.needs_draw && !immediate {
            return Ok(());
        }
        let due = immediate
            || self
                .last_draw
                .is_none_or(|last| last.elapsed() >= DRAW_INTERVAL);
        if due {
            self.draw(term).map_err(Into::into)?;
        }
        Ok(())
    }

    /// Sync the row cache, apply follow, and render the three surfaces:
    /// sidebar | chat over composer over the status line. A takeover (Q6)
    /// replaces the whole layout.
    fn draw<B>(&mut self, term: &mut Terminal<B>) -> Result<(), B::Error>
    where
        B: Backend,
    {
        // Full-screen takeover: the chat surfaces stay live underneath
        // (frames keep folding into the store) but are not drawn.
        if !matches!(self.mode, Mode::Chat) {
            term.draw(|frame| {
                let area = frame.area();
                let notice = self.current_notice();
                match &self.mode {
                    Mode::Approval(takeover) => frame.render_widget(
                        ApprovalView {
                            takeover,
                            notice,
                            theme: &self.theme,
                        },
                        area,
                    ),
                    Mode::Question(takeover) => frame.render_widget(
                        QuestionView {
                            takeover,
                            notice,
                            theme: &self.theme,
                        },
                        area,
                    ),
                    Mode::Chat => {}
                }
            })?;
            self.last_draw = Some(Instant::now());
            self.needs_draw = false;
            self.draws += 1;
            return Ok(());
        }

        let size = term.size()?;
        let full = Rect::new(0, 0, size.width, size.height);
        let sidebar_width = sidebar_width(size.width);
        let [sidebar_area, right] =
            Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Fill(1)])
                .areas(full);
        let composer_height = (self.composer.line_count() as u16 + 1).clamp(2, 8);
        let [chat_area, composer_area, status_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .areas(right);

        self.view.viewport_height = chat_area.height;
        let chat_height = chat_area.height;
        let width = chat_area.width;

        self.sidebar.clamp(self.sessions.len());
        let session_id = self.active_session.clone();
        if let Some(session_id) = &session_id {
            self.row_cache
                .sync(&self.store, session_id, width, &self.theme);
            self.row_cache
                .render_dirty(&self.store, session_id, width, &self.theme);
            if self.view.follow {
                let total = self.row_cache.lines().len();
                self.view.offset = total.saturating_sub(chat_height as usize);
            }
        }
        let status = self.status_line(&self.theme);
        let offset = self.view.offset;

        term.draw(|frame| {
            if sidebar_width > 0 {
                frame.render_widget(
                    SidebarView {
                        sessions: &self.sessions,
                        active: self.active_session.as_ref(),
                        selected: self.sidebar.selected,
                        focused: self.focus == Focus::Sidebar,
                        theme: &self.theme,
                    },
                    sidebar_area,
                );
            }
            if let Some(session_id) = &session_id {
                frame.render_widget(
                    ChatView {
                        store: &self.store,
                        session_id,
                        offset,
                        row_cache: &mut self.row_cache,
                    },
                    chat_area,
                );
            }
            frame.render_widget(
                ComposerView {
                    composer: &self.composer,
                    focused: self.focus == Focus::Composer,
                    theme: &self.theme,
                },
                composer_area,
            );
            frame.render_widget(Paragraph::new(status), status_area);

            // The real terminal cursor marks the focused composer.
            if self.focus == Focus::Composer {
                let inner = Rect {
                    x: composer_area.x,
                    y: composer_area.y + 1,
                    width: composer_area.width,
                    height: composer_area.height.saturating_sub(1),
                };
                let (row, col, _) = self.composer.caret_layout(inner.width);
                let y = (inner.y + row).min(inner.bottom().saturating_sub(1));
                frame.set_cursor_position((inner.x + col, y));
            }

            // The theme picker floats above the composer (mirrors the seed
            // popup placement; the theme registry list is centered).
            if self.theme_picker.open {
                let popup = ThemePopup {
                    themes: &self.themes.themes,
                    selected: self.theme_picker.selected,
                    current: &self.theme,
                };
                let (width, height) = popup.size(right.width);
                let area = Rect {
                    x: right.x + right.width.saturating_sub(width) / 2,
                    y: composer_area.y.saturating_sub(height),
                    width,
                    height: height.min(composer_area.y),
                };
                if area.height > 0 {
                    frame.render_widget(popup, area);
                }
            }

            // The seed popup floats above the composer.
            if let Some(kind) = self.composer.popup() {
                let popup = SeedPopup {
                    kind,
                    selected: self.composer.popup_selected(),
                    theme: &self.theme,
                };
                let (width, height) = popup.size(right.width);
                let area = Rect {
                    x: right.x,
                    y: composer_area.y.saturating_sub(height),
                    width,
                    height: height.min(composer_area.y),
                };
                if area.height > 0 {
                    frame.render_widget(popup, area);
                }
            }
        })?;

        self.last_draw = Some(Instant::now());
        self.needs_draw = false;
        self.draws += 1;
        Ok(())
    }

    /// Spawn `session.prompt` for a submitted composer buffer (mode `queue`,
    /// one text part — web parity). The result comes back as
    /// [`AppEvent::PromptDone`]; errors toast without stalling the loop.
    /// No-op without an attached client (keyless tests) or an active session.
    fn dispatch_prompt(&mut self, text: String, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let (Some(client), Some(session_id)) = (self.client.clone(), self.active_session.clone())
        else {
            return;
        };
        tokio::spawn(async move {
            let result = client
                .session_prompt(
                    session_id,
                    PromptMode::Queue,
                    vec![PromptContentPart::Text { text }],
                    None,
                )
                .await;
            let _ = event_tx.send(AppEvent::PromptDone { result });
        });
    }

    /// Spawn the approval answer POST. The loop keeps pumping while it is in
    /// flight; the result arrives as [`AppEvent::AnswerDone`] and is applied
    /// in [`App::on_answer_done`]. While in flight the takeover ignores
    /// further answer keys and shows a "sending…" hint. Without an attached
    /// client (keyless tests) the resolution is optimistic — there is no
    /// gateway to answer.
    fn answer_approval(
        &mut self,
        outcome: ApprovalResponseOutcome,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        let Mode::Approval(takeover) = &mut self.mode else {
            return;
        };
        if takeover.sending {
            return; // an answer is already in flight
        }
        let tag = AnswerTag::Approval {
            approval_id: takeover.approval_id.clone(),
            outcome,
        };
        let Some(client) = self.client.clone() else {
            // Keyless: resolve optimistically (mirrors the pre-back-channel
            // behavior with no client attached).
            let AnswerTag::Approval { approval_id, .. } = &tag else {
                return;
            };
            self.pending_approvals.remove(approval_id);
            self.mode = self.next_takeover().unwrap_or(Mode::Chat);
            self.set_toast(match outcome {
                ApprovalResponseOutcome::AllowedOnce => "allowed once",
                ApprovalResponseOutcome::Rejected => "rejected",
            });
            return;
        };
        takeover.sending = true;
        self.hint = Some("sending…".into());
        let rpc_id = takeover.rpc_id.clone();
        let session_id = takeover.session_id.clone();
        let approval_id = takeover.approval_id.clone();
        tokio::spawn(async move {
            let result = client
                .respond_approval(rpc_id, session_id, approval_id, outcome)
                .await;
            let _ = event_tx.send(AppEvent::AnswerDone { tag, result });
        });
    }

    /// Spawn the question answer POST — same spawned policy as
    /// [`App::answer_approval`]; one answer entry per question (`selected`
    /// carries the option labels).
    fn answer_question(&mut self, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let Mode::Question(takeover) = &mut self.mode else {
            return;
        };
        if takeover.sending {
            return;
        }
        let answer = AskUserQuestionAnswer {
            answers: takeover
                .questions
                .iter()
                .map(|question| QuestionAnswerItem {
                    id: question.item.id.clone(),
                    selected: question.selected_labels(),
                    custom: None,
                })
                .collect(),
        };
        let tag = AnswerTag::Question(takeover.rpc_id.clone());
        let rpc_id_echo = takeover.rpc_id.clone();
        let Some(client) = self.client.clone() else {
            self.pending_questions.remove(&rpc_id_echo.to_string());
            self.mode = self.next_takeover().unwrap_or(Mode::Chat);
            self.set_toast("answered");
            return;
        };
        takeover.sending = true;
        self.hint = Some("sending…".into());
        let rpc_id = takeover.rpc_id.clone();
        let session_id = takeover.session_id.clone();
        tokio::spawn(async move {
            let result = client
                .respond_question(rpc_id.clone(), session_id, answer)
                .await;
            let _ = event_tx.send(AppEvent::AnswerDone { tag, result });
        });
    }

    /// Apply a finished answer: success resolves the takeover it belongs to
    /// (pending dropped, next takeover promoted or back to chat, toast);
    /// failure (transport error or a `not-pending`/`bad-response` receipt)
    /// toasts and STAYS in the takeover with `sending` re-armed so the user
    /// can retry.
    fn on_answer_done(&mut self, tag: AnswerTag, result: Result<RpcReceipt, ClientError>) {
        self.hint = None; // clear the "sending…" hint
        let accepted = matches!(&result, Ok(receipt) if receipt.accepted);
        if accepted {
            match &tag {
                AnswerTag::Approval { approval_id, .. } => {
                    self.pending_approvals.remove(approval_id);
                }
                AnswerTag::Question(rpc_id) => {
                    self.pending_questions.remove(&rpc_id.to_string());
                }
            }
            // Resolve only if this takeover is still the displayed one (a
            // newer frame may have replaced it while the answer was in
            // flight); a stale success still drops its pending entry.
            let current = match (&tag, &self.mode) {
                (AnswerTag::Approval { approval_id, .. }, Mode::Approval(takeover))
                    if takeover.approval_id == *approval_id =>
                {
                    true
                }
                (AnswerTag::Question(rpc_id), Mode::Question(takeover))
                    if takeover.rpc_id == *rpc_id =>
                {
                    true
                }
                _ => false,
            };
            if current {
                self.mode = self.next_takeover().unwrap_or(Mode::Chat);
            }
            let toast = match &tag {
                AnswerTag::Approval { outcome, .. } => match outcome {
                    ApprovalResponseOutcome::AllowedOnce => "allowed once",
                    ApprovalResponseOutcome::Rejected => "rejected",
                },
                AnswerTag::Question(_) => "answered",
            };
            self.set_toast(toast);
            return;
        }
        // Failure: stay in the takeover and re-arm the answer keys.
        let reason = match &result {
            Err(error) => error.to_string(),
            Ok(receipt) => match receipt.reason {
                Some(RpcReceiptReason::NotPending) => "not pending".to_string(),
                Some(RpcReceiptReason::BadResponse) => "bad response".to_string(),
                None => "not accepted".to_string(),
            },
        };
        self.set_toast(format!("answer failed: {reason}"));
        match (&tag, &mut self.mode) {
            (AnswerTag::Approval { approval_id, .. }, Mode::Approval(takeover))
                if takeover.approval_id == *approval_id =>
            {
                takeover.sending = false;
            }
            (AnswerTag::Question(rpc_id), Mode::Question(takeover))
                if takeover.rpc_id == *rpc_id =>
            {
                takeover.sending = false;
            }
            _ => {}
        }
    }

    /// The one-line status: session id · last seq · truncated flag · running
    /// · focused surface · transient hint · toast · error.
    fn status_line(&self, theme: &Theme) -> Line<'static> {
        let mut parts: Vec<Line<'static>> = Vec::new();
        let body = |text: String| Span::styled(text, Style::default().fg(theme.text));
        match &self.active_session {
            Some(session_id) => parts.push(Line::from(body(format!("session {session_id}")))),
            None => parts.push(Line::from(body("no session".into()))),
        }
        if let Some(state) = self
            .active_session
            .as_ref()
            .and_then(|session_id| self.store.session(session_id))
        {
            parts.push(Line::from(body(format!("seq {}", state.last_seq))));
            if state.truncated {
                parts.push(Line::from(body("truncated".into())));
            }
        }
        if self.session_running() {
            parts.push(Line::from(body("running".into())));
        }
        parts.push(Line::from(body(format!("focus: {}", self.focus.label()))));
        if let Some(hint) = &self.hint {
            parts.push(Line::from(Span::styled(hint.clone(), style::hint(theme))));
        }
        if let Some((toast, _)) = &self.toast {
            parts.push(Line::from(Span::styled(toast.clone(), style::hint(theme))));
        }
        if let Some(error) = &self.last_error {
            parts.push(Line::from(Span::styled(
                format!("error: {error}"),
                Style::default().fg(theme.error),
            )));
        }
        Line::from(
            parts
                .into_iter()
                .enumerate()
                .flat_map(|(i, line)| {
                    let mut spans = line.spans;
                    if i > 0 {
                        let mut joined = vec![Span::raw(" · ")];
                        joined.append(&mut spans);
                        joined
                    } else {
                        spans
                    }
                })
                .collect::<Vec<_>>(),
        )
    }
}

/// RAII restore of the raw-mode/alternate-screen terminal state. Create it
/// right after `enable_raw_mode` + `EnterAlternateScreen`; Drop restores on
/// normal exit and on panic.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        use crossterm::execute;
        use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Production terminal setup: raw mode + alternate screen.
pub fn setup_terminal()
-> Result<Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>, AppError> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

/// Production terminal teardown (explicit; `TerminalGuard` covers panics).
pub fn teardown_terminal() {
    let _ = TerminalGuard;
}
