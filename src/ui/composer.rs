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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::i18n::{Locale, tr};
use crate::ui::style;

/// Which catalog popup the buffer currently triggers. The entry list is
/// owned by the app (`App::popup_entries`): `/` mirrors the core slash
/// commands statically (the web's `command.list` has no gateway RPC — see
/// `src/ui/catalog.rs`), `@` sources `skill.list` through the back-channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Slash,
    At,
}

impl PopupKind {
    /// The popup's i18n key (`popup.commands` / `popup.mentions`).
    pub fn title(self) -> &'static str {
        match self {
            PopupKind::Slash => "popup.commands",
            PopupKind::At => "popup.mentions",
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

    /// The strip height in rows this buffer needs: the line count plus the
    /// top rule, floored at 2 and capped at `max` (the caller's ceiling).
    /// The buffer is unbounded — past `max` the strip stops growing and the
    /// caret-follow scroll ([`Composer::caret_layout`] + the paragraph
    /// scroll) keeps the caret visible beyond the visible strip.
    pub fn layout_height(&self, max: u16) -> u16 {
        (self.line_count() as u16 + 1).min(max).max(2)
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

    /// Replace the buffer wholesale (the Ctrl+P launcher inserts the picked
    /// entry before dispatching). Resets caret and popup state.
    pub fn set_text(&mut self, text: &str) {
        self.buffer = text.to_string();
        self.caret = self.buffer.len();
        self.popup_dismissed = false;
        self.popup_selected = 0;
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

    /// Move the popup highlight (clamped to the app's entry count).
    pub fn popup_move(&mut self, delta: isize, len: usize) {
        let last = len.saturating_sub(1) as isize;
        self.popup_selected = (self.popup_selected as isize + delta).clamp(0, last) as usize;
    }

    /// Insert the highlighted entry's text plus a trailing space (the space
    /// closes the popup: the trigger requires a whitespace-free buffer).
    pub fn popup_accept(&mut self, insert: &str) {
        self.buffer = format!("{insert} ");
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

    /// Place the caret at `(row, col_cells)` — a mouse click in the
    /// composer's content area (#12). `col_cells` is in LINE space (the
    /// caller adds the current horizontal scroll), cell-width-based
    /// (CJK-safe), clamped to the end of the target line.
    pub fn click_to_cell(&mut self, row: usize, col_cells: u16) {
        let row = row.min(self.line_count().saturating_sub(1));
        let line = self.line(row);
        let mut cells = 0usize;
        let mut chars = 0usize;
        for ch in line.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if cells + w > col_cells as usize {
                break;
            }
            cells += w;
            chars += 1;
        }
        self.caret =
            self.line_start(row) + line.chars().take(chars).map(char::len_utf8).sum::<usize>();
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
/// bright, unfocused dim. #11: the strip carries the `panel_bg` fill and a
/// 2/2 horizontal padding inside the rule.
pub struct ComposerView<'a> {
    pub composer: &'a Composer,
    pub focused: bool,
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl Widget for ComposerView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // The whole strip (rule row included) is a panel surface. With the
        // Reset default theme the fill is a no-op.
        buf.set_style(area, style::panel_fill(self.theme));
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(if self.focused {
                style::border_focused(self.theme)
            } else {
                style::border(self.theme)
            })
            .padding(Padding::horizontal(2));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let (row, _, scroll) = self.composer.caret_layout(inner.width);
        if self.composer.is_empty() {
            Paragraph::new(Line::styled(
                tr(self.locale, "composer.placeholder"),
                style::hint(self.theme),
            ))
            .render(inner, buf);
        } else {
            // #46: vertical caret-follow — the strip is capped by the
            // caller's `layout_height` ceiling, so a buffer taller than
            // the strip scrolls to keep the caret row as the bottom
            // visible line (the horizontal scroll stays the caret's).
            let vscroll = row.saturating_sub(inner.height.saturating_sub(1));
            Paragraph::new(self.composer.buffer().to_string())
                .scroll((vscroll, scroll))
                .render(inner, buf);
        }
    }
}

/// The `/` or `@` catalog popup: a small floating grouped list rendered
/// above the composer by the draw loop (caller clears the area first via
/// [`Clear`]). Entries come from the app (commands mirror / skills RPC);
/// group headers separate them and a loading line shows while the `@`
/// catalog is in flight.
pub struct SeedPopup<'a> {
    pub kind: PopupKind,
    pub entries: &'a [crate::ui::catalog::CatalogEntry],
    pub selected: usize,
    pub loading: bool,
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl SeedPopup<'_> {
    /// Outer size for the popup (border included): rows + group headers +
    /// border, capped at the room above the composer.
    pub fn size(&self, available: u16, room: u16) -> (u16, u16) {
        let text = self
            .entries
            .iter()
            .map(|entry| entry.label.len() + entry.hint.len())
            .max()
            .unwrap_or(0);
        let rows = self.entries.len()
            + group_count(self.entries)
            + usize::from(self.loading && self.entries.is_empty());
        let width = (text + 8) as u16;
        let height = (rows as u16 + 2).min(room);
        // #19: never wider than the terminal (the min is a floor).
        (width.max(16).min(available), height)
    }
}

/// The number of group-header rows the entries need.
fn group_count(entries: &[crate::ui::catalog::CatalogEntry]) -> usize {
    let mut groups = 0;
    let mut previous: Option<&str> = None;
    for entry in entries {
        if previous != Some(entry.group) {
            groups += 1;
            previous = Some(entry.group);
        }
    }
    groups
}

impl Widget for SeedPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title_style(
                Style::new()
                    .add_modifier(Modifier::BOLD)
                    .fg(self.theme.accent),
            )
            .title(tr(self.locale, self.kind.title()));
        let inner = block.inner(area);
        block.render(area, buf);
        // #11 popup treatment: panel_bg fill after Clear, inside the border.
        buf.set_style(inner, style::panel_fill(self.theme));
        let mut y = inner.y;
        let mut previous_group: Option<&str> = None;
        for (i, entry) in self.entries.iter().enumerate() {
            if y >= inner.bottom() {
                break;
            }
            // A group header line when the group changes.
            if previous_group != Some(entry.group) {
                previous_group = Some(entry.group);
                buf.set_line(
                    inner.x,
                    y,
                    &Line::styled(
                        format!(" {}", tr(self.locale, entry.group)),
                        style::header(self.theme),
                    ),
                    inner.width,
                );
                y += 1;
                if y >= inner.bottom() {
                    break;
                }
            }
            if i == self.selected {
                buf.set_style(
                    Rect::new(inner.x, y, inner.width, 1),
                    style::selection(self.theme),
                );
            }
            let line = Line::from(vec![
                Span::raw(format!(" {}", entry.label)),
                Span::styled(format!("  {}", entry.hint), style::hint(self.theme)),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
            y += 1;
        }
        if self.loading && self.entries.is_empty() && y < inner.bottom() {
            buf.set_line(
                inner.x,
                y,
                &Line::styled(tr(self.locale, "catalog.loading"), style::hint(self.theme)),
                inner.width,
            );
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
        composer.popup_move(1, 3);
        assert_eq!(composer.popup_selected(), 1);
        composer.popup_accept("/clear");
        assert_eq!(composer.buffer(), "/clear ");
        assert_eq!(composer.popup(), None, "trailing space closes the popup");
    }

    #[test]
    fn popup_move_clamps() {
        let mut composer = composer("@");
        composer.popup_move(-1, 3);
        assert_eq!(composer.popup_selected(), 0);
        composer.popup_move(99, 3);
        assert_eq!(composer.popup_selected(), 2);
    }

    #[test]
    fn caret_layout_scrolls_long_lines() {
        let composer = composer("abcdefghij");
        let (row, col, scroll) = composer.caret_layout(5);
        assert_eq!(row, 0);
        assert_eq!(col, 4, "caret stays inside the area");
        assert_eq!(scroll, 6);
    }

    #[test]
    fn caret_follow_scrolls_tall_buffers_vertically() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut c = Composer::new();
        for i in 0..200 {
            for ch in format!("line-{i}").chars() {
                c.insert_char(ch);
            }
            if i < 199 {
                c.newline();
            }
        }
        let backend = TestBackend::new(30, 3);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                ComposerView {
                    composer: &c,
                    focused: false,
                    theme: &crate::theme::Theme::default(),
                    locale: Locale::En,
                },
                f.area(),
            )
        })
        .unwrap();
        let text = format!("{}", term.backend());
        // Inner height 2 (the top border): a 200-line buffer scrolls so
        // the caret row (line-199) is the bottom visible line — never the
        // top of the buffer with the caret off-screen.
        assert!(text.contains("line-199"), "caret line visible: {text}");
        assert!(text.contains("line-198"), "the line above it: {text}");
        assert!(
            !text.contains("line-0"),
            "buffer start scrolled away: {text}"
        );
    }

    #[test]
    fn layout_height_floors_grows_and_caps() {
        assert_eq!(
            Composer::new().layout_height(8),
            2,
            "empty buffer keeps the 2-row floor"
        );
        let three = composer("a\nb\nc");
        assert_eq!(three.layout_height(8), 4, "line count + the top rule");
        // 200 newlines: the strip grows with content up to the ceiling.
        let book = composer(&"x\n".repeat(199));
        assert_eq!(book.line_count(), 200);
        assert_eq!(book.layout_height(8), 8, "capped at the caller ceiling");
        assert_eq!(book.layout_height(250), 201, "grows to the ceiling");
    }
}
