//! The sidebar: the session list from the gateway, grouped by workspace.
//!
//! Layout: a full-height left pane separated from the chat by a single
//! vertical rule (no box). The first line is the "Sessions" header; groups
//! follow: one per workspace (header = workspace title, sessions nested
//! under it), then an "ungrouped" group for sessions no workspace claims,
//! then a collapsed "archived (N)" header at the foot (its sessions are
//! listed but hidden — out of j/k navigation until an expand action lands
//! in a later lane). With no workspaces and no archived sessions the list
//! renders FLAT (no group headers) — exactly the pre-grouping look.
//!
//! Rows: the active session carries a bold `●` marker at all times; the
//! selection row is reversed, but only while the sidebar has focus (an
//! unfocused sidebar shows no selection — there is nothing to operate on).
//! Running sessions get a dim `· running` suffix.
//!
//! The list is the attach flow's `session.list` + `workspace.list`
//! snapshots plus live host-stream updates (`App::handle_host_frame`).
//! Selection is session-space: `SidebarState::selected` counts only
//! visible session rows (group headers are never selectable, and j/k
//! crosses group boundaries as if they weren't there).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use crate::i18n::{Locale, tr, trf};
use crate::ui::composer::Composer;
use crate::ui::style;
use crate::wire::session::{SessionId, SessionSummary, WorkspaceId};
use crate::wire::workspace::WorkspaceView;

/// Sidebar pane width; the pane is hidden below 60 total columns.
pub const SIDEBAR_WIDTH: u16 = 22;

/// Sidebar width for a terminal `total` columns wide (collapse gracefully).
pub fn sidebar_width(total: u16) -> u16 {
    if total < 60 { 0 } else { SIDEBAR_WIDTH }
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
/// - then the archived group (collapsed, count in the header).
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
            collapsed: true,
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

/// One display row: a group header or a session row.
enum DisplayRow {
    /// Group header text (workspace title / ungrouped / archived label).
    Header(String),
    /// A session row: index into `SidebarView::sessions`, and its ordinal
    /// in session-space (for the selection highlight).
    Session { index: usize, ordinal: usize },
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
        let block = Block::new()
            .borders(Borders::RIGHT)
            .border_style(if self.focused {
                style::border_focused(self.theme)
            } else {
                style::border(self.theme)
            });
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        buf.set_line(
            inner.x,
            inner.y,
            &Line::styled(tr(self.locale, "sidebar.header"), style::header(self.theme)),
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
            return;
        }

        // Flatten groups into display rows, tracking each visible session's
        // display position and session-space ordinal.
        let mut rows: Vec<DisplayRow> = Vec::new();
        let mut ordinal = 0;
        let mut selected_display = 0;
        for group in self.groups {
            if let Some(title) = &group.title {
                rows.push(DisplayRow::Header(title.clone()));
            }
            if group.collapsed {
                continue;
            }
            for index in &group.sessions {
                if ordinal == self.selected {
                    selected_display = rows.len();
                }
                rows.push(DisplayRow::Session {
                    index: *index,
                    ordinal,
                });
                ordinal += 1;
            }
        }

        let visible = inner.height.saturating_sub(1) as usize;
        // Keep the selected row visible (scrolls so it sits at the edge).
        let start = selected_display.saturating_sub(visible.saturating_sub(1));
        let grouped = self.groups.iter().any(|group| group.title.is_some());
        for (row_index, row) in rows.iter().enumerate().skip(start) {
            let line_index = row_index - start + 1;
            if line_index as u16 >= inner.height {
                break;
            }
            let y = inner.y + line_index as u16;
            match row {
                DisplayRow::Header(title) => {
                    buf.set_line(
                        inner.x,
                        y,
                        &Line::styled(title.as_str(), style::header(self.theme)),
                        inner.width,
                    );
                }
                DisplayRow::Session { index, ordinal } => {
                    let summary = &self.sessions[*index];
                    // The rename editor replaces the selected row while
                    // open (mirrors the queue editor's inline line).
                    if let Some(editor) = self.editor
                        && self.focused
                        && *ordinal == self.selected
                    {
                        if editor.buffer().is_empty() {
                            buf.set_line(
                                inner.x,
                                y,
                                &Line::styled(
                                    format!(" > {}", tr(self.locale, "sidebar.rename_hint")),
                                    style::hint(self.theme),
                                ),
                                inner.width,
                            );
                        } else {
                            let line = Line::from(vec![
                                Span::styled(" > ", style::active(self.theme)),
                                Span::styled(
                                    editor.buffer().to_string(),
                                    style::active(self.theme),
                                ),
                                Span::styled("|", style::hint(self.theme)),
                            ]);
                            buf.set_line(inner.x, y, &line, inner.width);
                        }
                        continue;
                    }
                    if self.focused && *ordinal == self.selected {
                        buf.set_style(
                            Rect::new(inner.x, y, inner.width, 1),
                            style::selection(self.theme),
                        );
                    }
                    let is_active = self.active == Some(&summary.session_id);
                    // Grouped sessions nest one space under their header;
                    // flat mode keeps the pre-grouping column exactly.
                    let marker = match (grouped, is_active) {
                        (true, true) => " ● ",
                        (true, false) => "   ",
                        (false, true) => "● ",
                        (false, false) => "  ",
                    };
                    let mut spans = vec![Span::styled(marker, style::active(self.theme))];
                    spans.push(Span::raw(label(summary)));
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
    }
}

/// Row label: the session title projection when present, else the id.
fn label(summary: &SessionSummary) -> String {
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
    match title {
        Some(title) if !title.trim().is_empty() => title.to_string(),
        _ => summary.session_id.0.clone(),
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
        let groups = build_groups(&sessions, &[], &[], &[], Locale::En);
        assert_eq!(
            groups,
            vec![SidebarGroup {
                title: None,
                collapsed: false,
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
        let groups = build_groups(&sessions, &workspaces, &order, &archived, Locale::En);
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
        let groups = build_groups(&sessions, &workspaces, &[], &[], Locale::En);
        let titles: Vec<_> = groups.iter().map(|g| g.title.as_deref()).collect();
        assert_eq!(titles, vec![Some("alpha")]);
    }

    #[test]
    fn archived_overrides_workspace_membership() {
        let sessions = vec![summary("s1")];
        let workspaces = vec![workspace("wA", "alpha", &["s1"])];
        let archived = vec![SessionId("s1".into())];
        let groups = build_groups(&sessions, &workspaces, &[], &archived, Locale::En);
        let titles: Vec<_> = groups.iter().map(|g| g.title.as_deref()).collect();
        assert_eq!(titles, vec![Some("▸ archived (1)")]);
        assert_eq!(SidebarGroup::visible_len(&groups), 0);
    }
}
