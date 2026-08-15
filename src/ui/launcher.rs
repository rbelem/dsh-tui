//! The Ctrl+P global launcher (Q17): a fuzzy search over every client
//! command, the cached skills, and the settings actions. Picking an entry
//! dispatches immediately — commands and skills go straight through the
//! prompt path (no leading-input state, mirroring the web's launcher
//! semantics), actions execute in place. Groups reuse the catalog's group
//! keys (commands / skills) plus a launcher-only actions group.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::i18n::Locale;

/// What picking a launcher entry does.
#[derive(Debug, Clone, PartialEq)]
pub enum LauncherAction {
    /// Insert the text into the composer and submit it through the prompt
    /// path (a `/command` or `@skill` token — the gateway dispatches slash
    /// text via `session.prompt`).
    Dispatch { text: String },
    /// Open the settings view (the Ctrl+, action).
    OpenSettings,
    /// Toggle the theme picker (the Ctrl+T action).
    OpenThemePicker,
    /// Cycle the UI locale (the Ctrl+L action).
    CycleLocale,
    /// Quit (the Ctrl+Q action).
    Quit,
}

/// One launcher row: the group header key, the display label, the hint, and
/// the action it dispatches.
#[derive(Debug, Clone, PartialEq)]
pub struct LauncherEntry {
    /// i18n key for the group header line (`catalog.group.*` /
    /// `launcher.group.actions`).
    pub group: &'static str,
    pub label: String,
    pub hint: String,
    pub action: LauncherAction,
}

fn command(text: &str, hint_key: &'static str, locale: Locale) -> LauncherEntry {
    LauncherEntry {
        group: "catalog.group.commands",
        label: text.to_string(),
        hint: crate::i18n::tr(locale, hint_key).to_string(),
        action: LauncherAction::Dispatch {
            text: text.to_string(),
        },
    }
}

fn skill(name: &str, description: &str) -> LauncherEntry {
    LauncherEntry {
        group: "catalog.group.skills",
        label: name.to_string(),
        hint: description.to_string(),
        // The `@` trigger rides along, like the composer's `@` catalog.
        action: LauncherAction::Dispatch {
            text: format!("@{name}"),
        },
    }
}

fn action(
    locale: Locale,
    label_key: &'static str,
    shortcut: &'static str,
    action: LauncherAction,
) -> LauncherEntry {
    LauncherEntry {
        group: "launcher.group.actions",
        label: crate::i18n::tr(locale, label_key).to_string(),
        hint: shortcut.to_string(),
        action,
    }
}

/// The full launcher catalog for `locale`: mirrored commands (the
/// `SLASH_COMMANDS` list the `/`-menu reuses), the cached skills (raw
/// host descriptions), and the settings actions (labels localized,
/// shortcuts language-neutral).
pub fn launcher_entries(
    locale: Locale,
    skills: &[crate::wire::skills::SkillEntry],
) -> Vec<LauncherEntry> {
    let mut entries = Vec::new();
    for (text, hint_key) in crate::ui::catalog::SLASH_COMMANDS {
        entries.push(command(text, hint_key, locale));
    }
    for entry in skills {
        entries.push(skill(&entry.name, &entry.description));
    }
    entries.push(action(
        locale,
        "launcher.action.settings",
        "Ctrl+,",
        LauncherAction::OpenSettings,
    ));
    entries.push(action(
        locale,
        "launcher.action.themes",
        "Ctrl+T",
        LauncherAction::OpenThemePicker,
    ));
    entries.push(action(
        locale,
        "launcher.action.locale",
        "Ctrl+L",
        LauncherAction::CycleLocale,
    ));
    entries.push(action(
        locale,
        "launcher.action.quit",
        "Ctrl+Q",
        LauncherAction::Quit,
    ));
    entries
}

/// Subsequence-match score: each matched char scores by its consecutive
/// run (chars adjacent in the label accumulate), so runs rank above
/// scattered matches; ties keep the stable entry order. `None` when the
/// needle isn't a subsequence of the label.
pub fn fuzzy_score(needle: &str, label: &str) -> Option<u32> {
    let needle: Vec<char> = needle.to_ascii_lowercase().chars().collect();
    let label: Vec<char> = label.to_ascii_lowercase().chars().collect();
    if needle.is_empty() {
        return Some(u32::MAX);
    }
    let mut score = 0u32;
    let mut run = 0u32;
    let mut prev: Option<usize> = None;
    let mut from = 0usize;
    for &c in &needle {
        let mut found = None;
        for (j, &l) in label.iter().enumerate().skip(from) {
            if l == c {
                found = Some(j);
                break;
            }
        }
        let j = found?;
        run = if prev == Some(j.saturating_sub(1)) {
            run + 1
        } else {
            1
        };
        score += run * 10 + 1;
        prev = Some(j);
        from = j + 1;
    }
    Some(score)
}

/// Filter `entries` by the typed search text: subsequence matches only,
/// ranked by [`fuzzy_score`] (higher first — consecutive runs beat
/// scattered matches; the empty needle keeps the full list in order).
pub fn fuzzy_filter(entries: &[LauncherEntry], needle: &str) -> Vec<LauncherEntry> {
    let mut scored: Vec<(u32, usize, &LauncherEntry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| fuzzy_score(needle, &entry.label).map(|score| (score, i, entry)))
        .collect();
    // Higher score first; stable so ties keep the original entry order.
    scored.sort_by(|(a, _, _), (b, _, _)| b.cmp(a));
    scored
        .into_iter()
        .map(|(_, _, entry)| entry.clone())
        .collect()
}

