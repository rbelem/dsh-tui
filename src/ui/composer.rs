//! The composer: a hand-rolled multi-line input (ratatui has no input
//! widget) plus the seeded `/` and `@` menus that float above it.
//!
//! Buffer + caret model: one `String`, one byte-index caret (always on a
//! char boundary). Editing covers char insert, backspace/delete, and
//! caret moves (Left/Right/Home/End, Up/Down across lines). Enter submits,
//! Shift+Enter inserts a newline (web parity, ticket 05 Q14).
//!
//! v1 rendering does NOT wrap: long lines scroll horizontally to keep the
//! caret visible (`caret_layout`). Wrapping is a later-lane refinement.
//!
//! The `/` and `@` popups are v1 seeds: a static list of 2-3 items shown
//! while the buffer is a bare trigger prefix. The real command catalog
//! rides `command.execute` in a later lane.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::ui::style;

/// One seeded popup entry: the text inserted on accept and a short hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedItem {
    pub text: &'static str,
    pub hint: &'static str,
}

/// Seeded `/` entries (v1 static; the real catalog rides `command.execute`).
const SLASH_ITEMS: &[SeedItem] = &[
    SeedItem {
        text: "/compact",
        hint: "compact the session",
    },
    SeedItem {
        text: "/clear",
        hint: "clear the chat",
    },
    SeedItem {
        text: "/help",
        hint: "show help",
    },
];

/// Seeded `@` entries (placeholders; mentions are a later lane).
const AT_ITEMS: &[SeedItem] = &[
    SeedItem {
        text: "@file",
        hint: "mention a file (soon)",
    },
    SeedItem {
        text: "@session",
        hint: "mention a session (soon)",
    },
];

/// Which seed popup the buffer currently triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Slash,
    At,
}

impl PopupKind {
    pub fn items(self) -> &'static [SeedItem] {
        match self {
            PopupKind::Slash => SLASH_ITEMS,
            PopupKind::At => AT_ITEMS,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            PopupKind::Slash => "commands",
            PopupKind::At => "mentions",
        }
    }
}

/// The composer buffer and caret.
#[derive(Debug, Default)]
pub struct Composer {
    buffer: String,
    /// Byte-index caret; always on a char boundary.
    caret: usize,
    /// Esc dismisses the popup until the buffer changes again.
    popup_dismissed: bool,
    popup_selected: usize,
}

