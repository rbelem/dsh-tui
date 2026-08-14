//! zh/en i18n (ticket 07 i18n row): keyed string tables, one per locale,
//! plus the [`Locale`] type threaded through every surface like the theme.
//!
//! Two entry points: [`tr`] for static strings, [`trf`] for strings with
//! positional `{0}`/`{1}` placeholders (the counted strings: "N queued",
//! "compacted N messages", "question N of M"). Keys are kebab-dotted
//! (`sidebar.empty`, `toast.saved`, `marker.tool`); a missing key returns
//! the key itself — never a panic, always debuggable on screen.
//!
//! Locale resolution ([`Locale::detect`]): the persisted config wins, then
//! `DSH_TUI_LOCALE`, then the `LANG`/`LC_ALL` prefix (`zh*` → Zh), else En.

mod strings_en;
mod strings_zh;

/// Every en key, sorted (table-completeness tests walk both tables).
pub fn en_keys() -> Vec<&'static str> {
    strings_en::STRINGS.iter().map(|(key, _)| *key).collect()
}

/// Every zh key, sorted (table-completeness tests walk both tables).
pub fn zh_keys() -> Vec<&'static str> {
    strings_zh::STRINGS.iter().map(|(key, _)| *key).collect()
}

/// The UI locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    #[default]
    En,
    Zh,
}

impl Locale {
    /// Parse a locale string loosely: `en`/`en-US/…` → En, `zh`/`zh-CN/…`
    /// → Zh (anything else → `None`).
    pub fn parse(value: &str) -> Option<Locale> {
        let lower = value.to_ascii_lowercase();
        if lower.starts_with("en") {
            Some(Locale::En)
        } else if lower.starts_with("zh") {
            Some(Locale::Zh)
        } else {
            None
        }
    }

    /// Resolve the startup locale: the persisted config value, then
    /// `DSH_TUI_LOCALE`, then `LANG`/`LC_ALL` (`zh*` → Zh), else En.
    pub fn detect(config: Option<&str>) -> Locale {
        if let Some(locale) = config.and_then(Locale::parse) {
            return locale;
        }
        if let Some(locale) = std::env::var("DSH_TUI_LOCALE")
            .ok()
            .and_then(|value| Locale::parse(&value))
        {
            return locale;
        }
        for var in ["LC_ALL", "LANG"] {
            if let Some(locale) = std::env::var(var)
                .ok()
                .and_then(|value| Locale::parse(&value))
            {
                return locale;
            }
        }
        Locale::default()
    }

    /// The other locale (Ctrl+L cycles).
    pub fn next(self) -> Locale {
        match self {
            Locale::En => Locale::Zh,
            Locale::Zh => Locale::En,
        }
    }

    /// The locale's own name (the Ctrl+L toast and the settings form).
    pub fn native_name(self) -> &'static str {
        match self {
            Locale::En => "English",
            Locale::Zh => "中文",
        }
    }

    /// The config-file value ("en" / "zh").
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Zh => "zh",
        }
    }
}

/// The static string for `key` in `locale` (the key itself on a miss).
pub fn tr(locale: Locale, key: &'static str) -> &'static str {
    let table = match locale {
        Locale::En => strings_en::STRINGS,
        Locale::Zh => strings_zh::STRINGS,
    };
    table
        .binary_search_by(|(probe, _)| probe.cmp(&key))
        .map(|index| table[index].1)
        .unwrap_or(key)
}

/// [`tr`] with positional placeholders: `{0}`, `{1}`, … replaced in order
/// (an arg with no matching placeholder is ignored; a placeholder with no
/// arg is left verbatim).
pub fn trf(locale: Locale, key: &'static str, args: &[&str]) -> String {
    let mut out = tr(locale, key).to_string();
    for (index, arg) in args.iter().enumerate() {
        out = out.replace(&format!("{{{index}}}"), arg);
    }
    out
}
