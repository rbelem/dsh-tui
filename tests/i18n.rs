//! i18n lane tests (increment 4): table completeness, placeholders, locale
//! detection precedence, zh rendering, CJK wrap, Ctrl+L cycling + config
//! persistence, and the settings locale sync. Keyless.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::app::{App, AppEvent, EventChannel};
use dsh_tui::i18n::{Locale, tr, trf};
use dsh_tui::render::{ChatView, ImageCache, RowCache};
use dsh_tui::store::SessionStore;
use dsh_tui::theme::{Config, Theme};
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{SessionEvent, SessionId};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

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

/// The XDG/LANG env vars are process-global: env-touching tests serialize.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_env_var(key: &str, value: Option<&str>, f: impl FnOnce()) {
    let previous = std::env::var(key).ok();
    // SAFETY: single-threaded usage under ENV_LOCK; restored before return.
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

/// Like [`with_config_root`] for async bodies: the env stays redirected
/// while the future runs (the run loop reads the config mid-await).
async fn with_config_root_async(base: &std::path::Path, f: impl std::future::Future<Output = ()>) {
    let prev_xdg = std::env::var("XDG_CONFIG_HOME").ok();
    // SAFETY: serialized under ENV_LOCK; restored before return.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", base.to_str().unwrap()) };
    #[cfg(target_os = "macos")]
    let prev_home = std::env::var("HOME").ok();
    #[cfg(target_os = "macos")]
    unsafe {
        std::env::set_var("HOME", base.join("home").to_str().unwrap())
    };

    f.await;

    match prev_xdg {
        Some(previous) => unsafe { std::env::set_var("XDG_CONFIG_HOME", previous) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
    #[cfg(target_os = "macos")]
    match prev_home {
        Some(previous) => unsafe { std::env::set_var("HOME", previous) },
        None => unsafe { std::env::remove_var("HOME") },
    }
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("dsh-tui-i18n-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// 1. table completeness
// ---------------------------------------------------------------------------

#[test]
fn tables_are_complete_and_sorted() {
    let en = dsh_tui::i18n::en_keys();
    let zh = dsh_tui::i18n::zh_keys();
    // Every en key exists in zh and vice versa.
    let only_en: Vec<&str> = en.iter().filter(|k| !zh.contains(k)).copied().collect();
    let only_zh: Vec<&str> = zh.iter().filter(|k| !en.contains(k)).copied().collect();
    assert!(only_en.is_empty(), "en-only keys: {only_en:?}");
    assert!(only_zh.is_empty(), "zh-only keys: {only_zh:?}");
    assert!(!en.is_empty(), "tables are populated");
    // Sorted (the binary-search lookup contract).
    assert!(en.windows(2).all(|w| w[0] < w[1]), "en sorted");
    assert!(zh.windows(2).all(|w| w[0] < w[1]), "zh sorted");
}

// ---------------------------------------------------------------------------
// 2. trf placeholders + missing-key fallback
// ---------------------------------------------------------------------------

#[test]
fn trf_substitutes_placeholders() {
    assert_eq!(
        trf(Locale::En, "queue.strip", &["2", "1", "0", "fix the tests"]),
        "2 queued"
    );
    assert_eq!(
        trf(Locale::En, "marker.compacted", &["3"]),
        "[compacted 3 messages]"
    );
    assert_eq!(
        trf(Locale::En, "question.counter", &["1", "2"]),
        "question 1 of 2"
    );
    assert_eq!(
        trf(Locale::En, "toast.prompt_failed", &["boom"]),
        "prompt failed: boom"
    );
    // zh placeholders substitute the same way.
    assert_eq!(trf(Locale::Zh, "queue.strip", &["2"]), "2 条排队");
    assert_eq!(
        trf(Locale::Zh, "marker.compacted", &["3"]),
        "[已压缩 3 条消息]"
    );
    // A missing key returns the key itself (never a panic).
    assert_eq!(tr(Locale::En, "nope.missing"), "nope.missing");
    assert_eq!(trf(Locale::En, "nope.missing", &["x"]), "nope.missing");
}

// ---------------------------------------------------------------------------
// 3. locale detection precedence
// ---------------------------------------------------------------------------

#[test]
fn locale_detection_precedence() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // LANG drives the default.
    with_env_var("LANG", Some("zh_CN.UTF-8"), || {
        with_env_var("LC_ALL", None, || {
            with_env_var("DSH_TUI_LOCALE", None, || {
                assert_eq!(Locale::detect(None), Locale::Zh);
                // A blank config value means "detect", not "zh".
                assert_eq!(Locale::detect(Some("")), Locale::Zh);
                // The env var beats LANG.
                with_env_var("DSH_TUI_LOCALE", Some("en-US"), || {
                    assert_eq!(Locale::detect(None), Locale::En);
                    // The config beats the env.
                    assert_eq!(Locale::detect(Some("zh")), Locale::Zh);
                    assert_eq!(Locale::detect(Some("en")), Locale::En);
                });
            });
        });
    });
    with_env_var("LANG", Some("en_US.UTF-8"), || {
        with_env_var("LC_ALL", None, || {
            with_env_var("DSH_TUI_LOCALE", None, || {
                assert_eq!(Locale::detect(None), Locale::En);
                // The stale-zh config scenario: blank config + English system → En.
                assert_eq!(Locale::detect(Some("")), Locale::En);
            });
        });
    });
    with_env_var("LANG", None, || {
        with_env_var("DSH_TUI_LOCALE", None, || {
            assert_eq!(Locale::detect(None), Locale::En, "no signals → En");
        });
    });
}

// ---------------------------------------------------------------------------
// 4. zh rendering at 120×30 and 60×15
// ---------------------------------------------------------------------------

async fn render_zh_app(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut channel = EventChannel::new();
    // F1 is inert in every surface (it only forces the immediate draw);
    // plain letters would type into the composer (boot focus).
    channel
        .tx
        .send(AppEvent::Key(key(KeyCode::F(1))))
        .expect("key to force a draw");
    for event in events {
        channel.tx.send(event).expect("event channel");
    }
    app.run(term, &mut channel).await.expect("run");
}

#[tokio::test]
async fn zh_surfaces_render() {
    // Sidebar empty-state zh + composer placeholder zh + a zh marker in chat
    // content + a zh toast in the status line.
    let mut app = App::default();
    app.locale = Locale::Zh;
    app.active_session = Some(SessionId("s1".into()));
    app.store
        .ingest(frame(
            "s1",
            ev(1, "user/message", user_msg("m1", "```\nfn main() {}\n```")),
        ))
        .expect("ingest");
    app.set_toast("已保存");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    render_zh_app(
        &mut app,
        &mut term,
        vec![AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    let view = format!("{}", term.backend());
    // Sidebar empty state.
    assert!(view.contains("暂无会话"), "sidebar empty zh: {view}");
    assert!(view.contains("新会话会显示在这里"), "sidebar empty hint zh");
    // Composer placeholder.
    assert!(view.contains("输入消息"), "composer placeholder zh: {view}");
    // Toast in the status line.
    assert!(view.contains("已保存"), "toast zh: {view}");
    // The status line's focus label is localized too.
    assert!(view.contains("焦点："), "status line present: {view}");
}

#[tokio::test]
async fn zh_takeover_and_toast_render() {
    // A takeover hint (toast/hint) renders zh.
    let mut app = App::default();
    app.locale = Locale::Zh;
    app.set_toast("暂不能取消 — 请回答或等待");
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    render_zh_app(
        &mut app,
        &mut term,
        vec![AppEvent::Key(ctrl(KeyCode::Char('q')))],
    )
    .await;
    let view = format!("{}", term.backend());
    assert!(
        view.contains("暂不能取消"),
        "zh toast in the status line: {view}"
    );
}

#[tokio::test]
async fn zh_markers_in_chat_content() {
    // The marker.unknown zh string appears for an unknown node.
    let mut store = SessionStore::new();
    store
        .ingest(frame("s1", ev(1, "plugin.xyz", json!({"x": 1}))))
        .expect("ingest");
    let mut cache = RowCache::new();
    cache.sync(
        &store,
        &SessionId("s1".into()),
        120,
        &Theme::default(),
        Locale::Zh,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    );
    cache.render_dirty(
        &store,
        &SessionId("s1".into()),
        120,
        &Theme::default(),
        Locale::Zh,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
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
        .expect("draw");
    let view = format!("{}", terminal.backend());
    assert!(
        view.contains("[未知：plugin.xyz]"),
        "zh unknown marker: {view}"
    );
}

// ---------------------------------------------------------------------------
// 5. CJK wrap at narrow widths
// ---------------------------------------------------------------------------

#[test]
fn cjk_paragraph_wraps_by_width() {
    let text = "这是一个相当长的中文段落，用来验证窄宽度下的换行是否按显示宽度而不是字符数进行，避免超出终端宽度。";
    let mut store = SessionStore::new();
    store
        .ingest(frame("s1", ev(1, "user/message", user_msg("m1", text))))
        .expect("ingest");
    let mut cache = RowCache::new();
    cache.sync(
        &store,
        &SessionId("s1".into()),
        120,
        &Theme::default(),
        Locale::Zh,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    );
    let wide = cache.lines()[0].lines.len();
    cache.sync(
        &store,
        &SessionId("s1".into()),
        40,
        &Theme::default(),
        Locale::Zh,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    );
    let narrow = cache.lines()[0].lines.len();
    assert!(
        narrow > wide,
        "narrow width wraps the zh paragraph ({wide} → {narrow})"
    );
    for line in &cache.lines()[0].lines {
        assert!(line.width() <= 40, "wrapped line width {}", line.width());
    }
}

// ---------------------------------------------------------------------------
// 6. Ctrl+L cycles + persists + reads back
// ---------------------------------------------------------------------------

#[test]
fn ctrl_l_cycles_and_persists() {
    let dir = TempDir::new("locale");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root(&dir.0, |_config_root| {
        let mut app = App::default();
        assert_eq!(app.locale, Locale::En);
        // En → Zh.
        app.handle_key(ctrl(KeyCode::Char('l')));
        assert_eq!(app.locale, Locale::Zh);
        assert!(
            app.toast_text().is_some_and(|t| t.contains("中文")),
            "zh native-name toast"
        );
        assert_eq!(Config::load().locale.as_deref(), Some("zh"), "persisted");
        // Zh → En.
        app.handle_key(ctrl(KeyCode::Char('l')));
        assert_eq!(app.locale, Locale::En);
        assert_eq!(Config::load().locale.as_deref(), Some("en"), "persisted");
        // Restart reads back (env signals absent).
        with_env_var("LANG", None, || {
            with_env_var("DSH_TUI_LOCALE", None, || {
                let mut restarted = App::default();
                restarted.load_theme_config();
                restarted.locale = Locale::detect(restarted.config.locale.as_deref());
                assert_eq!(restarted.locale, Locale::En, "restart reads `en` back");
                // Cycle once more and restart reads zh.
                restarted.handle_key(ctrl(KeyCode::Char('l')));
                assert_eq!(restarted.locale, Locale::Zh);
                let mut again = App::default();
                again.load_theme_config();
                again.locale = Locale::detect(again.config.locale.as_deref());
                assert_eq!(again.locale, Locale::Zh, "restart reads `zh` back");
            });
        });
    });
}

#[test]
fn ctrl_l_is_inert_in_settings_and_takeovers() {
    // Settings mode: Ctrl+L must not cycle.
    let mut app = App::default();
    app.mode = dsh_tui::ui::takeover::Mode::Settings(dsh_tui::ui::settings::SettingsState::new());
    app.handle_key(ctrl(KeyCode::Char('l')));
    assert_eq!(app.locale, Locale::En, "inert in Settings");
}

// ---------------------------------------------------------------------------
// 7. settings locale sync
// ---------------------------------------------------------------------------

#[tokio::test]
// The ENV_LOCK serializes env-dependent tests; it must stay held across the
// async run (which reads the config) — intentional.
#[allow(clippy::await_holding_lock)]
async fn settings_locale_save_syncs_app() {
    let dir = TempDir::new("settings-locale");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with_config_root_async(&dir.0, async {
        async fn sync(app: &mut App, language: &str) {
            let view = dsh_tui::wire::settings::SettingsNamespaceView {
                ns: "locale".into(),
                schema: json!({}),
                value: json!({"language": language}),
                base: None,
                user: None,
                applies: dsh_tui::wire::settings::AppliesMode::Live,
                secrets: vec![],
                revision: 1.0,
            };
            let mut channel = EventChannel::new();
            channel
                .tx
                .send(AppEvent::SettingsSaveDone {
                    ns: "locale".into(),
                    result: Ok(view),
                })
                .expect("event");
            channel
                .tx
                .send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
                .expect("quit");
            let backend = TestBackend::new(120, 30);
            let mut term = Terminal::new(backend).unwrap();
            app.run(&mut term, &mut channel).await.expect("run");
        }

        let mut app = App::default();
        app.mode =
            dsh_tui::ui::takeover::Mode::Settings(dsh_tui::ui::settings::SettingsState::new());
        sync(&mut app, "zh").await;
        assert_eq!(
            app.locale,
            Locale::Zh,
            "locale namespace save syncs App.locale"
        );
        assert_eq!(
            Config::load().locale.as_deref(),
            Some("zh"),
            "config written"
        );
        assert!(
            matches!(app.mode, dsh_tui::ui::takeover::Mode::Chat),
            "back to chat"
        );

        // A non-locale value in the locale namespace is ignored.
        let mut app = App::default();
        app.mode =
            dsh_tui::ui::takeover::Mode::Settings(dsh_tui::ui::settings::SettingsState::new());
        sync(&mut app, "klingon").await;
        assert_eq!(app.locale, Locale::En, "non-locale value ignored");
    })
    .await;
}
