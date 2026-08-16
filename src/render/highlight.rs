//! Syntax highlighting for fenced code blocks (#5).
//!
//! Wraps [syntect] with the [two_face] grammar bundle (~100 languages) — the
//! same engine stack as the OpenAI codex TUI. Pure-Rust only: syntect is
//! built with `regex-fancy` (fancy-regex), no onig/C.
//!
//! The grammar database is one process-global [`OnceLock<SyntaxSet>`], loaded
//! lazily on first use (`two_face::syntax::extra_newlines()`).
//!
//! Highlighting maps each span's syntect scope stack through a pure,
//! table-driven classifier ([`classify`]) into the app's semantic theme
//! tokens (comment→`muted`, keyword→`accent`, string→`success`,
//! number→`warning`, function→`code`, type→`text`+bold, default→`text`) —
//! no grammar colors, the theme stays the single source of color. Spans carry
//! foreground only; the row cache's `panel_bg` code fill is unchanged.
//!
//! Unknown languages and oversized inputs fall back to `None` — the caller
//! renders the block exactly as before (all-`code` token).

use std::sync::OnceLock;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::{HighlightState, Highlighter};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::theme::Theme;

/// The process-global grammar database (~100 languages, two-face's bundle).
static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();

/// Guardrails (the codex-TUI limits): pathological inputs skip highlighting
/// and fall back to the plain rendering.
const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
const MAX_HIGHLIGHT_LINES: usize = 10_000;
const MAX_HIGHLIGHT_LINE_BYTES: usize = 4 * 1024;

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

/// Force the grammar-database load. The app calls this at startup
// ([`crate::app::App`]'s `Default` impl): the lazy `OnceLock` would
// otherwise stall the first fenced-code render for the ~200ms two-face
// dump load.
pub fn warmup() {
    let _ = syntax_set();
}

// ---------------------------------------------------------------------------
// scope → theme-token classifier (#5)
// ---------------------------------------------------------------------------

/// The theme token a code span maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeToken {
    /// `comment.*`
    Muted,
    /// `keyword.*` (incl. `keyword.control.*` / `keyword.storage.*`)
    Accent,
    /// `string.*` / `regexp`
    Success,
    /// `constant.numeric` / `constant.language`
    Warning,
    /// `entity.name.function` / `support.function`
    Code,
    /// `entity.name.type` / `storage.type` — `text` + bold
    Type,
    /// anything else inside a highlighted block
    Plain,
}

/// Classify a scope stack (dotted scope paths, outermost first — exactly as
/// syntect's `ScopeStack` yields them) into a theme token. Pure and
/// table-driven: the first rule in priority order that matches any scope in
/// the stack wins; an unmatched (or empty) stack is [`CodeToken::Plain`].
pub fn classify<S: AsRef<str>>(scopes: &[S]) -> CodeToken {
    const RULES: &[(&str, CodeToken)] = &[
        ("comment.", CodeToken::Muted),
        ("keyword.", CodeToken::Accent),
        ("string.", CodeToken::Success),
        ("regexp", CodeToken::Success),
        ("constant.numeric", CodeToken::Warning),
        ("constant.language", CodeToken::Warning),
        ("entity.name.function", CodeToken::Code),
        ("support.function", CodeToken::Code),
        ("entity.name.type", CodeToken::Type),
        ("storage.type", CodeToken::Type),
    ];
    for (prefix, token) in RULES {
        if scopes
            .iter()
            .any(|scope| scope.as_ref().starts_with(prefix))
        {
            return *token;
        }
    }
    CodeToken::Plain
}
/// The ratatui style for a classified token: foreground from the theme
/// token (`Type` adds BOLD). No background — the row cache paints the
/// `panel_bg` code fill at full content width.
pub fn token_style(token: CodeToken, theme: &Theme) -> Style {
    let (color, bold) = match token {
        CodeToken::Muted => (theme.muted, false),
        CodeToken::Accent => (theme.accent, false),
        CodeToken::Success => (theme.success, false),
        CodeToken::Warning => (theme.warning, false),
        CodeToken::Code => (theme.code, false),
        CodeToken::Type => (theme.text, true),
        CodeToken::Plain => (theme.text, false),
    };
    let mut style = Style::default().fg(color);
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

// ---------------------------------------------------------------------------
// language resolution
// ---------------------------------------------------------------------------

/// Try to find a syntect `SyntaxReference` for the given language identifier
/// (token, name, or file extension), patching the few aliases two-face's
/// bundle cannot resolve on its own.
fn find_syntax(lang: &str) -> Option<&'static SyntaxReference> {
    let syntax_set = syntax_set();
    let normalized = lang.to_ascii_lowercase();
    let patched = match normalized.as_str() {
        "csharp" | "c-sharp" => "c#",
        "golang" => "go",
        "python3" => "python",
        "shell" => "bash",
        _ => lang,
    };
    if let Some(syntax) = syntax_set.find_syntax_by_token(patched) {
        return Some(syntax);
    }
    if let Some(syntax) = syntax_set.find_syntax_by_name(patched) {
        return Some(syntax);
    }
    let lower = patched.to_ascii_lowercase();
    if let Some(syntax) = syntax_set
        .syntaxes()
        .iter()
        .find(|syntax| syntax.name.to_ascii_lowercase() == lower)
    {
        return Some(syntax);
    }
    syntax_set.find_syntax_by_extension(lang)
}

