//! The sidebar: the session list from the gateway, grouped by workspace.
//!
//! Layout (#11): a full-height left pane with NO box — it is separated from
//! the chat by the `panel_bg` fill vs the main `bg` (plus the top-level
//! 1-cell spacing gap). Content: the "Sessions" header, one blank row, the
//! groups (header = workspace title, sessions nested under it), then an
//! "ungrouped" group for sessions no workspace claims, then the archived
//! group at the foot — collapsed to a "archived (N)" header by default (its
//! sessions are listed but hidden — out of j/k navigation); `e` in the
//! sidebar expands it for the app lifetime (no persistence) and its sessions
//! render as rows. With no workspaces and no archived sessions the list
//! renders FLAT (no group headers) — exactly the pre-grouping look. A muted
//! `dsh-tui` footer line anchors the bottom.
//!
//! Rows: the active session carries a bold `●` marker at all times; the
//! selection row is bold with an accent `▎` stripe, but only while the
//! sidebar has focus (an unfocused sidebar shows no selection — there is
//! nothing to operate on). Running sessions get a dim `· running` suffix.
//!
//! The list is the attach flow's `session.list` + `workspace.list`
//! snapshots plus live host-stream updates (`App::handle_host_frame`).
//! Selection is session-space: `SidebarState::selected` counts only
//! visible session rows (group headers are never selectable, and j/k
//! crosses group boundaries as if they weren't there).

use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::i18n::{Locale, tr, trf};
use crate::ui::composer::Composer;
use crate::ui::style;
use crate::wire::session::{SessionId, SessionSummary, WorkspaceId};
use crate::wire::workspace::WorkspaceView;

/// Sidebar pane width; the pane is permanent only at the wide tier (≥80
/// columns) — at 60–79 and below it renders as the `s`-toggled drawer
/// overlay instead (a 12-col icon strip is illegible and eats 20% of the
/// screen; the drawer shows full titles on demand).
pub const SIDEBAR_WIDTH: u16 = 22;

/// Sidebar width for a terminal `total` columns wide (tiered, #19): the
/// permanent 22-col pane at ≥80, nothing below (the drawer replaces it).
pub fn sidebar_width(total: u16) -> u16 {
    if total >= 80 { SIDEBAR_WIDTH } else { 0 }
}

/// One sidebar group: a header (workspace title or an i18n label) plus the
/// sessions under it, as indices into the app's session list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarGroup {
    /// Header text; `None` in flat mode (no header row rendered).
    pub title: Option<String>,
    /// Collapsed groups render only the header; their sessions are hidden
    /// and excluded from navigation (the archived group, v1).
    pub collapsed: bool,
    /// The archived group (the footer group): its header click toggles
    /// `archived_expanded` (the only collapsible group in the v1 model).
    pub is_archived: bool,
    /// Member sessions, as indices into `SidebarView::sessions`.
    pub sessions: Vec<usize>,
}

impl SidebarGroup {
    /// Visible (navigable) session count across `groups`.
    pub fn visible_len(groups: &[SidebarGroup]) -> usize {
        groups
            .iter()
            .filter(|group| !group.collapsed)
            .map(|group| group.sessions.len())
            .sum()
    }

    /// The session-list index of the `selected`-th visible session.
    pub fn visible_session(groups: &[SidebarGroup], selected: usize) -> Option<usize> {
        let mut rest = selected;
        for group in groups.iter().filter(|group| !group.collapsed) {
            if rest < group.sessions.len() {
                return Some(group.sessions[rest]);
            }
            rest -= group.sessions.len();
        }
        None
    }
}

