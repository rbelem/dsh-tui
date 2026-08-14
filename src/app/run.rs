//! The main loop and draw (Q3): single-threaded store + draw, coalesced at
//! ~16ms, plus the raw-mode/alternate-screen lifecycle.

use std::time::Instant;

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use crate::app::event::AppEvent;
use crate::app::{Action, App, AppError, DRAW_INTERVAL};
use crate::render::chat_view::ChatView;

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
        events: &mut mpsc::UnboundedReceiver<AppEvent>,
    ) -> Result<(), AppError>
    where
        B: Backend,
        B::Error: Into<AppError>,
    {
        let mut tick = tokio::time::interval(DRAW_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                maybe = events.recv() => {
                    match maybe {
                        Some(AppEvent::Key(key)) => {
                            if self.handle_key(key) == Some(Action::Quit) {
                                self.running = false;
                                break;
                            }
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::Frame(frame)) => {
                            match self.store.ingest(frame) {
                                Ok(()) => {}
                                Err(error) => self.last_error = Some(error.to_string()),
                            }
                            self.needs_draw = true;
                            self.draw_if_due(term, false)?;
                        }
                        Some(AppEvent::Resize(_width, height)) => {
                            // Q10: width change → full re-render.
                            self.view.viewport_height = height;
                            self.row_cache.invalidate_all();
                            self.needs_draw = true;
                            self.draw_if_due(term, true)?;
                        }
                        Some(AppEvent::Tick) => self.draw_if_due(term, false)?,
                        None => break,
                    }
                }
                _ = tick.tick() => self.draw_if_due(term, false)?,
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

    /// Sync the row cache, apply follow, and render: chat area = full minus
    /// one status line.
    fn draw<B>(&mut self, term: &mut Terminal<B>) -> Result<(), B::Error>
    where
        B: Backend,
    {
        let size = term.size()?;
        let chat_height = size.height.saturating_sub(1);
        let width = size.width;
        self.view.viewport_height = chat_height;

        let session_id = self.active_session.clone();
        if let Some(session_id) = &session_id {
            self.row_cache.sync(&self.store, session_id, width);
            self.row_cache.render_dirty(&self.store, session_id, width);
            if self.view.follow {
                let total = self.row_cache.lines().len();
                self.view.offset = total.saturating_sub(chat_height as usize);
            }
        }
        let status = self.status_line();
        let offset = self.view.offset;

        term.draw(|frame| {
            let [chat_area, status_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
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
            frame.render_widget(Paragraph::new(status), status_area);
        })?;

        self.last_draw = Some(Instant::now());
        self.needs_draw = false;
        self.draws += 1;
        Ok(())
    }

    /// The one-line status: session id · last seq · truncated flag · error.
    fn status_line(&self) -> Line<'static> {
        let mut parts: Vec<String> = Vec::new();
        match &self.active_session {
            Some(session_id) => parts.push(format!("session {session_id}")),
            None => parts.push("no session".into()),
        }
        if let Some(state) = self
            .active_session
            .as_ref()
            .and_then(|session_id| self.store.session(session_id))
        {
            parts.push(format!("seq {}", state.last_seq));
            if state.truncated {
                parts.push("truncated".into());
            }
        }
        if let Some(error) = &self.last_error {
            parts.push(format!("error: {error}"));
        }
        Line::raw(parts.join(" · "))
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
