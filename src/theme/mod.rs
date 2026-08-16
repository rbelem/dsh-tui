//! Theme registry (ticket 07): semantic color tokens, bundled + user themes,
//! the Ctrl+T picker, and config persistence.
//!
//! Tokens (design contract): `accent`/`muted`/`error`/`warning`/`success`/
//! `code`/`bg`/`text`, plus the #11 additions `panel_bg`/`border` and the
//! user-message tint `user_bg`. The
//! default theme is terminal-following — every token is `Reset`, preserving
//! the modifiers-only look; palette themes render on truecolor terminals
//! (RGB as-is) and 256-color terminals (RGB snapped to the nearest xterm
//! index via [`detect::best_color`], #4). 16-color terminals keep the
//! Reset-based default.
//!
//! Bundled themes live in `themes/*.toml` (embedded via `include_str!`); user
//! themes live in `$XDG_CONFIG_HOME/dsh-tui/themes/*.toml` (or
//! `~/.config/dsh-tui/themes/`), loaded at startup, replacing same-named
//! bundled entries. The picker persists the choice to
//! `$XDG_CONFIG_HOME/dsh-tui/config.toml` (`theme = "name"`).
//!
//! With no explicit theme, [`detect::detect_color_mode`] picks the startup
//! default on color terminals: `dsh-dark` for dark schemes (and for
//! detection failures — issue #11: OSC 11 unanswered must never leave the
//! app monochrome), `dsh-light` for light ones (OSC 11 terminal query, then
//! env signals, then desktop settings). `DSH_THEME=<name>` beats the
//! persisted config; the Reset-based neutral stays available opt-in
//! (non-truecolor terminals, or an explicit `default` name).

pub mod bundled;
pub mod detect;

use std::collections::HashMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};
use serde::{Deserialize, Serialize};

/// One semantic theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub accent: Color,
    pub muted: Color,
    pub error: Color,
    pub warning: Color,
    pub success: Color,
    pub code: Color,
    pub bg: Color,
    pub text: Color,
    /// Panel/interior fill (sidebar, composer strip, popup interiors) —
    /// a stepped shade of `bg` (#11). Reset in the terminal-following
    /// default, so non-truecolor terminals skip bg fills entirely.
    pub panel_bg: Color,
    /// Subtle pane/box border color (#11). Distinct from `muted` so the
    /// palette can tune rule visibility independently of text.
    pub border: Color,
    /// Background tint behind user-message text — a stepped shade of `bg`
    /// (slightly lighter in dark themes, slightly darker in light ones).
    /// `None` = flat text (the terminal-following default).
    pub user_bg: Option<Color>,
}

impl Default for Theme {
    /// The terminal-following default: every token is `Reset`, so the
    /// modifiers-only look (the pre-theme renderer) is the fallback when no
    /// palette theme is configured.
    fn default() -> Self {
        Theme {
            name: "default".into(),
            accent: Color::Reset,
            muted: Color::Reset,
            error: Color::Reset,
            warning: Color::Reset,
            success: Color::Reset,
            code: Color::Reset,
            bg: Color::Reset,
            text: Color::Reset,
            panel_bg: Color::Reset,
            border: Color::Reset,
            user_bg: None,
        }
    }
}

impl Theme {
    /// Parse a theme from its TOML text (`name` + the `#rrggbb` hexes).
    /// The optional tokens: `panel_bg`/`border` fall back to `bg`/`muted`,
    /// `user_bg` to no tint (flat user text) — backward-compatible migration,
    /// existing user configs keep parsing and render unchanged.
    pub fn from_toml_str(toml: &str) -> Result<Self, ThemeError> {
        let raw: ThemeToml = toml::from_str(toml)
            .map_err(|e| ThemeError::Invalid(format!("bad theme TOML: {e}")))?;
        let name = raw.name.clone();
        let parse = |field: &str, value: &str| -> Result<Color, ThemeError> {
            parse_hex(value)
                .map_err(|reason| ThemeError::Invalid(format!("theme `{name}` {field}: {reason}")))
        };
        Ok(Theme {
            name: raw.name,
            accent: parse("accent", &raw.accent)?,
            muted: parse("muted", &raw.muted)?,
            error: parse("error", &raw.error)?,
            warning: parse("warning", &raw.warning)?,
            success: parse("success", &raw.success)?,
            code: parse("code", &raw.code)?,
            bg: parse("bg", &raw.bg)?,
            text: parse("text", &raw.text)?,
            panel_bg: match raw.panel_bg {
                Some(value) => parse("panel_bg", &value)?,
                None => parse("bg", &raw.bg)?,
            },
            border: match raw.border {
                Some(value) => parse("border", &value)?,
                None => parse("muted", &raw.muted)?,
            },
            // `user_bg` is optional with no fallback: absent = flat user
            // text (a theme without it renders exactly as before).
            user_bg: match raw.user_bg {
                Some(value) => Some(parse("user_bg", &value)?),
                None => None,
            },
        })
    }

