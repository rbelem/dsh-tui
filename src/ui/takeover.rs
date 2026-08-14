//! Full-screen takeovers for answerable frames (ticket 05 Q6/Q13): the
//! approval prompt and the question prompt. A takeover replaces the whole
//! three-region layout — one dim-bordered body, calm and focused, with a
//! clear action line at the bottom. There is no "dismiss": the server waits
//! for a response, so every takeover ends in an answer or a resolution.
//!
//! Key routing: while a takeover is open, ALL keys go to it — chat,
//! composer, and sidebar keys are inert (`q` does not quit; `Ctrl+C` stays
//! the global panic-button quit, the one documented exception).
//!
//! v1 scope (documented TODOs): one takeover at a time — a new approval
//! replaces the current one (the rest stay pending and are promoted when
//! the displayed one resolves; a proper pending queue is a later lane);
//! allow-always-preset and the full-access risk-ack second keypress need
//! the permissions projection (not yet in the store); questions have no
//! cancel (Esc is a no-op with a hint).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};

use crate::i18n::{Locale, tr, trf};
use crate::ui::style;
use crate::wire::approvals::ApprovalRequestId;
use crate::wire::events::{AskUserQuestionItem, QuestionIntent, QuestionOption};
use crate::wire::rpc::RpcId;
use crate::wire::session::SessionId;

/// The app mode: the normal three-region chat, or a full-screen takeover.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Mode {
    #[default]
    Chat,
    Approval(ApprovalTakeover),
    Question(QuestionTakeover),
    /// The two-pane settings view (Ctrl+, opens, Esc closes).
    Settings(crate::ui::settings::SettingsState),
}

/// The displayed approval: the `approval/requested` frame's fields plus the
/// echo target and the matching tool call's one-line summary from the store
/// (when the call id is known there).
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalTakeover {
    pub session_id: SessionId,
    pub approval_id: ApprovalRequestId,
    /// The envelope rpcId of the requested frame — the respond echo target.
    pub rpc_id: RpcId,
    pub tool_name: String,
    pub call_id: Option<String>,
    pub reason: Option<String>,
    /// One-line summary of the matching tool node (`name args…`), when the
    /// store has the call.
    pub tool_summary: Option<String>,
    /// An answer is in flight: further answer keys are ignored, the action
    /// line shows a "sending…" hint.
    pub sending: bool,
}

/// One question's interaction state: its options (synthesized for a bare
/// plan-review intent — v1 heuristic: approve text + "Refuse"), the cursor,
/// and the multi-select toggles. A single-select question's answer is its
/// cursor row (the cursor IS the selection — no toggle needed).
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionState {
    pub item: AskUserQuestionItem,
    pub options: Vec<QuestionOption>,
    pub cursor: usize,
    pub selected: Vec<usize>,
    pub multi: bool,
}

impl QuestionState {
    pub fn new(item: AskUserQuestionItem) -> Self {
        let options = item.options.clone().unwrap_or_else(|| {
            match &item.intent {
                // A plan review without options answers with the intent's
                // approve text or a refusal (web: Approve/Refuse/Chat; the
                // chat reply is the composer, not an option).
                Some(QuestionIntent::PlanReview { approve }) => vec![
                    QuestionOption {
                        label: approve.clone(),
                        description: None,
                    },
                    QuestionOption {
                        label: "Refuse".into(),
                        description: None,
                    },
                ],
                None => Vec::new(),
            }
        });
        QuestionState {
            multi: item.multi_select == Some(true),
            item,
            options,
            cursor: 0,
            selected: Vec::new(),
        }
    }