// ---------------------------------------------------------------------------
// the highlighter
// ---------------------------------------------------------------------------

/// Highlight `code` as `lang`, mapping every span's scope stack through
/// [`classify`] into theme-token styles. `None` = the language is unknown or
/// the input exceeds the guardrails — the caller falls back to its plain
/// rendering. One `Line` per source line; line endings are stripped (the
/// trailing newline pulldown-cmark emits produces no phantom line).
pub fn highlight_code(code: &str, lang: &str, theme: &Theme) -> Option<Vec<Line<'static>>> {
    if code.is_empty()
        || code.len() > MAX_HIGHLIGHT_BYTES
        || code.lines().count() > MAX_HIGHLIGHT_LINES
        || code
            .lines()
            .any(|line| line.len() > MAX_HIGHLIGHT_LINE_BYTES)
    {
        return None;
    }
    let syntax = find_syntax(lang)?;
    let mut parse_state = ParseState::new(syntax);
    // The highlighter's theme is never read — spans are styled through the
    // classifier — but HighlightState is the scope-stack owner, and its
    // `path` persists across lines (multi-line strings/comments).
    let mut state = HighlightState::new(
        &Highlighter::new(&syntect::highlighting::Theme::default()),
        ScopeStack::new(),
    );
    let mut lines = Vec::new();
    for raw in LinesWithEndings::from(code) {
        // Parse the line WITH its ending — grammars close end-of-line
        // contexts (e.g. shell comments) on the newline itself.
        let ops = parse_state.parse_line(raw, syntax_set()).ok()?;
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut pos = 0;
        for (end, op) in &ops {
            if *end > pos {
                push_classified_span(&mut spans, &raw[pos..*end], &state.path, theme);
            }
            state.path.apply_with_hook(op, |_, _| {}).ok()?;
            pos = *end;
        }
        if pos < raw.len() {
            push_classified_span(&mut spans, &raw[pos..], &state.path, theme);
        }
        // Strip the line ending from span content (CRLF too), dropping spans
        // that were nothing but it — same trimming as the codex TUI.
        let mut cleaned: Vec<Span<'static>> = Vec::with_capacity(spans.len());
        for mut span in spans {
            let text = span.content.trim_end_matches(['\n', '\r']).to_string();
            if text.is_empty() {
                continue;
            }
            span.content = text.into();
            cleaned.push(span);
        }
        if cleaned.is_empty() {
            cleaned.push(Span::raw(String::new()));
        }
        lines.push(Line::from(cleaned));
    }
    Some(lines)
}