    /// Snap every RGB token to what the terminal can actually render
    /// ([`detect::best_color`]), leaving non-RGB tokens (`Reset`, `Indexed`)
    /// untouched. Applied when a theme is resolved for the app (#4) — the
    /// registry keeps the exact RGB values, so the picker preview and user
    /// themes stay faithful on truecolor terminals.
    pub fn snapped(&self, level: detect::ColorLevel) -> Theme {
        let snap = |color: Color| match color {
            Color::Rgb(r, g, b) => detect::best_color((r, g, b), level),
            other => other,
        };
        Theme {
            name: self.name.clone(),
            accent: snap(self.accent),
            muted: snap(self.muted),
            error: snap(self.error),
            warning: snap(self.warning),
            success: snap(self.success),
            code: snap(self.code),
            bg: snap(self.bg),
            text: snap(self.text),
            panel_bg: snap(self.panel_bg),
            border: snap(self.border),
            user_bg: self.user_bg.map(snap),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ThemeToml {
    name: String,
    accent: String,
    muted: String,
    error: String,
    warning: String,
    success: String,
    code: String,
    bg: String,
    text: String,
    panel_bg: Option<String>,
    border: Option<String>,
    user_bg: Option<String>,
}

/// Parse `#rrggbb` (the bundled themes' canonical form; 3-digit shorthands
/// are not accepted).
fn parse_hex(hex: &str) -> Result<Color, String> {
    let hex = hex.trim();
    let Some(body) = hex.strip_prefix('#') else {
        return Err(format!("`{hex}`: expected #rrggbb"));
    };
    if body.len() != 6 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("`{hex}`: expected #rrggbb"));
    }
    let r = u8::from_str_radix(&body[0..2], 16).expect("validated hex");
    let g = u8::from_str_radix(&body[2..4], 16).expect("validated hex");
    let b = u8::from_str_radix(&body[4..6], 16).expect("validated hex");
    Ok(Color::Rgb(r, g, b))
}

/// Theme loading/parsing failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ThemeError {
    #[error("invalid theme: {0}")]
    Invalid(String),
    #[error("config error: {0}")]
    Config(String),
}

/// The available themes: bundled first, then user themes (same-name user
/// themes replace bundled entries).
#[derive(Debug, Clone, Default)]
pub struct ThemeRegistry {
    pub themes: Vec<Theme>,
}

impl ThemeRegistry {
    /// The bundled registry (embedded TOMLs).
    pub fn bundled() -> Self {
        let mut registry = ThemeRegistry { themes: Vec::new() };
        for (_, toml) in bundled::BUNDLED {
            match Theme::from_toml_str(toml) {
                Ok(theme) => registry.themes.push(theme),
                Err(error) => panic!("bundled theme broken: {error}"),
            }
        }
        registry
    }

    /// Load `$XDG_CONFIG_HOME/dsh-tui/themes/*.toml` (or
    /// `~/.config/dsh-tui/themes/`), replacing same-named bundled entries.
    /// Corrupt files are skipped — a bad user theme must not break startup.
    pub fn load_user_dir(&mut self) {
        let Some(dir) = user_themes_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(theme) = Theme::from_toml_str(&text) else {
                continue; // corrupt theme: skipped
            };
            if let Some(existing) = self.themes.iter_mut().find(|t| t.name == theme.name) {
                *existing = theme;
            } else {
                self.themes.push(theme);
            }
        }
    }

    /// The theme with `name`, if present.
    pub fn find(&self, name: &str) -> Option<&Theme> {
        self.themes.iter().find(|theme| theme.name == name)
    }
}