impl Composer {
    pub fn new() -> Self {
        Composer::default()
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Logical line count (height math; rendering does not wrap).
    pub fn line_count(&self) -> usize {
        self.buffer.split('\n').count()
    }

    /// Insert one char at the caret; any buffer change re-arms the popup.
    pub fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.caret, c);
        self.caret += c.len_utf8();
        self.popup_dismissed = false;
    }

    pub fn newline(&mut self) {
        self.insert_char('\n');
    }

    /// Remove the char before the caret.
    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let prev = self.buffer[..self.caret]
            .chars()
            .next_back()
            .map(char::len_utf8)
            .unwrap_or(0);
        self.buffer.replace_range(self.caret - prev..self.caret, "");
        self.caret -= prev;
        self.popup_dismissed = false;
    }

    /// Remove the char at the caret.
    pub fn delete(&mut self) {
        if self.caret >= self.buffer.len() {
            return;
        }
        let next = self.buffer[self.caret..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(0);
        self.buffer.replace_range(self.caret..self.caret + next, "");
        self.popup_dismissed = false;
    }

    pub fn move_left(&mut self) {
        if self.caret > 0 {
            let prev = self.buffer[..self.caret]
                .chars()
                .next_back()
                .map(char::len_utf8)
                .unwrap_or(0);
            self.caret -= prev;
        }
    }

    pub fn move_right(&mut self) {
        if self.caret < self.buffer.len() {
            let next = self.buffer[self.caret..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(0);
            self.caret += next;
        }
    }

    /// Caret to the start of the current line.
    pub fn move_home(&mut self) {
        let (row, _) = self.caret_position();
        self.caret = self.line_start(row);
    }

    /// Caret to the end of the current line.
    pub fn move_end(&mut self) {
        let (row, _) = self.caret_position();
        let start = self.line_start(row);
        self.caret = start + self.line(row).len();
    }

    /// Caret to the previous line, preserving the char column where possible.
    pub fn move_up(&mut self) {
        self.move_line(-1);
    }

    /// Caret to the next line, preserving the char column where possible.
    pub fn move_down(&mut self) {
        self.move_line(1);
    }

    /// Take the buffer for submission, resetting the composer.
    pub fn take(&mut self) -> String {
        self.caret = 0;
        self.popup_dismissed = false;
        self.popup_selected = 0;
        std::mem::take(&mut self.buffer)
    }

    /// The seed popup the buffer triggers, if not dismissed: the buffer is a
    /// bare trigger prefix — starts with `/` or `@` and has no whitespace.
    pub fn popup(&self) -> Option<PopupKind> {
        if self.popup_dismissed || self.buffer.contains(char::is_whitespace) {
            return None;
        }
        match self.buffer.as_bytes().first() {
            Some(b'/') => Some(PopupKind::Slash),
            Some(b'@') => Some(PopupKind::At),
            _ => None,
        }
    }

    pub fn popup_selected(&self) -> usize {
        self.popup_selected
    }

    /// Move the popup highlight (clamped).
    pub fn popup_move(&mut self, delta: isize) {
        let Some(kind) = self.popup() else { return };
        let last = kind.items().len().saturating_sub(1) as isize;
        self.popup_selected = (self.popup_selected as isize + delta).clamp(0, last) as usize;
    }

    /// Insert the highlighted popup item plus a trailing space (the space
    /// closes the popup: the trigger requires a whitespace-free buffer).
    pub fn popup_accept(&mut self) {
        let Some(kind) = self.popup() else { return };
        let items = kind.items();
        let item = items[self.popup_selected.min(items.len() - 1)];
        self.buffer = format!("{} ", item.text);
        self.caret = self.buffer.len();
        self.popup_selected = 0;
        self.popup_dismissed = false;
    }

    /// Esc: close the popup until the buffer changes again.
    pub fn popup_dismiss(&mut self) {
        self.popup_dismissed = true;
        self.popup_selected = 0;
    }

    /// Caret as (row, char column) within the logical lines.
    pub fn caret_position(&self) -> (usize, usize) {
        let before = &self.buffer[..self.caret];
        let row = before.matches('\n').count();
        let col = before
            .rsplit('\n')
            .next()
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0);
        (row, col)
    }

    /// Caret layout for an area `width` cells wide: (row, visible column,
    /// horizontal scroll). The scroll keeps the caret inside the area; the
    /// widget applies it and the draw loop positions the terminal cursor.
    pub fn caret_layout(&self, width: u16) -> (u16, u16, u16) {
        let (row, col_chars) = self.caret_position();
        let line = self.line(row);
        let prefix: String = line.chars().take(col_chars).collect();
        let col_cells = UnicodeWidthStr::width(prefix.as_str()) as u16;
        let usable = width.saturating_sub(1);
        let scroll = col_cells.saturating_sub(usable);
        (row as u16, col_cells - scroll, scroll)
    }

    /// The `n`-th logical line (empty string when out of range).
    fn line(&self, n: usize) -> &str {
        self.buffer.split('\n').nth(n).unwrap_or("")
    }

    /// Byte offset of the `n`-th line's first char.
    fn line_start(&self, n: usize) -> usize {
        self.buffer
            .split('\n')
            .take(n)
            .map(|line| line.len() + 1)
            .sum()
    }

    fn move_line(&mut self, delta: isize) {
        let (row, col) = self.caret_position();
        let rows = self.line_count() as isize;
        let target = (row as isize + delta).clamp(0, rows - 1) as usize;
        let target_line = self.line(target);
        let col = col.min(target_line.chars().count());
        let byte_col: usize = target_line.chars().take(col).map(char::len_utf8).sum();
        self.caret = self.line_start(target) + byte_col;
    }
}

/// The composer strip: a top rule (the pane seam) and the buffer below it.
/// Empty + focused shows a dim placeholder; the focused pane's rule is
/// bright, unfocused dim.
pub struct ComposerView<'a> {
    pub composer: &'a Composer,
    pub focused: bool,
}

/// Placeholder shown in the empty composer.
const PLACEHOLDER: &str = "type a message — enter to send";

impl Widget for ComposerView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(if self.focused {
                style::BORDER_FOCUSED
            } else {
                style::BORDER
            });
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let (_, _, scroll) = self.composer.caret_layout(inner.width);
        if self.composer.is_empty() {
            Paragraph::new(Line::styled(PLACEHOLDER, style::HINT)).render(inner, buf);
        } else {
            Paragraph::new(self.composer.buffer().to_string())
                .scroll((0, scroll))
                .render(inner, buf);
        }
    }
}

