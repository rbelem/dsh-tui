//! The first-run onboarding (#47): a two-question Q&A — the workspace
//! directory path and the agent preset — rendered as a full-screen
//! takeover, mirroring the web's first-run flow. The state machine lives
//! here (pure), the keys on `App::handle_onboarding_key` (they need the
//! config + the run loop's RPC dispatch), and the completion writes the
//! `onboarding_complete` flag back to the config.
//!
//! #50: the Workspace question is an fzf-style picker. The candidate list
//! assembles the gateway's existing workspaces (updated_at desc) on top and
//! the zoxide-recent directories (`zoxide query -l`, recency order) below,
//! deduped by path (workspace wins); typing filters it with the launcher's
//! fuzzy scorer, j/k move the highlight, and Enter commits — the selected
//! candidate, else the typed path, else the current working directory. The
//! zoxide list is fetched lazily ONCE on the first Workspace-step touch
//! ([`OnboardingState::ensure_zoxide`]); tests inject the dirs + mark it
//! fetched, so they never shell out.

use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Widget, Wrap};

use crate::i18n::{Locale, tr, trf};
use crate::theme::Theme;
use crate::ui::composer::Composer;
use crate::ui::style;
use crate::wire::workspace::WorkspaceView;

/// Which question the flow is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnboardingStep {
    /// Workspace directory path (the fzf-style picker, #50).
    #[default]
    Workspace,
    /// Orchestrator agent preset name (a text field; may be left empty).
    Preset,
}

// ---------------------------------------------------------------------------
// the candidate picker (#50)
// ---------------------------------------------------------------------------

/// The gateway workspaces' paths, ordered by `updated_at` descending (the
/// picker's top section). `updated_at` is an ISO timestamp, which compares
/// lexicographically; ties keep the input order (stable sort).
pub fn workspace_paths_sorted(workspaces: &[WorkspaceView]) -> Vec<String> {
    let mut sorted = workspaces.to_vec();
    sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sorted.into_iter().map(|ws| ws.path).collect()
}

/// Assemble the candidate list as (workspaces, recents), both deduped: the
/// workspace section (already updated_at-desc) on top, the zoxide-recent
/// section (recency order) below, deduped by path — a workspace wins over a
/// zoxide duplicate (first occurrence kept).
pub fn candidate_sections(
    workspace_paths: &[String],
    zoxide: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut seen: HashSet<&str> = HashSet::new();
    let workspaces: Vec<String> = workspace_paths
        .iter()
        .filter(|path| seen.insert(path.as_str()))
        .cloned()
        .collect();
    let recents: Vec<String> = zoxide
        .iter()
        .filter(|path| seen.insert(path.as_str()))
        .cloned()
        .collect();
    (workspaces, recents)
}

/// The full flat candidate list (workspaces then recents) for the picker.
pub fn assemble_candidates(workspaces: &[WorkspaceView], zoxide: &[String]) -> Vec<String> {
    let workspace_paths = workspace_paths_sorted(workspaces);
    let (ws, recent) = candidate_sections(&workspace_paths, zoxide);
    ws.into_iter().chain(recent).collect()
}

/// The fzf filter over one candidate section: subsequence matches of
/// `needle`, ranked by the launcher's [`crate::ui::launcher::fuzzy_score`]
/// (higher first; ties keep the section order). The empty needle keeps the
/// whole section in order.
pub fn filter_candidates(section: &[String], needle: &str) -> Vec<String> {
    let mut scored: Vec<(u32, &String)> = section
        .iter()
        .filter_map(|path| {
            crate::ui::launcher::fuzzy_score(needle, path).map(|score| (score, path))
        })
        .collect();
    // Higher score first; a stable sort keeps ties in section order.
    scored.sort_by(|(a, _), (b, _)| b.cmp(a));
    scored.into_iter().map(|(_, path)| path.clone()).collect()
}