/// Whether the terminal can render palette themes (truecolor): the `COLORTERM`
/// environment variable is set to `truecolor` or `24bit`. When it is not, the
/// app stays on the Reset-based default rather than emitting RGB the
/// terminal cannot show.
pub fn terminal_supports_color() -> bool {
    matches!(
        std::env::var("COLORTERM").as_deref(),
        Ok("truecolor") | Ok("24bit")
    )
}

/// `$XDG_CONFIG_HOME/dsh-tui/themes` (or `~/.config/dsh-tui/themes`).
fn user_themes_dir() -> Option<PathBuf> {
    Some(config_root()?.join("themes"))
}

/// `$XDG_CONFIG_HOME/dsh-tui` (or `~/.config/dsh-tui`). `dirs` resolves the
/// directory per call, so tests can point it at a temp dir — via
/// `XDG_CONFIG_HOME` on Linux, via `HOME` on macOS (which ignores
/// `XDG_CONFIG_HOME` and uses `~/Library/Application Support`).
fn config_root() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("dsh-tui"))
}

// ---------------------------------------------------------------------------
// config persistence
// ---------------------------------------------------------------------------

/// The persisted app config (`config.toml`).
/// The `[gateway]` config section (#34/#35): the port fallback (below
/// `--port` and `DSH_PORT`) and whether a dead port auto-starts the
/// gateway on launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// The gateway port fallback; `None` = the 3080 default.
    #[serde(default)]
    pub port: Option<u16>,
    /// Auto-start the gateway when the resolved port isn't serving
    /// (default true — the herdr model; `false` keeps the error path).
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            port: None,
            auto_start: true,
        }
    }
}

fn default_auto_start() -> bool {
    true
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The active theme name; `None` = the terminal-following default.
    #[serde(default)]
    pub theme: Option<String>,
    /// The persisted UI locale (`"en"` / `"zh"`); `None` = the terminal-
    /// following default (`Locale::detect` resolves env when unset).
    #[serde(default)]
    pub locale: Option<String>,
    /// User-customizable keybindings; empty = the built-in defaults.
    #[serde(default)]
    pub keymap: Keymap,
    /// Gateway lifecycle settings (port fallback + auto-start).
    #[serde(default)]
    pub gateway: GatewayConfig,
}

impl Config {
    /// Read the config file; a missing or corrupt file yields the default.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Config::default();
        };
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    /// Write the config file (creating the directory).
    pub fn save(&self) -> Result<(), ThemeError> {
        let Some(path) = Self::path() else {
            return Err(ThemeError::Config("no config directory".into()));
        };
        let text =
            toml::to_string(self).map_err(|e| ThemeError::Config(format!("serialize: {e}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ThemeError::Config(format!("create {}: {e}", parent.display())))?;
        }
        std::fs::write(&path, text)
            .map_err(|e| ThemeError::Config(format!("write {}: {e}", path.display())))
    }

    /// `$XDG_CONFIG_HOME/dsh-tui/config.toml` (or `~/.config/dsh-tui/config.toml`).
    pub fn path() -> Option<PathBuf> {
        Some(config_root()?.join("config.toml"))
    }
}

// ---------------------------------------------------------------------------
// keybindings (`[keymap]` in config.toml)
// ---------------------------------------------------------------------------

/// The built-in key specs — the defaults a fresh config starts from.
/// `[keymap]` in `config.toml` overrides any of them by action name.
pub const DEFAULT_KEYBINDINGS: &[(&str, &str)] = &[
    // global
    ("quit", "ctrl+q"),
    ("cancel", "ctrl+c"),
    ("locale", "ctrl+l"),
    ("theme-picker", "ctrl+t"),
    ("settings", "ctrl+,"),
    ("launcher", "ctrl+p"),
    ("queue", "alt+q"),
    // mouse selection mode (#12): `v` in the chat arms drag-to-select;
    // the image viewer moved to `i` (it keeps its hardcoded binding as a
    // fallback when `image-viewer` is rebound, but `v` is selection's).
    ("selection-toggle", "v"),
    ("image-viewer", "i"),
    // #19: the narrow-terminal drawer (`s` toggles it at <80 cols; never
    // intercepts composer typing — the keymap check is focus-gated).
    ("drawer-toggle", "s"),
    // composer
    ("composer.submit", "enter"),
    ("composer.newline", "shift+enter"),
    ("composer.backspace", "backspace"),
    ("composer.delete", "delete"),
    ("composer.left", "left"),
    ("composer.right", "right"),
    ("composer.home", "home"),
    ("composer.end", "end"),
    ("composer.up", "up"),
    ("composer.down", "down"),
    ("composer.quit-eof", "ctrl+d"),
    ("composer.focus-chat", "esc"),
];

/// A parsed key spec (`ctrl+q`, `shift+enter`, `g`, `G`, `alt+up`...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeySpec {
    code: KeyCode,
    control: bool,
    alt: bool,
    shift: bool,
}

