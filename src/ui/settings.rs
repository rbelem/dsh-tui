//! The settings view (ticket 07 Settings row): a full-screen two-pane
//! surface — a nav list on the left, the selected namespace's form on the
//! right — driven by `settings.describe` and written via `settings.update`.
//!
//! Nav: five hardcoded sections (General, Models, Plugins, Agent presets,
//! Permission presets), each mapped to a same-named settings namespace when
//! `settings.describe` exposes one (there is no list/read RPC — describe
//! returns every namespace). Namespaces describe exposes that no section
//! claims are appended as extra nav entries; a section without a namespace
//! shows a "not exposed" pane.
//!
//! The form is schema-driven (v1): the namespace's `schema.properties` (a
//! JSON-schema-ish object) gives each field's label (`title`, else the key)
//! and kind — string/number/boolean edit inline, string enums cycle, and
//! anything else renders read-only raw JSON. Fields sort by key (serde_json
//! maps are BTreeMaps). The whole view is loopback RPCs; themes stay in the
//! Ctrl+T picker (available here too), not in this view.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding, Paragraph, Widget, Wrap};

use crate::i18n::{Locale, tr, trf};
use crate::ui::style;
use crate::wire::settings::{AppliesMode, SettingsNamespaceView};
use unicode_width::UnicodeWidthStr;

/// The v1 nav sections: (label, candidate namespace, i18n key). The label
/// is the en display string; the key translates it (discovered namespaces
/// have no key and render their raw label).
pub const SECTIONS: &[(&str, &str, &str)] = &[
    ("General", "general", "settings.section.general"),
    ("Models", "models", "settings.section.models"),
    ("Plugins", "plugins", "settings.section.plugins"),
    (
        "Agent presets",
        "agent-presets",
        "settings.section.agent_presets",
    ),
    (
        "Permission presets",
        "permission-presets",
        "settings.section.permission_presets",
    ),
];

/// Which pane holds the keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsFocus {
    #[default]
    Nav,
    Form,
}

/// One nav entry: the section label and the namespace it binds to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSection {
    pub label: String,
    pub ns: String,
    /// The i18n key for the label (`None` for discovered namespaces).
    pub label_key: Option<&'static str>,
}

impl SettingsSection {
    /// The display label for `locale`: translated when a key exists, else
    /// the raw (host-provided) label.
    pub fn label_for(&self, locale: Locale) -> String {
        match self.label_key {
            Some(key) => tr(locale, key).to_string(),
            None => self.label.clone(),
        }
    }
}

/// Pad `text` to `width` display cells (CJK-safe: `{:24}` pads by chars,
/// which misaligns wide labels).
fn pad_width(text: &str, width: usize) -> String {
    let text_width = UnicodeWidthStr::width(text);
    let mut padded = text.to_string();
    padded.push_str(&" ".repeat(width.saturating_sub(text_width)));
    padded
}

/// How a field edits, parsed from its schema property.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// `"type": "string"` — typed inline editor.
    Text,
    /// `"type": "number" | "integer"` — typed inline editor, validated.
    Number,
    /// `"type": "boolean"` — Space/Enter toggles.
    Boolean,
    /// string `"enum"` — Space/Enter cycles the options.
    Choice(Vec<String>),
    /// Anything else — read-only raw JSON (documented v1 fallback).
    Raw,
}

/// One form field: the schema-derived label/kind plus the working value.
/// The working copy edits freely; `dirty` compares it against the
/// described value and the save PATCH carries only changed keys.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsField {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub value: serde_json::Value,
}

impl SettingsField {
    /// The value rendered on the form row.
    pub fn display_value(&self) -> String {
        match &self.value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        }
    }
}

/// One namespace's editable form: the described view plus the parsed
/// fields (the working copy the user edits).
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsForm {
    pub view: SettingsNamespaceView,
    pub fields: Vec<SettingsField>,
    /// The selected field (Up/Down).
    pub cursor: usize,
    /// The inline editor while a string/number field is being edited.
    pub editing: Option<LineEditor>,
}

