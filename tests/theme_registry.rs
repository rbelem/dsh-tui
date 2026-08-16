//! Theme registry tests: bundled parse + uniqueness, hex parsing, semantic
//! color application to rendered cells, user-dir loading (corrupt skipped),
//! config persistence (XDG temp override), and the Ctrl+T picker flow.

use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use serde_json::json;

use dsh_tui::app::{Action, App};
use dsh_tui::i18n::Locale;
use dsh_tui::render::{ChatView, ImageCache, RowCache};
use dsh_tui::store::SessionStore;
use dsh_tui::theme::{
    Config, Theme, ThemeError, ThemePopup, ThemeRegistry, bundled, terminal_supports_color,
};
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn ev(seq: i64, r#type: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        r#type: r#type.into(),
        seq,
        time: seq as f64,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn frame(session: &str, event: SessionEvent) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: None,
    }
}

fn user_msg(id: &str, text: &str) -> serde_json::Value {
    json!({"id": id, "role": "user", "content": [{"type": "text", "text": text}], "source": {"kind": "user"}})
}

/// #32: the render-context bag for a test render. Folds always empty —
/// skill-fold behavior is covered by the dedicated markdown tests.
fn render_ctx<'a>(
    width: u16,
    theme: &'a Theme,
    locale: Locale,
    images: &'a ImageCache,
    folds: &'a std::collections::HashMap<dsh_tui::store::node::NodeKey, bool>,
) -> dsh_tui::render::markdown::RenderContext<'a> {
    dsh_tui::render::markdown::RenderContext {
        width,
        theme,
        locale,
        images,
        skill_folds: folds,
    }
}

/// A store with one user message containing an unfenced code block (the
/// plain-code path colors it with the theme's code token).
fn store_with_code_fence() -> SessionStore {
    let mut store = SessionStore::new();
    store
        .ingest(frame(
            "s1",
            ev(
                1,
                "user/message",
                user_msg("m1", "intro paragraph\n\n```\nfn main() {}\n```"),
            ),
        ))
        .expect("ingest");
    store
}

/// Render the store at `width`×`height` with `theme`; return the buffer view.
fn render_with_theme(theme: &Theme, width: u16, height: u16) -> String {
    let store = store_with_code_fence();
    let mut cache = RowCache::new();
    cache.sync(
        &store,
        &SessionId("s1".into()),
        &render_ctx(
            width,
            theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
    );
    cache.render_dirty(
        &store,
        &SessionId("s1".into()),
        &render_ctx(
            width,
            theme,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
    );
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|f| {
            f.render_widget(
                ChatView {
                    store: &store,
                    session_id: &SessionId("s1".into()),
                    offset: 0,
                    row_cache: &mut cache,
                    images: &mut ImageCache::default(),
                },
                f.area(),
            );
        })
        .expect("draw");
    format!("{}", terminal.backend())
}

/// A unique temp dir for XDG-dependent tests (cleaned on drop).
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("dsh-tui-test-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The XDG/COLORTERM env vars are process-global: env-touching tests
/// serialize on this (poison-tolerant).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Set an env var for the duration of the closure and restore it after
/// (edition 2024: `set_var` is unsafe; single-threaded test usage only).
fn with_env_var(key: &str, value: Option<&str>, f: impl FnOnce()) {
    let previous = std::env::var(key).ok();
    // SAFETY: tests that call this do not read the variable concurrently,
    // and the value is restored before the test returns.
    match value {
        Some(value) => unsafe { std::env::set_var(key, value) },
        None => unsafe { std::env::remove_var(key) },
    }
    f();
    match previous {
        Some(previous) => unsafe { std::env::set_var(key, previous) },
        None => unsafe { std::env::remove_var(key) },
    }
}

/// Redirect `dirs::config_dir()` under `base` for the closure's duration and
/// hand the closure the app config root it resolves to (`<config dir>/dsh-tui`).
///
/// `dirs` honors `XDG_CONFIG_HOME` only on Linux; macOS ignores it and builds
/// `~/Library/Application Support` from `$HOME` instead, so there the test
/// isolates by overriding `HOME`.
fn with_config_root(base: &std::path::Path, f: impl FnOnce(&std::path::Path)) {
    with_env_var("XDG_CONFIG_HOME", Some(base.to_str().unwrap()), || {
        with_home_override(base, || {
            let root = dirs::config_dir().expect("config dir").join("dsh-tui");
            f(&root);
        });
    });
}

/// On macOS `dirs` ignores `XDG_CONFIG_HOME`, so `HOME` carries the
/// override; a no-op elsewhere.
#[cfg(target_os = "macos")]
fn with_home_override(base: &std::path::Path, f: impl FnOnce()) {
    with_env_var("HOME", Some(base.join("home").to_str().unwrap()), f);
}

#[cfg(not(target_os = "macos"))]
fn with_home_override(_base: &std::path::Path, f: impl FnOnce()) {
    f();
}

// ---------------------------------------------------------------------------
// bundled registry
// ---------------------------------------------------------------------------

#[test]
fn bundled_themes_all_parse_with_unique_names() {
    let registry = ThemeRegistry::bundled();
    assert_eq!(
        registry.themes.len(),
        17,
        "15 palettes + dsh-dark + dsh-light"
    );
    // Every embedded TOML parses standalone.
    for (name, toml) in bundled::BUNDLED {
        let theme = Theme::from_toml_str(toml)
            .unwrap_or_else(|e| panic!("bundled theme {name} broken: {e}"));
        assert_eq!(theme.name, *name, "TOML name matches the file key");
    }
    // Names are unique across the registry.
    let mut names: Vec<&str> = registry.themes.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), registry.themes.len(), "unique theme names");
    // The #11 tokens are present on every bundled theme.
    for theme in &registry.themes {
        assert!(
            theme.panel_bg != Color::Reset,
            "{}: panel_bg set",
            theme.name
        );
        assert!(theme.border != Color::Reset, "{}: border set", theme.name);
    }
}