/// Parse a key-spec string. `None` = unparseable (the caller falls back to
/// the built-in default — a bad config can never hijack a key).
fn parse_key_spec(spec: &str) -> Option<KeySpec> {
    let parts: Vec<&str> = spec.split('+').collect();
    let (key, mods) = parts.split_last()?;
    let mut control = false;
    let mut alt = false;
    let mut shift = false;
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => control = true,
            "alt" => alt = true,
            "shift" => shift = true,
            _ => return None,
        }
    }
    let code = match *key {
        "esc" => KeyCode::Esc,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "space" => KeyCode::Char(' '),
        ch if ch.chars().count() == 1 => {
            let c = ch.chars().next().expect("single char");
            // An uppercase char implies shift (vim's `G`), except in a
            // chord where it's just the shifted spelling (`Ctrl+Q`).
            if c.is_uppercase() && !control && !alt {
                shift = true;
            }
            KeyCode::Char(c.to_ascii_lowercase())
        }
        _ => return None,
    };
    Some(KeySpec {
        code,
        control,
        alt,
        shift,
    })
}

impl KeySpec {
    /// Does this key event match the spec? Modifiers must match exactly
    /// (a bare `g` never matches `ctrl+g`); an uppercase char in the spec
    /// requires shift and also matches the shifted code crossterm reports
    /// (`ctrl+shift+x` arrives as `Char('X')` + CONTROL|SHIFT).
    fn matches(&self, key: KeyEvent) -> bool {
        let code_ok = match self.code {
            KeyCode::Char(c) => {
                key.code == KeyCode::Char(c)
                    || (self.shift && key.code == KeyCode::Char(c.to_ascii_uppercase()))
            }
            other => key.code == other,
        };
        code_ok
            && key.modifiers.contains(KeyModifiers::CONTROL) == self.control
            && key.modifiers.contains(KeyModifiers::ALT) == self.alt
            && key.modifiers.contains(KeyModifiers::SHIFT) == self.shift
    }
}

/// User-customizable keybindings: action name → key-spec string.
///
/// An absent, empty, or unparseable spec falls back to the built-in default
/// for that action, so a broken `[keymap]` section disables nothing.
///
/// `parsed` caches the resolved effective spec per action (`None` = unknown
/// action), so hot paths (`handle_key` consults bindings several times per
/// key event) never re-parse a spec or re-scan the defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keymap {
    #[serde(flatten)]
    bindings: HashMap<String, String>,
    /// The effective spec cache: configured spec when present and parseable,
    /// else the built-in default; `None` = unknown action. Derived state —
    /// never serialized (skipped on write, defaulted on read).
    #[serde(skip)]
    parsed: std::cell::RefCell<HashMap<String, Option<KeySpec>>>,
}

impl PartialEq for Keymap {
    /// Equality is the configured bindings — `parsed` is derived cache
    /// (a freshly deserialized copy compares equal to a used one).
    fn eq(&self, other: &Self) -> bool {
        self.bindings == other.bindings
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap {
            bindings: HashMap::new(),
            parsed: std::cell::RefCell::new(HashMap::new()),
        }
    }
}

impl Keymap {
    /// Does `key` trigger `action`? Falls back to the built-in default when
    /// the configured spec is missing, empty, or unparseable. The resolved
    /// effective spec is cached per action (invalidated by [`Keymap::set`]).
    pub fn matches(&self, action: &str, key: KeyEvent) -> bool {
        let Some(spec) = self.effective_spec(action) else {
            return false;
        };
        spec.matches(key)
    }