/// Derive the sidebar's group model from the app state:
///
/// - workspaces in display order (`workspace_order` first, unlisted
///   workspaces appended) — a session belongs to the FIRST workspace that
///   claims its id, empty workspace groups are skipped;
/// - then the ungrouped group (non-archived sessions no workspace claims);
/// - then the archived group (collapsed unless `archived_expanded` —
///   app-lifetime state, no persistence — its sessions then render as
///   rows and join j/k navigation; the count stays in the header).
///
/// No workspaces and no archived sessions → a single title-less group:
/// the flat look. Archived sessions always land in the archived group,
/// even when a workspace also lists them.
pub fn build_groups(
    sessions: &[SessionSummary],
    workspaces: &[WorkspaceView],
    workspace_order: &[WorkspaceId],
    archived: &[SessionId],
    locale: Locale,
    archived_expanded: bool,
) -> Vec<SidebarGroup> {
    if sessions.is_empty() {
        return Vec::new();
    }
    let is_archived = |index: usize| archived.contains(&sessions[index].session_id);
    let live: Vec<usize> = (0..sessions.len())
        .filter(|index| !is_archived(*index))
        .collect();
    let archived_group_sessions: Vec<usize> = (0..sessions.len())
        .filter(|index| is_archived(*index))
        .collect();

    if workspaces.is_empty() && archived_group_sessions.is_empty() {
        // Flat mode: the pre-grouping look, one title-less group.
        return vec![SidebarGroup {
            title: None,
            collapsed: false,
            is_archived: false,
            sessions: (0..sessions.len()).collect(),
        }];
    }

    let mut groups = Vec::new();
    let mut claimed = vec![false; sessions.len()];
    let index_of: std::collections::HashMap<&SessionId, usize> = sessions
        .iter()
        .enumerate()
        .map(|(index, summary)| (&summary.session_id, index))
        .collect();
    // Workspace display order: the durable order first (ids that still
    // exist), then any workspace the order frame doesn't know about.
    let mut ordered: Vec<&WorkspaceView> = workspace_order
        .iter()
        .filter_map(|id| workspaces.iter().find(|ws| &ws.workspace_id == id))
        .collect();
    ordered.extend(
        workspaces
            .iter()
            .filter(|ws| !workspace_order.contains(&ws.workspace_id)),
    );
    for workspace in ordered {
        // Members in the workspace's OWN session_ids order (the host
        // maintains it — workspaceInsertSessionBefore), first claim wins.
        let members: Vec<usize> = workspace
            .session_ids
            .iter()
            .filter_map(|id| index_of.get(id).copied())
            .filter(|index| !is_archived(*index) && !claimed[*index])
            .collect();
        for index in &members {
            claimed[*index] = true;
        }
        if members.is_empty() {
            continue;
        }
        groups.push(SidebarGroup {
            title: Some(workspace.title.clone()),
            collapsed: false,
            is_archived: false,
            sessions: members,
        });
    }

    let ungrouped: Vec<usize> = live
        .iter()
        .copied()
        .filter(|index| !claimed[*index])
        .collect();
    if !ungrouped.is_empty() {
        groups.push(SidebarGroup {
            title: Some(tr(locale, "sidebar.ungrouped").to_string()),
            collapsed: false,
            is_archived: false,
            sessions: ungrouped,
        });
    }
    if !archived_group_sessions.is_empty() {
        groups.push(SidebarGroup {
            title: Some(trf(
                locale,
                "sidebar.archived",
                &[&archived_group_sessions.len().to_string()],
            )),
            collapsed: !archived_expanded,
            is_archived: true,
            sessions: archived_group_sessions,
        });
    }
    groups
}

/// Sidebar interaction state (the list itself lives on `App::sessions`).
#[derive(Debug, Default)]
pub struct SidebarState {
    /// Selected VISIBLE session index (session-space over non-collapsed
    /// groups; group headers are never selectable).
    pub selected: usize,
}

impl SidebarState {
    /// Move the selection by `delta` sessions (clamped to the list).
    pub fn move_by(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        let last = len.saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    pub fn first(&mut self) {
        self.selected = 0;
    }

    pub fn last(&mut self, len: usize) {
        self.selected = len.saturating_sub(1);
    }

    /// Keep the selection inside the visible session count (live updates
    /// shrink the list: session-removed, workspace-removed reflow, archive).
    pub fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
    }
}

/// One display row: a group header or a session row. Shared by the renderer
/// and the mouse hit-testing (#12): a click on a `Header` toggles that
/// group's collapse; a click on a `Session` selects it.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayRow {
    /// A group header: the group's index into the sidebar's group list
    /// (the title renders from `groups[i].title`).
    Header(usize),
    /// A session row: index into `SidebarView::sessions`, and its ordinal
    /// in session-space (for the selection highlight).
    Session { index: usize, ordinal: usize },
}