fn push_classified_span(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    path: &ScopeStack,
    theme: &Theme,
) {
    let scopes: Vec<String> = path.as_slice().iter().map(Scope::to_string).collect();
    let token = classify(&scopes);
    spans.push(Span::styled(text.to_string(), token_style(token, theme)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::from_toml_str(
            r##"
name = "highlight-test"
accent = "#111111"
muted = "#222222"
error = "#333333"
warning = "#444444"
success = "#555555"
code = "#666666"
bg = "#777777"
text = "#888888"
panel_bg = "#999999"
border = "#aaaaaa"
"##,
        )
        .expect("valid theme")
    }

    #[test]
    fn classifier_maps_each_scope_family_to_its_token() {
        // One case per prefix rule, in table priority order.
        let cases: &[(&[&str], CodeToken)] = &[
            (&["comment.line.double-slash.rust"], CodeToken::Muted),
            (&["comment.block.rust"], CodeToken::Muted),
            (&["keyword.control.rust"], CodeToken::Accent),
            (&["keyword.other.rust"], CodeToken::Accent),
            (&["keyword.operator.arithmetic.rust"], CodeToken::Accent),
            (&["string.quoted.double.rust"], CodeToken::Success),
            (&["string.regexp.rust"], CodeToken::Success),
            (&["regexp.unquoted.rust"], CodeToken::Success),
            (&["constant.numeric.rust"], CodeToken::Warning),
            (&["constant.language.boolean.rust"], CodeToken::Warning),
            (&["entity.name.function.rust"], CodeToken::Code),
            (&["support.function.rust"], CodeToken::Code),
            (&["entity.name.type.rust"], CodeToken::Type),
            (&["storage.type.rust"], CodeToken::Type),
            (&["source.rust"], CodeToken::Plain),
            (&["meta.function.rust"], CodeToken::Plain),
        ];
        for (scopes, expected) in cases {
            assert_eq!(
                classify(scopes),
                *expected,
                "classify({scopes:?}) — rule mismatch"
            );
        }
    }

    #[test]
    fn classifier_priority_is_table_order_not_stack_order() {
        // The table's priority wins regardless of where the scope sits in
        // the stack: comment (rule 1) beats keyword (rule 2), keyword beats
        // string (rule 3), function (rule 5) beats the plain default.
        assert_eq!(
            classify(&["string.quoted.rust", "comment.line.rust"]),
            CodeToken::Muted
        );
        assert_eq!(
            classify(&["keyword.control.rust", "string.quoted.rust"]),
            CodeToken::Accent
        );
        assert_eq!(
            classify(&["source.rust", "entity.name.function.rust"]),
            CodeToken::Code
        );
        // keyword.operator stays accent even though it sits on a plain meta.
        assert_eq!(
            classify(&["meta.expression.rust", "keyword.operator.rust"]),
            CodeToken::Accent
        );
    }

    #[test]
    fn unknown_and_empty_scopes_pass_through_to_plain() {
        assert_eq!(classify(&[] as &[&str]), CodeToken::Plain);
        assert_eq!(
            classify(&["variable.other.rust", "meta.function.rust"]),
            CodeToken::Plain
        );
        assert_eq!(classify(&["punctuation.definition.rust"]), CodeToken::Plain);
    }

    #[test]
    fn token_style_maps_tokens_to_theme_colors() {
        let theme = theme();
        assert_eq!(token_style(CodeToken::Muted, &theme).fg, Some(theme.muted));
        assert_eq!(
            token_style(CodeToken::Accent, &theme).fg,
            Some(theme.accent)
        );
        assert_eq!(
            token_style(CodeToken::Success, &theme).fg,
            Some(theme.success)
        );
        assert_eq!(
            token_style(CodeToken::Warning, &theme).fg,
            Some(theme.warning)
        );
        assert_eq!(token_style(CodeToken::Code, &theme).fg, Some(theme.code));
        assert_eq!(token_style(CodeToken::Plain, &theme).fg, Some(theme.text));
    }

    #[test]
    fn token_style_types_are_bold_text() {
        let theme = theme();
        let style = token_style(CodeToken::Type, &theme);
        assert_eq!(style.fg, Some(theme.text));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        // The other tokens are never bold.
        for token in [
            CodeToken::Muted,
            CodeToken::Accent,
            CodeToken::Success,
            CodeToken::Warning,
            CodeToken::Code,
            CodeToken::Plain,
        ] {
            assert!(
                !token_style(token, &theme)
                    .add_modifier
                    .contains(Modifier::BOLD),
                "{token:?} must not be bold"
            );
        }
    }

    #[test]
    fn unknown_language_and_empty_input_return_none() {
        let theme = theme();
        assert!(highlight_code("fn main() {}", "not-a-language", &theme).is_none());
        assert!(highlight_code("", "rust", &theme).is_none());
    }

    #[test]
    fn oversized_inputs_return_none() {
        let theme = theme();
        let huge = "x".repeat(MAX_HIGHLIGHT_BYTES + 1);
        assert!(highlight_code(&huge, "rust", &theme).is_none());
        let many_lines = "let x = 1;\n".repeat(MAX_HIGHLIGHT_LINES + 1);
        assert!(highlight_code(&many_lines, "rust", &theme).is_none());
        let long_line = "x".repeat(MAX_HIGHLIGHT_LINE_BYTES + 1);
        assert!(highlight_code(&long_line, "bash", &theme).is_none());
    }

    #[test]
    fn highlight_preserves_content_and_line_count() {
        let theme = theme();
        let code = "fn main() {\n    println!(\"hi\");\n}\n";
        let lines = highlight_code(code, "rust", &theme).expect("rust highlights");
        let reconstructed: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(reconstructed.join("\n"), code.trim_end_matches('\n'));
        assert_eq!(lines.len(), 3, "trailing newline adds no phantom line");
    }

    #[test]
    fn rust_keyword_and_string_spans_carry_mapped_colors() {
        let theme = theme();
        let lines = highlight_code("fn main() { let s = \"hi\"; }", "rust", &theme)
            .expect("rust highlights");
        let spans: Vec<Span<'static>> = lines[0].spans.clone();
        let span_of = |needle: &str| spans.iter().find(|s| s.content.as_ref() == needle);
        // `fn`/`let` are storage.type in the Rust grammar → bold text.
        for keyword in ["fn", "let"] {
            let span = span_of(keyword).unwrap_or_else(|| panic!("{keyword:?} span"));
            assert_eq!(span.style.fg, Some(theme.text), "{keyword:?} → text");
            assert!(
                span.style.add_modifier.contains(Modifier::BOLD),
                "{keyword:?} → bold"
            );
        }
        // `main` is entity.name.function; `=` keyword.operator; `"hi"`
        // string.quoted.double (the quotes are their own spans).
        assert_eq!(span_of("main").unwrap().style.fg, Some(theme.code));
        assert_eq!(span_of("=").unwrap().style.fg, Some(theme.accent));
        assert_eq!(span_of("hi").unwrap().style.fg, Some(theme.success));
        assert_eq!(span_of("\"").unwrap().style.fg, Some(theme.success));
        assert_eq!(span_of("{").unwrap().style.fg, Some(theme.text));
    }

    #[test]
    fn bash_highlights_with_comment_support_function_and_string() {
        let theme = theme();
        let lines =
            highlight_code("echo \"hi\" # done\nls -la", "bash", &theme).expect("bash highlights");
        let spans: Vec<Span<'static>> = lines.iter().flat_map(|line| line.spans.clone()).collect();
        let span_of = |needle: &str| spans.iter().find(|s| s.content.as_ref() == needle);
        assert_eq!(span_of("echo").unwrap().style.fg, Some(theme.code));
        assert_eq!(span_of("hi").unwrap().style.fg, Some(theme.success));
        assert_eq!(span_of("#").unwrap().style.fg, Some(theme.muted));
        assert_eq!(span_of(" done").unwrap().style.fg, Some(theme.muted));
        // The comment closes at the newline — the next line is plain text,
        // not swallowed into the comment.
        assert_eq!(span_of("ls").unwrap().style.fg, Some(theme.text));
        assert_eq!(span_of("la").unwrap().style.fg, Some(theme.text));
    }

    #[test]
    fn json_keys_numbers_and_booleans_map_correctly() {
        let theme = theme();
        let lines =
            highlight_code(r#"{"a": 1, "b": true}"#, "json", &theme).expect("json highlights");
        let spans: Vec<Span<'static>> = lines[0].spans.clone();
        let span_of = |needle: &str| spans.iter().find(|s| s.content.as_ref() == needle);
        assert_eq!(span_of("a").unwrap().style.fg, Some(theme.success));
        assert_eq!(span_of("1").unwrap().style.fg, Some(theme.warning));
        assert_eq!(span_of("true").unwrap().style.fg, Some(theme.warning));
    }

    #[test]
    fn aliases_resolve_to_a_highlight() {
        let theme = theme();
        for (alias, code) in [
            ("shell", "echo hi"),
            ("python3", "print(1)"),
            ("golang", "package main"),
            ("csharp", "class C {}"),
        ] {
            assert!(
                highlight_code(code, alias, &theme).is_some(),
                "alias {alias:?} should resolve"
            );
        }
    }
}
