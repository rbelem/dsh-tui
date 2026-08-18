//! The sidebar context menu (#46): a small bordered popup listing the
//! actions for a workspace or a session — the web parity for the per-row
//! kebab menus. `j`/`k` (or arrows) move the cursor, `Enter` executes the
//! highlighted action, `Esc` closes; everything else is inert while it's
//! open (the app routes through [`crate::app::App::handle_context_menu_key`]).
//!
//! The popup is a pure list widget; the app decides what each action
//! dispatches (rename editors, RPCs through the back-channel).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::i18n::{Locale, tr};
use crate::theme::Theme;
use crate::ui::style;
use crate::wire::session::{SessionId, WorkspaceId};

/// The row the menu opened on.
#[derive(Debug, Clone, PartialEq)]
pub enum ContextMenuTarget {
    Workspace { id: WorkspaceId },
    Session { id: SessionId },
}

/// One menu action (the app maps it to the existing dispatch paths).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuAction {
    Rename,
    Fork,
    Archive,
    DeleteWorkspace,
}

/// A menu entry: the display label plus the action it executes.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMenuItem {
    pub label: String,
    pub action: ContextMenuAction,
}

/// The open menu: its target row, the entries, and the cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMenuState {
    pub target: ContextMenuTarget,
    pub items: Vec<ContextMenuItem>,
    pub selected: usize,
}

impl ContextMenuState {
    /// The session menu: Rename / Fork session / Archive session.
    pub fn for_session(session_id: SessionId, locale: Locale) -> Self {
        ContextMenuState {
            target: ContextMenuTarget::Session { id: session_id },
            items: vec![
                ContextMenuItem {
                    label: tr(locale, "context_menu.rename").to_string(),
                    action: ContextMenuAction::Rename,
                },
                ContextMenuItem {
                    label: tr(locale, "context_menu.fork_session").to_string(),
                    action: ContextMenuAction::Fork,
                },
                ContextMenuItem {
                    label: tr(locale, "context_menu.archive_session").to_string(),
                    action: ContextMenuAction::Archive,
                },
            ],
            selected: 0,
        }
    }

    /// The workspace menu: Rename / Delete workspace.
    pub fn for_workspace(workspace_id: WorkspaceId, locale: Locale) -> Self {
        ContextMenuState {
            target: ContextMenuTarget::Workspace { id: workspace_id },
            items: vec![
                ContextMenuItem {
                    label: tr(locale, "context_menu.rename").to_string(),
                    action: ContextMenuAction::Rename,
                },
                ContextMenuItem {
                    label: tr(locale, "context_menu.delete_workspace").to_string(),
                    action: ContextMenuAction::DeleteWorkspace,
                },
            ],
            selected: 0,
        }
    }

    /// Move the cursor (clamped).
    pub fn move_cursor(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }
}

/// The popup widget: a bordered, panel-filled list with a `▸` cursor.
pub struct ContextMenuPopup<'a> {
    pub target: &'a ContextMenuTarget,
    pub items: &'a [ContextMenuItem],
    pub selected: usize,
    pub theme: &'a Theme,
    pub locale: Locale,
}

impl ContextMenuPopup<'_> {
    /// Outer size (border included) for an available width: the widest
    /// label + the marker column, capped at the room available.
    pub fn size(&self, available: u16, room: u16) -> (u16, u16) {
        let text = self
            .items
            .iter()
            .map(|item| item.label.len())
            .max()
            .unwrap_or(0);
        let width = ((text as u16) + 8).max(20).min(available);
        let height = (self.items.len() as u16 + 2).min(room);
        (width, height)
    }
}

