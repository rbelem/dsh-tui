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
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use crate::app::event::{AnswerTag, AppEvent, EventChannel, QueueActionKind};
use crate::app::{Action, App, AppError, AtCatalog, DRAW_INTERVAL, Focus};
use crate::client::ClientError;
use crate::render::chat_view::{ChatView, LiveChatState};
use crate::store::node::NodeData;
use crate::theme::Theme;
use crate::theme::ThemePopup;
use crate::ui::composer::{ComposerView, SeedPopup};
use crate::ui::launcher::LauncherPopup;
use crate::ui::queue::{QueuePopup, QueueStrip};
use crate::ui::sidebar::{SidebarView, sidebar_width};
use crate::ui::style;
use crate::ui::takeover::{ApprovalView, Mode, QuestionView};
use crate::wire::approvals::ApprovalResponseOutcome;
use crate::wire::questions::{AskUserQuestionAnswer, QuestionAnswerItem};
use crate::wire::rpc::{RpcReceipt, RpcReceiptReason};
use crate::wire::session::{
    PromptContentPart, PromptMode, SessionHistoryValue, SessionId, SessionModelsValue,
    SessionSearchItem, SessionSearchValue, SessionSummary, SessionUpdateQueueValue,
    UpdateQueueAction,
};
use crate::wire::skills::SkillListValue;

/// #15: the running-spinner braille frames, cycled per tick in the status
/// line's right cluster while a session runs. Defined (and re-exported
/// from) the chat view, where the #39 tool-header indicator shares them.
pub use crate::render::chat_view::{SPINNER_FRAMES, format_elapsed};

/// #36: the wrapped height of `text` at `width` cells — greedy per-word
/// packing, an UPPER bound of the paragraph wrap (div_ceil under-counts:
/// word boundaries rarely pack perfectly). Mirrors ratatui's WordWrapper:
/// a word wraps when it would exceed the line, and an over-wide word
/// (CJK runs, long tokens) spans `ceil(w / width)` rows of its own.
fn wrapped_height(text: &str, width: u16) -> usize {
    let width = width as usize;
    if width == 0 || text.is_empty() {
        return 1;
    }
    let mut rows = 1usize;
    let mut used = 0usize;
    for word in text.split_whitespace() {
        let w = UnicodeWidthStr::width(word);
        if used > 0 && used + 1 + w > width {
            rows += 1;
            used = 0;
        }
        if w > width {
            rows += w.div_ceil(width) - 1;
        }
        used += w + usize::from(used > 0);
    }
    rows
}

/// #19: truncate `text` to `max` display cells, appending a `…` ellipsis
/// (CJK-safe: cut by cell width, never splitting a wide char).
fn truncate_ellipsis(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w + 1 > max {
            break;
        }
        out.push(ch);
        width += w;
    }
    format!("{out}…")
}

