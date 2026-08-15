//! Light/dark detection tests (issue #1): OSC 11 reply parsing, BT.709
//! luminance classification, the query read/timeout contract, environment
//! signals (`GTK_THEME`, `COLORFGBG`), and App startup integration
//! (an explicit config theme wins; with none, detection picks
//! catppuccin-frappe / catppuccin-latte).

use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::time::Duration;

use dsh_tui::app::App;
use dsh_tui::theme::detect::{
    ColorMode, classify, detect_color_mode, parse_osc11_response, query_osc11,
};

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

// ---------------------------------------------------------------------------
// OSC 11 parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_osc11_accepts_all_forms() {
    // A full terminal reply: OSC prefix, `rgb:` with 4 hex digits per
    // channel, BEL terminator.
    assert_eq!(
        parse_osc11_response(b"\x1b]11;rgb:0f0f/0f0f/0f0f\x07"),
        Some((15, 15, 15)),
        "4-digit channels take the high byte"
    );
    // Two hex digits per channel.
    assert_eq!(parse_osc11_response(b"rgb:0f/0f/0f"), Some((15, 15, 15)));
    // The #rrggbb form.
    assert_eq!(parse_osc11_response(b"#ffffff"), Some((255, 255, 255)));
    // The high byte of a 4-digit channel is the value.
    assert_eq!(
        parse_osc11_response(b"rgb:ffff/0000/0000"),
        Some((255, 0, 0))
    );
    // ST (ESC \) termination is tolerated too.
    assert_eq!(
        parse_osc11_response(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
        Some((255, 255, 255))
    );
}

#[test]
fn parse_osc11_rejects_garbage() {
    assert_eq!(parse_osc11_response(b""), None);
    assert_eq!(parse_osc11_response(b"rgb:zz/zz/zz"), None);
    assert_eq!(parse_osc11_response(b"nonsense"), None);
    // Truncated and malformed # forms.
    assert_eq!(parse_osc11_response(b"#fff"), None);
    assert_eq!(parse_osc11_response(b"#gggggg"), None);
}

// ---------------------------------------------------------------------------
// luminance classification
// ---------------------------------------------------------------------------

#[test]
fn classify_light_vs_dark() {
    assert_eq!(classify((255, 255, 255)), ColorMode::Light);
    assert_eq!(classify((0, 0, 0)), ColorMode::Dark);
    assert_eq!(classify((15, 15, 15)), ColorMode::Dark);
    // Mid-gray: whichever side the luminance math lands on, the classifier
    // must agree with the same math.
    let luminance = 0.2126 * (128.0 / 255.0) + 0.7152 * (128.0 / 255.0) + 0.0722 * (128.0 / 255.0);
    let expected = if luminance >= 0.5 {
        ColorMode::Light
    } else {
        ColorMode::Dark
    };
    assert_eq!(classify((128, 128, 128)), expected);
    // A dark red stays dark (BT.709 weights red low).
    assert_eq!(classify((128, 0, 0)), ColorMode::Dark);
}

// ---------------------------------------------------------------------------
// the query contract: pipe response + silence timeout
// ---------------------------------------------------------------------------

#[test]
fn query_osc11_reads_a_pipe_response() {
    let mut writer = std::io::sink();
    let mut reader = Cursor::new(b"\x1b]11;rgb:1a1a/2b2b/3c3c\x1b\\".to_vec());
    let parsed = query_osc11(&mut writer, &mut reader, Duration::from_millis(200));
    assert_eq!(parsed, Some((0x1a, 0x2b, 0x3c)));
}

/// A reader that never yields data: sleeps per read, then reports EOF.
struct Silent;

impl Read for Silent {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(Duration::from_millis(200));
        Ok(0)
    }
}

#[test]
fn query_osc11_times_out_on_silence() {
    let mut writer = std::io::sink();
    let mut reader = Silent;
    let parsed = query_osc11(&mut writer, &mut reader, Duration::from_millis(50));
    assert_eq!(parsed, None);
}

// ---------------------------------------------------------------------------
// environment signals
// ---------------------------------------------------------------------------

#[test]
fn env_signal_via_gtk_theme() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_env_var("GTK_THEME", Some("Adwaita:dark"), || {
        // Full chain: the OSC 11 layer self-skips (stdin is not a tty in
        // tests), so the GTK_THEME signal decides.
        assert_eq!(detect_color_mode(), Some(ColorMode::Dark));
    });
    with_env_var("GTK_THEME", Some("Adwaita:light"), || {
        assert_eq!(detect_color_mode(), Some(ColorMode::Light));
    });
}

#[test]
fn env_signal_via_colorfgbg() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_env_var("GTK_THEME", None, || {
        // The last `;`-separated field is the background: 0 is clearly dark.
        with_env_var("COLORFGBG", Some("15;0"), || {
            assert_eq!(detect_color_mode(), Some(ColorMode::Dark));
        });
        // 255 is clearly light.
        with_env_var("COLORFGBG", Some("15;255"), || {
            assert_eq!(detect_color_mode(), Some(ColorMode::Light));
        });
    });
}

// ---------------------------------------------------------------------------
// App startup integration
// ---------------------------------------------------------------------------

#[test]
fn config_theme_wins_over_detection() {
    let dir = TempDir::new("detect-config");
    std::fs::create_dir_all(dir.path().join("dsh-tui")).expect("config dir");
    std::fs::write(
        dir.path().join("dsh-tui").join("config.toml"),
        "theme = \"nord\"\n",
    )
    .expect("write config");

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_env_var(
        "XDG_CONFIG_HOME",
        Some(dir.path().to_str().unwrap()),
        || {
            with_env_var("COLORTERM", Some("truecolor"), || {
                with_env_var("GTK_THEME", Some("Adwaita:dark"), || {
                    let mut app = App::default();
                    app.load_theme_config();
                    assert_eq!(app.theme.name, "nord");
                    assert_eq!(app.config.theme.as_deref(), Some("nord"));
                });
            });
        },
    );
}

#[test]
fn detection_picks_frappe_for_dark() {
    let dir = TempDir::new("detect-dark");
    std::fs::create_dir_all(dir.path().join("dsh-tui")).expect("config dir");

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_env_var(
        "XDG_CONFIG_HOME",
        Some(dir.path().to_str().unwrap()),
        || {
            with_env_var("COLORTERM", Some("truecolor"), || {
                // Dark scheme → catppuccin frappe.
                with_env_var("GTK_THEME", Some("Adwaita:dark"), || {
                    let mut app = App::default();
                    app.load_theme_config();
                    assert_eq!(app.theme.name, "catppuccin-frappe");
                });
                // Light scheme → catppuccin latte.
                with_env_var("GTK_THEME", Some("Adwaita:light"), || {
                    let mut app = App::default();
                    app.load_theme_config();
                    assert_eq!(app.theme.name, "catppuccin-latte");
                });
            });
        },
    );
}