impl Widget for ContextMenuPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title_style(
                Style::new()
                    .add_modifier(Modifier::BOLD)
                    .fg(self.theme.accent),
            )
            .title(match self.target {
                ContextMenuTarget::Workspace { .. } => {
                    tr(self.locale, "context_menu.title_workspace")
                }
                ContextMenuTarget::Session { .. } => tr(self.locale, "context_menu.title_session"),
            });
        let inner = block.inner(area);
        block.render(area, buf);
        // #11 popup treatment: panel_bg fill after Clear, inside the border.
        buf.set_style(inner, style::panel_fill(self.theme));
        for (row, item) in self.items.iter().enumerate() {
            if row as u16 >= inner.height {
                break;
            }
            let y = inner.y + row as u16;
            let cursor = self.selected == row;
            if cursor {
                buf.set_style(
                    Rect::new(inner.x, y, inner.width, 1),
                    style::selection(self.theme),
                );
            }
            let marker = if cursor { "▸ " } else { "  " };
            buf.set_line(
                inner.x,
                y,
                &Line::from(vec![
                    Span::styled(marker, style::active(self.theme)),
                    Span::styled(item.label.clone(), Style::default().fg(self.theme.text)),
                ]),
                inner.width,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Render the popup and return the screen text plus the cursor row.
    fn render_popup(
        target: &ContextMenuTarget,
        items: &[ContextMenuItem],
        selected: usize,
        locale: Locale,
    ) -> (String, Option<u16>) {
        let backend = TestBackend::new(30, 8);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                ContextMenuPopup {
                    target,
                    items,
                    selected,
                    theme: &Theme::default(),
                    locale,
                },
                f.area(),
            )
        })
        .unwrap();
        let mut cursor = None;
        for y in 0..8u16 {
            for x in 0..30u16 {
                if term
                    .backend()
                    .buffer()
                    .cell((x, y))
                    .is_some_and(|cell| cell.symbol() == "▸")
                {
                    cursor = Some(y);
                }
            }
        }
        (format!("{}", term.backend()), cursor)
    }

    fn session_menu() -> Vec<ContextMenuItem> {
        ContextMenuState::for_session(SessionId("s1".into()), Locale::En).items
    }

    fn workspace_menu() -> Vec<ContextMenuItem> {
        ContextMenuState::for_workspace(WorkspaceId("w1".into()), Locale::En).items
    }

    #[test]
    fn menus_contain_the_parity_actions_per_target() {
        let session: Vec<String> = session_menu()
            .iter()
            .map(|item| item.label.clone())
            .collect();
        assert_eq!(
            session,
            vec!["Rename", "Fork session", "Archive session"],
            "session menu"
        );
        let workspace: Vec<String> = workspace_menu()
            .iter()
            .map(|item| item.label.clone())
            .collect();
        assert_eq!(
            workspace,
            vec!["Rename", "Delete workspace"],
            "workspace menu"
        );
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let mut menu = ContextMenuState::for_session(SessionId("s1".into()), Locale::En);
        assert_eq!(menu.selected, 0);
        menu.move_cursor(1);
        assert_eq!(menu.selected, 1);
        menu.move_cursor(1);
        assert_eq!(menu.selected, 2, "clamped at the last entry");
        menu.move_cursor(-5);
        assert_eq!(menu.selected, 0, "clamped at the first entry");
    }

    #[test]
    fn render_shows_entries_and_the_cursor() {
        let (text, cursor) = render_popup(
            &ContextMenuTarget::Session {
                id: SessionId("s1".into()),
            },
            &session_menu(),
            0,
            Locale::En,
        );
        assert!(text.contains(" session "), "title: {text}");
        assert!(text.contains("Rename"), "rename entry: {text}");
        assert!(text.contains("Fork session"), "fork entry: {text}");
        assert!(text.contains("Archive session"), "archive entry: {text}");
        assert_eq!(cursor, Some(1), "cursor on the first entry");

        let (_, cursor) = render_popup(
            &ContextMenuTarget::Session {
                id: SessionId("s1".into()),
            },
            &session_menu(),
            2,
            Locale::En,
        );
        assert_eq!(cursor, Some(3), "cursor moved with the selection");

        let (text, _) = render_popup(
            &ContextMenuTarget::Workspace {
                id: WorkspaceId("w1".into()),
            },
            &workspace_menu(),
            0,
            Locale::En,
        );
        assert!(text.contains(" workspace "), "workspace title: {text}");
        assert!(text.contains("Delete workspace"), "delete entry: {text}");
    }

    #[test]
    fn size_fits_entries_and_caps() {
        let target = ContextMenuTarget::Session {
            id: SessionId("s1".into()),
        };
        let items = session_menu();
        let p = ContextMenuPopup {
            target: &target,
            items: &items,
            selected: 0,
            theme: &Theme::default(),
            locale: Locale::En,
        };
        assert_eq!(p.size(30, 8), (23, 5), "3 entries + border");
        assert_eq!(p.size(10, 8), (10, 5), "width caps at available");
        assert_eq!(p.size(30, 3), (23, 3), "height caps at the room");
    }

    #[test]
    fn empty_menu_and_tiny_area_stay_graceful() {
        // move_cursor on an empty menu is a no-op.
        let mut menu = ContextMenuState {
            target: ContextMenuTarget::Session {
                id: SessionId("s1".into()),
            },
            items: Vec::new(),
            selected: 0,
        };
        menu.move_cursor(1);
        assert_eq!(menu.selected, 0);

        // Rendering into an area too small for even one entry stops at the
        // inner edge without panicking.
        let backend = TestBackend::new(4, 4);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                ContextMenuPopup {
                    target: &ContextMenuTarget::Session {
                        id: SessionId("s1".into()),
                    },
                    items: &session_menu(),
                    selected: 0,
                    theme: &Theme::default(),
                    locale: Locale::En,
                },
                f.area(),
            )
        })
        .unwrap();
    }

    #[test]
    fn zh_locale_renders() {
        let items = ContextMenuState::for_session(SessionId("s1".into()), Locale::Zh).items;
        let (text, _) = render_popup(
            &ContextMenuTarget::Session {
                id: SessionId("s1".into()),
            },
            &items,
            0,
            Locale::Zh,
        );
        assert!(text.contains("重命名"), "zh rename: {text}");
        assert!(text.contains("分叉会话"), "zh fork: {text}");
        assert!(text.contains("归档会话"), "zh archive: {text}");
    }
}