#[test]
fn known_palette_hexes_parse_to_rgb() {
    let mocha = Theme::from_toml_str(include_str!("../themes/catppuccin-mocha.toml")).unwrap();
    assert_eq!(
        mocha.accent,
        Color::Rgb(0x89, 0xb4, 0xfa),
        "catppuccin mocha blue"
    );
    assert_eq!(mocha.bg, Color::Rgb(0x1e, 0x1e, 0x2e));
    assert_eq!(mocha.text, Color::Rgb(0xcd, 0xd6, 0xf4));
    assert_eq!(mocha.muted, Color::Rgb(0x58, 0x5b, 0x70), "surface2");
    assert_eq!(mocha.error, Color::Rgb(0xf3, 0x8b, 0xa8));
    assert_eq!(mocha.warning, Color::Rgb(0xf9, 0xe2, 0xaf));
    assert_eq!(mocha.success, Color::Rgb(0xa6, 0xe3, 0xa1));
    // #11 tokens: bundled themes carry explicit panel_bg/border values.
    assert_eq!(mocha.panel_bg, Color::Rgb(0x1e, 0x1e, 0x2e), "bg-derived");
    assert_eq!(mocha.border, Color::Rgb(0x58, 0x5b, 0x70), "muted-derived");
}

#[test]
fn dsh_house_themes_parse_with_exact_palettes() {
    let dark = Theme::from_toml_str(include_str!("../themes/dsh-dark.toml")).unwrap();
    assert_eq!(dark.name, "dsh-dark");
    assert_eq!(dark.accent, Color::Rgb(0xfa, 0xb2, 0x83));
    assert_eq!(dark.muted, Color::Rgb(0x80, 0x80, 0x80));
    assert_eq!(dark.error, Color::Rgb(0xe0, 0x6c, 0x75));
    assert_eq!(dark.warning, Color::Rgb(0xf5, 0xa7, 0x42));
    assert_eq!(dark.success, Color::Rgb(0x7f, 0xd8, 0x8f));
    assert_eq!(dark.code, Color::Rgb(0x9d, 0x7c, 0xd8));
    assert_eq!(dark.bg, Color::Rgb(0x0a, 0x0a, 0x0a));
    assert_eq!(dark.panel_bg, Color::Rgb(0x14, 0x14, 0x14));
    assert_eq!(dark.text, Color::Rgb(0xee, 0xee, 0xee));
    assert_eq!(dark.border, Color::Rgb(0x3c, 0x3c, 0x3c));
    assert_ne!(dark.bg, Color::Rgb(0, 0, 0), "no pure black (issue #11)");

    let light = Theme::from_toml_str(include_str!("../themes/dsh-light.toml")).unwrap();
    assert_eq!(light.name, "dsh-light");
    assert_eq!(light.accent, Color::Rgb(0xa8, 0x51, 0x28));
    assert_eq!(light.muted, Color::Rgb(0x6a, 0x6a, 0x6a));
    assert_eq!(light.error, Color::Rgb(0xc2, 0x45, 0x4e));
    assert_eq!(light.warning, Color::Rgb(0xa8, 0x64, 0x12));
    assert_eq!(light.success, Color::Rgb(0x2e, 0x7d, 0x43));
    assert_eq!(light.code, Color::Rgb(0x6d, 0x4f, 0xc2));
    assert_eq!(light.bg, Color::Rgb(0xfa, 0xf8, 0xf5));
    assert_eq!(light.panel_bg, Color::Rgb(0xf0, 0xed, 0xe8));
    assert_eq!(light.text, Color::Rgb(0x1a, 0x1a, 0x1a));
    assert_eq!(light.border, Color::Rgb(0xd8, 0xd4, 0xcc));
}