impl SettingsForm {
    /// Parse a described namespace view into a form: one field per
    /// `schema.properties` entry (plus any value keys the schema doesn't
    /// declare — shown read-only), working values seeded from `value`.
    pub fn from_view(view: SettingsNamespaceView) -> Self {
        let properties = view
            .schema
            .get("properties")
            .and_then(serde_json::Value::as_object);
        let mut keys: Vec<String> = properties
            .map(|props| props.keys().cloned().collect())
            .unwrap_or_default();
        if let Some(values) = view.value.as_object() {
            for key in values.keys() {
                if !keys.contains(key) {
                    keys.push(key.clone());
                }
            }
        }
        let fields = keys
            .into_iter()
            .map(|key| {
                let property = properties.and_then(|props| props.get(&key));
                let label = property
                    .and_then(|prop| prop.get("title"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&key)
                    .to_string();
                let kind = field_kind(property);
                let value = view
                    .value
                    .get(&key)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                SettingsField {
                    key,
                    label,
                    kind,
                    value,
                }
            })
            .collect();
        SettingsForm {
            view,
            fields,
            cursor: 0,
            editing: None,
        }
    }

    /// The PATCH payload for `settings.update`: only keys whose working
    /// value differs from the described value.
    pub fn patch(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut patch = serde_json::Map::new();
        for field in &self.fields {
            let described = self.view.value.get(&field.key);
            if described != Some(&field.value) {
                patch.insert(field.key.clone(), field.value.clone());
            }
        }
        patch
    }

    /// Whether any working value diverges from the described value.
    pub fn dirty(&self) -> bool {
        !self.patch().is_empty()
    }

    /// Rebuild from a fresh describe (after a save or a conflict refresh):
    /// keep the cursor where it still fits, drop edits.
    pub fn refresh(&mut self, view: SettingsNamespaceView) {
        let cursor = self.cursor;
        *self = SettingsForm::from_view(view);
        self.cursor = cursor.min(self.fields.len().saturating_sub(1));
    }

    /// Commit the inline editor into the selected field. Numbers that
    /// don't parse keep the editor open (`Err` with a hint).
    pub fn commit_edit(&mut self) -> Result<(), &'static str> {
        let Some(editor) = self.editing.take() else {
            return Ok(());
        };
        let Some(field) = self.fields.get_mut(self.cursor) else {
            return Ok(());
        };
        let value = match field.kind {
            FieldKind::Number => match editor.buffer.parse::<f64>() {
                Ok(number) => serde_json::json!(number),
                Err(_) => {
                    self.editing = Some(editor);
                    return Err("not a number");
                }
            },
            _ => serde_json::Value::String(editor.buffer),
        };
        field.value = value;
        Ok(())
    }
}

/// Parse a schema property into a field kind.
fn field_kind(property: Option<&serde_json::Value>) -> FieldKind {
    let Some(property) = property else {
        return FieldKind::Raw;
    };
    if let Some(options) = property.get("enum").and_then(serde_json::Value::as_array) {
        let options: Vec<String> = options
            .iter()
            .filter_map(|option| option.as_str().map(str::to_string))
            .collect();
        if !options.is_empty() {
            return FieldKind::Choice(options);
        }
    }
    match property.get("type").and_then(serde_json::Value::as_str) {
        Some("string") => FieldKind::Text,
        Some("number" | "integer") => FieldKind::Number,
        Some("boolean") => FieldKind::Boolean,
        _ => FieldKind::Raw,
    }
}

/// A minimal single-line editor for string/number fields (the composer is
/// multi-line with seed popups — overkill here). Buffer + byte caret on a
/// char boundary, mirroring the composer's primitives.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineEditor {
    pub buffer: String,
    pub caret: usize,
}

