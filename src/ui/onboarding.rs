//! The first-run onboarding (#47): a minimal two-question Q&A — the
//! workspace directory path and the agent preset — rendered as a
//! full-screen takeover, mirroring the web's first-run flow. The state
//! machine lives here (pure), the keys on `App::handle_onboarding_key`
//! (they need the config + the run loop's RPC dispatch), and the
//! completion writes the `onboarding_complete` flag back to the config.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Widget, Wrap};

use crate::i18n::{Locale, tr};
use crate::theme::Theme;
use crate::ui::composer::Composer;
use crate::ui::style;

/// Which question the flow is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnboardingStep {
    /// Workspace directory path (Composer-style path entry).
    #[default]
    Workspace,
    /// Orchestrator agent preset name (a text field; may be left empty).
    Preset,
}

/// The onboarding Q&A state: the current step and the two inline editors
/// (the preset is optional — an empty Enter skips it). `Mode` derives
/// `Clone`/`PartialEq`, but a `Composer` is neither — implement them
/// manually: equality is the step, a clone starts fresh editors (only ever
/// used by `Mode`'s derives, never during the flow).
#[derive(Debug)]
pub struct OnboardingState {
    pub step: OnboardingStep,
    pub path_editor: Composer,
    pub preset_editor: Composer,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for OnboardingState {
    fn clone(&self) -> Self {
        OnboardingState::new()
    }
}

impl PartialEq for OnboardingState {
    fn eq(&self, other: &Self) -> bool {
        self.step == other.step
    }
}

impl OnboardingState {
    pub fn new() -> Self {
        OnboardingState {
            step: OnboardingStep::Workspace,
            path_editor: Composer::new(),
            preset_editor: Composer::new(),
        }
    }
}

/// The onboarding takeover view: a bordered panel with the question, the
/// inline path/preset editor, and the Esc hint.
pub struct OnboardingView<'a> {
    pub state: &'a OnboardingState,
    /// The transient notice (a toast/hint) rendered ABOVE the action line
    /// (a full-screen takeover renders it like the settings view).
    pub notice: Option<&'a str>,
    pub theme: &'a Theme,
    pub locale: Locale,
}

impl Widget for OnboardingView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title_style(
                Style::new()
                    .add_modifier(Modifier::BOLD)
                    .fg(self.theme.accent),
            )
            .title(tr(self.locale, "onboarding.title"));
        let inner = block.inner(area);
        block.render(area, buf);
        buf.set_style(inner, style::panel_fill(self.theme));

        // Center the question block vertically.
        let question = match self.state.step {
            OnboardingStep::Workspace => tr(self.locale, "onboarding.question_workspace"),
            OnboardingStep::Preset => tr(self.locale, "onboarding.question_preset"),
        };
        let mut body: Vec<Line> = Vec::new();
        body.push(Line::styled(
            question.to_string(),
            Style::default()
                .fg(self.theme.text)
                .add_modifier(Modifier::BOLD),
        ));
        // The Composer-styled inline editor (the `▎ >` prompt mirrors the
        // sidebar rename/path editors).
        let (editor, placeholder) = match self.state.step {
            OnboardingStep::Workspace => (
                self.state.path_editor.buffer(),
                tr(self.locale, "onboarding.placeholder_workspace"),
            ),
            OnboardingStep::Preset => (
                self.state.preset_editor.buffer(),
                tr(self.locale, "onboarding.placeholder"),
            ),
        };
        body.push(Line::from(vec![
            Span::styled("▎", style::selection_stripe(self.theme)),
            Span::styled(" > ", style::active(self.theme)),
            Span::styled(
                if editor.is_empty() {
                    placeholder.to_string()
                } else {
                    editor.to_string()
                },
                style::active(self.theme),
            ),
            Span::styled("|", style::hint(self.theme)),
        ]));
        // A filler line so the block has some air above the action line.
        body.push(Line::raw(""));
        body.push(Line::styled(
            tr(self.locale, "onboarding.skip").to_string(),
            style::hint(self.theme),
        ));
        if let Some(notice) = self.notice {
            body.push(Line::styled(notice.to_string(), style::warning(self.theme)));
        }

        let width = inner.width.saturating_sub(4).max(8);
        let para = Paragraph::new(body)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false });
        let height = 8.min(inner.height);
        let top = inner.y + inner.height.saturating_sub(height) / 2;
        let content = Rect {
            x: inner.x + inner.width.saturating_sub(width) / 2,
            y: top,
            width,
            height,
        };
        if content.width > 0 && content.height > 0 {
            para.render(content, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(state: &OnboardingState, locale: Locale) -> String {
        let backend = TestBackend::new(60, 12);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                OnboardingView {
                    state,
                    notice: None,
                    theme: &Theme::default(),
                    locale,
                },
                f.area(),
            )
        })
        .unwrap();
        format!("{}", term.backend())
    }

    #[test]
    fn first_step_asks_for_the_workspace_path() {
        let state = OnboardingState::new();
        let view = render(&state, Locale::En);
        assert!(view.contains("first run"), "title: {view}");
        assert!(
            view.contains("Where should the workspace live?"),
            "workspace question: {view}"
        );
        assert!(view.contains("workspace"), "path placeholder: {view}");
    }

    #[test]
    fn second_step_asks_for_the_preset() {
        let mut state = OnboardingState::new();
        state.path_editor = Composer::new();
        for c in "/tmp/ws".chars() {
            state.path_editor.insert_char(c);
        }
        state.step = OnboardingStep::Preset;
        let view = render(&state, Locale::En);
        assert!(
            view.contains("Which agent preset"),
            "preset question: {view}"
        );
        // The preset step shows its own (empty) editor with the generic
        // placeholder — the path question has moved on.
        assert!(view.contains("type"), "placeholder: {view}");
        assert!(!view.contains("/tmp/ws"), "path editor retired: {view}");
    }

    #[test]
    fn zh_renders() {
        let state = OnboardingState::new();
        let view = render(&state, Locale::Zh);
        assert!(view.contains("首次运行"), "zh title: {view}");
        assert!(view.contains("工作区"), "zh question: {view}");
    }
}