    /// Move the cursor over the options (clamped).
    pub fn move_cursor(&mut self, delta: isize) {
        if self.options.is_empty() {
            return;
        }
        let last = self.options.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    /// Space: toggle the cursor row (multi-select only; single-select follows
    /// the cursor, so there is nothing to toggle).
    pub fn toggle(&mut self) {
        if !self.multi {
            return;
        }
        match self.selected.iter().position(|&i| i == self.cursor) {
            Some(position) => {
                self.selected.remove(position);
            }
            None => self.selected.push(self.cursor),
        }
    }

    /// The answer's `selected` slot: the option LABELS (the wire carries
    /// strings, not indices).
    pub fn selected_labels(&self) -> Vec<String> {
        if self.options.is_empty() {
            return Vec::new();
        }
        if self.multi {
            let mut selected = self.selected.clone();
            selected.sort_unstable();
            return selected
                .into_iter()
                .map(|i| self.options[i].label.clone())
                .collect();
        }
        vec![
            self.options[self.cursor.min(self.options.len() - 1)]
                .label
                .clone(),
        ]
    }
}

/// The displayed question frame: one or more questions answered together
/// (`Tab` cycles the focused question; `Enter` submits all).
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionTakeover {
    pub session_id: SessionId,
    /// The envelope rpcId of the requested frame — the respond echo target
    /// (`question/resolved.questionRpcId` names the same value).
    pub rpc_id: RpcId,
    pub questions: Vec<QuestionState>,
    /// The focused question (cursor + Space apply to it).
    pub focused: usize,
    /// An answer is in flight (see [`ApprovalTakeover::sending`]).
    pub sending: bool,
}

impl QuestionTakeover {
    pub fn new(session_id: SessionId, rpc_id: RpcId, questions: Vec<AskUserQuestionItem>) -> Self {
        QuestionTakeover {
            session_id,
            rpc_id,
            questions: questions.into_iter().map(QuestionState::new).collect(),
            focused: 0,
            sending: false,
        }
    }

    /// Tab: cycle the focused question.
    pub fn focus_next(&mut self) {
        if !self.questions.is_empty() {
            self.focused = (self.focused + 1) % self.questions.len();
        }
    }

    /// Whether any question rides the plan-review intent (the takeover title
    /// says "plan review" then).
    fn plan_review(&self) -> bool {
        self.questions.iter().any(|question| {
            matches!(
                question.item.intent,
                Some(QuestionIntent::PlanReview { .. })
            )
        })
    }
}

/// The approval takeover body (full-screen).
pub struct ApprovalView<'a> {
    pub takeover: &'a ApprovalTakeover,
    /// Transient notice (toast/hint), rendered dim at the bottom.
    pub notice: Option<&'a str>,
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl Widget for ApprovalView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title(tr(self.locale, "takeover.approval"))
            .padding(ratatui::widgets::Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        let takeover = self.takeover;
        let mut lines: Vec<Line> = vec![
            Line::raw(""),
            Line::styled(&takeover.tool_name, style::active(self.theme)),
        ];
        if let Some(reason) = &takeover.reason {
            lines.push(Line::raw(trf(self.locale, "approval.reason", &[reason])));
        }
        lines.push(Line::raw(""));
        let mut context = trf(
            self.locale,
            "approval.context",
            &[takeover.session_id.as_ref()],
        );
        if let Some(call_id) = &takeover.call_id {
            context.push_str(&trf(self.locale, "approval.context_call", &[call_id]));
        }
        lines.push(Line::styled(context, style::hint(self.theme)));
        if let Some(summary) = &takeover.tool_summary {
            lines.push(Line::styled(
                trf(self.locale, "approval.tool_call", &[summary]),
                style::hint(self.theme),
            ));
        }
        // TODO: allow-always-preset + the full-access risk-ack second
        // keypress (Q13) — needs the permissions projection in the store.
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("y", style::active(self.theme)),
            Span::raw(tr(self.locale, "takeover.allow_once")),
            Span::styled("n", style::active(self.theme)),
            Span::raw(tr(self.locale, "takeover.reject")),
        ]));
        if let Some(notice) = self.notice {
            lines.push(Line::styled(notice, style::hint(self.theme)));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }
}