impl LineEditor {
    pub fn new(text: String) -> Self {
        let caret = text.len();
        LineEditor {
            buffer: text,
            caret,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.caret, c);
        self.caret += c.len_utf8();
    }

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
    }

    pub fn move_left(&mut self) {
        if self.caret == 0 {
            return;
        }
        let prev = self.buffer[..self.caret]
            .chars()
            .next_back()
            .map(char::len_utf8)
            .unwrap_or(0);
        self.caret -= prev;
    }

    pub fn move_right(&mut self) {
        if let Some(c) = self.buffer[self.caret..].chars().next() {
            self.caret += c.len_utf8();
        }
    }

    /// The buffer with a caret glyph spliced in (the takeover-style views
    /// don't place the real terminal cursor; `▌` marks it instead).
    pub fn display(&self) -> String {
        let mut shown = self.buffer.clone();
        shown.insert(self.caret, '▌');
        shown
    }
}

/// The settings view state: nav sections, the loaded forms, and focus.
/// Lives inside [`crate::ui::takeover::Mode::Settings`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SettingsState {
    pub sections: Vec<SettingsSection>,
    /// The selected nav row.
    pub selected: usize,
    pub focus: SettingsFocus,
    /// Parsed forms keyed by namespace (filled by the describe result).
    pub forms: HashMap<String, SettingsForm>,
    /// A describe is in flight (opening or a conflict refresh).
    pub loading: bool,
    /// A save is in flight (further save keys ignored).
    pub saving: bool,
}

impl SettingsState {
    pub fn new() -> Self {
        SettingsState {
            sections: SECTIONS
                .iter()
                .map(|(label, ns, key)| SettingsSection {
                    label: (*label).to_string(),
                    ns: (*ns).to_string(),
                    label_key: Some(*key),
                })
                .collect(),
            loading: true,
            ..SettingsState::default()
        }
    }

    /// Fold a describe result in: parse each namespace's form, and append
    /// namespaces no v1 section claims as extra nav entries.
    pub fn apply_describe(&mut self, value: crate::wire::settings::SettingsDescribeValue) {
        self.loading = false;
        self.forms = value
            .namespaces
            .into_iter()
            .map(|view| (view.ns.clone(), SettingsForm::from_view(view)))
            .collect();
        let claimed: Vec<&str> = SECTIONS.iter().map(|(_, ns, _)| *ns).collect();
        let mut extras: Vec<String> = self
            .forms
            .keys()
            .filter(|ns| !claimed.contains(&ns.as_str()))
            .cloned()
            .collect();
        extras.sort();
        for ns in extras {
            self.sections.push(SettingsSection {
                label: ns.clone(),
                ns,
                label_key: None,
            });
        }
        self.selected = self.selected.min(self.sections.len().saturating_sub(1));
    }

    /// The selected section's namespace, when loaded.
    pub fn selected_form(&self) -> Option<&SettingsForm> {
        let section = self.sections.get(self.selected)?;
        self.forms.get(&section.ns)
    }

    pub fn selected_form_mut(&mut self) -> Option<&mut SettingsForm> {
        let section = self.sections.get(self.selected)?;
        self.forms.get_mut(&section.ns)
    }

    /// Whether any loaded form has unsaved edits (the Esc warning).
    pub fn dirty(&self) -> bool {
        self.forms.values().any(SettingsForm::dirty)
    }

    /// Move the nav cursor (clamped), resetting the form focus.
    pub fn move_selection(&mut self, delta: isize) {
        if self.sections.is_empty() {
            return;
        }
        let last = self.sections.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        if let Some(form) = self.selected_form_mut() {
            form.cursor = form.cursor.min(form.fields.len().saturating_sub(1));
        }
    }
}

/// The settings body (full-screen): a dim-bordered frame like the
/// takeovers, split into the nav list and the selected namespace's form.
pub struct SettingsView<'a> {
    pub state: &'a SettingsState,
    /// Transient notice (toast/hint), rendered dim at the bottom.
    pub notice: Option<&'a str>,
    pub theme: &'a crate::theme::Theme,
    pub locale: Locale,
}