/// Flatten `groups` into display rows and compute the scroll window's first
/// visible row. The window is selection-driven (the selected row stays
/// visible, sitting at the edge while scrolling), so the mouse hit-test
/// reproduces exactly what the renderer draws. `inner_height` is the pane's
/// content height (the "Sessions" header row, the blank row under it, and
/// the footer line are outside the window).
pub fn display_layout(
    groups: &[SidebarGroup],
    selected: usize,
    inner_height: u16,
) -> (Vec<DisplayRow>, usize) {
    let mut rows: Vec<DisplayRow> = Vec::new();
    let mut ordinal = 0;
    let mut selected_display = 0;
    for (group_index, group) in groups.iter().enumerate() {
        if group.title.is_some() {
            rows.push(DisplayRow::Header(group_index));
        }
        if group.collapsed {
            continue;
        }
        for index in &group.sessions {
            if ordinal == selected {
                selected_display = rows.len();
            }
            rows.push(DisplayRow::Session {
                index: *index,
                ordinal,
            });
            ordinal += 1;
        }
    }
    let visible = inner_height.saturating_sub(3) as usize;
    let start = selected_display.saturating_sub(visible.saturating_sub(1));
    (rows, start)
}

/// The sidebar widget: header + groups of session rows.
pub struct SidebarView<'a> {
    pub sessions: &'a [SessionSummary],
    pub groups: &'a [SidebarGroup],
    pub active: Option<&'a SessionId>,
    pub selected: usize,
    pub focused: bool,
    /// The inline rename editor (`r`): while open it replaces the selected
    /// session row with an editable line (mirrors the queue editor).
    pub editor: Option<&'a Composer>,
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl Widget for SidebarView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        // No block (#11): the pane is separated by background contrast and
        // the top-level spacing gap; the `panel_bg` fill marks the surface.
        // With the Reset default theme the fill is a no-op.
        buf.set_style(area, style::panel_fill(self.theme));
        let inner = area.inner(Margin {
            horizontal: 2,
            vertical: 0,
        });
        if inner.width == 0 {
            return;
        }
        // The "Sessions" header, then 1 blank row; rows start at inner.y+2.
        buf.set_line(
            inner.x,
            inner.y,
            &Line::styled(tr(self.locale, "sidebar.header"), style::hint(self.theme)),
            inner.width,
        );

        if self.sessions.is_empty() {
            let y = inner.y + 2;
            if y < inner.bottom() {
                buf.set_line(
                    inner.x,
                    y,
                    &Line::raw(tr(self.locale, "sidebar.empty")),
                    inner.width,
                );
            }
            if y + 1 < inner.bottom() {
                buf.set_line(
                    inner.x,
                    y + 1,
                    &Line::styled(
                        tr(self.locale, "sidebar.empty_hint"),
                        style::hint(self.theme),
                    ),
                    inner.width,
                );
            }
            self.render_footer(inner, buf);
            return;
        }

        // Flatten groups into display rows (shared with the mouse
        // hit-testing — a click must land on the same row the renderer
        // drew). The window is selection-driven.
        let (rows, start) = display_layout(self.groups, self.selected, inner.height);
        let grouped = self.groups.iter().any(|group| group.title.is_some());
        for (row_index, row) in rows.iter().enumerate().skip(start) {
            let line_index = row_index - start + 2;
            if line_index as u16 >= inner.height.saturating_sub(1) {
                break; // the footer row is reserved
            }
            let y = inner.y + line_index as u16;
            match row {
                DisplayRow::Header(group_index) => {
                    let title = self.groups[*group_index]
                        .title
                        .as_deref()
                        .unwrap_or_default();
                    buf.set_line(
                        inner.x,
                        y,
                        &Line::styled(title, style::hint(self.theme)),
                        inner.width,
                    );
                }
                DisplayRow::Session { index, ordinal } => {
                    let summary = &self.sessions[*index];
                    let selected = self.focused && *ordinal == self.selected;
                    // The rename editor replaces the selected row while
                    // open (mirrors the queue editor's inline line).
                    if let Some(editor) = self.editor
                        && selected
                    {
                        let line = Line::from(vec![
                            Span::styled("▎", style::selection_stripe(self.theme)),
                            Span::styled(" > ", style::active(self.theme)),
                            Span::styled(
                                if editor.buffer().is_empty() {
                                    tr(self.locale, "sidebar.rename_hint").to_string()
                                } else {
                                    editor.buffer().to_string()
                                },
                                style::active(self.theme),
                            ),
                            Span::styled("|", style::hint(self.theme)),
                        ]);
                        buf.set_line(inner.x, y, &line, inner.width);
                        continue;
                    }
                    if selected {
                        // #11: bold + accent `▎` stripe (no REVERSED) —
                        // state carried by glyph shape + weight.
                        buf.set_style(
                            Rect::new(inner.x, y, inner.width, 1),
                            style::selection(self.theme),
                        );
                    }
                    let is_active = self.active == Some(&summary.session_id);
                    // Grouped sessions nest one space under their header;
                    // flat mode keeps the pre-grouping column exactly. The
                    // selection stripe occupies the leading cell, keeping
                    // the label column aligned across rows.
                    let marker = match (grouped, is_active) {
                        (true, true) => " ● ",
                        (true, false) => "   ",
                        (false, true) => "● ",
                        (false, false) => "  ",
                    };
                    let mut spans = vec![if selected {
                        Span::styled("▎", style::selection_stripe(self.theme))
                    } else {
                        Span::raw(" ")
                    }];
                    spans.push(Span::styled(marker, style::active(self.theme)));
                    // Session titles in text; session-ids muted; the active
                    // session's title is bold (hierarchy by weight).
                    let label_style = match title_of(summary) {
                        Some(_) => {
                            Style::default()
                                .fg(self.theme.text)
                                .add_modifier(if is_active {
                                    Modifier::BOLD
                                } else {
                                    Modifier::empty()
                                })
                        }
                        None => style::hint(self.theme),
                    };
                    spans.push(Span::styled(label(summary), label_style));
                    if summary.running {
                        spans.push(Span::styled(
                            tr(self.locale, "sidebar.running"),
                            style::hint(self.theme),
                        ));
                    }
                    buf.set_line(inner.x, y, &Line::from(spans), inner.width);
                }
            }
        }
        self.render_footer(inner, buf);
    }
}