/// The question takeover body (full-screen).
pub struct QuestionView<'a> {
    pub takeover: &'a QuestionTakeover,
    /// Transient notice (toast/hint), rendered dim at the bottom.
    pub notice: Option<&'a str>,
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl Widget for QuestionView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = if self.takeover.plan_review() {
            tr(self.locale, "takeover.plan_review")
        } else {
            tr(self.locale, "takeover.question")
        };
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title(title)
            .padding(ratatui::widgets::Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        let takeover = self.takeover;
        let many = takeover.questions.len() > 1;
        let mut lines: Vec<Line> = vec![Line::raw("")];
        for (i, question) in takeover.questions.iter().enumerate() {
            let focused = i == takeover.focused;
            if many {
                let label = trf(
                    self.locale,
                    "question.counter",
                    &[&(i + 1).to_string(), &takeover.questions.len().to_string()],
                );
                lines.push(Line::styled(label, style::hint(self.theme)));
            }
            if let Some(header) = &question.item.header {
                lines.push(Line::styled(header, style::active(self.theme)));
            }
            lines.push(Line::raw(&question.item.question));
            if let Some(detail) = &question.item.detail {
                lines.push(Line::styled(detail, style::hint(self.theme)));
            }
            if question.options.is_empty() {
                lines.push(Line::styled(
                    tr(self.locale, "question.no_options"),
                    style::hint(self.theme),
                ));
            }
            for (row, option) in question.options.iter().enumerate() {
                let marker = if question.multi {
                    if question.selected.contains(&row) {
                        "[x] "
                    } else {
                        "[ ] "
                    }
                } else if focused && row == question.cursor {
                    "› "
                } else {
                    "  "
                };
                let mut spans = vec![Span::raw(format!("{marker}{}", option.label))];
                if let Some(description) = &option.description {
                    spans.push(Span::styled(
                        format!(" — {description}"),
                        style::hint(self.theme),
                    ));
                }
                lines.push(Line::from(spans));
                // Reverse the cursor row of the focused question.
                if focused && row == question.cursor {
                    let y = inner.y + lines.len() as u16 - 1;
                    if y < inner.bottom() {
                        buf.set_style(
                            Rect::new(inner.x, y, inner.width, 1),
                            style::selection(self.theme),
                        );
                    }
                }
            }
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(vec![
            Span::styled("tab", style::active(self.theme)),
            Span::raw(tr(self.locale, "question.action_tab")),
            Span::styled("space", style::active(self.theme)),
            Span::raw(tr(self.locale, "question.action_toggle")),
            Span::styled("enter", style::active(self.theme)),
            Span::raw(tr(self.locale, "question.action_submit")),
        ]));
        if let Some(notice) = self.notice {
            lines.push(Line::styled(notice, style::hint(self.theme)));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(options: Option<Vec<QuestionOption>>, multi: Option<bool>) -> AskUserQuestionItem {
        AskUserQuestionItem {
            id: "q1".into(),
            question: "pick one".into(),
            header: None,
            detail: None,
            options,
            multi_select: multi,
            intent: None,
        }
    }

    fn opt(label: &str) -> QuestionOption {
        QuestionOption {
            label: label.into(),
            description: None,
        }
    }

    #[test]
    fn single_select_answers_with_the_cursor_row() {
        let mut question = QuestionState::new(item(Some(vec![opt("a"), opt("b"), opt("c")]), None));
        assert_eq!(question.selected_labels(), vec!["a".to_string()]);
        question.move_cursor(2);
        assert_eq!(question.selected_labels(), vec!["c".to_string()]);
        question.move_cursor(1);
        assert_eq!(question.selected_labels(), vec!["c".to_string()], "clamped");
        // Space is a no-op on single-select.
        question.toggle();
        assert_eq!(question.selected_labels(), vec!["c".to_string()]);
    }

    #[test]
    fn multi_select_toggles_labels() {
        let mut question =
            QuestionState::new(item(Some(vec![opt("a"), opt("b"), opt("c")]), Some(true)));
        question.toggle();
        question.move_cursor(2);
        question.toggle();
        assert_eq!(
            question.selected_labels(),
            vec!["a".to_string(), "c".to_string()]
        );
        question.toggle();
        assert_eq!(question.selected_labels(), vec!["a".to_string()]);
    }

    #[test]
    fn bare_plan_review_synthesizes_approve_and_refuse() {
        let mut question = item(None, None);
        question.intent = Some(QuestionIntent::PlanReview {
            approve: "Ship it".into(),
        });
        let question = QuestionState::new(question);
        assert_eq!(
            question
                .options
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Ship it", "Refuse"]
        );
    }

    #[test]
    fn tab_cycles_questions() {
        let mut takeover = QuestionTakeover::new(
            SessionId("s1".into()),
            RpcId("rpc-1".into()),
            vec![item(None, None), item(None, None)],
        );
        takeover.focus_next();
        assert_eq!(takeover.focused, 1);
        takeover.focus_next();
        assert_eq!(takeover.focused, 0, "wraps");
    }
}
