//! The trajectory view (#44): an agent-run timeline ledger, web-parity with
//! the session view's Trajectory tab. It folds the SAME retained event
//! window the chat node list derives from ([`crate::store::SessionState::events`])
//! — no separate retention: the widget is a pure function of the store.
//!
//! Shape of the web ledger: a header row (Duration / Actual time + counters:
//! turns, calls, input, model, tools), then TOOL rows (`name args`), their
//! `→` result summaries, and ASSISTANT rows (text snippets), grouped into
//! turn/step headers, finishing with a "Load earlier history" pager row. A
//! gap marker sits at the ledger's top when the retained window was evicted
//! at the head ([`crate::store::SessionState::truncated`]).

use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::i18n::{Locale, tr, trf};
use crate::store::SessionStore;
use crate::store::event_data::{ContentBlock, EventData};
use crate::theme::Theme;
use crate::ui::style;
use crate::wire::session::SessionId;

/// One ledger row (the trajectory "page" is a flat list of these).
#[derive(Debug, Clone, PartialEq)]
pub enum TrajectoryRow {
    /// The retained window's head was evicted (events older than
    /// `oldest_seq` are gone).
    Gap,
    /// `turn/start`: a new turn's header.
    Turn { turn: i64 },
    /// `step/start`: a step's sub-header.
    Step { step: i64 },
    /// `tool/call`: `TOOL name args` (args truncated at render).
    Tool { name: String, args: String },
    /// `tool/result`: the `→` result summary (text blocks, truncated).
    ToolResult { text: String },
    /// `assistant/message`: `ASSISTANT` + the message's text snippet.
    Assistant { text: String },
    /// The web's "Load earlier history" pager (static in v1 — history
    /// pagination is a later lane).
    LoadEarlier,
}

/// Fold the retained event window into ledger rows (pure; shared by the
/// widget and the app's scroll clamping). Always ends with the web's
/// "Load earlier history" pager row.
pub fn ledger_rows(store: &SessionStore, session_id: &SessionId) -> Vec<TrajectoryRow> {
    let mut rows: Vec<TrajectoryRow> = Vec::new();
    let Some(state) = store.session(session_id) else {
        rows.push(TrajectoryRow::LoadEarlier);
        return rows;
    };
    if state.truncated {
        rows.push(TrajectoryRow::Gap);
    }
    for stored in state.events() {
        match &stored.data {
            EventData::TurnStart { turn } => rows.push(TrajectoryRow::Turn { turn: *turn }),
            EventData::StepStart { step, .. } => rows.push(TrajectoryRow::Step { step: *step }),
            EventData::ToolCall {
                name, arguments, ..
            } => rows.push(TrajectoryRow::Tool {
                name: name.clone(),
                args: arguments.trim().to_string(),
            }),
            EventData::ToolResult { message, .. } => {
                let text = message
                    .tool_result_block()
                    .and_then(|block| match block {
                        ContentBlock::ToolResult { content, .. } => text_snippet(content),
                        _ => None,
                    })
                    .unwrap_or_default();
                rows.push(TrajectoryRow::ToolResult { text });
            }
            EventData::AssistantMessage { message, .. } => rows.push(TrajectoryRow::Assistant {
                text: text_snippet(&message.content).unwrap_or_default(),
            }),
            // Intermediate chunks, prompts, todo/request bookkeeping, and
            // unknown events have no ledger row (the web ledger shows the
            // settled tool/assistant rows only).
            _ => {}
        }
    }
    rows.push(TrajectoryRow::LoadEarlier);
    rows
}

/// The first text blocks of `content`, single-space joined (whitespace
/// trimmed) — the row snippet.
fn text_snippet(blocks: &[ContentBlock]) -> Option<String> {
    let texts: Vec<String> = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                Some(text.trim().to_string())
            }
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join(" "))
    }
}

/// The aggregate header counters, as (turns, calls, input_tokens, model, tools).
fn counts(
    store: &SessionStore,
    session_id: &SessionId,
) -> (usize, usize, i64, Option<String>, usize) {
    let Some(state) = store.session(session_id) else {
        return (0, 0, 0, None, 0);
    };
    let mut turns = 0usize;
    let mut calls = 0usize;
    let mut input = 0i64;
    let mut model = None;
    let mut tools = 0usize;
    for stored in state.events() {
        match &stored.data {
            EventData::TurnStart { .. } => turns += 1,
            EventData::AssistantMessage { usage, .. } => {
                calls += 1;
                if let Some(usage) = usage {
                    input += usage.input_tokens;
                }
            }
            EventData::RequestContext {
                model: model_name, ..
            } => model = Some(model_name.clone()),
            EventData::ToolCall { .. } => tools += 1,
            _ => {}
        }
    }
    (turns, calls, input, model, tools)
}

/// The window's wall-clock span (last event time − first), for the
/// "Duration / Actual time" header; 0 when the window is empty.
fn duration(store: &SessionStore, session_id: &SessionId) -> f64 {
    let Some(state) = store.session(session_id) else {
        return 0.0;
    };
    let events = state.events();
    let Some(first) = events.first().map(|s| s.event.time) else {
        return 0.0;
    };
    let last = events.iter().map(|s| s.event.time).fold(first, f64::max);
    (last - first).max(0.0)
}