/// The onboarding Q&A state: the current step, the two inline editors (the
/// preset is optional — an empty Enter skips it), and the #50 picker
/// inputs. `Mode` derives `Clone`/`PartialEq`, but a `Composer` is neither —
/// implement them manually: equality is the step, a clone starts fresh
/// editors (only ever used by `Mode`'s derives, never during the flow).
#[derive(Debug)]
pub struct OnboardingState {
    pub step: OnboardingStep,
    pub path_editor: Composer,
    pub preset_editor: Composer,
    /// #50: the gateway workspace paths (updated_at desc) — the picker's
    /// top section. Seeded at entry; injectable for tests.
    pub workspace_paths: Vec<String>,
    /// #50: the zoxide-recent directories (recency order), fetched lazily
    /// once the Workspace step is first touched; injectable for tests.
    pub zoxide_dirs: Vec<String>,
    /// #50: whether zoxide was already fetched (guards the lazy shell-out;
    /// tests inject dirs + set this to stay hermetic).
    pub zoxide_fetched: bool,
    /// #50: the current working directory — Enter with a blank path and no
    /// highlighted candidate commits it. Read once at entry; injectable.
    pub cwd: String,
    /// #50: the picker's highlighted row (index into the combined filtered
    /// list: workspaces then recents).
    pub selection: usize,
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
    /// The bare default (no candidates) — the widget/unit-test path; never
    /// shells out.
    pub fn new() -> Self {
        OnboardingState {
            step: OnboardingStep::Workspace,
            path_editor: Composer::new(),
            preset_editor: Composer::new(),
            workspace_paths: Vec::new(),
            zoxide_dirs: Vec::new(),
            zoxide_fetched: false,
            cwd: String::new(),
            selection: 0,
        }
    }

    /// Every picker input injected: the workspace paths (already
    /// updated_at-desc), the zoxide-recent dirs (recency order), and the
    /// cwd. Marks zoxide as fetched — the hermetic/test path, nothing
    /// shells out.
    pub fn with_candidates(
        workspace_paths: Vec<String>,
        zoxide_dirs: Vec<String>,
        cwd: String,
    ) -> Self {
        OnboardingState {
            step: OnboardingStep::Workspace,
            path_editor: Composer::new(),
            preset_editor: Composer::new(),
            workspace_paths,
            zoxide_dirs,
            zoxide_fetched: true,
            cwd,
            selection: 0,
        }
    }

    /// The app-entry path: the workspace paths + the real cwd; zoxide is
    /// fetched lazily on the first Workspace-step touch.
    pub fn for_workspaces(workspace_paths: Vec<String>, cwd: String) -> Self {
        OnboardingState {
            zoxide_fetched: false,
            ..Self::with_candidates(workspace_paths, Vec::new(), cwd)
        }
    }

    /// Fetch the zoxide-recent list once (guarded by `zoxide_fetched`); a
    /// missing binary or a failing run yields an empty list — the graceful
    /// fallback (workspaces-only + manual typing + the cwd default).
    pub fn ensure_zoxide(&mut self) {
        if self.zoxide_fetched {
            return;
        }
        self.zoxide_fetched = true;
        self.zoxide_dirs = fetch_zoxide();
    }

    /// The picker's visible rows for the current editor needle: (workspace
    /// matches, recent matches), each filtered and score-ranked.
    pub fn visible_candidates(&self) -> (Vec<String>, Vec<String>) {
        let (workspaces, recents) = candidate_sections(&self.workspace_paths, &self.zoxide_dirs);
        let needle = self.path_editor.buffer();
        (
            filter_candidates(&workspaces, needle),
            filter_candidates(&recents, needle),
        )
    }

    /// The combined visible row count (the selection's clamp).
    pub fn visible_len(&self) -> usize {
        let (workspaces, recents) = self.visible_candidates();
        workspaces.len() + recents.len()
    }

    /// The highlighted visible candidate's path, when the selection is in
    /// range.
    pub fn selected_path(&self) -> Option<String> {
        let (workspaces, recents) = self.visible_candidates();
        if self.selection < workspaces.len() {
            return workspaces.get(self.selection).cloned();
        }
        recents.get(self.selection - workspaces.len()).cloned()
    }

    /// The path Enter commits on the Workspace step: the highlighted
    /// candidate, else the typed path (trimmed), else the cwd.
    pub fn workspace_commit_path(&self) -> String {
        if let Some(path) = self.selected_path() {
            return path;
        }
        let typed = self.path_editor.buffer().trim();
        if !typed.is_empty() {
            return typed.to_string();
        }
        self.cwd.clone()
    }
}