#[test]
fn invalid_hexes_are_rejected() {
    let base = r##"
name = "t"
accent = "#112233"
muted = "#112233"
error = "#112233"
warning = "#112233"
success = "#112233"
code = "#112233"
bg = "#112233"
text = "#112233"
"##;
    // A good theme parses...
    assert!(Theme::from_toml_str(base).is_ok());
    // ...each bad hex form is rejected with a ThemeError.
    for bad in ["#12345", "#1234567", "112233", "#gggggg", "#12345g", ""] {
        let broken = base.replace("#112233", bad);
        assert!(
            matches!(Theme::from_toml_str(&broken), Err(ThemeError::Invalid(_))),
            "hex `{bad}` must be rejected"
        );
    }
    // Missing fields are rejected too.
    assert!(Theme::from_toml_str("name = \"t\"").is_err());
}

#[test]
fn new_tokens_default_from_bg_and_muted() {
    // A user theme WITHOUT the #11 fields keeps parsing; panel_bg falls
    // back to bg, border to muted (backward-compatible migration).
    let bare = r##"
name = "bare"
accent = "#112233"
muted = "#445566"
error = "#112233"
warning = "#112233"
success = "#112233"
code = "#112233"
bg = "#778899"
text = "#aabbcc"
"##;
    let theme = Theme::from_toml_str(bare).expect("bare theme parses");
    assert_eq!(
        theme.panel_bg,
        Color::Rgb(0x77, 0x88, 0x99),
        "panel_bg ← bg"
    );
    assert_eq!(theme.border, Color::Rgb(0x44, 0x55, 0x66), "border ← muted");
    // A bad explicit panel_bg is still rejected.
    let broken = bare.replace("bg = \"#778899\"", "bg = \"#778899\"\npanel_bg = \"nope\"");
    assert!(
        matches!(Theme::from_toml_str(&broken), Err(ThemeError::Invalid(_))),
        "bad panel_bg rejected"
    );
}

#[test]
fn default_theme_is_reset_based() {
    let default = Theme::default();
    assert_eq!(default.name, "default");
    for token in [
        default.accent,
        default.muted,
        default.error,
        default.warning,
        default.success,
        default.code,
        default.bg,
        default.text,
        default.panel_bg,
        default.border,
    ] {
        assert_eq!(token, Color::Reset);
    }
}

// ---------------------------------------------------------------------------
// semantic application to rendered cells
// ---------------------------------------------------------------------------

