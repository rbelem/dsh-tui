//! Palette-aware color snapping tests (#4): `ColorLevel` env detection,
//! `best_color` CIE76 nearest-matching over the xterm-256 cube/grays and
//! the 16-color named palette, and the app load path (COLORTERM=256color
//! yields `Color::Indexed` tokens; truecolor keeps RGB).

use std::path::PathBuf;

use ratatui::style::Color;

use dsh_tui::app::App;
use dsh_tui::theme::detect::{ColorLevel, best_color, detect_color_level};

// ---------------------------------------------------------------------------
// helpers (mirrors tests/theme_registry.rs)
// ---------------------------------------------------------------------------

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

/// The env vars are process-global: env-touching tests serialize on this
/// (poison-tolerant).
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
// color-level detection
// ---------------------------------------------------------------------------

#[test]
fn color_level_from_env() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_env_var("TERM", Some("xterm"), || {
        with_env_var("COLORTERM", Some("truecolor"), || {
            assert_eq!(detect_color_level(), ColorLevel::TrueColor);
        });
        with_env_var("COLORTERM", Some("24bit"), || {
            assert_eq!(detect_color_level(), ColorLevel::TrueColor);
        });
        with_env_var("COLORTERM", Some("256color"), || {
            assert_eq!(detect_color_level(), ColorLevel::Ansi256);
        });
        // No COLORTERM: `TERM=*-256color` still means 256 colors...
        with_env_var("COLORTERM", None, || {
            with_env_var("TERM", Some("xterm-256color"), || {
                assert_eq!(detect_color_level(), ColorLevel::Ansi256);
            });
            // ...and a plain TERM means 16.
            with_env_var("TERM", Some("xterm"), || {
                assert_eq!(detect_color_level(), ColorLevel::Ansi16);
            });
            with_env_var("TERM", None, || {
                assert_eq!(detect_color_level(), ColorLevel::Ansi16);
            });
        });
    });
}

// ---------------------------------------------------------------------------
// best_color
// ---------------------------------------------------------------------------

#[test]
fn best_color_truecolor_passthrough() {
    assert_eq!(
        best_color((0x89, 0xb4, 0xfa), ColorLevel::TrueColor),
        Color::Rgb(0x89, 0xb4, 0xfa)
    );
}

#[test]
fn best_color_ansi256_snaps_to_exact_palette_members() {
    // Pure red → cube corner 196, black → 16, white → 231 (distance 0).
    assert_eq!(
        best_color((255, 0, 0), ColorLevel::Ansi256),
        Color::Indexed(196)
    );
    assert_eq!(
        best_color((0, 0, 0), ColorLevel::Ansi256),
        Color::Indexed(16)
    );
    assert_eq!(
        best_color((255, 255, 255), ColorLevel::Ansi256),
        Color::Indexed(231)
    );
    // A cube member off the corners: (95, 135, 175) → index 67.
    assert_eq!(
        best_color((95, 135, 175), ColorLevel::Ansi256),
        Color::Indexed(67)
    );
    // A gray ramp member: (118, 118, 118) → gray 243.
    assert_eq!(
        best_color((118, 118, 118), ColorLevel::Ansi256),
        Color::Indexed(243)
    );
}

#[test]
fn best_color_ansi256_picks_the_nearest_candidate() {
    // CIE76 ordering: a point 1 unit off palette member 67 must snap to it,
    // not to a member ~40 units away in any channel.
    assert_eq!(
        best_color((96, 136, 176), ColorLevel::Ansi256),
        Color::Indexed(67)
    );
    // Near-white snaps to 231 (255,255,255), not to the 188 gray
    // (215,215,215) — the nearer neighbor wins.
    assert_eq!(
        best_color((250, 250, 250), ColorLevel::Ansi256),
        Color::Indexed(231)
    );
}

#[test]
fn best_color_ansi16_snaps_to_named_colors() {
    assert_eq!(best_color((0, 0, 0), ColorLevel::Ansi16), Color::Black);
    assert_eq!(best_color((255, 0, 0), ColorLevel::Ansi16), Color::Red);
    assert_eq!(best_color((0, 255, 0), ColorLevel::Ansi16), Color::Green);
    assert_eq!(best_color((0, 0, 255), ColorLevel::Ansi16), Color::Blue);
    assert_eq!(
        best_color((255, 255, 255), ColorLevel::Ansi16),
        Color::White
    );
    assert_eq!(best_color((128, 128, 128), ColorLevel::Ansi16), Color::Gray);
}

#[test]
fn snapped_theme_passes_non_rgb_tokens_through() {
    let default = dsh_tui::theme::Theme::default();
    let snapped = default.snapped(ColorLevel::Ansi256);
    assert_eq!(snapped.bg, Color::Reset, "Reset tokens stay Reset");
    assert_eq!(snapped.user_bg, None, "None stays None");
}

// ---------------------------------------------------------------------------
// app load path
// ---------------------------------------------------------------------------

#[test]
fn load_theme_config_snaps_rgb_on_ansi256() {
    let dir = TempDir::new("snap-256");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(dir.path(), |_config_root| {
        let mut app = App::default();
        with_env_var("DSH_THEME", Some("catppuccin-mocha"), || {
            with_env_var("COLORTERM", Some("256color"), || {
                app.load_theme_config();
            });
        });
        assert_eq!(app.theme.name, "catppuccin-mocha");
        // Every RGB token (user_bg included) is now an xterm index.
        assert_eq!(
            app.theme.accent,
            best_color((0x89, 0xb4, 0xfa), ColorLevel::Ansi256)
        );
        assert!(matches!(app.theme.accent, Color::Indexed(_)));
        assert_eq!(
            app.theme.user_bg,
            Some(best_color((0x26, 0x26, 0x38), ColorLevel::Ansi256))
        );
        assert!(matches!(app.theme.user_bg, Some(Color::Indexed(_))));
    });
}

#[test]
fn load_theme_config_keeps_rgb_on_truecolor() {
    let dir = TempDir::new("snap-true");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(dir.path(), |_config_root| {
        let mut app = App::default();
        with_env_var("DSH_THEME", Some("catppuccin-mocha"), || {
            with_env_var("COLORTERM", Some("truecolor"), || {
                app.load_theme_config();
            });
        });
        assert_eq!(app.theme.accent, Color::Rgb(0x89, 0xb4, 0xfa));
        assert_eq!(
            app.theme.user_bg,
            Some(Color::Rgb(0x26, 0x26, 0x38)),
            "user_bg stays RGB on truecolor"
        );
    });
}

#[test]
fn load_theme_config_keeps_default_on_ansi16() {
    let dir = TempDir::new("snap-16");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(dir.path(), |_config_root| {
        let mut app = App::default();
        with_env_var("DSH_THEME", Some("catppuccin-mocha"), || {
            with_env_var("COLORTERM", None, || {
                with_env_var("TERM", Some("xterm"), || {
                    app.load_theme_config();
                });
            });
        });
        assert_eq!(
            app.theme.name, "default",
            "16-color terminals keep the Reset-based default"
        );
    });
}