/// #26: the responsive tier of a terminal width — 0: too-small (<32),
/// 1: drawer (32–79), 2: wide (≥80). A drawer state is only meaningful
/// inside tier 1; tier transitions close it.
fn drawer_tier(width: u16) -> u8 {
    if width < crate::app::TOO_SMALL_WIDTH {
        0
    } else if width < 80 {
        1
    } else {
        2
    }
}

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
                                Some(Action::CancelTurn) => self.cancel_turn(event_tx.clone()),
                                Some(Action::FetchSettings) => {
                                    self.fetch_settings(event_tx.clone())
                                }
                                Some(Action::SaveSettings) => {
                                    self.save_settings(event_tx.clone())
                                }
                                Some(Action::SwitchSession(session_id)) => {
                                    self.fetch_history(session_id.clone(), event_tx.clone());
                                    // #43: the model fetch rides the switch
                                    // (attach/switch contract).
                                    self.fetch_models(session_id, event_tx.clone());
                                }
                                Some(Action::QueueRemove) => {
                                    self.queue_action(UpdateQueueAction::Remove, event_tx.clone())
                                }
                                Some(Action::QueueSteer) => {
                                    self.queue_action(UpdateQueueAction::Steer, event_tx.clone())
                                }
                                Some(Action::QueueEdit(text)) => {
                                    let content = vec![crate::wire::session::ContentBlock {
                                        r#type: "text".into(),
                                        extra: serde_json::Map::from_iter([(
                                            "text".to_string(),
                                            serde_json::Value::String(text),
                                        )]),
                                    }];
                                    self.queue_action(
                                        UpdateQueueAction::Edit { content },
                                        event_tx.clone(),
                                    )
                                }
                                Some(Action::RequestCatalog) => {
                                    self.request_catalog(event_tx.clone())
                                }
                                Some(Action::RenameSession { session_id, title }) => {
                                    self.rename_session(session_id, title, event_tx.clone())
                                }
                                Some(Action::RenameWorkspace { workspace_id, title }) => {
                                    self.rename_workspace(workspace_id, title, event_tx.clone())
                                }
                                Some(Action::DeleteWorkspace(workspace_id)) => {
                                    self.delete_workspace(workspace_id, event_tx.clone())
                                }
                                Some(Action::ForkSession(session_id)) => {
                                    self.fork_session(session_id, event_tx.clone())
                                }
                                Some(Action::ArchiveSession(session_id)) => {
                                    self.archive_session(session_id, event_tx.clone())
                                }
                                Some(Action::CreateWorkspace(path)) => {
                                    self.create_workspace(path, event_tx.clone())
                                }
                                Some(Action::CreateSession { workspace_id }) => {
                                    self.create_session(workspace_id, event_tx.clone())
                                }
                                Some(Action::SearchSessions(query)) => {
                                    self.search_sessions(query, event_tx.clone())
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
                        Some(AppEvent::AttachmentDone { attachment_id, result }) => {
                            self.on_attachment_done(attachment_id, result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::RenameDone { session_id, result }) => {
                            self.on_rename_done(session_id, result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::ForkDone { result }) => {
                            self.on_fork_done(result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::ArchiveDone { session_id, result }) => {
                            self.on_archive_done(session_id, result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::WorkspaceCreateDone { result }) => {
                            self.on_workspace_create_done(result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::WorkspaceRenameDone { result }) => {
                            self.on_workspace_rename_done(result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::WorkspaceDeleteDone {
                            workspace_id,
                            result,
                        }) => {
                            self.on_workspace_delete_done(workspace_id, result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::SessionSearchDone { query, result }) => {
                            self.on_search_done(query, result, event_tx.clone());
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::SessionCreateDone { result }) => {
                            self.on_session_create_done(result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::CancelDone { result }) => {
                            match result {
                                Ok(_) => self.set_toast(crate::i18n::tr(self.locale, "toast.cancelled")),
                                Err(error) => self.set_toast(crate::i18n::trf(
                                    self.locale,
                                    "toast.cancel_failed",
                                    &[&error.to_string()],
                                )),
                            }
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::HistoryLoaded { session_id, result }) => {
                            self.on_history_loaded(session_id, result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                            self.drain_attachment_needs(event_tx.clone());
                        }
                        Some(AppEvent::ModelsLoaded { session_id, result }) => {
                            self.on_models_loaded(session_id, result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::QueueActionDone { kind, result }) => {
                            self.on_queue_action_done(kind, result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::CatalogLoaded { result }) => {
                            self.on_catalog_loaded(result);
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
                            self.drain_attachment_needs(event_tx.clone());
                        }
                        Some(AppEvent::SettingsDescribeDone { result }) => {
                            self.on_settings_described(result);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::SettingsSaveDone { ns, result }) => {
                            self.on_settings_saved(ns, result, event_tx.clone());
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::HostFrame(frame)) => {
                            self.handle_host_frame(frame);
                            self.needs_draw = true;
                            self.draw_if_due(term, false)?;
                            self.drain_attachment_needs(event_tx.clone());
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
                            self.drain_attachment_needs(event_tx.clone());
                        }
                        Some(AppEvent::Resize(width, height)) => {
                            // Q10: width change → full re-render. #26: an
                            // open drawer never survives a tier-CHANGING
                            // resize in either direction (the tiers: <32
                            // too-small, 32–79 drawer, ≥80 wide) — close
                            // + restore the prior focus so no stale drawer
                            // state leaks across a boundary (<32
                            // round-trips included; same-tier resizes keep
                            // the drawer). 6f/6g: the pane-only sidebar
                            // popups (view options) and the Add editor drop
                            // with the tier too — their surfaces don't
                            // exist below 80.
                            if drawer_tier(width) != drawer_tier(self.terminal_width) {
                                if self.drawer_open {
                                    self.close_drawer();
                                }
                                self.view_options = None;
                                self.workspace_editor = None;
                            }
                            self.view.viewport_height = height;
                            self.row_cache.invalidate_all();
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::Mouse(mouse)) => {
                            // #12: clicks/wheel/selection; draws coalesce
                            // wheel bursts into one redraw per tick. Plain
                            // `Moved` events (every pointer motion while
                            // capture is on) schedule NO draw — there is no
                            // hover chrome, and repainting per motion would
                            // flood the 16ms redraw budget (and rebuild the
                            // selection line-widths on every frame).
                            let is_move =
                                matches!(mouse.kind, crossterm::event::MouseEventKind::Moved);
                            self.handle_mouse(mouse);
                            if is_move {
                                continue;
                            }
                            self.needs_draw = true;
                            self.draw_if_due(term, false)?;
                        }
                        Some(AppEvent::Paste(text)) => {
                            self.handle_paste(text);
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::Tick) => {
                            self.expire_toast();
                            self.expire_copied_flash();
                            self.advance_spinner();
                            self.draw_if_due(term, false)?;
                        }
                        None => break,
                    }
                }
                _ = tick.tick() => {
                    self.expire_toast();
                    self.expire_copied_flash();
                    self.advance_spinner();
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

    /// #51: force one immediate frame — the first-frame fast paint: the
    /// onboarding takeover paints BEFORE the blocking attach round-trip in
    /// `main::run_app` (the attach streams the workspace picker afterwards
    /// via [`crate::app::App::on_attach_workspace_list`]).
    pub fn paint<B>(&mut self, term: &mut Terminal<B>) -> Result<(), AppError>
    where
        B: Backend,
        B::Error: Into<AppError>,
    {
        self.draw(term).map_err(Into::into)
    }

    /// Sync the row cache, apply follow, and render the three surfaces:
    /// sidebar | chat over composer over the status line. A takeover (Q6)
    /// replaces the whole layout.
    fn draw<B>(&mut self, term: &mut Terminal<B>) -> Result<(), B::Error>
    where
        B: Backend,
    {
        let size = term.size()?;
        self.terminal_width = size.width;
        // #36: the approval dialog's transient notice (owned — a
        // &self.current_notice() call inside the draw closure would
        // collide with its field borrows).
        let approval_notice = if matches!(self.mode, Mode::Approval(_)) {
            self.current_notice().map(str::to_string)
        } else {
            None
        };
        let approval_notice = approval_notice.as_deref();
        // #19: below 32 cols the too-small screen replaces every surface
        // (takeovers included); a resize back restores it live.
        if size.width < crate::app::TOO_SMALL_WIDTH {
            return self.draw_too_small(term);
        }
        // Full-screen takeover: the chat surfaces stay live underneath
        // (frames keep folding into the store) but are not drawn. #36:
        // the APPROVAL is no longer full-screen — it falls through to the
        // chat draw and overlays the live chat as a dialog at the end;
        // questions/settings/image keep the takeover.
        if !matches!(self.mode, Mode::Chat | Mode::Approval(_)) {
            term.draw(|frame| {
                let area = frame.area();
                // Owned: the viewer arm borrows `self.image_cache` mutably,
                // which an Option<&str> into self.toast/hint would conflict.
                let notice = self.current_notice().map(str::to_string);
                let notice = notice.as_deref();
                match &self.mode {
                    Mode::Question(takeover) => frame.render_widget(
                        QuestionView {
                            takeover,
                            notice,
                            theme: &self.theme,
                            locale: self.locale,
                        },
                        area,
                    ),
                    Mode::Settings(state) => frame.render_widget(
                        crate::ui::settings::SettingsView {
                            state,
                            notice,
                            theme: &self.theme,
                            locale: self.locale,
                        },
                        area,
                    ),
                    Mode::Onboarding(state) => frame.render_widget(
                        crate::ui::onboarding::OnboardingView {
                            state,
                            notice,
                            theme: &self.theme,
                            locale: self.locale,
                        },
                        area,
                    ),
                    Mode::Image(viewer) => frame.render_widget(
                        crate::ui::image_viewer::ImageViewerView {
                            viewer,
                            images: &mut self.image_cache,
                            protocol: self.image_protocol,
                            notice,
                            theme: &self.theme,
                            locale: self.locale,
                        },
                        area,
                    ),
                    // #36: the approval is not a takeover — the gate
                    // above excludes it, so this arm is unreachable (the
                    // dialog renders over the chat draw instead).
                    Mode::Approval(_) => {}
                    Mode::Chat => {}
                }
                // The theme picker floats over the settings view too (it's
                // the one place themes live; chat draws its own copy).
                if self.theme_picker.open {
                    let popup = ThemePopup {
                        themes: &self.themes.themes,
                        selected: self.theme_picker.selected,
                        current: &self.theme,
                        locale: self.locale,
                    };
                    let (width, height) = popup.size(area.width);
                    let popup_area = Rect {
                        x: area.x + area.width.saturating_sub(width) / 2,
                        y: area.y + area.height.saturating_sub(height) / 2,
                        width,
                        height: height.min(area.height),
                    };
                    if popup_area.height > 0 {
                        frame.render_widget(popup, popup_area);
                    }
                }
            })?;
            self.last_draw = Some(Instant::now());
            self.needs_draw = false;
            self.draws += 1;
            return Ok(());
        }

        let size = term.size()?;
        let full = Rect::new(0, 0, size.width, size.height);
        let sidebar_width = sidebar_width(size.width, self.sidebar_collapsed);
        // #11 pane construction: an explicit 1-cell gap column between the
        // sidebar and the main pane — the frame-wide `bg` fill (drawn first
        // in the closure) shows through it, so the panes are separated by
        // background contrast, not a rule character. Dropped below 80
        // columns so narrow terminals keep every column (the doubled-gap
        // risk around a Length(0) strip can't occur: the sidebar only
        // collapses below 60, where the gap is already 0).
        let gap = if size.width >= 80 { 1 } else { 0 };
        // The spacing is applied INSIDE the solver (ratatui 0.30): split
        // yields one rect per constraint, and the 1-cell gap lands between
        // them — the sidebar keeps its exact `Length`, the right pane
        // starts after the gap, and the gap column shows the frame `bg`.
        let panes = Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Fill(1)])
            .spacing(gap)
            .split(full);
        let (sidebar_area, right) = (panes[0], panes[1]);
        // #19: the drawer tier (<80 cols) — no permanent sidebar; the
        // on-demand overlay is `min(width, 30)` wide, full height, and
        // the hit-testing rects track it (closed → zero, so mouse events
        // fall through to the chat/composer).
        let drawer_area = if size.width < 80 && self.drawer_open {
            Rect {
                x: full.x,
                y: full.y,
                width: size.width.min(30),
                height: size.height,
            }
        } else {
            Rect::default()
        };
        // The queue strip docks between the chat and the composer while the
        // active session has queue items; an emptied queue closes the popup.
        let queue_empty = self.active_queue().is_empty();
        if queue_empty {
            self.queue_popup_open = false;
        }
        let queue_height = u16::from(!queue_empty);
        let composer_height = self.composer.layout_height(size.height / 2);
        // #41: the session header (title · preset · jobs) docks ABOVE the
        // chat — one row, hidden when no session is active (the empty-state
        // rule: no header over the hero). #38/#39: the status area is now
        // TWO rows — line 1 the model/effort/context segments, line 2 the
        // metrics bar with the state indicator.
        let header_height = u16::from(self.active_session.is_some());
        let [
            header_area,
            chat_area,
            queue_area,
            composer_area,
            status1_area,
            status2_area,
        ] = Layout::vertical([
            Constraint::Length(header_height),
            Constraint::Fill(1),
            Constraint::Length(queue_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(right);

        // #12: the surface rects the mouse hit-testing reads (stored per
        // draw — the first mouse event before any draw is a no-op). #19:
        // in the drawer tier `sidebar_area` is the DRAWER's inner rect
        // (inside its border) when open — the permanent column doesn't
        // exist below 80, and a closed drawer is a zero rect so mouse
        // events fall through to the chat/composer.
        let permanent_sidebar = sidebar_area;
        let drawer_inner = if drawer_area.width > 0 {
            drawer_area.inner(Margin {
                horizontal: 1,
                vertical: 1,
            })
        } else {
            Rect::default()
        };
        self.sidebar_area = if size.width < 80 {
            drawer_inner
        } else {
            permanent_sidebar
        };
        self.chat_area = chat_area;
        self.composer_area = composer_area;

        // ChatView reserves 1 blank top row, so the visible content rows are
        // one fewer than the pane height; follow/clamp math uses the content
        // height so the tail always lands on the bottom row.
        let chat_height = chat_area.height.saturating_sub(1);
        self.view.viewport_height = chat_height;
        // The chat's 2/2 content margin lives inside ChatView; the row cache
        // wraps at the same content width so cached lines always fit.
        let width = crate::render::chat_view::content_width(chat_area.width);

        let sidebar_groups = self.sidebar_groups();
        self.sidebar
            .clamp(crate::ui::sidebar::SidebarGroup::visible_len(
                &sidebar_groups,
            ));
        let session_id = self.active_session.clone();
        if let Some(session_id) = &session_id {
            let ctx = crate::render::markdown::RenderContext {
                width,
                theme: &self.theme,
                locale: self.locale,
                images: &self.image_cache,
                skill_folds: &self.skill_folds,
            };
            self.row_cache.sync(&self.store, session_id, &ctx);
            self.row_cache.render_dirty(&self.store, session_id, &ctx);
            if self.view.follow {
                // #44: follow anchors in the VISIBLE view's row space —
                // the trajectory ledger's rows in trajectory mode, the
                // chat's rendered lines otherwise.
                let total: usize = if self.view_mode == crate::app::ViewMode::Trajectory {
                    crate::render::trajectory::ledger_rows(&self.store, session_id).len()
                } else {
                    self.row_cache
                        .lines()
                        .iter()
                        .map(|row| row.lines.len())
                        .sum()
                };
                self.view.offset = total.saturating_sub(chat_height as usize);
            }
        }
        // #39: track running tool nodes (call present, no settled result)
        // while the session has a turn in flight — the chat view paints a
        // live spinner + elapsed on their headers. Pruned to the current
        // running set, so a settled or removed tool stops animating; a
        // dead session clears the map (no frozen spinner).
        if self.session_running() {
            let mut running = std::collections::HashSet::new();
            if let Some(session_id) = &session_id
                && let Some(state) = self.store.session(session_id)
            {
                for node in &state.nodes {
                    if matches!(
                        &node.data,
                        NodeData::Tool {
                            call: Some(_),
                            result: None,
                            ..
                        }
                    ) {
                        running.insert(node.key.clone());
                        self.running_tool_since
                            .entry(node.key.clone())
                            .or_insert_with(Instant::now);
                    }
                }
            }
            self.running_tool_since
                .retain(|key, _| running.contains(key));
        } else {
            self.running_tool_since.clear();
        }
        // #33: fold state lives only for currently cached nodes — pruned
        // when the node leaves the cache (removed, compacted, or an
        // inactive session). The retain runs against the post-sync rows,
        // so a toggle on a cached node always survives.
        self.skill_folds.retain(|key, _| {
            self.row_cache
                .lines()
                .iter()
                .any(|row| row.node_key == *key)
        });
        // #11 status line: two clusters — left context (session · seq ·
        // mode, muted separators), right state indicator. The right chunk is
        // sized to its content so it never wraps; the left absorbs all
        // truncation.
        let (status_left, status_right) = self.status_line(&self.theme);
        // #38/#43: status line 1's model/effort/context segments (the
        // wide-tier row — rendered at ≥80 only, like the old full cluster).
        let status1_left = self.status_meta_line(&self.theme);
        // #41: the session header line, precomputed (the draw closure's
        // disjoint-borrow rule — it holds `&mut self.row_cache`).
        let header_text = self.header_line();
        let status_width = status_right
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()) as u16)
            .sum::<u16>()
            .max(1);
        let offset = self.view.offset;
        // #12: the mouse-selection highlight, precomputed to buffer-space
        // rects (content row → buffer row via the content rect; cols
        // clamped to each line's text width so blank rows never highlight).
        let selection_overlay: Vec<Rect> = self
            .selection
            .map(|(anchor, current)| {
                let (start, end) = if (anchor.row, anchor.col) <= (current.row, current.col) {
                    (anchor, current)
                } else {
                    (current, anchor)
                };
                let content = self.chat_content_rect();
                // #22: the flat line-widths are cached per render, keyed by
                // the viewport offset + the transcript's flat line count —
                // both change exactly when the overlay's inputs do (scroll
                // shifts the offset; any content change re-renders rows, so
                // the generation moves). A live selection's overlay no
                // longer rescan the whole transcript on every draw.
                let line_widths: Vec<u16> = match &self.selection_widths_cache {
                    // #22: keyed by the viewport offset + the row cache's
                    // render generation — both change exactly when the
                    // overlay's inputs do (scroll shifts the offset; ANY
                    // content change — even a same-count streaming append
                    // to the last line — bumps the generation, which a
                    // flat-line-count key would miss).
                    Some((offset, generation, widths))
                        if *offset == self.view.offset
                            && *generation == self.row_cache.generation() =>
                    {
                        widths.clone()
                    }
                    _ => {
                        let widths: Vec<u16> = self
                            .row_cache
                            .lines()
                            .iter()
                            .flat_map(|row| row.lines.iter())
                            .map(|line| line.width() as u16)
                            .collect();
                        self.selection_widths_cache = Some((
                            self.view.offset,
                            self.row_cache.generation(),
                            widths.clone(),
                        ));
                        widths
                    }
                };
                (start.row..=end.row)
                    .filter_map(|row| {
                        // #21: rows are absolute cache-line indices — map
                        // into the viewport (rows outside the visible
                        // window highlight nothing, but stay part of the
                        // anchored range the copy uses).
                        let Some(screen_row) = row.checked_sub(self.view.offset) else {
                            return None; // above the viewport
                        };
                        if screen_row as u16 >= content.height {
                            return None; // below the viewport
                        }
                        let line_width = line_widths.get(row)?;
                        let col_start = if row == start.row { start.col } else { 0 };
                        let col_end = if row == end.row { end.col } else { u16::MAX };
                        let col_start = col_start.min(*line_width);
                        let col_end = col_end.min(*line_width);
                        if col_end <= col_start {
                            return None;
                        }
                        Some(Rect {
                            x: content.x + col_start,
                            y: content.y + screen_row as u16,
                            width: col_end - col_start,
                            height: 1,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Field-level chain (not `self.active_queue()`) so the borrow stays
        // disjoint from `&mut self.row_cache` inside the closure.
        let queue_items = self
            .active_session
            .as_ref()
            .and_then(|session_id| self.store.session(session_id))
            .and_then(|state| state.queue.as_ref())
            .map(|queue| queue.items.as_slice())
            .unwrap_or(&[]);

        // Computed before the draw closure: a `self` method call inside
        // would borrow all of `self` while the closure holds `row_cache`.
        let popup_kind = self.composer.popup();
        let popup_entries = self.popup_entries();
        let popup_loading = matches!(self.at_catalog, Some(AtCatalog { loading: true, .. }));
        let popup_selected = self.composer.popup_selected();
        let launcher_open = self.launcher.is_some();
        let launcher_entries = self.launcher_entries_filtered();
        let launcher_search = self
            .launcher
            .as_ref()
            .map(|launcher| launcher.search.buffer().to_string())
            .unwrap_or_default();
        let launcher_selected = self
            .launcher
            .as_ref()
            .map(|launcher| launcher.selected)
            .unwrap_or(0);
        // Loading shows only while a skill.list fetch is in flight and no
        // skills are cached yet (a failed fetch leaves the flag off).
        let launcher_loading = matches!(
            &self.at_catalog,
            Some(AtCatalog { loading: true, skills }) if skills.is_empty()
        );
        // New-session picker data, precomputed for the same disjoint-borrow
        // reason (the draw closure holds `&mut self.row_cache`).
        let new_session_entries = self.new_session_entries();
        let new_session_state = self
            .new_session
            .as_ref()
            .map(|state| (state.selected, state.sending));
        // Sidebar-search popup data (same precompute rule; the results are
        // cloned so the closure never borrows the app's search state).
        let sidebar_search_state = self
            .sidebar_search
            .as_ref()
            .map(|state| (state.selected, state.sending));
        let sidebar_search_query = self
            .sidebar_search
            .as_ref()
            .map(|state| state.query.buffer().to_string())
            .unwrap_or_default();
        let sidebar_search_results: Vec<SessionSearchItem> = self
            .sidebar_search
            .as_ref()
            .map(|state| state.results.clone())
            .unwrap_or_default();
        // 6f: view-options popup data (same precompute rule; the current
        // choices are Copy fields, read disjoint from the closure).
        let view_options_selected = self.view_options.as_ref().map(|state| state.selected);
        let sidebar_flat = self.sidebar_flat;
        let order_by_updated = self.order_by_updated;

        term.draw(|frame| {
            // #11: frame-wide `bg` fill first — the chat/status/queue paint
            // over it, the sidebar's `panel_bg` fill covers its own pane,
            // and the 1-cell gap column shows the main `bg`. With the Reset
            // default theme the fill is a no-op (non-truecolor terminals
            // skip bg fills entirely).
            let area = frame.area();
            frame
                .buffer_mut()
                .set_style(area, Style::new().bg(self.theme.bg));
            if sidebar_width > 0 {
                if self.sidebar_collapsed {
                    // 6b: the collapsed gutter — the `»` reopen affordance,
                    // vertically centered (the whole 1-col strip is a click
                    // target in the app's mouse handling).
                    let y = sidebar_area.y + sidebar_area.height.saturating_sub(1) / 2;
                    frame.buffer_mut().set_stringn(
                        sidebar_area.x,
                        y,
                        "»",
                        1,
                        crate::ui::style::hint(&self.theme),
                    );
                } else {
                    frame.render_widget(
                        SidebarView {
                            sessions: &self.sessions,
                            groups: &sidebar_groups,
                            active: self.active_session.as_ref(),
                            selected: self.sidebar.selected,
                            focused: self.focus == Focus::Sidebar,
                            editor: self.rename_editor.as_ref().map(|(_, editor)| editor),
                            workspace_rename: self
                                .workspace_rename
                                .as_ref()
                                .map(|(id, editor)| (id, editor)),
                            workspace_editor: self.workspace_editor.as_ref(),
                            drawer: false,
                            theme: &self.theme,
                            locale: self.locale,
                        },
                        sidebar_area,
                    );
                }
            }
            // #44: the chat-area view — the chat transcript or the
            // trajectory ledger — dispatches on `view_mode` (both render
            // into the same `chat_area`; no session shows the hero either
            // way).
            match self.view_mode {
                crate::app::ViewMode::Chat => {
                    if let Some(session_id) = &session_id {
                        frame.render_widget(
                            ChatView {
                                store: &self.store,
                                session_id,
                                offset,
                                row_cache: &mut self.row_cache,
                                images: &mut self.image_cache,
                                // #39: the live running/elapsed overlay for tool
                                // headers (empty map = idle — no per-tick chrome).
                                live: Some(LiveChatState {
                                    frame: self.spinner_frame,
                                    running: &self.running_tool_since,
                                    spinner_style: style::active(&self.theme),
                                    elapsed_style: style::hint(&self.theme),
                                }),
                            },
                            chat_area,
                        );
                    } else {
                        // No session selected: the empty-chat hero (title,
                        // subtitle, key hints) instead of a blank panel.
                        frame.render_widget(
                            crate::ui::HeroView {
                                theme: &self.theme,
                                locale: self.locale,
                            },
                            chat_area,
                        );
                    }
                }
                crate::app::ViewMode::Trajectory => {
                    if let Some(session_id) = &session_id {
                        frame.render_widget(
                            crate::render::TrajectoryView {
                                store: &self.store,
                                session_id,
                                offset,
                                theme: &self.theme,
                                locale: self.locale,
                            },
                            chat_area,
                        );
                    } else {
                        // No session selected: the same empty-chat hero.
                        frame.render_widget(
                            crate::ui::HeroView {
                                theme: &self.theme,
                                locale: self.locale,
                            },
                            chat_area,
                        );
                    }
                }
            }
            // #12: the transient selection highlight (REVERSED — reintroduced
            // for text selection only; the `▎`-stripe chrome is a different
            // verb). Painted after the chat so it always wins.
            if !selection_overlay.is_empty() {
                let highlight = Style::new().add_modifier(Modifier::REVERSED);
                for rect in &selection_overlay {
                    frame.buffer_mut().set_style(*rect, highlight);
                }
            }
            if queue_height > 0 {
                frame.render_widget(
                    QueueStrip {
                        items: queue_items,
                        theme: &self.theme,
                        locale: self.locale,
                    },
                    queue_area,
                );
            }
            frame.render_widget(
                ComposerView {
                    composer: &self.composer,
                    focused: self.focus == Focus::Composer,
                    theme: &self.theme,
                    locale: self.locale,
                },
                composer_area,
            );
            // #41: the session header — `Session: <title> | Agent preset:
            // <preset> | Background jobs: <n>` — one row above the chat.
            // Rendered whenever a session is active (the area is zero
            // otherwise); the text truncates with `…` like the status
            // clusters. Segments omit individually: no preset, or no
            // running jobs, and the segment drops out.
            if header_height > 0
                && let Some(header) = header_text.as_deref()
            {
                let text = truncate_ellipsis(header, header_area.width as usize);
                frame.render_widget(Paragraph::new(Line::raw(text)), header_area);
            }
            // #11 status line (TWO rows since #38/#41): each row is
            // [Fill(1) left, Length(indicator) right] — a single Line
            // can't right-align; the right cluster never wraps, the left
            // truncates first. 1/1 horizontal inset. #19: tiers — <40
            // indicators only (line 2); 40–79 first-span-left only (line
            // 2, truncated with `…`); ≥80 the full clusters. Line 1 (the
            // model/effort/context row) is a wide-tier row — hidden
            // below 80 like the old full cluster was.
            let status1_area = status1_area.inner(Margin {
                horizontal: 1,
                vertical: 0,
            });
            let status2_area = status2_area.inner(Margin {
                horizontal: 1,
                vertical: 0,
            });
            if size.width >= 80 && !status1_left.is_empty() {
                frame.render_widget(Paragraph::new(Line::from(status1_left)), status1_area);
            }
            if size.width < 40 {
                // #30: below 40 the left cluster is hidden, so no status
                // hint can render here — the `≡` affordance at the chat's
                // top-left is the drawer's discoverability path at 32–39.
                frame.render_widget(
                    Paragraph::new(Line::from(status_right)).right_aligned(),
                    status2_area,
                );
            } else {
                let [status_left_area, status_right_area] =
                    Layout::horizontal([Constraint::Fill(1), Constraint::Length(status_width)])
                        .areas(status2_area);
                // #30: while the drawer is open it covers the status row's
                // left edge (full height) — shift the left cluster past its
                // right edge so the drawer hint stays visible.
                let left_area = if self.drawer_open {
                    Rect {
                        x: status_left_area.x + size.width.min(30),
                        ..status_left_area
                    }
                } else {
                    status_left_area
                };
                let left_line = if size.width < 80 {
                    // The first-span-only cluster below 80: the hint (the
                    // drawer's `s sessions · esc close`) takes the slot
                    // while the drawer is open, else the first left span —
                    // now `seq N`, the session id having moved to the
                    // header — truncated with `…` to fit the left area.
                    let span = match &self.hint {
                        Some(hint) => {
                            Span::styled(hint.clone(), crate::ui::style::hint(&self.theme))
                        }
                        None => status_left
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Span::raw("")),
                    };
                    let text = truncate_ellipsis(span.content.as_ref(), left_area.width as usize);
                    Line::from(Span::styled(text, span.style))
                } else {
                    Line::from(status_left)
                };
                frame.render_widget(Paragraph::new(left_line), left_area);
                frame.render_widget(
                    Paragraph::new(Line::from(status_right)).right_aligned(),
                    status_right_area,
                );
            }

            // #19: the drawer-tier overlay — painted AFTER the chat (it's
            // an overlay): a bordered, panel_bg interior (the popup
            // pattern, Clear first: with the Reset default theme the
            // panel_bg fill is a no-op, so the Clear is what keeps the
            // underlying chat from showing through) with the SidebarView
            // inside. The chat layout is untouched while it's open (no
            // layout shift).
            if drawer_area.width > 0 {
                frame.render_widget(ratatui::widgets::Clear, drawer_area);
                frame.render_widget(
                    ratatui::widgets::Block::bordered().border_style(style::border(&self.theme)),
                    drawer_area,
                );
                frame.render_widget(
                    SidebarView {
                        sessions: &self.sessions,
                        groups: &sidebar_groups,
                        active: self.active_session.as_ref(),
                        selected: self.sidebar.selected,
                        focused: true, // the drawer owns focus while open
                        editor: self.rename_editor.as_ref().map(|(_, editor)| editor),
                        workspace_rename: self
                            .workspace_rename
                            .as_ref()
                            .map(|(id, editor)| (id, editor)),
                        workspace_editor: None, // the drawer has no Add button
                        drawer: true,
                        theme: &self.theme,
                        locale: self.locale,
                    },
                    drawer_inner,
                );
            }
            // #19: the drawer-tier affordance — `≡` at the chat's top-left
            // (muted closed, accent open; itself a click target). Painted
            // last: while the drawer is open it doubles as its corner.
            if size.width < 80 {
                let affordance = if self.drawer_open {
                    Style::new()
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    style::hint(&self.theme)
                };
                frame
                    .buffer_mut()
                    .set_stringn(chat_area.x, chat_area.y, "≡", 1, affordance);
            }

            // The queue popup docks above the strip (view-only v1).
            if self.queue_popup_open {
                let popup = QueuePopup {
                    items: queue_items,
                    scroll: self.queue_scroll,
                    theme: &self.theme,
                    locale: self.locale,
                    editor: self.queue_editor.as_ref(),
                };
                let anchor = if queue_height > 0 {
                    queue_area.y
                } else {
                    composer_area.y
                };
                let (width, height) = popup.size(right.width, anchor);
                let area = Rect {
                    x: right.x,
                    y: anchor.saturating_sub(height),
                    width,
                    height,
                };
                if area.height > 0 {
                    frame.render_widget(popup, area);
                }
            }

            // The real terminal cursor marks the focused composer. The
            // inner rect mirrors ComposerView's top rule + 2/2 padding.
            if self.focus == Focus::Composer {
                let inner = Rect {
                    x: composer_area.x + 2,
                    y: composer_area.y + 1,
                    width: composer_area.width.saturating_sub(4),
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
                    locale: self.locale,
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

            // The seed popup floats above the composer, fed by the app's
            // entry list (commands mirror / cached skills).
            if let Some(kind) = popup_kind {
                let popup = SeedPopup {
                    kind,
                    entries: &popup_entries,
                    selected: popup_selected,
                    loading: popup_loading,
                    theme: &self.theme,
                    locale: self.locale,
                };
                let room = composer_area.y;
                let (width, height) = popup.size(right.width, room);
                let area = Rect {
                    x: right.x,
                    y: composer_area.y.saturating_sub(height),
                    width,
                    height: height.min(room),
                };
                if area.height > 0 {
                    frame.render_widget(popup, area);
                }
            }

            // The Ctrl+P launcher: a centered overlay above the composer
            // (mirrors the theme picker placement).
            if launcher_open {
                let popup = LauncherPopup {
                    entries: &launcher_entries,
                    selected: launcher_selected,
                    search: &launcher_search,
                    loading: launcher_loading,
                    theme: &self.theme,
                    locale: self.locale,
                };
                let room = composer_area.y;
                let (width, height) = popup.size(right.width, room);
                let area = Rect {
                    x: right.x + right.width.saturating_sub(width) / 2,
                    y: composer_area.y.saturating_sub(height),
                    width,
                    height: height.min(room),
                };
                if area.height > 0 {
                    frame.render_widget(popup, area);
                }
            }

            // The new-session picker (`n`): a centered overlay above the
            // composer (the launcher's placement).
            if let Some((selected, sending)) = new_session_state {
                let popup = crate::ui::new_session::NewSessionPopup {
                    entries: &new_session_entries,
                    selected,
                    sending,
                    theme: &self.theme,
                    locale: self.locale,
                };
                let room = composer_area.y;
                let (width, height) = popup.size(right.width, room);
                let area = Rect {
                    x: right.x + right.width.saturating_sub(width) / 2,
                    y: composer_area.y.saturating_sub(height),
                    width,
                    height: height.min(room),
                };
                if area.height > 0 {
                    frame.render_widget(popup, area);
                }
            }

            // The sidebar search popup (`/`): a centered overlay over the
            // sidebar pane, sized like the new-session picker.
            if let Some((selected, sending)) = sidebar_search_state {
                let popup = crate::ui::search::SidebarSearchPopup {
                    query: &sidebar_search_query,
                    results: &sidebar_search_results,
                    selected,
                    sending,
                    theme: &self.theme,
                    locale: self.locale,
                };
                let (width, height) = popup.size(sidebar_area.width, sidebar_area.height);
                let area = Rect {
                    x: sidebar_area.x + sidebar_area.width.saturating_sub(width) / 2,
                    y: sidebar_area.y + sidebar_area.height.saturating_sub(height) / 2,
                    width,
                    height: height.min(sidebar_area.height),
                };
                if area.height > 0 {
                    frame.render_widget(popup, area);
                }
            }

            // 6f: the view-options popup (`Options` button): a centered
            // overlay over the sidebar pane, sized like the search popup.
            if let Some(selected) = view_options_selected {
                let popup = crate::ui::view_options::ViewOptionsPopup {
                    selected,
                    flat: sidebar_flat,
                    order_updated: order_by_updated,
                    theme: &self.theme,
                    locale: self.locale,
                };
                let (width, height) = popup.size(sidebar_area.width, sidebar_area.height);
                let area = Rect {
                    x: sidebar_area.x + sidebar_area.width.saturating_sub(width) / 2,
                    y: sidebar_area.y + sidebar_area.height.saturating_sub(height) / 2,
                    width,
                    height: height.min(sidebar_area.height),
                };
                if area.height > 0 {
                    frame.render_widget(popup, area);
                }
            }

            // #46: the context menu (the sidebar's `m` key / the kebab
            // clicks): a centered overlay over the sidebar pane, sized
            // like the view-options popup.
            if let Some(menu) = &self.context_menu {
                let popup = crate::ui::context_menu::ContextMenuPopup {
                    target: &menu.target,
                    items: &menu.items,
                    selected: menu.selected,
                    theme: &self.theme,
                    locale: self.locale,
                };
                let (width, height) = popup.size(sidebar_area.width, sidebar_area.height);
                let area = Rect {
                    x: sidebar_area.x + sidebar_area.width.saturating_sub(width) / 2,
                    y: sidebar_area.y + sidebar_area.height.saturating_sub(height) / 2,
                    width,
                    height: height.min(sidebar_area.height),
                };
                if area.height > 0 {
                    frame.render_widget(popup, area);
                }
            }

            // #36: the approval request overlays the live chat as a
            // centered dialog — the popup treatment (Clear + bordered
            // block + panel_bg interior; ApprovalView provides all of it,
            // rendered into the dialog rect instead of the full frame).
            // Width clamps to min(64, chat region − 4) per the #19 popup
            // rule; height fits the wrapped content, capped at the chat
            // region; centered over the chat (never the composer/status).
            if let Mode::Approval(takeover) = &self.mode {
                let notice = approval_notice;
                let width = 64u16.min(chat_area.width.saturating_sub(4));
                let inner_width = width.saturating_sub(4); // border 2 + padding 2
                // #36: the estimate consumes the SAME lines the view
                // renders (prefixes included — they drifted once, clipping
                // the action line) and is an upper bound of the wrap.
                let content_height = |compact: bool| {
                    ApprovalView {
                        takeover,
                        notice,
                        theme: &self.theme,
                        locale: self.locale,
                        compact,
                    }
                    .lines()
                    .iter()
                    .map(|line| wrapped_height(&line.to_string(), inner_width))
                    .sum::<usize>()
                };
                // Tiny chat area: drop the reason/summary hints (never the
                // y/n action line) before the height cap.
                let compact = 2 + content_height(false) > chat_area.height as usize;
                // The area includes the block's top/bottom border rows.
                let height = ((2 + content_height(compact)) as u16)
                    .min(chat_area.height)
                    .max(3);
                let area = Rect {
                    x: chat_area.x + chat_area.width.saturating_sub(width) / 2,
                    y: chat_area.y + chat_area.height.saturating_sub(height) / 2,
                    width,
                    height,
                };
                frame.render_widget(ratatui::widgets::Clear, area);
                frame.render_widget(
                    ApprovalView {
                        takeover,
                        notice,
                        theme: &self.theme,
                        locale: self.locale,
                        compact,
                    },
                    area,
                );
            }
        })?;

        self.last_draw = Some(Instant::now());
        self.needs_draw = false;
        self.draws += 1;
        Ok(())
    }

    /// #19: the full-screen "terminal too small" screen (<32 cols): the
    /// two-tone wordmark, the title in `text`, the hint in `muted` —
    /// centered, themed. `q` still quits; a resize back to ≥32 restores
    /// the prior screen (the caller renders it on the next draw).
    fn draw_too_small<B>(&mut self, term: &mut Terminal<B>) -> Result<(), B::Error>
    where
        B: Backend,
    {
        term.draw(|frame| {
            let area = frame.area();
            frame
                .buffer_mut()
                .set_style(area, Style::new().bg(self.theme.bg));
            let title = crate::i18n::tr(self.locale, "too_small.title");
            let hint = crate::i18n::tr(self.locale, "too_small.hint");
            let wordmark_width = ("dsh".len() + "-tui".len()) as u16;
            let title_width = UnicodeWidthStr::width(title) as u16;
            let hint_width = UnicodeWidthStr::width(hint) as u16;
            let block_width = wordmark_width.max(title_width).max(hint_width);
            let y = area.y + area.height.saturating_sub(3) / 2;
            let x = area.x + area.width.saturating_sub(block_width) / 2;
            let wordmark = Line::from(vec![
                Span::styled(
                    "dsh",
                    Style::new()
                        .add_modifier(Modifier::BOLD)
                        .fg(self.theme.accent),
                ),
                Span::styled(
                    "-tui",
                    Style::new()
                        .add_modifier(Modifier::BOLD)
                        .fg(self.theme.text),
                ),
            ]);
            frame.buffer_mut().set_line(x, y, &wordmark, area.width);
            frame.buffer_mut().set_line(
                x + (block_width - title_width) / 2,
                y + 1,
                &Line::styled(title, Style::default().fg(self.theme.text)),
                area.width,
            );
            frame.buffer_mut().set_line(
                x + (block_width - hint_width) / 2,
                y + 2,
                &Line::styled(hint, crate::ui::style::hint(&self.theme)),
                area.width,
            );
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

    /// Drain the attachment needs: for each caption-only attachment not
    /// cached and not in flight, mark it pending and spawn the
    /// `session.attachment` fetch. The result arrives as
    /// [`AppEvent::AttachmentDone`] and populates [`ImageCache`]. Called
    /// after store-changing events only — a failed fetch must not
    /// self-trigger (the caption-only row stays until the next store
    /// change re-encounters it, and each encounter retries once).
    /// Spawn `session.rename` for the sidebar `r` editor (the loop keeps
    /// pumping; the result lands as [`AppEvent::RenameDone`]). One sidebar
    /// action in flight at a time (`sidebar_action_sending`, mirroring the
    /// queue actions); without a client the action is dropped silently.
    fn rename_session(
        &mut self,
        session_id: SessionId,
        title: String,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.sidebar_action_sending {
            return;
        }
        self.sidebar_action_sending = true;
        self.rename_editor = None; // a commit closes the editor
        tokio::spawn(async move {
            let result = client.session_rename(session_id.clone(), title).await;
            let _ = event_tx.send(AppEvent::RenameDone { session_id, result });
        });
    }

    /// Spawn `session.fork` for the sidebar `f` key (the child appears via
    /// `host/session-added`; the result lands as [`AppEvent::ForkDone`]).
    fn fork_session(&mut self, session_id: SessionId, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.sidebar_action_sending {
            return;
        }
        self.sidebar_action_sending = true;
        tokio::spawn(async move {
            let result = client.session_fork(session_id, None).await;
            let _ = event_tx.send(AppEvent::ForkDone { result });
        });
    }

    /// Spawn `workspace.archiveSession` for the sidebar `a` key; the value
    /// is the FULL updated archive set, swapped in on completion.
    fn archive_session(
        &mut self,
        session_id: SessionId,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.sidebar_action_sending {
            return;
        }
        self.sidebar_action_sending = true;
        tokio::spawn(async move {
            let result = client.workspace_archive_session(session_id.clone()).await;
            let _ = event_tx.send(AppEvent::ArchiveDone { session_id, result });
        });
    }

    /// Apply a finished rename: update the session row's `title` projection
    /// in place (the sidebar label reads it) and toast; a failure toasts
    /// with the guard re-armed and no state change.
    fn on_rename_done(
        &mut self,
        session_id: SessionId,
        result: Result<crate::wire::session::SessionRenameValue, ClientError>,
    ) {
        self.sidebar_action_sending = false;
        match result {
            Ok(value) => {
                if let Some(summary) = self
                    .sessions
                    .iter_mut()
                    .find(|summary| summary.session_id == session_id)
                {
                    let projections = summary.projections.get_or_insert_with(|| {
                        crate::wire::session::SessionProjectionsBlock {
                            as_of_seq: value.seq,
                            values: Default::default(),
                        }
                    });
                    projections
                        .values
                        .insert("title".into(), serde_json::json!(value.title));
                }
                self.set_toast(crate::i18n::tr(self.locale, "toast.renamed"));
            }
            Err(error) => self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.sidebar_action_failed",
                &[&error.to_string()],
            )),
        }
    }

    /// Apply a finished fork: toast the new session id (the row itself
    /// arrives via `host/session-added`).
    fn on_fork_done(
        &mut self,
        result: Result<crate::wire::session::SessionForkValue, ClientError>,
    ) {
        self.sidebar_action_sending = false;
        match result {
            Ok(value) => self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.forked",
                &[&value.session_id.0],
            )),
            Err(error) => self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.sidebar_action_failed",
                &[&error.to_string()],
            )),
        }
    }

    /// Apply a finished archive: swap in the host's full archive set and
    /// clamp the sidebar selection (archived-beats-membership re-renders
    /// the group). The active session, if archived, keeps its chat content
    /// but becomes unreachable by nav — the host frame path behaves the
    /// same (ids + clamp only).
    fn on_archive_done(
        &mut self,
        session_id: SessionId,
        result: Result<crate::wire::workspace::WorkspaceArchiveSessionValue, ClientError>,
    ) {
        self.sidebar_action_sending = false;
        match result {
            Ok(value) => {
                self.archived_session_ids = value.archived_session_ids;
                self.sidebar
                    .clamp(crate::ui::sidebar::SidebarGroup::visible_len(
                        &self.sidebar_groups(),
                    ));
                self.set_toast(crate::i18n::tr(self.locale, "toast.archived"));
                let _ = session_id;
            }
            Err(error) => self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.sidebar_action_failed",
                &[&error.to_string()],
            )),
        }
    }

    /// 6g: spawn `workspace.create` for the Add button's path editor; the
    /// result lands as [`AppEvent::WorkspaceCreateDone`]. One sidebar
    /// action in flight at a time (the shared `sidebar_action_sending`
    /// guard — the editor's hint row shows "creating…" while it's set).
    fn create_workspace(&mut self, path: String, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.sidebar_action_sending {
            return;
        }
        self.sidebar_action_sending = true;
        tokio::spawn(async move {
            let result = client.workspace_create(path).await;
            let _ = event_tx.send(AppEvent::WorkspaceCreateDone { result });
        });
    }

    /// 6g: apply a finished `workspace.create`: fold the returned
    /// WorkspaceView into the sidebar list — a NEW workspace appends to
    /// `workspaces` and the durable order, a pre-existing one upserts in
    /// place (its session membership may have changed) — and close the
    /// editor. Errors toast through the shared sidebar-action failure
    /// surface.
    fn on_workspace_create_done(
        &mut self,
        result: Result<crate::wire::workspace::WorkspaceCreateValue, ClientError>,
    ) {
        self.sidebar_action_sending = false;
        self.workspace_editor = None;
        match result {
            Ok(value) => {
                if value.created {
                    if !self
                        .workspaces
                        .iter()
                        .any(|ws| ws.workspace_id == value.workspace.workspace_id)
                    {
                        self.workspaces.push(value.workspace.clone());
                    }
                    if !self.workspace_order.contains(&value.workspace.workspace_id) {
                        self.workspace_order
                            .push(value.workspace.workspace_id.clone());
                    }
                } else if let Some(existing) = self
                    .workspaces
                    .iter_mut()
                    .find(|ws| ws.workspace_id == value.workspace.workspace_id)
                {
                    *existing = value.workspace;
                }
                self.sidebar
                    .clamp(crate::ui::sidebar::SidebarGroup::visible_len(
                        &self.sidebar_groups(),
                    ));
            }
            Err(error) => self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.sidebar_action_failed",
                &[&error.to_string()],
            )),
        }
    }

    /// #46: spawn `workspace.rename` for the context menu's rename; the
    /// result lands as [`AppEvent::WorkspaceRenameDone`]. One sidebar
    /// action in flight at a time (the shared `sidebar_action_sending`
    /// guard).
    fn rename_workspace(
        &mut self,
        workspace_id: crate::wire::session::WorkspaceId,
        title: String,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.sidebar_action_sending {
            return;
        }
        self.sidebar_action_sending = true;
        self.workspace_rename = None; // a commit closes the editor
        tokio::spawn(async move {
            let result = client.workspace_rename(workspace_id, title).await;
            let _ = event_tx.send(AppEvent::WorkspaceRenameDone { result });
        });
    }

    /// #46: apply a finished `workspace.rename`: swap the workspace row's
    /// title in place (the sidebar header reads it) and toast; a failure
    /// toasts with the guard re-armed and no state change.
    fn on_workspace_rename_done(
        &mut self,
        result: Result<crate::wire::workspace::WorkspaceRenameValue, ClientError>,
    ) {
        self.sidebar_action_sending = false;
        match result {
            Ok(value) => {
                if let Some(existing) = self
                    .workspaces
                    .iter_mut()
                    .find(|ws| ws.workspace_id == value.workspace.workspace_id)
                {
                    existing.title = value.workspace.title;
                }
                self.set_toast(crate::i18n::tr(self.locale, "toast.renamed"));
            }
            Err(error) => self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.sidebar_action_failed",
                &[&error.to_string()],
            )),
        }
    }

    /// #46: spawn `workspace.delete` for the context menu's delete; the
    /// result lands as [`AppEvent::WorkspaceDeleteDone`].
    fn delete_workspace(
        &mut self,
        workspace_id: crate::wire::session::WorkspaceId,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.sidebar_action_sending {
            return;
        }
        self.sidebar_action_sending = true;
        tokio::spawn(async move {
            let result = client.workspace_delete(workspace_id.clone()).await;
            let _ = event_tx.send(AppEvent::WorkspaceDeleteDone {
                workspace_id,
                result,
            });
        });
    }

    /// #46: apply a finished `workspace.delete`: drop the workspace row and
    /// its durable-order entry (its sessions reflow to ungrouped via the
    /// membership derivation) and toast; a failure toasts with the guard
    /// re-armed.
    fn on_workspace_delete_done(
        &mut self,
        workspace_id: crate::wire::session::WorkspaceId,
        result: Result<crate::wire::workspace::WorkspaceDeleteValue, ClientError>,
    ) {
        self.sidebar_action_sending = false;
        match result {
            Ok(_) => {
                self.workspaces.retain(|ws| ws.workspace_id != workspace_id);
                self.workspace_order.retain(|id| *id != workspace_id);
                self.sidebar
                    .clamp(crate::ui::sidebar::SidebarGroup::visible_len(
                        &self.sidebar_groups(),
                    ));
                self.set_toast(crate::i18n::tr(self.locale, "toast.workspace_deleted"));
            }
            Err(error) => self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.sidebar_action_failed",
                &[&error.to_string()],
            )),
        }
    }

    /// Spawn `session.create` for the new-session picker (the loop keeps
    /// pumping; the result lands as [`AppEvent::SessionCreateDone`]). One
    /// create in flight at a time (the picker's `sending` guard); without
    /// a client the action is dropped silently.
    fn create_session(
        &mut self,
        workspace_id: Option<crate::wire::session::WorkspaceId>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(state) = &mut self.new_session else {
            return;
        };
        if state.sending {
            return;
        }
        state.sending = true;
        tokio::spawn(async move {
            let result = client.session_create(workspace_id, None, None, None).await;
            let _ = event_tx.send(AppEvent::SessionCreateDone { result });
        });
    }

    /// Apply a finished `session.create`: on success close the picker,
    /// toast the new id, insert the summary locally (blank, just-now — a
    /// `host/session-added` frame may ALSO arrive; the dedup guard in
    /// `handle_host_frame` skips it) and switch straight to the new
    /// session (a fresh session has no history to fetch). On failure:
    /// toast, the picker stays open with the guard re-armed, no state
    /// change.
    fn on_session_create_done(
        &mut self,
        result: Result<crate::wire::session::SessionCreateValue, ClientError>,
    ) {
        if let Some(state) = &mut self.new_session {
            state.sending = false;
        }
        match result {
            Ok(value) => {
                self.new_session = None;
                self.set_toast(crate::i18n::trf(
                    self.locale,
                    "toast.session_created",
                    &[&value.session_id.0],
                ));
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let session_id = value.session_id;
                if !self
                    .sessions
                    .iter()
                    .any(|summary| summary.session_id == session_id)
                {
                    self.sessions.insert(
                        0,
                        crate::wire::session::SessionSummary {
                            session_id: session_id.clone(),
                            updated_at: now,
                            running: false,
                            blank: true,
                            parent_session_id: None,
                            origin: None,
                            cwd: None,
                            agent_preset: value.agent_preset,
                            projections: None,
                        },
                    );
                }
                // Switch to the new session (switch_to_selected semantics,
                // without the Enter path).
                self.store.open_session(session_id.clone());
                self.row_cache.invalidate_all();
                self.active_session = Some(session_id);
                // #43: brand-new session — no stale model from the last one.
                self.session_model = None;
                self.view.offset = 0;
                self.view.follow = true;
                self.hint = None;
            }
            Err(error) => self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.create_failed",
                &[&error.to_string()],
            )),
        }
    }

    /// Spawn `session.search` for the sidebar search popup (the loop keeps
    /// pumping; the result lands as [`AppEvent::SessionSearchDone`]). One
    /// search in flight at a time (the popup's `sending` guard); without a
    /// client the action is dropped silently.
    fn search_sessions(&mut self, query: String, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(state) = &mut self.sidebar_search else {
            return;
        };
        if state.sending {
            return;
        }
        state.sending = true;
        tokio::spawn(async move {
            let result = client.session_search(query.clone()).await;
            let _ = event_tx.send(AppEvent::SessionSearchDone { query, result });
        });
    }

    /// Apply a finished `session.search`: clear the in-flight guard and
    /// fold the rows (selection back to the top). A result whose echoed
    /// query no longer matches the buffer is STALE — the user typed on
    /// while the POST was in flight — so it is dropped and the current
    /// buffer is searched again (latest wins; each keystroke can only
    /// trigger one follow-up round trip). A failure clears the rows (the
    /// grouped list is restored) and toasts briefly — the popup itself
    /// stays open for a corrected query or Esc.
    fn on_search_done(
        &mut self,
        query: String,
        result: Result<SessionSearchValue, ClientError>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        let Some(state) = &mut self.sidebar_search else {
            return; // the popup closed while the POST was in flight
        };
        state.sending = false;
        match result {
            Ok(value) if state.query.buffer() == query => {
                state.results = value.items;
                state.selected = 0;
            }
            Ok(_) => {
                let current = state.query.buffer().to_string();
                self.search_sessions(current, event_tx);
            }
            Err(error) => {
                state.results.clear();
                state.selected = 0;
                self.set_toast(crate::i18n::trf(
                    self.locale,
                    "search.failed",
                    &[&error.to_string()],
                ));
            }
        }
    }

    fn drain_attachment_needs(&mut self, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let (Some(client), Some(session_id)) = (self.client.clone(), self.active_session.clone())
        else {
            return;
        };
        for attachment_id in self.attachment_needs() {
            if !self.pending_attachments.insert(attachment_id.clone()) {
                continue;
            }
            let client = client.clone();
            let session_id = session_id.clone();
            let event_tx = event_tx.clone();
            tokio::spawn(async move {
                let result = client
                    .session_attachment(session_id, attachment_id.clone())
                    .await;
                let _ = event_tx.send(AppEvent::AttachmentDone {
                    attachment_id,
                    result,
                });
            });
        }
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
                ApprovalResponseOutcome::AllowedOnce => {
                    crate::i18n::tr(self.locale, "toast.allowed_once")
                }
                ApprovalResponseOutcome::Rejected => crate::i18n::tr(self.locale, "toast.rejected"),
            });
            return;
        };
        takeover.sending = true;
        self.hint = Some(crate::i18n::tr(self.locale, "hint.sending").into());
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
            self.set_toast(crate::i18n::tr(self.locale, "toast.answered"));
            return;
        };
        takeover.sending = true;
        self.hint = Some(crate::i18n::tr(self.locale, "hint.sending").into());
        let rpc_id = takeover.rpc_id.clone();
        let session_id = takeover.session_id.clone();
        tokio::spawn(async move {
            let result = client
                .respond_question(rpc_id.clone(), session_id, answer)
                .await;
            let _ = event_tx.send(AppEvent::AnswerDone { tag, result });
        });
    }

    /// Spawn `session.updateQueue` for the focused queue item: the loop
    /// keeps pumping; the result arrives as [`AppEvent::QueueActionDone`].
    /// No-op without a client, an active session, or a focused item.
    fn queue_action(
        &mut self,
        action: UpdateQueueAction,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        let kind = match &action {
            UpdateQueueAction::Remove => QueueActionKind::Remove,
            UpdateQueueAction::Steer => QueueActionKind::Steer,
            UpdateQueueAction::Edit { .. } => QueueActionKind::Edit,
        };
        let (Some(client), Some(session_id)) = (self.client.clone(), self.active_session.clone())
        else {
            return;
        };
        let Some(item_id) = self.focused_queue_item().map(|item| item.id.clone()) else {
            return;
        };
        if self.queue_action_sending {
            return;
        }
        self.queue_action_sending = true;
        self.queue_editor = None; // a commit closes the editor
        tokio::spawn(async move {
            let result = client
                .session_update_queue(session_id, item_id, action)
                .await;
            let _ = event_tx.send(AppEvent::QueueActionDone { kind, result });
        });
    }

    /// Spawn `skill.list` for the `@` catalog: the loop keeps pumping; the
    /// result arrives as [`AppEvent::CatalogLoaded`]. Marks the catalog as
    /// loading so repeated popup opens don't re-fetch. No-op without a
    /// client or an active session.
    fn request_catalog(&mut self, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let (Some(client), Some(session_id)) = (self.client.clone(), self.active_session.clone())
        else {
            return;
        };
        if matches!(self.at_catalog, Some(AtCatalog { loading: true, .. })) {
            return;
        }
        let mut catalog = self.at_catalog.clone().unwrap_or_default();
        catalog.loading = true;
        self.at_catalog = Some(catalog);
        tokio::spawn(async move {
            let result = client.skill_list(session_id).await;
            let _ = event_tx.send(AppEvent::CatalogLoaded { result });
        });
    }

    /// Apply a finished `skill.list`: cache the entries (a failure stays
    /// uncached — the next popup open retries) and clear the loading flag.
    fn on_catalog_loaded(&mut self, result: Result<SkillListValue, ClientError>) {
        self.at_catalog = Some(AtCatalog {
            loading: false,
            ..self.at_catalog.clone().unwrap_or_default()
        });
        match result {
            Ok(value) => {
                self.at_catalog = Some(AtCatalog {
                    skills: value.skills,
                    loading: false,
                });
            }
            Err(error) => {
                self.set_toast(crate::i18n::trf(
                    self.locale,
                    "catalog.failed",
                    &[&error.to_string()],
                ));
            }
        }
    }

    /// Apply a finished queue action: toast the outcome and clear the
    /// in-flight guard (the next `session/queue` frame reflects the change;
    /// there is no optimistic mutation).
    fn on_queue_action_done(
        &mut self,
        kind: QueueActionKind,
        result: Result<SessionUpdateQueueValue, ClientError>,
    ) {
        self.queue_action_sending = false;
        match result {
            Ok(_) => {
                let key = match kind {
                    QueueActionKind::Remove => "queue.removed",
                    QueueActionKind::Steer => "queue.steered",
                    QueueActionKind::Edit => "queue.edited",
                };
                self.set_toast(crate::i18n::tr(self.locale, key));
            }
            Err(error) => self.set_toast(crate::i18n::trf(
                self.locale,
                "queue.action_failed",
                &[&error.to_string()],
            )),
        }
    }

    /// Spawn `session.cancel` for the active session (Q15): the loop keeps
    /// pumping; the result arrives as [`AppEvent::CancelDone`]. No-op without
    /// a client or an active session.
    fn cancel_turn(&mut self, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let (Some(client), Some(session_id)) = (self.client.clone(), self.active_session.clone())
        else {
            return;
        };
        tokio::spawn(async move {
            let result = client.session_cancel(session_id).await;
            let _ = event_tx.send(AppEvent::CancelDone { result });
        });
    }

    /// Spawn `session.history` for the switched-to session (Q9 resume): the
    /// page lands as [`AppEvent::HistoryLoaded`] and is folded by the
    /// stale-guarded [`App::on_history_loaded`]. The status line shows a
    /// "loading history…" hint while in flight. No-op without a client.
    fn fetch_history(&mut self, session_id: SessionId, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.history_loading = Some(session_id.clone());
        self.hint = Some(crate::i18n::tr(self.locale, "hint.loading_history").into());
        tokio::spawn(async move {
            let result = client
                .session_history(session_id.clone(), None, Some(200))
                .await;
            let _ = event_tx.send(AppEvent::HistoryLoaded { session_id, result });
        });
    }

    /// #43: spawn `session.models` for the switched-to session; the result
    /// lands as [`AppEvent::ModelsLoaded`] and is cached by the
    /// stale-guarded [`App::on_models_loaded`]. Tolerance: an unavailable
    /// gateway leaves the cache empty — the Model/Effort status segments
    /// just stay hidden. No-op without a client.
    fn fetch_models(&mut self, session_id: SessionId, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        tokio::spawn(async move {
            let result = client.session_models(session_id.clone()).await;
            let _ = event_tx.send(AppEvent::ModelsLoaded { session_id, result });
        });
    }

    /// Spawn `settings.describe` for the settings view (open, or the
    /// conflict refresh): the result arrives as
    /// [`AppEvent::SettingsDescribeDone`]. No-op without a client — the
    /// view stays on its "not exposed" panes.
    fn fetch_settings(&mut self, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let Some(client) = self.client.clone() else {
            if let Mode::Settings(state) = &mut self.mode {
                state.loading = false;
            }
            return;
        };
        tokio::spawn(async move {
            let result = client.settings_describe().await;
            let _ = event_tx.send(AppEvent::SettingsDescribeDone { result });
        });
    }

    /// Fold a describe result into the settings view (only while it is
    /// open — a late result after Esc is dropped). A failure toasts and
    /// leaves the view on its empty panes.
    fn on_settings_described(
        &mut self,
        result: Result<crate::wire::settings::SettingsDescribeValue, ClientError>,
    ) {
        let Mode::Settings(state) = &mut self.mode else {
            return;
        };
        match result {
            Ok(value) => state.apply_describe(value),
            Err(error) => {
                state.loading = false;
                self.set_toast(format!("settings failed: {error}"));
            }
        }
    }

    /// Spawn `settings.update` for the selected form: the patch is only the
    /// changed keys, `expectedRevision` rides the described revision
    /// (optimistic concurrency — a stale write comes back
    /// `settings-conflict`). The result arrives as
    /// [`AppEvent::SettingsSaveDone`]. No-op without a client.
    fn save_settings(&mut self, event_tx: mpsc::UnboundedSender<AppEvent>) {
        let (Some(client), Mode::Settings(state)) = (self.client.clone(), &self.mode) else {
            return;
        };
        let Some(section) = state.sections.get(state.selected) else {
            return;
        };
        let Some(form) = state.forms.get(&section.ns) else {
            return;
        };
        let ns = section.ns.clone();
        let revision = form.view.revision;
        let patch = form.patch();
        tokio::spawn(async move {
            let result = client.settings_update(&ns, Some(revision), patch).await;
            let _ = event_tx.send(AppEvent::SettingsSaveDone { ns, result });
        });
    }

    /// Apply a finished save: success refreshes the form from the returned
    /// view, toasts `saved`, and returns to the chat; a `settings-conflict`
    /// toasts `conflict — refreshed` and re-describes to the latest
    /// revision (edits are dropped — the freshest values win); any other
    /// error toasts and stays in the view with saving re-armed. A late
    /// result after the view closed is dropped.
    fn on_settings_saved(
        &mut self,
        ns: String,
        result: Result<crate::wire::settings::SettingsWriteValue, ClientError>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) {
        self.hint = None; // clear the "saving…" hint
        let Mode::Settings(state) = &mut self.mode else {
            return;
        };
        state.saving = false;
        match result {
            Ok(view) => {
                // The `locale` namespace drives the UI locale: its `language`
                // value, when it parses as a locale, syncs App.locale and the
                // config (any other value is ignored).
                if ns == "locale"
                    && let Some(language) = view.value.get("language").and_then(|v| v.as_str())
                    && let Some(locale) = crate::i18n::Locale::parse(language)
                {
                    self.locale = locale;
                    self.config.locale = Some(language.to_string());
                    let _ = self.config.save();
                    self.row_cache.invalidate_all();
                }
                if let Some(form) = state.forms.get_mut(&ns) {
                    form.refresh(view);
                }
                self.mode = Mode::Chat;
                self.set_toast(crate::i18n::tr(self.locale, "toast.saved"));
            }
            Err(ClientError::Rpc(crate::wire::rpc::RpcError::SettingsConflict { .. })) => {
                state.loading = true;
                self.set_toast(crate::i18n::tr(self.locale, "toast.conflict_refreshed"));
                self.fetch_settings(event_tx);
            }
            Err(error) => {
                self.set_toast(crate::i18n::trf(
                    self.locale,
                    "toast.save_failed",
                    &[&error.to_string()],
                ));
            }
        }
    }

    /// Apply a loaded history page for the CURRENT active session only. A
    /// late result for a session the user already switched away from is
    /// dropped silently (stale guard: no store write, no hint touch) — the
    /// newest switch's load wins. A failure for the active session toasts.
    fn on_history_loaded(
        &mut self,
        session_id: SessionId,
        result: Result<SessionHistoryValue, ClientError>,
    ) {
        if self.active_session.as_ref() != Some(&session_id) {
            return; // stale result for a session we already left
        }
        self.history_loading = None;
        self.hint = None;
        match result {
            Ok(history) => {
                let entries = history
                    .events
                    .into_iter()
                    .map(|entry| (entry.event, entry.view))
                    .collect();
                if let Err(error) = self.store.ingest_history(&session_id, entries) {
                    self.last_error = Some(error.to_string());
                }
            }
            Err(error) => self.set_toast(crate::i18n::trf(
                self.locale,
                "toast.history_failed",
                &[&error.to_string()],
            )),
        }
    }

    /// #43: cache the active session's model selection (stale-guarded —
    /// a result for a session we already left is dropped, keeping the
    /// cleared-by-switch cache honest). A failed fetch leaves the cache
    /// unchanged (still `None` after a switch — the segments hide).
    fn on_models_loaded(
        &mut self,
        session_id: SessionId,
        result: Result<SessionModelsValue, ClientError>,
    ) {
        if self.active_session.as_ref() != Some(&session_id) {
            return; // stale result for a session we already left
        }
        if let Ok(value) = result {
            self.session_model = Some(value.current);
        }
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
                    ApprovalResponseOutcome::AllowedOnce => {
                        crate::i18n::tr(self.locale, "toast.allowed_once")
                    }
                    ApprovalResponseOutcome::Rejected => {
                        crate::i18n::tr(self.locale, "toast.rejected")
                    }
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

    /// The two status-line clusters (#11): left = session · seq · mode with
    /// muted `·` separators (plus the transient hint/toast/error text),
    /// right = one colored state indicator — `⠋` accent spinner (running),
    /// `●` success (idle), `△` warning (truncated history), `✕` error.
    fn status_line(&self, theme: &Theme) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let body = |text: String| Span::styled(text, Style::default().fg(theme.text));
        let mut parts: Vec<Span<'static>> = Vec::new();
        // #41: the session id moved to the header — the empty state keeps
        // its "no session" left-cluster text.
        if self.active_session.is_none() {
            parts.push(body(
                crate::i18n::tr(self.locale, "status.no_session").into(),
            ));
        }
        let mut truncated = false;
        if let Some(state) = self
            .active_session
            .as_ref()
            .and_then(|session_id| self.store.session(session_id))
        {
            parts.push(body(crate::i18n::trf(
                self.locale,
                "status.seq",
                &[&state.last_seq.to_string()],
            )));
            truncated = state.truncated;
        }
        parts.push(body(crate::i18n::trf(
            self.locale,
            "status.focus",
            &[self.focus.label(self.locale)],
        )));
        // #39: the metrics-bar cluster on status line 2 — the full web
        // header metric set. Each segment omits when its underlying data
        // is absent from the retained window (no fabricated timings):
        // turns/steps from the folded nodes; LLM duration from in-window
        // TurnStart→TurnEnd pairs; tool duration from settled call→result
        // times; TTFT from the first chunk of each measurable turn; tok/s
        // guards the div-by-zero; cache hit from the disjoint input/cache
        // counts; input/output with K/M compaction.
        if let Some(state) = self
            .active_session
            .as_ref()
            .and_then(|session_id| self.store.session(session_id))
        {
            let stats = crate::store::session_stats(state);
            let mut segments: Vec<String> = Vec::new();
            if stats.turns > 0 {
                segments.push(format!(
                    "{} · {}",
                    crate::i18n::trf(self.locale, "stats.turns", &[&stats.turns.to_string()],),
                    crate::i18n::trf(self.locale, "stats.steps", &[&stats.steps.to_string()]),
                ));
            }
            // The web's pairings: `LLM x · Tool call y` and
            // `TTFT avg z · N tok/s` — each member hides independently.
            let mut timing: Vec<String> = Vec::new();
            if stats.measured_turns > 0 {
                timing.push(crate::i18n::trf(
                    self.locale,
                    "stats.llm",
                    &[&crate::render::chat_view::format_duration_compact(
                        stats.llm_seconds,
                    )],
                ));
            }
            if stats.measured_tools > 0 {
                timing.push(crate::i18n::trf(
                    self.locale,
                    "stats.tool",
                    &[&crate::render::chat_view::format_duration_compact(
                        stats.tool_seconds,
                    )],
                ));
            }
            if !timing.is_empty() {
                segments.push(timing.join(" · "));
            }
            let mut latency: Vec<String> = Vec::new();
            if let Some(ttft) = stats.ttft_seconds {
                latency.push(crate::i18n::trf(
                    self.locale,
                    "stats.ttft",
                    &[&format!("{ttft:.1}")],
                ));
            }
            if let Some(tps) = crate::store::tokens_per_second(&stats) {
                latency.push(crate::i18n::trf(
                    self.locale,
                    "stats.tok_s",
                    &[&format!("{tps:.0}")],
                ));
            }
            if !latency.is_empty() {
                segments.push(latency.join(" · "));
            }
            // Cache hit = cache reads / (cache reads + uncached input) —
            // the disjoint-count definition of the wire.
            let cache_base = stats.input_tokens + stats.cache_read_tokens;
            if cache_base > 0 {
                let pct = stats.cache_read_tokens * 100 / cache_base;
                // The template carries the `%` — pass the bare number.
                segments.push(crate::i18n::trf(
                    self.locale,
                    "stats.cache",
                    &[&pct.to_string()],
                ));
            }
            if stats.input_tokens + stats.output_tokens > 0 {
                segments.push(format!(
                    "{} · {}",
                    crate::i18n::trf(
                        self.locale,
                        "stats.input",
                        &[&crate::render::chat_view::format_tokens(stats.input_tokens)],
                    ),
                    crate::i18n::trf(
                        self.locale,
                        "stats.output",
                        &[&crate::render::chat_view::format_tokens(
                            stats.output_tokens
                        )],
                    ),
                ));
            }
            if !segments.is_empty() {
                parts.push(body(segments.join(" | ")));
            }
        }
        if let Some(hint) = &self.hint {
            parts.push(Span::styled(hint.clone(), style::hint(theme)));
        }
        if let Some((toast, _)) = &self.toast {
            parts.push(Span::styled(toast.clone(), style::hint(theme)));
        }
        if let Some(error) = &self.last_error {
            parts.push(Span::styled(
                crate::i18n::trf(self.locale, "status.error", &[error]),
                Style::default().fg(theme.error),
            ));
        }
        // Muted ` · ` separators between the left-cluster parts.
        let mut left = Vec::with_capacity(parts.len() * 2 - 1);
        for (i, span) in parts.into_iter().enumerate() {
            if i > 0 {
                left.push(Span::styled(" · ", style::hint(theme)));
            }
            left.push(span);
        }
        let indicator = if self.last_error.is_some() {
            ("✕", Style::default().fg(theme.error))
        } else if self.session_running() {
            // #15: the animated braille spinner (the frame advances per
            // tick while busy; idle draws nothing).
            (
                SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()],
                style::active(theme),
            )
        } else if truncated {
            ("△", style::warning(theme))
        } else {
            ("●", Style::default().fg(theme.success))
        };
        // #12: the `copied · N chars` flash replaces the indicator for its
        // ~2s lifetime (a status-line flash — no toast system). #38: while
        // running, the busy-elapsed time renders after the spinner, muted
        // like the ` · ` separators (idle draws nothing extra).
        let right = if let Some((text, at)) = &self.copied_flash
            && at.elapsed() < crate::app::COPY_FLASH_TTL
        {
            vec![Span::styled(
                text.clone(),
                Style::default().fg(theme.success),
            )]
        } else if let Some(since) = self.busy_since {
            vec![
                Span::styled(indicator.0, indicator.1),
                Span::styled(" · ", style::hint(theme)),
                Span::styled(format_elapsed(since.elapsed()), style::hint(theme)),
            ]
        } else {
            vec![Span::styled(indicator.0, indicator.1)]
        };
        (left, right)
    }

    /// #38/#43: status line 1's segments — `Model: <model> | Effort:
    /// <effort> | Context: <abs tok> · <pct>% used`, as one body-styled
    /// cluster (raw ` | ` separators, per the web header's form). Each
    /// segment hides independently: model/effort absent until the
    /// `session.models` fetch lands (or the gateway lacks it); context
    /// absent until the session reports usage (the context segment shows
    /// with or without a window — the `request/context` event often never
    /// arrives, so a window-less session still renders `Context: N tok`).
    /// The "Full access" permission badge is deliberately absent —
    /// verified not on the wire. `Vec::new()` (the caller skips the row).
    fn status_meta_line(&self, theme: &Theme) -> Vec<Span<'static>> {
        let body = |text: String| Span::styled(text, Style::default().fg(theme.text));
        let mut segments: Vec<String> = Vec::new();
        // #43: the permission segment rides FIRST (the web header's
        // `Full access | Model: …` order), from the active session's
        // `permissions` projection (`currentValue`). Display-only — no
        // RPC, no /permission execution; hidden when the projection is
        // absent (older gateways) or its value is unparseable.
        if let Some(label) = self.active_summary().and_then(permission_label) {
            segments.push(label);
        }
        if let Some(selection) = &self.session_model {
            segments.push(crate::i18n::trf(
                self.locale,
                "status1.model",
                &[&selection.model],
            ));
            if let Some(effort) = &selection.reasoning_effort {
                segments.push(crate::i18n::trf(self.locale, "status1.effort", &[effort]));
            }
        }
        if let Some(state) = self
            .active_session
            .as_ref()
            .and_then(|session_id| self.store.session(session_id))
        {
            let stats = crate::store::session_stats(state);
            let used = stats.input_tokens + stats.cache_read_tokens;
            if used > 0 {
                if let Some(window) = stats.context_window
                    && window > 0
                {
                    let pct = (used as f64 * 100.0 / window as f64).floor() as i64;
                    // The template carries the `%` — pass the bare numbers.
                    segments.push(crate::i18n::trf(
                        self.locale,
                        "status1.context",
                        &[
                            &crate::render::chat_view::format_tokens_abs(used),
                            &pct.to_string(),
                        ],
                    ));
                } else {
                    // No window (the `request/context` event never
                    // arrived): the usage-only form.
                    segments.push(crate::i18n::trf(
                        self.locale,
                        "status1.context_used",
                        &[&crate::render::chat_view::format_tokens_abs(used)],
                    ));
                }
            }
        }
        if segments.is_empty() {
            Vec::new()
        } else {
            vec![body(segments.join(" | "))]
        }
    }

    /// The active session's summary row (the sidebar list — summaries
    /// carry the projections block, where the #43 permission value and the
    /// #41 title/preset live). `None` with no active session or when the
    /// list hasn't seen it yet.
    fn active_summary(&self) -> Option<&SessionSummary> {
        let active = self.active_session.as_ref()?;
        self.sessions
            .iter()
            .find(|summary| &summary.session_id == active)
    }

    /// #41: the session header line — `Session: <title> | Agent preset:
    /// <preset> | Background jobs: <n>` — built from the active session's
    /// summary (title projection, else the id) and the store's live
    /// running-job count. Segments omit individually: a summary without
    /// an agent preset (older gateways), or zero running jobs, drops the
    /// segment. `None` with no active session — the caller's header area
    /// is zero-rowed there anyway.
    fn header_line(&self) -> Option<String> {
        let summary = self.active_summary()?;
        let title =
            crate::ui::sidebar::title_of(summary).unwrap_or_else(|| summary.session_id.0.clone());
        let mut segments = vec![crate::i18n::trf(self.locale, "header.session", &[&title])];
        if let Some(preset) = &summary.agent_preset {
            segments.push(crate::i18n::trf(self.locale, "header.preset", &[preset]));
        }
        if let Some(state) = self
            .active_session
            .as_ref()
            .and_then(|session_id| self.store.session(session_id))
            && let Some(jobs) = state.running_jobs
            && jobs > 0
        {
            segments.push(crate::i18n::trf(
                self.locale,
                "header.jobs",
                &[&jobs.to_string()],
            ));
        }
        Some(segments.join(" | "))
    }
}