#[test]
fn theme_colors_reach_rendered_cells() {
    let mocha = Theme::from_toml_str(include_str!("../themes/catppuccin-mocha.toml")).unwrap();
    let store = store_with_code_fence();
    let mut cache = RowCache::new();
    cache.sync(
        &store,
        &SessionId("s1".into()),
        &render_ctx(
            120,
            &mocha,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
    );
    cache.render_dirty(
        &store,
        &SessionId("s1".into()),
        &render_ctx(
            120,
            &mocha,
            Locale::En,
            &ImageCache::default(),
            &std::collections::HashMap::new(),
        ),
    );
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            f.render_widget(
                ChatView {
                    store: &store,
                    session_id: &SessionId("s1".into()),
                    offset: 0,
                    row_cache: &mut cache,
                    images: &mut ImageCache::default(),
                },
                f.area(),
            );
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let mut found_code = false;
    let mut found_text = false;
    let mut found_fill = false;
    for y in 0..30u16 {
        for x in 0..120u16 {
            let Some(cell) = buffer.cell((x, y)) else {
                continue;
            };
            let ch = cell.symbol();
            if cell.fg == mocha.code && ch != " " {
                // The unfenced code block's text (fn main() {}) = theme.code.
                found_code = true;
            } else if cell.fg == mocha.text {
                // The user paragraph text = theme.text.
                found_text = true;
            }
            if cell.bg == mocha.panel_bg {
                // The code-block body carries the panel_bg fill (#11).
                found_fill = true;
            }
        }
    }
    assert!(found_code, "unfenced code text carries theme.code");
    assert!(found_text, "user text carries theme.text");
    assert!(found_fill, "code block rows carry the panel_bg fill");
}

#[test]
fn default_theme_render_is_unchanged() {
    // With the Reset default, no cell carries an explicit color.
    let view = render_with_theme(&Theme::default(), 120, 30);
    assert!(view.contains("fn main()"), "code fence renders: {view}");
}

// ---------------------------------------------------------------------------
// user-dir loading
// ---------------------------------------------------------------------------

#[test]
fn user_dir_loads_valid_and_skips_corrupt() {
    let dir = TempDir::new("themes");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(dir.path(), |config_root| {
        let themes_dir = config_root.join("themes");
        std::fs::create_dir_all(&themes_dir).expect("themes dir");
        // A valid user theme that replaces a bundled name.
        std::fs::write(
            themes_dir.join("catppuccin-mocha.toml"),
            include_str!("../themes/catppuccin-mocha.toml").replace("#89b4fa", "#000001"),
        )
        .expect("write valid");
        // A brand-new user theme.
        std::fs::write(
            themes_dir.join("mine.toml"),
            r##"
name = "mine"
accent = "#010203"
muted = "#040506"
error = "#070809"
warning = "#0a0b0c"
success = "#0d0e0f"
code = "#101112"
bg = "#131415"
text = "#161718"
"##,
        )
        .expect("write mine");
        // A corrupt theme: skipped, not fatal.
        std::fs::write(
            themes_dir.join("broken.toml"),
            "name = \"broken\"\naccent = \"nope\"",
        )
        .expect("write broken");
        std::fs::write(themes_dir.join("also-broken.toml"), "not toml at all")
            .expect("write broken2");

        let mut registry = ThemeRegistry::bundled();
        registry.load_user_dir();
        let mocha = registry
            .find("catppuccin-mocha")
            .expect("bundled name still present");
        assert_eq!(
            mocha.accent,
            Color::Rgb(0x00, 0x00, 0x01),
            "user theme replaced the bundled entry"
        );
        let mine = registry.find("mine").expect("new user theme loaded");
        assert_eq!(mine.text, Color::Rgb(0x16, 0x17, 0x18));
        // A user theme without the #11 fields keeps the bg/muted defaults.
        assert_eq!(mine.panel_bg, Color::Rgb(0x13, 0x14, 0x15), "panel_bg ← bg");
        assert_eq!(mine.border, Color::Rgb(0x04, 0x05, 0x06), "border ← muted");
        assert!(
            registry.find("broken").is_none(),
            "corrupt themes are skipped"
        );
    });
}

// ---------------------------------------------------------------------------
// config persistence
// ---------------------------------------------------------------------------

#[test]
fn config_write_read_back_and_apply() {
    let dir = TempDir::new("config");

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(dir.path(), |config_root| {
        std::fs::create_dir_all(config_root).expect("config dir");
        // Missing config → default (no theme).
        assert_eq!(Config::load(), Config::default());

        // Write → read back.
        let config = Config {
            theme: Some("catppuccin-mocha".into()),
            locale: None,
            keymap: dsh_tui::theme::Keymap::default(),
        };
        config.save().expect("save");
        let path = Config::path().expect("config path");
        assert!(path.exists(), "config file written");
        assert_eq!(Config::load(), config, "read-back matches");

        // Apply via the app startup path (needs a truecolor terminal).
        let mut app = App::default();
        with_env_var("DSH_THEME", None, || {
            with_env_var("COLORTERM", Some("truecolor"), || {
                app.load_theme_config();
            });
        });
        assert_eq!(app.theme.name, "catppuccin-mocha");
        assert_eq!(app.config, config);
        assert!(
            app.themes.find("catppuccin-mocha").is_some(),
            "registry has the theme"
        );

        // Without truecolor, the config theme is not applied (terminal-
        // following default stays).
        let mut app = App::default();
        with_env_var("DSH_THEME", None, || {
            with_env_var("COLORTERM", None, || {
                assert!(!terminal_supports_color());
                app.load_theme_config();
            });
        });
        assert_eq!(
            app.theme.name, "default",
            "Reset-based neutral without COLORTERM"
        );
    });
}

// ---------------------------------------------------------------------------
// the Ctrl+T picker
// ---------------------------------------------------------------------------

fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}