impl Widget for SettingsView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::bordered()
            .border_style(style::border(self.theme))
            .title(tr(self.locale, "settings.title"))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        block.render(area, buf);

        // Nav column: fixed, capped so narrow terminals keep a usable form.
        let nav_width = (inner.width / 3).clamp(16, 28);
        let [nav_area, form_area] =
            Layout::horizontal([Constraint::Length(nav_width), Constraint::Fill(1)]).areas(inner);
        let [nav_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(nav_area);

        // --- nav pane ---
        let mut nav_lines: Vec<Line> = vec![Line::raw("")];
        for (row, section) in self.state.sections.iter().enumerate() {
            let known = self.state.forms.contains_key(&section.ns);
            let label = section.label_for(self.locale);
            let label = if known {
                label
            } else {
                format!("{label}{}", tr(self.locale, "settings.unexposed_mark"))
            };
            let style = if row == self.state.selected {
                style::selection(self.theme)
            } else if known {
                style::header(self.theme)
            } else {
                style::hint(self.theme)
            };
            nav_lines.push(Line::styled(format!(" {label}"), style));
        }
        Paragraph::new(nav_lines).render(nav_area, buf);

        // --- form pane ---
        let form_focused = self.state.focus == SettingsFocus::Form;
        let mut lines: Vec<Line> = vec![Line::raw("")];
        if self.state.loading {
            lines.push(Line::styled(
                tr(self.locale, "settings.loading"),
                style::hint(self.theme),
            ));
        } else if let Some(form) = self.state.selected_form() {
            let applies = match form.view.applies {
                AppliesMode::Live => tr(self.locale, "settings.applies_live"),
                AppliesMode::Restart => tr(self.locale, "settings.applies_restart"),
            };
            lines.push(Line::styled(
                trf(
                    self.locale,
                    "settings.revision",
                    &[applies, &format!("{:.0}", form.view.revision)],
                ),
                style::hint(self.theme),
            ));
            lines.push(Line::raw(""));
            for (row, field) in form.fields.iter().enumerate() {
                let focused = form_focused && row == form.cursor;
                let editing = focused && form.editing.is_some();
                let marker = if focused { "› " } else { "  " };
                let value = if editing {
                    form.editing
                        .as_ref()
                        .map(LineEditor::display)
                        .unwrap_or_default()
                } else {
                    field.display_value()
                };
                let changed = form.view.value.get(&field.key) != Some(&field.value);
                let read_only = matches!(field.kind, FieldKind::Raw);
                let mut spans = vec![
                    Span::raw(format!("{marker}{}", pad_width(&field.label, 24))),
                    Span::styled(
                        value,
                        if read_only {
                            style::hint(self.theme)
                        } else if editing {
                            style::active(self.theme)
                        } else {
                            ratatui::style::Style::default().fg(self.theme.text)
                        },
                    ),
                ];
                if read_only {
                    spans.push(Span::styled(
                        format!("  {}", tr(self.locale, "settings.read_only")),
                        style::hint(self.theme),
                    ));
                }
                if changed {
                    spans.push(Span::styled("  *", style::warning(self.theme)));
                }
                lines.push(Line::from(spans));
                if focused {
                    let y = inner.y + lines.len() as u16 - 1;
                    if y < form_area.bottom() {
                        buf.set_style(
                            Rect::new(form_area.x, y, form_area.width, 1),
                            style::selection(self.theme),
                        );
                    }
                }
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("enter", style::active(self.theme)),
                Span::raw(tr(self.locale, "settings.action_edit")),
                Span::styled("ctrl+s", style::active(self.theme)),
                Span::raw(tr(self.locale, "settings.action_save")),
                Span::styled("esc", style::active(self.theme)),
                Span::raw(tr(self.locale, "settings.action_close")),
            ]));
        } else if let Some(section) = self.state.sections.get(self.state.selected) {
            lines.push(Line::styled(
                trf(self.locale, "settings.not_exposed", &[&section.ns]),
                style::hint(self.theme),
            ));
        }
        if let Some(notice) = self.notice {
            lines.push(Line::styled(notice, style::hint(self.theme)));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(form_area, buf);

        // --- footer: pane-switch hint ---
        let hint = match self.state.focus {
            SettingsFocus::Nav => tr(self.locale, "settings.footer_nav"),
            SettingsFocus::Form => tr(self.locale, "settings.footer_form"),
        };
        Paragraph::new(Line::styled(hint, style::hint(self.theme))).render(footer_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(
        ns: &str,
        schema: serde_json::Value,
        value: serde_json::Value,
    ) -> SettingsNamespaceView {
        SettingsNamespaceView {
            ns: ns.into(),
            schema,
            value,
            base: None,
            user: None,
            applies: AppliesMode::Live,
            secrets: vec![],
            revision: 1.0,
        }
    }

    fn general_view() -> SettingsNamespaceView {
        view(
            "general",
            serde_json::json!({"type": "object", "properties": {
                "language": {"type": "string", "title": "Language"},
                "maxTokens": {"type": "number", "title": "Max tokens"},
                "verbose": {"type": "boolean", "title": "Verbose"},
                "logLevel": {"type": "string", "enum": ["quiet", "normal"], "title": "Log level"},
                "metadata": {"type": "object"}
            }}),
            serde_json::json!({"language": "en", "maxTokens": 4096, "verbose": false, "logLevel": "normal", "metadata": {"a": 1}, "extra": "x"}),
        )
    }

    #[test]
    fn form_parses_kinds_labels_and_extras() {
        let form = SettingsForm::from_view(general_view());
        let kinds: Vec<(&str, &FieldKind)> = form
            .fields
            .iter()
            .map(|field| (field.key.as_str(), &field.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("language", &FieldKind::Text),
                (
                    "logLevel",
                    &FieldKind::Choice(vec!["quiet".into(), "normal".into()])
                ),
                ("maxTokens", &FieldKind::Number),
                ("metadata", &FieldKind::Raw),
                ("verbose", &FieldKind::Boolean),
                ("extra", &FieldKind::Raw),
            ],
            "schema-declared fields sort by key; undeclared value keys append read-only"
        );
        assert_eq!(form.fields[0].label, "Language");
        assert_eq!(form.fields[5].label, "extra", "no title → the key");
    }

    #[test]
    fn patch_carries_only_changed_keys() {
        let mut form = SettingsForm::from_view(general_view());
        assert!(!form.dirty());
        form.fields[0].value = serde_json::json!("zh"); // language
        form.fields[4].value = serde_json::json!(true); // verbose
        let patch = form.patch();
        assert_eq!(
            patch,
            serde_json::json!({"language": "zh", "verbose": true})
                .as_object()
                .cloned()
                .unwrap()
        );
        assert!(form.dirty());
    }

    #[test]
    fn refresh_drops_edits_and_clamps_the_cursor() {
        let mut form = SettingsForm::from_view(general_view());
        form.cursor = 5;
        form.fields[1].value = serde_json::json!("zh");
        form.refresh(view(
            "general",
            serde_json::json!({}),
            serde_json::json!({}),
        ));
        assert!(form.fields.is_empty());
        assert_eq!(form.cursor, 0);
        assert!(!form.dirty());
    }

    #[test]
    fn commit_rejects_a_bad_number_and_keeps_editing() {
        let mut form = SettingsForm::from_view(general_view());
        form.cursor = 2; // maxTokens
        form.editing = Some(LineEditor::new("abc".into()));
        assert_eq!(form.commit_edit(), Err("not a number"));
        assert!(form.editing.is_some());
        form.editing = Some(LineEditor::new("8192".into()));
        assert_eq!(form.commit_edit(), Ok(()));
        assert_eq!(form.fields[2].value, serde_json::json!(8192.0));
    }

    #[test]
    fn apply_describe_appends_unclaimed_namespaces() {
        let mut state = SettingsState::new();
        assert!(state.loading);
        state.apply_describe(crate::wire::settings::SettingsDescribeValue {
            writable: true,
            has_document: true,
            namespaces: vec![
                general_view(),
                view("plugins", serde_json::json!({}), serde_json::json!({})),
                view("locale", serde_json::json!({}), serde_json::json!({})),
            ],
        });
        assert!(!state.loading);
        let labels: Vec<&str> = state
            .sections
            .iter()
            .map(|section| section.label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec![
                "General",
                "Models",
                "Plugins",
                "Agent presets",
                "Permission presets",
                "locale"
            ]
        );
        assert!(state.forms.contains_key("general"));
    }
}