/// `1m 04s` / `3.2s` — the web header's duration format.
fn format_duration(secs: f64) -> String {
    if secs >= 60.0 {
        let total = secs as u64;
        format!("{}m {:02}s", total / 60, total % 60)
    } else {
        format!("{secs:.1}s")
    }
}

/// Truncate `text` to `max` cells with a `…` ellipsis (CJK-safe: cut by
/// cell width, never splitting a wide char).
fn truncate(text: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
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

/// The trajectory ledger: header lines (title + duration, counters), then
/// the fold of the retained event window, honoring the scroll offset.
pub struct TrajectoryView<'a> {
    pub store: &'a SessionStore,
    pub session_id: &'a SessionId,
    /// Scroll offset in ledger rows (the app's `view.offset`).
    pub offset: usize,
    pub theme: &'a Theme,
    pub locale: Locale,
}

/// Rows above the ledger body: the title line and the counters line
/// (the blank top spacer mirrors ChatView's 1-row inset).
pub const HEADER_ROWS: u16 = 3;

impl Widget for TrajectoryView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height < HEADER_ROWS + 1 || area.width == 0 {
            return;
        }
        let inner = area.inner(Margin {
            horizontal: 2,
            vertical: 0,
        });
        let (turns, calls, input, model, tools) = counts(self.store, self.session_id);
        let all = ledger_rows(self.store, self.session_id);
        // Clamp the offset to the last visible row.
        let visible = area.height.saturating_sub(HEADER_ROWS) as usize;
        let first = self
            .offset
            .min(all.len().saturating_sub(visible.max(1)))
            .min(all.len());
        // The title line: `trajectory · Duration / Actual time: X`.
        let duration = format_duration(duration(self.store, self.session_id));
        let title = Line::from(vec![
            Span::styled(
                format!(" {} ", tr(self.locale, "trajectory.title")),
                style::active(self.theme),
            ),
            Span::styled(
                format!(
                    " {}: {duration}",
                    tr(self.locale, "trajectory.header_duration")
                ),
                style::hint(self.theme),
            ),
        ]);
        buf.set_line(inner.x, inner.y, &title, inner.width);
        // The counters line: `Turns 3 · Calls 5 · …` (bold labels, plain
        // values — the model segment drops out when unknown).
        let model = model.unwrap_or_default();
        let mut counters = Vec::new();
        let mut push = |label: &'static str, value: String| {
            if !counters.is_empty() {
                counters.push(Span::raw(" · "));
            }
            counters.push(Span::styled(
                format!("{label} {value}"),
                Style::default()
                    .fg(self.theme.text)
                    .add_modifier(Modifier::BOLD),
            ));
        };
        push(
            tr(self.locale, "trajectory.header_turns"),
            turns.to_string(),
        );
        push(
            tr(self.locale, "trajectory.header_calls"),
            calls.to_string(),
        );
        push(
            tr(self.locale, "trajectory.header_input"),
            input.to_string(),
        );
        if !model.is_empty() {
            push(tr(self.locale, "trajectory.header_model"), model);
        }
        push(
            tr(self.locale, "trajectory.header_tools"),
            tools.to_string(),
        );
        buf.set_line(inner.x, inner.y + 1, &Line::from(counters), inner.width);
        // blank spacer row
        for (row, y) in all.iter().skip(first).zip(inner.y + 2..) {
            if y >= inner.bottom() {
                break;
            }
            let line = match row {
                TrajectoryRow::Gap => Line::styled(
                    tr(self.locale, "trajectory.gap"),
                    style::warning(self.theme),
                ),
                TrajectoryRow::Turn { turn } => Line::styled(
                    format!(
                        "▸ {}",
                        trf(self.locale, "trajectory.turn", &[&turn.to_string()])
                    ),
                    style::header(self.theme),
                ),
                TrajectoryRow::Step { step } => Line::styled(
                    format!(
                        "  {}",
                        trf(self.locale, "trajectory.step", &[&step.to_string()])
                    ),
                    style::hint(self.theme),
                ),
                TrajectoryRow::Tool { name, args } => {
                    let body = if args.is_empty() {
                        name.clone()
                    } else {
                        format!("{name} {args}")
                    };
                    Line::from(vec![
                        Span::styled(
                            format!("{} ", tr(self.locale, "trajectory.tool")),
                            style::active(self.theme),
                        ),
                        Span::styled(
                            truncate(&body, inner.width.saturating_sub(6) as usize),
                            Style::default().fg(self.theme.text),
                        ),
                    ])
                }
                TrajectoryRow::ToolResult { text } => Line::from(vec![
                    Span::styled("→ ", style::active(self.theme)),
                    Span::styled(
                        truncate(text, inner.width.saturating_sub(2) as usize),
                        Style::default().fg(self.theme.text),
                    ),
                ]),
                TrajectoryRow::Assistant { text } => Line::from(vec![
                    Span::styled(
                        format!("{} ", tr(self.locale, "trajectory.assistant")),
                        style::active(self.theme),
                    ),
                    Span::styled(
                        truncate(text, inner.width.saturating_sub(11) as usize),
                        Style::default().fg(self.theme.text),
                    ),
                ]),
                TrajectoryRow::LoadEarlier => Line::styled(
                    tr(self.locale, "trajectory.load_earlier"),
                    style::hint(self.theme),
                ),
            };
            buf.set_line(inner.x, y, &line, inner.width);
        }
    }
}