/// `zoxide query -l`: the recency-ordered recent-directory list. Errors
/// and a missing binary yield an empty list (graceful fallback).
fn fetch_zoxide() -> Vec<String> {
    let Ok(output) = std::process::Command::new("zoxide")
        .arg("query")
        .arg("-l")
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// the takeover view
// ---------------------------------------------------------------------------

/// The onboarding takeover view: a bordered panel with the question, the
/// inline editor, the #50 candidate picker on the Workspace step, and the
/// hints.
pub struct OnboardingView<'a> {
    pub state: &'a OnboardingState,
    /// The transient notice (a toast/hint) rendered ABOVE the action line
    /// (a full-screen takeover renders it like the settings view).
    pub notice: Option<&'a str>,
    pub theme: &'a Theme,
    pub locale: Locale,
}

/// The picker's styled rows plus the combined row index of the selection
/// (for the scroll window). Section headers render above their matches.
fn picker_rows<'a>(
    state: &'a OnboardingState,
    locale: Locale,
    theme: &'a Theme,
) -> (Vec<Line<'static>>, usize) {
    let (workspaces, recents) = state.visible_candidates();
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut selected_row = 0usize;
    let mut combined = 0usize;
    for (section, header) in [
        (&workspaces, "onboarding.picker_workspaces"),
        (&recents, "onboarding.picker_recent"),
    ] {
        if section.is_empty() {
            continue;
        }
        rows.push(Line::styled(
            format!(" {} ", tr(locale, header)),
            style::hint(theme),
        ));
        for path in section {
            let selected = state.selection == combined;
            if selected {
                selected_row = rows.len();
            }
            rows.push(Line::from(vec![
                Span::styled(if selected { "▸ " } else { "  " }, style::active(theme)),
                Span::styled(
                    path.clone(),
                    Style::default().fg(theme.text).add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ),
            ]));
            combined += 1;
        }
    }
    (rows, selected_row)
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
        body.push(Line::raw(""));

        // #50: the picker list on the Workspace step, windowed to the rows
        // the pane has left after the fixed lines, scrolled to keep the
        // selection visible.
        if self.state.step == OnboardingStep::Workspace {
            let (rows, selected_row) = picker_rows(self.state, self.locale, self.theme);
            let fixed = 5 + usize::from(self.notice.is_some()); // q + editor + blank + hint + skip
            let budget = (inner.height as usize).saturating_sub(fixed).max(1);
            let start = if selected_row >= budget {
                selected_row - budget + 1
            } else {
                0
            };
            for row in rows.iter().skip(start).take(budget) {
                body.push(row.clone());
            }
            body.push(Line::raw(""));
            body.push(Line::styled(
                trf(self.locale, "onboarding.cwd_default", &[&self.state.cwd]),
                style::hint(self.theme),
            ));
        }
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
        let height = inner.height;
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
        render_at(state, locale, 60, 40)
    }

    fn render_at(state: &OnboardingState, locale: Locale, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
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
    fn clone_default_and_eq_still_work() {
        let a = OnboardingState::new();
        assert_eq!(a, OnboardingState::default(), "Default == new");
        let cloned = a.clone(); // a fresh editor, same step
        assert_eq!(a, cloned, "PartialEq: step equal");
        let mut preset = OnboardingState::new();
        preset.step = OnboardingStep::Preset;
        assert_ne!(a, preset, "PartialEq: step differs");
    }

    fn ws(path: &str, updated_at: &str) -> WorkspaceView {
        WorkspaceView {
            workspace_id: crate::wire::session::WorkspaceId(format!("w-{path}")),
            path: path.into(),
            title: path.into(),
            session_ids: vec![],
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: updated_at.into(),
        }
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

    // ------------------------------------------------------------------
    // #50: the picker
    // ------------------------------------------------------------------

    fn picker_state() -> OnboardingState {
        OnboardingState::with_candidates(
            vec!["/ws/alpha".into(), "/ws/beta".into()],
            vec!["/home/u/one".into(), "/home/u/two".into()],
            "/home/u".into(),
        )
    }

    #[test]
    fn workspace_paths_sorted_orders_by_updated_at_desc() {
        let workspaces = vec![
            ws("/old", "2026-01-01T00:00:00Z"),
            ws("/new", "2026-02-01T00:00:00Z"),
            ws("/mid", "2026-01-15T00:00:00Z"),
        ];
        assert_eq!(
            workspace_paths_sorted(&workspaces),
            vec!["/new", "/mid", "/old"]
        );
    }

    #[test]
    fn assemble_puts_workspaces_on_top_zoxide_below_and_dedupes() {
        // `/dup` appears in BOTH sections — the workspace wins, the zoxide
        // duplicate drops.
        let workspaces = vec![
            ws("/dup", "2026-01-01T00:00:00Z"),
            ws("/ws-a", "2026-01-02T00:00:00Z"),
        ];
        let zoxide: Vec<String> = vec!["/home/u/recent".into(), "/dup".into()];
        let assembled = assemble_candidates(&workspaces, &zoxide);
        assert_eq!(
            assembled,
            vec!["/ws-a", "/dup", "/home/u/recent"],
            "workspaces (updated_at desc) then zoxide, deduped"
        );
        // The per-section split keeps the same dedupe.
        let (ws_sec, rec_sec) = candidate_sections(&workspace_paths_sorted(&workspaces), &zoxide);
        assert_eq!(ws_sec, vec!["/ws-a", "/dup"]);
        assert_eq!(rec_sec, vec!["/home/u/recent"], "zoxide dup dropped");
    }

    #[test]
    fn filter_empty_keeps_order_and_ranks_matches() {
        let section = vec![
            "/tmp/ws".to_string(),
            "/w-s/other".to_string(),
            "/zzz".to_string(),
        ];
        // Empty needle: everything, original order.
        assert_eq!(filter_candidates(&section, ""), section);
        // Consecutive runs rank above scattered matches; non-matches drop.
        let ranked = filter_candidates(&section, "ws");
        assert_eq!(
            ranked,
            vec!["/tmp/ws", "/w-s/other"],
            "adjacent `ws` outranks the scattered `w…s`"
        );
        // No subsequence match → empty.
        assert!(filter_candidates(&section, "qqq").is_empty());
    }

    #[test]
    fn commit_path_prefers_selection_then_typed_then_cwd() {
        // With candidates visible, the highlighted one wins (blank editor →
        // the first workspace).
        let state = picker_state();
        assert_eq!(state.selected_path(), Some("/ws/alpha".into()));
        assert_eq!(state.workspace_commit_path(), "/ws/alpha");

        // A needle with no matches: the typed path wins.
        let mut state = picker_state();
        for c in "office".chars() {
            state.path_editor.insert_char(c);
        }
        state.selection = 0;
        assert_eq!(state.selected_path(), None);
        assert_eq!(state.workspace_commit_path(), "office");

        // Blank + no candidates at all: the cwd.
        let state = OnboardingState::with_candidates(vec![], vec![], "/home/u".into());
        assert_eq!(state.workspace_commit_path(), "/home/u");
    }

    #[test]
    fn render_shows_sections_and_highlights_the_selection() {
        let state = picker_state();
        let view = render(&state, Locale::En);
        assert!(view.contains("ws"), "a workspace path: {view}");
        assert!(
            view.contains("/home/u/recent") || view.contains("/home/u/one"),
            "a recent path"
        );
        assert!(view.contains("workspaces"), "workspaces header: {view}");
        assert!(view.contains("recent"), "recent header: {view}");

        // The selection row carries the `▸` marker before its path.
        let marker = view.find("▸").expect("selection marker");
        let alpha = view.find("/ws/alpha").expect("first workspace");
        assert!(marker < alpha, "marker sits on the selected row: {view}");

        // Moving the selection onto a recent row moves the marker there.
        let mut state = picker_state();
        state.selection = 2; // past both workspaces → the first recent
        let view = render(&state, Locale::En);
        let marker = view.find("▸").expect("selection marker");
        let recent = view.find("/home/u/one").expect("first recent");
        assert!(marker < recent, "marker on the recent row: {view}");
    }

    #[test]
    fn cwd_hint_renders_on_the_workspace_step() {
        let state = picker_state();
        let view = render(&state, Locale::En);
        assert!(view.contains("blank"), "cwd hint present: {view}");
    }

    #[test]
    fn picker_scrolls_to_keep_the_selection_visible() {
        // Many candidates in a short pane: the window scrolls so the
        // selection stays on screen and the scrolled-past head drops off.
        let zoxide: Vec<String> = (0..25).map(|i| format!("/z/recent-{i:02}")).collect();
        let mut state = OnboardingState::with_candidates(Vec::new(), zoxide, "/home/u".into());
        state.selection = 22;
        let view = render_at(&state, Locale::En, 40, 12);
        // 40×12 → inner height 10, budget = 10 − 5 = 5 rows: the selection
        // (row 22) forces start ≥ 18 — the head rows are gone.
        assert!(
            view.contains("/z/recent-22"),
            "selected row visible: {view}"
        );
        assert!(
            !view.contains("/z/recent-00"),
            "scrolled-past head dropped: {view}"
        );
    }
}