/// The Ctrl+P launcher popup: a centered overlay with a search line on
/// top, the grouped fuzzy results below, and a loading row while the skill
/// catalog is in flight. The search text comes from the app's launcher
/// state (a composer-buffer input); rendering stays read-only.
pub struct LauncherPopup<'a> {
    pub entries: &'a [LauncherEntry],
    pub selected: usize,
    pub search: &'a str,
    pub loading: bool,
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl LauncherPopup<'_> {
    /// Outer size (border included): the search line + group headers +
    /// entries + optional loading row, capped at the room available.
    pub fn size(&self, available: u16, room: u16) -> (u16, u16) {
        let text = self
            .entries
            .iter()
            .map(|entry| entry.label.len() + entry.hint.len())
            .max()
            .unwrap_or(0)
            .max(self.search.len());
        let groups = group_count(self.entries);
        let rows = 1 + self.entries.len() + groups + usize::from(self.loading);
        let width = (text + 10) as u16;
        let height = (rows as u16 + 2).min(room);
        (width.clamp(24, available.max(24)), height)
    }
}

/// The number of group-header rows the entries need.
fn group_count(entries: &[LauncherEntry]) -> usize {
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

impl Widget for LauncherPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(crate::ui::style::border(self.theme))
            .title_style(
                ratatui::style::Style::new()
                    .add_modifier(ratatui::style::Modifier::BOLD)
                    .fg(self.theme.accent),
            )
            .title(crate::i18n::tr(self.locale, "launcher.title"));
        let inner = block.inner(area);
        block.render(area, buf);
        // #11 popup treatment: panel_bg fill after Clear, inside the border.
        buf.set_style(inner, crate::ui::style::panel_fill(self.theme));
        let mut y = inner.y;

        // The search line: the typed text, or the placeholder while empty.
        let search_line = if self.search.is_empty() {
            Line::styled(
                format!(" {}", crate::i18n::tr(self.locale, "launcher.search")),
                crate::ui::style::hint(self.theme),
            )
        } else {
            Line::raw(format!(" {}", self.search))
        };
        buf.set_line(inner.x, y, &search_line, inner.width);
        y += 1;

        let mut previous_group: Option<&str> = None;
        for (i, entry) in self.entries.iter().enumerate() {
            if y >= inner.bottom() {
                break;
            }
            if previous_group != Some(entry.group) {
                previous_group = Some(entry.group);
                buf.set_line(
                    inner.x,
                    y,
                    &Line::styled(
                        format!(" {}", crate::i18n::tr(self.locale, entry.group)),
                        crate::ui::style::header(self.theme),
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
                    crate::ui::style::selection(self.theme),
                );
            }
            let line = Line::from(vec![
                Span::raw(format!(" {}", entry.label)),
                Span::styled(
                    format!("  {}", entry.hint),
                    crate::ui::style::hint(self.theme),
                ),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
            y += 1;
        }
        if y >= inner.bottom() {
            return;
        }
        if self.entries.is_empty() {
            buf.set_line(
                inner.x,
                y,
                &Line::styled(
                    format!(" {}", crate::i18n::tr(self.locale, "launcher.empty")),
                    crate::ui::style::hint(self.theme),
                ),
                inner.width,
            );
        } else if self.loading {
            buf.set_line(
                inner.x,
                y,
                &Line::styled(
                    format!(" {}", crate::i18n::tr(self.locale, "catalog.loading")),
                    crate::ui::style::hint(self.theme),
                ),
                inner.width,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, group: &'static str) -> LauncherEntry {
        LauncherEntry {
            group,
            label: label.to_string(),
            hint: String::new(),
            action: LauncherAction::Quit,
        }
    }

    #[test]
    fn fuzzy_score_ranks_runs_above_scattered() {
        assert_eq!(fuzzy_score("cle", "/clear"), Some(11 + 21 + 31));
        assert_eq!(fuzzy_score("cle", "cycle locale"), Some(11 + 11 + 21));
        assert_eq!(fuzzy_score("cle", "/model"), None, "not a subsequence");
        assert_eq!(fuzzy_score("", "/anything"), Some(u32::MAX));
        assert_eq!(
            fuzzy_score("MOD", "/model"),
            Some(11 + 21 + 31),
            "case-insensitive"
        );
    }

    #[test]
    fn fuzzy_filter_keeps_ties_in_order_and_filters() {
        let entries = vec![
            entry("/help", "commands"),
            entry("/compact", "commands"),
            entry("commit", "skills"),
        ];
        // "com" matches /compact and commit with equal scores; the stable
        // sort keeps the original order (commands before skills).
        let filtered = fuzzy_filter(&entries, "com");
        assert_eq!(
            filtered
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>(),
            vec!["/compact", "commit"]
        );
        assert!(fuzzy_filter(&entries, "zzz").is_empty());
        assert_eq!(
            fuzzy_filter(&entries, "").len(),
            3,
            "empty needle keeps all"
        );
    }
}
