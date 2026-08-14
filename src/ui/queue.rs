//! The queue surfaces (ticket 05 Q14/Q19): a one-line strip docked between
//! the chat and the composer while the active session has queued items, and
//! a view-only popup (`Alt+q`) listing them.
//!
//! The strip reads the store's `session/queue` snapshot at draw time — no
//! extra state, it appears and disappears with the snapshot. View-only v1:
//! edit/remove/steer ride `session.updateQueue` in a later lane (the wire
//! types exist; the RPC helper does not).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::i18n::{Locale, tr, trf};
use crate::theme::Theme;
use crate::ui::style;
use crate::wire::events::{MessageRole, QueueItem, QueuePlacement};

/// Popup rows visible at once (the popup caps at this + 2 border rows).
pub const QUEUE_POPUP_MAX_ROWS: usize = 8;

/// One-line content preview of a queue item: its text blocks joined,
/// whitespace collapsed. Non-text-only items show a placeholder.
pub fn item_preview(item: &QueueItem, locale: Locale) -> String {
    let text = item
        .message
        .content
        .iter()
        .filter_map(|block| block.text())
        .collect::<Vec<_>>()
        .join(" ");
    let preview = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if preview.is_empty() {
        tr(locale, "queue.no_text").into()
    } else {
        preview
    }
}

/// Short placement tag for the popup rows.
fn placement_tag(placement: QueuePlacement, locale: Locale) -> &'static str {
    match placement {
        QueuePlacement::Queued => tr(locale, "queue.tag_queued"),
        QueuePlacement::Steering => tr(locale, "queue.tag_steering"),
        QueuePlacement::Context => tr(locale, "queue.tag_context"),
    }
}

fn role_label(role: MessageRole, locale: Locale) -> &'static str {
    match role {
        MessageRole::System => tr(locale, "queue.role_system"),
        MessageRole::User => tr(locale, "queue.role_user"),
        MessageRole::Assistant => tr(locale, "queue.role_assistant"),
    }
}

/// The one-line dock above the composer: `N queued · first preview`, with
/// steering/context counts when present. Muted overall; the steering count
/// is the one accented segment (a steering item pre-empts the turn, so it
/// earns the warning color).
pub struct QueueStrip<'a> {
    pub items: &'a [QueueItem],
    pub theme: &'a Theme,
    pub locale: Locale,
}

impl Widget for QueueStrip<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.items.is_empty() || area.height == 0 || area.width == 0 {
            return;
        }
        let steering = self
            .items
            .iter()
            .filter(|item| item.placement == QueuePlacement::Steering)
            .count();
        let context = self
            .items
            .iter()
            .filter(|item| item.placement == QueuePlacement::Context)
            .count();
        let mut spans = vec![Span::styled(
            trf(self.locale, "queue.strip", &[&self.items.len().to_string()]),
            style::hint(self.theme),
        )];
        if steering > 0 {
            spans.push(Span::styled(
                trf(self.locale, "queue.steering", &[&steering.to_string()]),
                style::warning(self.theme),
            ));
        }
        if context > 0 {
            spans.push(Span::styled(
                trf(self.locale, "queue.context", &[&context.to_string()]),
                style::hint(self.theme),
            ));
        }
        spans.push(Span::styled(
            format!(" · {}", item_preview(&self.items[0], self.locale)),
            style::hint(self.theme),
        ));
        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
    }
}

/// The queue popup: placement tag + role + preview per item, docked above
/// the strip. Scroll state lives on the app; this renders the window.
pub struct QueuePopup<'a> {
    pub items: &'a [QueueItem],
    pub scroll: usize,
    pub theme: &'a Theme,
    pub locale: Locale,
}

impl QueuePopup<'_> {
    /// Outer size (border included) for an available width and the room
    /// above the strip.
    pub fn size(&self, available: u16, room: u16) -> (u16, u16) {
        let width = available.clamp(24, 64);
        let height = (self.items.len() as u16 + 2)
            .min(QUEUE_POPUP_MAX_ROWS as u16 + 2)
            .min(room);
        (width, height)
    }
}

impl Widget for QueuePopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title(tr(self.locale, "queue.title"));
        let inner = block.inner(area);
        block.render(area, buf);
        for (row, item) in self.items.iter().skip(self.scroll).enumerate() {
            if row as u16 >= inner.height {
                break;
            }
            let tag_style = match item.placement {
                QueuePlacement::Queued => style::active(self.theme),
                QueuePlacement::Steering => style::warning(self.theme),
                QueuePlacement::Context => style::hint(self.theme),
            };
            let line = Line::from(vec![
                Span::styled(
                    format!("[{}]", placement_tag(item.placement, self.locale)),
                    tag_style,
                ),
                Span::styled(
                    format!(" {}", role_label(item.message.role, self.locale)),
                    style::hint(self.theme),
                ),
                Span::raw(format!(" · {}", item_preview(item, self.locale))),
            ]);
            buf.set_line(inner.x, inner.y + row as u16, &line, inner.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::events::QueueMessage;
    use crate::wire::session::{ContentBlock, MessageId};

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock {
            r#type: "text".into(),
            extra: serde_json::Map::from_iter([(
                "text".to_string(),
                serde_json::Value::String(text.into()),
            )]),
        }
    }

    fn item(placement: QueuePlacement, text: &str) -> QueueItem {
        QueueItem {
            id: MessageId("m1".into()),
            placement,
            message: QueueMessage {
                id: MessageId("m1".into()),
                role: MessageRole::User,
                content: vec![text_block(text)],
                source: crate::wire::events::QueueMessageSource {
                    kind: "user".into(),
                },
            },
        }
    }

    #[test]
    fn preview_collapses_whitespace() {
        let item = item(QueuePlacement::Queued, "fix  the\ntests   please");
        assert_eq!(item_preview(&item, Locale::En), "fix the tests please");
    }

    #[test]
    fn preview_placeholder_for_non_text() {
        let mut item = item(QueuePlacement::Queued, "");
        item.message.content = vec![];
        assert_eq!(item_preview(&item, Locale::En), "(no text)");
    }

    #[test]
    fn popup_size_caps() {
        let items: Vec<QueueItem> = (0..20)
            .map(|i| item(QueuePlacement::Queued, &format!("item {i}")))
            .collect();
        let theme = Theme::default();
        let popup = QueuePopup {
            items: &items,
            scroll: 0,
            theme: &theme,
            locale: Locale::En,
        };
        let (width, height) = popup.size(100, 20);
        assert_eq!(height, QUEUE_POPUP_MAX_ROWS as u16 + 2, "row cap");
        assert_eq!(width, 64, "width cap");
        let (_, height) = popup.size(100, 5);
        assert_eq!(height, 5, "clamped to the room above");
    }
}
