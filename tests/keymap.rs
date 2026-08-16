//! `[keymap]` config: key-spec parsing, default fallback, and override
//! dispatch (issue #2 — customizable keybindings).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dsh_tui::theme::{Config, Keymap};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}
fn alt(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::ALT)
}
fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}
fn ctrl_shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT)
}

#[test]
fn defaults_match_the_builtin_bindings() {
    let km = Keymap::default();
    assert!(km.matches("quit", ctrl(KeyCode::Char('q'))));
    assert!(km.matches("cancel", ctrl(KeyCode::Char('c'))));
    assert!(km.matches("settings", ctrl(KeyCode::Char(','))));
    assert!(km.matches("locale", ctrl(KeyCode::Char('l'))));
    assert!(km.matches("theme-picker", ctrl(KeyCode::Char('t'))));
    assert!(km.matches("launcher", ctrl(KeyCode::Char('p'))));
    assert!(km.matches("queue", alt(KeyCode::Char('q'))));
    assert!(km.matches("selection-toggle", key(KeyCode::Char('v'))));
    assert!(km.matches("image-viewer", key(KeyCode::Char('i'))));
    assert!(km.matches("tool-details", key(KeyCode::Char('t'))));
    assert!(km.matches("drawer-toggle", key(KeyCode::Char('s'))));
    assert!(km.matches("composer.submit", key(KeyCode::Enter)));
    assert!(km.matches("composer.newline", shift(KeyCode::Enter)));
    assert!(km.matches("composer.backspace", key(KeyCode::Backspace)));
    assert!(km.matches("composer.delete", key(KeyCode::Delete)));
    assert!(km.matches("composer.left", key(KeyCode::Left)));
    assert!(km.matches("composer.right", key(KeyCode::Right)));
    assert!(km.matches("composer.home", key(KeyCode::Home)));
    assert!(km.matches("composer.end", key(KeyCode::End)));
    assert!(km.matches("composer.up", key(KeyCode::Up)));
    assert!(km.matches("composer.down", key(KeyCode::Down)));
    assert!(km.matches("composer.quit-eof", ctrl(KeyCode::Char('d'))));
    assert!(km.matches("composer.focus-chat", key(KeyCode::Esc)));
}

#[test]
fn modifiers_match_exactly() {
    let km = Keymap::default();
    // a plain q never triggers quit, nor does ctrl+shift+q
    assert!(!km.matches("quit", key(KeyCode::Char('q'))));
    assert!(!km.matches(
        "quit",
        KeyEvent::new(
            KeyCode::Char('Q'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )
    ));
    // a bare enter is not a newline
    assert!(!km.matches("composer.newline", key(KeyCode::Enter)));
    // plain backspace is not the composer quit-eof
    assert!(!km.matches("composer.quit-eof", key(KeyCode::Backspace)));
}

#[test]
fn overrides_replace_the_default() {
    let mut km = Keymap::default();
    km.set("quit", "ctrl+d");
    assert!(km.matches("quit", ctrl(KeyCode::Char('d'))));
    assert!(
        !km.matches("quit", ctrl(KeyCode::Char('q'))),
        "the default no longer matches once overridden"
    );
    // specs parse case-insensitively; an uppercase char implies shift
    km.set("quit", "Ctrl+Q");
    assert!(km.matches("quit", ctrl(KeyCode::Char('q'))));
    km.set("composer.submit", "G");
    assert!(km.matches("composer.submit", shift(KeyCode::Char('G'))));
    assert!(!km.matches("composer.submit", key(KeyCode::Char('g'))));
}

#[test]
fn invalid_and_empty_specs_fall_back_to_the_default() {
    let mut km = Keymap::default();
    km.set("quit", "nonsense+key");
    assert!(
        km.matches("quit", ctrl(KeyCode::Char('q'))),
        "an unparseable spec falls back to the default"
    );
    km.set("quit", "");
    assert!(
        km.matches("quit", ctrl(KeyCode::Char('q'))),
        "an empty spec falls back to the default"
    );
}

#[test]
fn unknown_actions_never_match() {
    let km = Keymap::default();
    assert!(!km.matches("no-such-action", key(KeyCode::Char('q'))));
}

#[test]
fn shifted_chords_arrive_with_the_shifted_code() {
    let mut km = Keymap::default();
    // ctrl+shift+x arrives as Char('X') with both modifiers
    km.set("quit", "ctrl+shift+x");
    assert!(km.matches("quit", ctrl_shift(KeyCode::Char('X'))));
    assert!(!km.matches("quit", ctrl(KeyCode::Char('x'))));
}

#[test]
fn config_round_trips_the_keymap_section() {
    let toml = r#"
theme = "catppuccin-frappe"

[keymap]
quit = "ctrl+d"
"#;
    let config: Config = toml::from_str(toml).expect("config parses");
    assert_eq!(config.theme.as_deref(), Some("catppuccin-frappe"));
    assert!(config.keymap.matches("quit", ctrl(KeyCode::Char('d'))));
    assert!(!config.keymap.matches("quit", ctrl(KeyCode::Char('q'))));
    // unspecified actions keep their defaults
    assert!(
        config
            .keymap
            .matches("composer.submit", key(KeyCode::Enter))
    );
    assert_eq!(config.keymap.configured("quit"), Some("ctrl+d"));
    // a save round-trip is stable
    let text = toml::to_string(&config).expect("config serializes");
    let again: Config = toml::from_str(&text).expect("config reparses");
    assert_eq!(again, config);
}

#[test]
fn config_without_keymap_uses_defaults() {
    let config: Config = toml::from_str("theme = \"nord\"").expect("config parses");
    assert!(config.keymap.matches("quit", ctrl(KeyCode::Char('q'))));
    assert_eq!(config.keymap.configured("quit"), None);
}