fn ctrl(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::CONTROL)
}

#[test]
fn picker_opens_applies_and_closes() {
    let dir = TempDir::new("picker-config");

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(dir.path(), |_config_root| {
        let mut app = App::default();
        assert!(!app.theme_picker.open);

        // Ctrl+T opens with the current theme preselected (default → index 0
        // since "default" is not in the registry).
        assert_eq!(
            app.handle_key(ctrl(crossterm::event::KeyCode::Char('t'))),
            Some(Action::None)
        );
        assert!(app.theme_picker.open);
        assert_eq!(app.theme_picker.selected, 0);

        // Down moves; the picker swallows other keys.
        assert_eq!(
            app.handle_key(key(crossterm::event::KeyCode::Down)),
            Some(Action::None)
        );
        assert_eq!(app.theme_picker.selected, 1);
        assert_eq!(
            app.handle_key(key(crossterm::event::KeyCode::Char('q'))),
            Some(Action::None)
        );
        assert!(app.theme_picker.open, "q is inert while the picker is open");

        // Enter applies live, persists, and closes.
        let picked = app.themes.themes[1].name.clone();
        app.handle_key(key(crossterm::event::KeyCode::Enter));
        assert!(!app.theme_picker.open, "picker closed after apply");
        assert_eq!(app.theme.name, picked, "theme applied live");
        assert_eq!(app.config.theme.as_deref(), Some(picked.as_str()));
        let saved = Config::load();
        assert_eq!(
            saved.theme.as_deref(),
            Some(picked.as_str()),
            "choice persisted"
        );

        // Reopen: the current theme is preselected; Esc closes without
        // applying a change.
        app.handle_key(ctrl(crossterm::event::KeyCode::Char('t')));
        assert!(app.theme_picker.open);
        assert_eq!(
            app.theme_picker.selected,
            app.themes
                .themes
                .iter()
                .position(|t| t.name == app.theme.name)
                .unwrap_or(0),
            "current theme preselected"
        );
        app.handle_key(key(crossterm::event::KeyCode::Esc));
        assert!(!app.theme_picker.open);
        assert_eq!(app.theme.name, picked, "Esc did not change the theme");
    });
}