/// #43: the permission display label for a projection `currentValue` —
/// `danger-full-access` special-cases to "Full access" (the web's label);
/// every other kebab value title-cases (`read-only` → `Read Only`,
/// `workspace-write` → `Workspace Write`, unknown modes degrade the same
/// way — never hidden for an unknown value, only for a missing one).
pub fn permission_display(value: &str) -> String {
    if value == "danger-full-access" {
        return "Full access".into();
    }
    value
        .split('-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// #43: the active session's permission label from its `permissions`
/// projection (`values["permissions"].currentValue` — the projection rides
/// every `session.list` row on the live gateway; the TUI was discarding
/// it). `None` when the projection, its `currentValue`, or the summary
/// itself is absent (older gateways) — the status segment hides.
pub fn permission_label(summary: &SessionSummary) -> Option<String> {
    let current = summary
        .projections
        .as_ref()?
        .values
        .get("permissions")?
        .get("currentValue")?
        .as_str()?;
    let label = permission_display(current);
    if label.is_empty() {
        None // an empty currentValue renders no segment
    } else {
        Some(label)
    }
}

/// RAII restore of the raw-mode/alternate-screen terminal state. Create it
/// right after `enable_raw_mode` + `EnterAlternateScreen`; Drop restores on
/// normal exit and on panic. Mouse capture and bracketed paste are disabled
/// BEFORE raw mode off (the inverse of setup).
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        use crossterm::event::{
            DisableBracketedPaste, DisableMouseCapture, PopKeyboardEnhancementFlags,
        };
        use crossterm::execute;
        use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
        let _ = execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            PopKeyboardEnhancementFlags,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

/// Production terminal setup: raw mode, alternate screen, mouse capture,
/// bracketed paste, and the CSI-u keyboard enhancement that makes
/// Shift+Enter distinct from plain Enter (`composer.newline`;
/// `DISAMBIGUATE_ESCAPE_CODES` alone — the minimal surface). Legacy
/// terminals ignore the push and Shift+Enter degrades to submit, exactly
/// as before.
pub fn setup_terminal()
-> Result<Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>, AppError> {
    use crossterm::event::{
        EnableBracketedPaste, EnableMouseCapture, KeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    };
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

/// Production terminal teardown (explicit; `TerminalGuard` covers panics).
pub fn teardown_terminal() {
    let _ = TerminalGuard;
}