/// The seeded `/` or `@` popup: a small floating list rendered above the
/// composer by the draw loop (caller clears the area first via [`Clear`]).
pub struct SeedPopup {
    pub kind: PopupKind,
    pub selected: usize,
}

impl SeedPopup {
    /// Outer size for the popup (border included) for an available width.
    pub fn size(&self, available: u16) -> (u16, u16) {
        let items = self.kind.items();
        let text = items.iter().map(|item| item.text.len()).max().unwrap_or(0);
        let hint = items.iter().map(|item| item.hint.len()).max().unwrap_or(0);
        let width = (text + hint + 6) as u16;
        let height = items.len() as u16 + 2;
        (width.clamp(16, available.max(16)), height)
    }
}

impl Widget for SeedPopup {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(style::BORDER)
            .title(self.kind.title());
        let inner = block.inner(area);
        block.render(area, buf);
        for (i, item) in self.kind.items().iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            let y = inner.y + i as u16;
            if i == self.selected {
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), style::SELECTION);
            }
            let line = Line::from(vec![
                Span::raw(format!(" {}  ", item.text)),
                Span::styled(item.hint, style::HINT),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composer(text: &str) -> Composer {
        let mut composer = Composer::new();
        for c in text.chars() {
            composer.insert_char(c);
        }
        composer
    }

    #[test]
    fn insert_and_backspace() {
        let mut composer = composer("helo");
        composer.backspace();
        composer.insert_char('l');
        composer.insert_char('o');
        assert_eq!(composer.buffer(), "hello");
    }

    #[test]
    fn multiline_editing() {
        let mut composer = composer("ab");
        composer.newline();
        composer.insert_char('c');
        assert_eq!(composer.buffer(), "ab\nc");
        assert_eq!(composer.line_count(), 2);
        composer.move_up();
        // Caret was at (1, 1); the column is preserved.
        assert_eq!(composer.caret_position(), (0, 1));
        composer.move_home();
        assert_eq!(composer.caret_position(), (0, 0));
        composer.move_down();
        // Column preserved where the target line allows; clamped otherwise.
        assert_eq!(composer.caret_position(), (1, 0));
        composer.move_end();
        assert_eq!(composer.caret_position(), (1, 1));
    }

    #[test]
    fn delete_removes_char_at_caret() {
        let mut composer = composer("abc");
        composer.move_home();
        composer.move_right();
        composer.delete();
        assert_eq!(composer.buffer(), "ac");
    }

    #[test]
    fn take_resets() {
        let mut composer = composer("hi");
        assert_eq!(composer.take(), "hi");
        assert!(composer.is_empty());
        assert_eq!(composer.caret_position(), (0, 0));
    }

    #[test]
    fn popup_triggers_and_dismisses() {
        let mut composer = Composer::new();
        assert_eq!(composer.popup(), None);
        composer.insert_char('/');
        assert_eq!(composer.popup(), Some(PopupKind::Slash));
        composer.popup_dismiss();
        assert_eq!(composer.popup(), None, "dismissed until the buffer changes");
        composer.insert_char('c');
        assert_eq!(composer.popup(), Some(PopupKind::Slash), "edits re-arm");
        composer.insert_char(' ');
        assert_eq!(composer.popup(), None, "whitespace closes the trigger");
    }

    #[test]
    fn popup_at_trigger() {
        let mut composer = composer("@");
        assert_eq!(composer.popup(), Some(PopupKind::At));
        composer.backspace();
        assert_eq!(composer.popup(), None);
    }

    #[test]
    fn popup_accept_inserts_and_closes() {
        let mut composer = composer("/");
        composer.popup_move(1);
        assert_eq!(composer.popup_selected(), 1);
        composer.popup_accept();
        assert_eq!(composer.buffer(), "/clear ");
        assert_eq!(composer.popup(), None, "trailing space closes the popup");
    }

    #[test]
    fn popup_move_clamps() {
        let mut composer = composer("@");
        composer.popup_move(-1);
        assert_eq!(composer.popup_selected(), 0);
        composer.popup_move(99);
        assert_eq!(composer.popup_selected(), AT_ITEMS.len() - 1);
    }

    #[test]
    fn caret_layout_scrolls_long_lines() {
        let composer = composer("abcdefghij");
        let (row, col, scroll) = composer.caret_layout(5);
        assert_eq!(row, 0);
        assert_eq!(col, 4, "caret stays inside the area");
        assert_eq!(scroll, 6);
    }
}