    /// The effective spec for `action`: the configured spec when present and
    /// parseable, else the built-in default (which is guaranteed valid);
    /// `None` for unknown actions. Parsed once per action, then cached.
    fn effective_spec(&self, action: &str) -> Option<KeySpec> {
        let mut cache = self.parsed.borrow_mut();
        if let Some(cached) = cache.get(action) {
            return *cached;
        }
        let default = DEFAULT_KEYBINDINGS
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, spec)| *spec);
        let effective = match (self.bindings.get(action).filter(|s| !s.is_empty()), default) {
            (Some(configured), Some(default)) => Some(
                parse_key_spec(configured)
                    .or_else(|| parse_key_spec(default))
                    .expect("built-in defaults are valid"),
            ),
            (None, Some(default)) => {
                Some(parse_key_spec(default).expect("built-in defaults are valid"))
            }
            (_, None) => None,
        };
        cache.insert(action.to_string(), effective);
        effective
    }

    /// Set a binding (used by tests and the config editor); invalidates the
    /// cached effective spec for that action.
    pub fn set(&mut self, action: &str, spec: impl Into<String>) {
        self.bindings.insert(action.into(), spec.into());
        self.parsed.borrow_mut().remove(action);
    }

    /// The configured spec for an action (not the default), if any.
    pub fn configured(&self, action: &str) -> Option<&str> {
        self.bindings.get(action).map(String::as_str)
    }
}

// ---------------------------------------------------------------------------
// the Ctrl+T picker
// ---------------------------------------------------------------------------

/// Open/selection state of the theme picker popup.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemePicker {
    pub open: bool,
    pub selected: usize,
}

/// The theme picker popup: a small floating list rendered above the composer
/// (mirrors the composer's seed popup machinery).
pub struct ThemePopup<'a> {
    pub themes: &'a [Theme],
    pub selected: usize,
    pub current: &'a Theme,
    pub locale: crate::i18n::Locale,
}

impl ThemePopup<'_> {
    /// Outer size for the popup (border included) for an available width.
    pub fn size(&self, available: u16) -> (u16, u16) {
        let text = self
            .themes
            .iter()
            .map(|theme| theme.name.len())
            .max()
            .unwrap_or(0);
        let width = (text + 8) as u16;
        let height = (self.themes.len() as u16).min(10) + 2;
        // #19: the popup never exceeds the terminal width — the min is a
        // floor, not a mandate (`available.max(16)` used to inflate the
        // popup past the terminal below its min-width).
        (width.max(16).min(available), height)
    }
}

impl Widget for ThemePopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(crate::ui::style::border(self.current))
            .title_style(
                Style::new()
                    .add_modifier(Modifier::BOLD)
                    .fg(self.current.accent),
            )
            .title(crate::i18n::tr(self.locale, "theme.picker_title"));
        let inner = block.inner(area);
        block.render(area, buf);
        // The #11 popup treatment: panel_bg fill after Clear, inside the
        // border (Clear resets to the terminal default, so fill after).
        buf.set_style(inner, crate::ui::style::panel_fill(self.current));
        // More themes than rows: scroll so the selection stays visible
        // (it sits at the bottom edge while scrolling, like the sidebar).
        let visible = inner.height as usize;
        let start = self.selected.saturating_sub(visible.saturating_sub(1));
        for (i, theme) in self.themes.iter().enumerate().skip(start) {
            let row = i - start;
            if row as u16 >= inner.height {
                break;
            }
            let y = inner.y + row as u16;
            let selected = i == self.selected;
            if selected {
                // #11: bold + accent `▎` stripe — state carried by glyph
                // shape + weight, never color alone (REVERSED dropped).
                buf.set_style(
                    Rect::new(inner.x, y, inner.width, 1),
                    crate::ui::style::selection(self.current),
                );
            }
            let marker = if theme.name == self.current.name {
                " •"
            } else {
                "  "
            };
            let line = Line::from(vec![
                if selected {
                    Span::styled("▎", crate::ui::style::selection_stripe(self.current))
                } else {
                    Span::raw(" ")
                },
                Span::raw(format!("{marker} ")),
                Span::styled(theme.name.clone(), crate::ui::style::hint(self.current)),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
        }
    }
}