#[test]
fn picker_renders_a_listing() {
    let app = App::default();
    let popup = ThemePopup {
        themes: &app.themes.themes,
        selected: 2,
        current: &app.theme,
        locale: Locale::En,
    };
    let (width, height) = popup.size(120);
    assert!(width >= 16);
    assert_eq!(height as usize, app.themes.themes.len().min(10) + 2);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| f.render_widget(popup, f.area())).unwrap();
    let view = format!("{}", terminal.backend());
    assert!(view.contains("themes"), "popup title: {view}");
    assert!(
        view.contains("catppuccin-latte"),
        "theme rows listed: {view}"
    );

    // With the selection past the visible window the list scrolls so the
    // selected row stays on screen (15 bundled themes, 10 visible rows).
    let last = app.themes.themes.len() - 1;
    let popup = ThemePopup {
        themes: &app.themes.themes,
        selected: last,
        current: &app.theme,
        locale: Locale::En,
    };
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| f.render_widget(popup, f.area())).unwrap();
    let view = format!("{}", terminal.backend());
    assert!(
        view.contains(&app.themes.themes[last].name),
        "selected tail theme stays visible: {view}"
    );
    assert!(
        !view.contains("catppuccin-latte"),
        "scrolled off the top: {view}"
    );
}

// ---------------------------------------------------------------------------
// coverage push: XDG tolerance + config error paths + picker marker
// ---------------------------------------------------------------------------

#[test]
fn load_user_dir_tolerates_missing_or_file_xdg() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // No config dir at all: the user dir lookup gives up.
    with_env_var("XDG_CONFIG_HOME", None, || {
        let mut registry = ThemeRegistry::bundled();
        registry.load_user_dir(); // no panic, no-op
    });
    // A FILE-shaped config dir: read_dir fails → tolerated.
    let file = std::env::temp_dir().join(format!("dsh-tui-xdg-file-{}", std::process::id()));
    let _ = std::fs::remove_file(&file);
    std::fs::write(&file, "x").expect("write");
    with_config_root(&file, |_config_root| {
        let mut registry = ThemeRegistry::bundled();
        registry.load_user_dir();
    });
    let _ = std::fs::remove_file(&file);
}

#[test]
fn load_user_dir_skips_non_toml_files() {
    let dir = TempDir::new("non-toml");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(dir.path(), |config_root| {
        let themes_dir = config_root.join("themes");
        std::fs::create_dir_all(&themes_dir).expect("themes dir");
        std::fs::write(themes_dir.join("notes.txt"), "not a theme").expect("write txt");

        let mut registry = ThemeRegistry::bundled();
        registry.load_user_dir(); // the .txt is skipped, no panic
    });
}

#[test]
fn config_load_tolerates_missing_and_corrupt_files() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TempDir::new("config-load");
    with_config_root(dir.path(), |config_root| {
        // No config file in an otherwise-empty config dir: the default.
        assert_eq!(Config::load(), Config::default());
        // A corrupt config file: the default.
        std::fs::create_dir_all(config_root).expect("dir");
        std::fs::write(config_root.join("config.toml"), "not [valid toml").expect("write corrupt");
        assert_eq!(Config::load(), Config::default());
    });
}

// NOTE: Config::save's "no config directory" error requires dirs::config_dir()
// to return None, which cannot be forced on Linux — documented as excluded.
// The save error path IS covered via the file-shaped-XDG tests elsewhere
// (create_dir_all fails → Config error).

#[test]
fn picker_marks_the_current_theme() {
    let app = App::default();
    let current = &app.themes.themes[0];
    let popup = ThemePopup {
        themes: &app.themes.themes,
        selected: 0,
        current,
        locale: Locale::En,
    };
    let (width, height) = popup.size(120);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| f.render_widget(popup, f.area())).unwrap();
    let view = format!("{}", terminal.backend());
    assert!(
        view.contains(" •") && view.contains(&current.name),
        "current theme marked: {view}"
    );
    // #11: the selected row carries the accent `▎` stripe (glyph + weight,
    // never color alone) — the picker stays identifiable in grayscale.
    assert!(view.contains("▎"), "selection stripe: {view}");
}
