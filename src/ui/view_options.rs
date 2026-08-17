//! The view-options popup (the sidebar's `Options` button, 6f): a small
//! centered overlay with two sections — "Group by" (Workspace / In one
//! list) and "Order by" (Manual / Last updated) — mirroring the
//! new-session picker's bordered popup treatment. `j`/`k` move the cursor
//! across the four choices, `Enter` toggles the section under the cursor,
//! `Esc` closes. In-memory only — no config persistence.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::i18n::{Locale, tr};
use crate::theme::Theme;
use crate::ui::style;

/// The options popup: the cursor (0..4 — the four choices in section
/// order) plus the current app state for the radio markers.
pub struct ViewOptionsPopup<'a> {
    pub selected: usize,
    /// The group-by choice (`true` = the flat single list).
    pub flat: bool,
    /// The order-by choice (`true` = last-updated desc).
    pub order_updated: bool,
    pub theme: &'a Theme,
    pub locale: Locale,
}

impl ViewOptionsPopup<'_> {
    /// Outer size (border included): the title + two section headers + the
    /// four choices, capped at the room available.
    pub fn size(&self, available: u16, room: u16) -> (u16, u16) {
        let labels = [
            "sidebar.group_by",
            "sidebar.group_workspace",
            "sidebar.group_flat",
            "sidebar.order_by",
            "sidebar.order_manual",
            "sidebar.order_updated",
        ];
        let text = labels
            .iter()
            .map(|key| tr(self.locale, key).len())
            .max()
            .unwrap_or(0);
        let width = (text as u16 + 8).max(24).min(available);
        // Title + 2 headers + 4 choices + 2 borders.
        let height = 8u16.min(room);
        (width, height)
    }
}

impl Widget for ViewOptionsPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title_style(
                Style::new()
                    .add_modifier(Modifier::BOLD)
                    .fg(self.theme.accent),
            )
            .title(tr(self.locale, "sidebar.options.title"));
        let inner = block.inner(area);
        block.render(area, buf);
        // #11 popup treatment: panel_bg fill after Clear, inside the border.
        buf.set_style(inner, style::panel_fill(self.theme));
        let mut y = inner.y;
        // The cursor index-space: 0..4, in section order.
        let mut row = 0usize;
        for (header, choices) in [
            (
                "sidebar.group_by",
                [
                    ("sidebar.group_workspace", !self.flat),
                    ("sidebar.group_flat", self.flat),
                ],
            ),
            (
                "sidebar.order_by",
                [
                    ("sidebar.order_manual", !self.order_updated),
                    ("sidebar.order_updated", self.order_updated),
                ],
            ),
        ] {
            if y >= inner.bottom() {
                break;
            }
            buf.set_line(
                inner.x,
                y,
                &Line::styled(
                    format!(" {}", tr(self.locale, header)),
                    style::header(self.theme),
                ),
                inner.width,
            );
            y += 1;
            for (key, chosen) in choices {
                if y >= inner.bottom() {
                    break;
                }
                let cursor = self.selected == row;
                if cursor {
                    buf.set_style(
                        Rect::new(inner.x, y, inner.width, 1),
                        style::selection(self.theme),
                    );
                }
                let marker = if cursor { "▸" } else { " " };
                let radio = if chosen { "●" } else { "○" };
                buf.set_line(
                    inner.x,
                    y,
                    &Line::from(vec![
                        Span::styled(marker, style::active(self.theme)),
                        Span::raw(" "),
                        Span::styled(radio, style::active(self.theme)),
                        Span::raw(" "),
                        Span::styled(tr(self.locale, key), Style::default().fg(self.theme.text)),
                    ]),
                    inner.width,
                );
                y += 1;
                row += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn popup(
        theme: &Theme,
        selected: usize,
        flat: bool,
        order_updated: bool,
    ) -> ViewOptionsPopup<'_> {
        ViewOptionsPopup {
            selected,
            flat,
            order_updated,
            theme,
            locale: Locale::En,
        }
    }

    /// Render the popup at 30×8 and return the screen text plus the
    /// cursor row (the `▸` marker's y, if any).
    fn render_popup(
        selected: usize,
        flat: bool,
        order_updated: bool,
        locale: Locale,
    ) -> (String, Option<u16>) {
        let backend = TestBackend::new(30, 8);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                ViewOptionsPopup {
                    selected,
                    flat,
                    order_updated,
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

    #[test]
    fn size_caps_to_available_and_room() {
        let theme = Theme::default();
        let p = popup(&theme, 0, false, false);
        // Narrow terminal: the width caps at what's available.
        assert_eq!(p.size(10, 8), (10, 8));
        // Floor of 24 when room allows; height caps at the room.
        assert_eq!(p.size(30, 8), (24, 8));
        assert_eq!(p.size(30, 3), (24, 3));
    }

    #[test]
    fn render_shows_sections_radios_and_cursor() {
        let (text, cursor) = render_popup(0, false, true, Locale::En);
        assert!(text.contains("View options"), "title: {text}");
        assert!(text.contains("Group by"), "group section: {text}");
        assert!(text.contains("Order by"), "order section: {text}");
        // Radios mirror the state: workspace chosen (flat=false), updated chosen.
        assert!(text.contains("● Workspace"), "workspace radio: {text}");
        assert!(text.contains("○ In one list"), "flat radio: {text}");
        assert!(text.contains("○ Manual"), "manual radio: {text}");
        assert!(text.contains("● Last updated"), "updated radio: {text}");
        // The cursor sits on the selected row (0 = the Workspace row).
        assert_eq!(cursor, Some(2), "cursor on the first choice row");
    }

    #[test]
    fn render_moves_cursor_and_radios_with_state() {
        let (text, cursor) = render_popup(3, true, false, Locale::En);
        assert!(text.contains("○ Workspace"), "workspace unselected: {text}");
        assert!(text.contains("● In one list"), "flat selected: {text}");
        assert!(text.contains("● Manual"), "manual selected: {text}");
        assert!(
            text.contains("○ Last updated"),
            "updated unselected: {text}"
        );
        assert_eq!(cursor, Some(6), "cursor on the last choice row");
    }

    #[test]
    fn render_uses_the_locale() {
        let (text, _) = render_popup(0, false, false, Locale::Zh);
        assert!(text.contains("视图选项"), "zh title: {text}");
        assert!(text.contains("分组方式"), "zh group section: {text}");
        assert!(text.contains("● 工作区"), "zh workspace radio: {text}");
    }
}