impl SidebarView<'_> {
    /// The muted `dsh-tui` footer anchoring the bottom of the pane.
    fn render_footer(&self, inner: Rect, buf: &mut Buffer) {
        if inner.height >= 1 {
            buf.set_line(
                inner.x,
                inner.bottom() - 1,
                &Line::styled("dsh-tui", style::hint(self.theme)),
                inner.width,
            );
        }
    }
}

/// The session title projection, when present (non-blank); `None` when the
/// row falls back to the session id.
fn title_of(summary: &SessionSummary) -> Option<String> {
    let title = summary
        .projections
        .as_ref()
        .and_then(|block| block.values.get("title"))
        .and_then(|value| {
            // The title projection rides both shapes on the wire: a bare
            // string, or the rename snapshot `{title, seq}`.
            value
                .as_str()
                .or_else(|| value.get("title").and_then(|v| v.as_str()))
        });
    title
        .filter(|title| !title.trim().is_empty())
        .map(str::to_string)
}

/// Row label: the session title projection when present, else the id.
fn label(summary: &SessionSummary) -> String {
    match title_of(summary) {
        Some(title) => title,
        None => summary.session_id.0.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str) -> SessionSummary {
        SessionSummary {
            session_id: SessionId(id.into()),
            updated_at: 0.0,
            running: false,
            blank: false,
            parent_session_id: None,
            origin: None,
            cwd: None,
            agent_preset: None,
            projections: None,
        }
    }

    fn workspace(id: &str, title: &str, session_ids: &[&str]) -> WorkspaceView {
        WorkspaceView {
            workspace_id: WorkspaceId(id.into()),
            path: format!("/tmp/{id}"),
            title: title.into(),
            session_ids: session_ids
                .iter()
                .map(|id| SessionId((*id).into()))
                .collect(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn flat_when_no_workspaces_or_archived() {
        let sessions = vec![summary("s1"), summary("s2")];
        let groups = build_groups(&sessions, &[], &[], &[], Locale::En, false);
        assert_eq!(
            groups,
            vec![SidebarGroup {
                title: None,
                collapsed: false,
                is_archived: false,
                sessions: vec![0, 1],
            }]
        );
    }

    #[test]
    fn groups_in_workspace_order_then_ungrouped_then_archived() {
        let sessions: Vec<_> = ["s1", "s2", "s3", "s4", "s5"]
            .into_iter()
            .map(summary)
            .collect();
        let workspaces = vec![
            workspace("wA", "alpha", &["s2"]),
            // beta claims in its OWN order (s3 before s1) — the sidebar
            // honors workspace.session_ids, not the session list order.
            workspace("wB", "beta", &["s3", "s1"]),
        ];
        // The durable order puts beta first; s4 is ungrouped, s5 archived.
        let order = vec![WorkspaceId("wB".into()), WorkspaceId("wA".into())];
        let archived = vec![SessionId("s5".into())];
        let groups = build_groups(&sessions, &workspaces, &order, &archived, Locale::En, false);
        let titles: Vec<_> = groups.iter().map(|g| g.title.as_deref()).collect();
        assert_eq!(
            titles,
            vec![
                Some("beta"),
                Some("alpha"),
                Some("ungrouped"),
                Some("▸ archived (1)")
            ]
        );
        assert_eq!(groups[0].sessions, vec![2, 0]); // beta: s3, then s1
        assert_eq!(groups[1].sessions, vec![1]); // alpha: s2
        assert_eq!(groups[2].sessions, vec![3]); // ungrouped: s4
        assert!(groups[3].collapsed);
        assert_eq!(groups[3].sessions, vec![4]); // archived: s5
        // Navigation sees only the non-collapsed rows.
        assert_eq!(SidebarGroup::visible_len(&groups), 4);
        assert_eq!(SidebarGroup::visible_session(&groups, 1), Some(0));
        assert_eq!(SidebarGroup::visible_session(&groups, 2), Some(1));
        assert_eq!(SidebarGroup::visible_session(&groups, 3), Some(3));
        assert_eq!(SidebarGroup::visible_session(&groups, 4), None);
    }

    #[test]
    fn first_claiming_workspace_wins_and_empty_groups_drop() {
        let sessions = vec![summary("s1")];
        let workspaces = vec![
            workspace("wA", "alpha", &["s1"]),
            workspace("wB", "beta", &["s1"]), // also claims s1 — loses
            workspace("wC", "gamma", &[]),    // empty — dropped
        ];
        let groups = build_groups(&sessions, &workspaces, &[], &[], Locale::En, false);
        let titles: Vec<_> = groups.iter().map(|g| g.title.as_deref()).collect();
        assert_eq!(titles, vec![Some("alpha")]);
    }

    #[test]
    fn archived_overrides_workspace_membership() {
        let sessions = vec![summary("s1")];
        let workspaces = vec![workspace("wA", "alpha", &["s1"])];
        let archived = vec![SessionId("s1".into())];
        let groups = build_groups(&sessions, &workspaces, &[], &archived, Locale::En, false);
        let titles: Vec<_> = groups.iter().map(|g| g.title.as_deref()).collect();
        assert_eq!(titles, vec![Some("▸ archived (1)")]);
        assert_eq!(SidebarGroup::visible_len(&groups), 0);
    }

    #[test]
    fn archived_group_expands_into_navigation() {
        let sessions = vec![summary("s1"), summary("s2"), summary("s3")];
        let archived = vec![SessionId("s3".into())];
        let groups = build_groups(&sessions, &[], &[], &archived, Locale::En, true);
        assert_eq!(groups.len(), 2, "ungrouped + archived");
        assert!(!groups[1].collapsed, "expanded: the rows join navigation");
        assert_eq!(groups[1].title.as_deref(), Some("▸ archived (1)"));
        assert_eq!(SidebarGroup::visible_len(&groups), 3);
        assert_eq!(SidebarGroup::visible_session(&groups, 2), Some(2));
        // Collapsed again: the archived row drops out of navigation.
        let collapsed = build_groups(&sessions, &[], &[], &archived, Locale::En, false);
        assert!(collapsed[1].collapsed);
        assert_eq!(SidebarGroup::visible_len(&collapsed), 2);
        assert_eq!(SidebarGroup::visible_session(&collapsed, 2), None);
    }
}
