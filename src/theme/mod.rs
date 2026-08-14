//! Theme registry (ticket 07): semantic color tokens, bundled + user themes,
//! the Ctrl+T picker, and config persistence.
//!
//! Tokens (design contract): `accent`/`muted`/`error`/`warning`/`success`/
//! `code`/`bg`/`text`. The default theme is terminal-following — every token
//! is `Reset`, preserving the modifiers-only look; palette themes render only
//! on terminals that report truecolor ([`terminal_supports_color`]).
//!
//! Bundled themes live in `themes/*.toml` (embedded via `include_str!`); user
//! themes live in `$XDG_CONFIG_HOME/dsh-tui/themes/*.toml` (or
//! `~/.config/dsh-tui/themes/`), loaded at startup, replacing same-named
//! bundled entries. The picker persists the choice to
//! `$XDG_CONFIG_HOME/dsh-tui/config.toml` (`theme = "name"`).
//!
//! TODO: light/dark override (force a light variant on dark terminals and
//! vice versa) — deferred.

pub mod bundled;

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
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
        }
    }
}

impl Theme {
    /// Parse a theme from its TOML text (`name` + eight `#rrggbb` hexes).
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
        })
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
/// XDG override per call, so tests can point it at a temp dir.
fn config_root() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("dsh-tui"))
}

// ---------------------------------------------------------------------------
// config persistence
// ---------------------------------------------------------------------------

/// The persisted app config (`config.toml`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The active theme name; `None` = the terminal-following default.
    #[serde(default)]
    pub theme: Option<String>,
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
        (width.clamp(16, available.max(16)), height)
    }
}

impl Widget for ThemePopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::bordered()
            .border_style(crate::ui::style::border(self.current))
            .title("themes");
        let inner = block.inner(area);
        block.render(area, buf);
        for (i, theme) in self.themes.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            let y = inner.y + i as u16;
            if i == self.selected {
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
                Span::raw(format!(" {marker} ")),
                Span::styled(theme.name.clone(), crate::ui::style::hint(self.current)),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
        }
    }
}
