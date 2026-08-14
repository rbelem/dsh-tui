# Rust TUI ecosystem mapping

Type: research
Status: resolved
Blocked by: —

## Question

Which Rust crates cover the parity needs and where are the gaps?

Research primary sources (crates.io, docs.rs, upstream repos) for:

- ratatui: current version, immediate-mode architecture, maintenance status.
- Markdown → styled terminal text (ratatui-markdown and alternatives).
- Terminal image protocols: kitty graphics, sixel, ueberzug/halfblocks — which
  terminals support which, and the accepted fallback tiers.
- CJK width handling (unicode-width, unicode-segmentation).
- Clipboard access (arboard and alternatives).
- Syntax-highlighted diff views (syntect, prettydiff).
- Terminal feature detection (terminfo, xterm queries, kitty protocol negotiation).
- TUI testing: ratatui test backend, insta snapshots, pty-based e2e.

For each area: recommended crate, maturity signal (downloads/maintenance/license),
and known gaps we would hand-roll. Then answer: is ratatui's immediate-mode draw loop
suitable for a large virtualized chat list with streaming updates, or is a
retained/VDOM approach (e.g. ink) a better fit?

## Answer

### Recommended stack (ratatui v0.30.2, MIT, 43.7M dl, active)

- **Framework**: `ratatui` 0.30.2 — immediate-mode, in-memory buffer diff per frame, built-in `TestBackend`. Widgets split since 0.29 into `ratatui-widgets` (no_std sub-crate; apps depend on `ratatui` only). **Gap: no built-in virtualization** — track viewport offset + slice data yourself.
- **Markdown**: `termimad` 0.35.1 (6.6M dl, MIT, used by broot) — pure-terminal ANSI styled output. `ratatui-markdown` 0.3.6 exists but young/low adoption. **Gap: neither handles streaming/partial-document rendering** — a chat UI needs an incremental markdown parser (pulldown-cmark state machine) feeding `Line`/`Span` construction. Hand-rolled.
- **Images**: `ratatui-image` 11.0.6 (687K dl, MIT) — first-class widget, auto protocol detection via `Picker::from_query_stdio()`: Kitty (Kitty/WezTerm/Ghostty/Rio) → iTerm2 inline → Sixel (foot/mlterm/xterm+mintty) → **Halfblocks universal fallback**. Handles font-size detection + negotiation. (`viuer` 0.11 simpler, prints to stdout, not widget-native.) **Fallback tier resolved: Kitty > iTerm2 > Sixel > Halfblocks, automatic.**
- **CJK width**: `unicode-width` 0.2.2 (735M dl, has `width_cjk()` for ambiguous) + `unicode-segmentation` 1.13.3 (499M dl) for grapheme clusters. Both no_std, battle-tested. Gap: emoji ZWJ sequences imperfect — layer graphemes → width per cluster.
- **Clipboard**: `arboard` 3.6.1 (40.4M dl, MIT, by 1Password) — macOS/Windows/X11/Wayland (opt-in feature). Gap: no clipboard monitoring — poll for approval flows.
- **Syntax-highlighted diff**: `syntect` 5.3.0 (22.9M dl, MIT) for highlighting + `prettydiff` 0.9 (7M dl) or `similar` for diff algorithm. **Gap: no ratatui-diff widget** — map hunk lines to syntect-colored `Line` spans. Moderate effort.
- **Feature detection**: `crossterm` 0.29 (173M dl) for size/raw/events; `ratatui-image`'s `Picker` handles image-protocol negotiation (DA1). terminfo crate dated; in practice hardcode sequences + env checks (`TERM`, `COLORTERM`). Comprehensive capability detection fragmented.
- **Testing**: `ratatui::backend::TestBackend` + `insta` 1.48 (88M dl) snapshot assertions (first-class ratatui integration); `portable-pty` 0.9 (11M dl) for PTY e2e; `serial_test` 4.0 for PTY/clipboard tests. **Gap: no turnkey TUI e2e harness** — build one: spawn app in PTY, send keystrokes, capture output, insta-snapshot.

### Verdict: immediate-mode is the better fit

Ratatui's immediate-mode is **suitable and well-suited** for a large virtualized chat list with streaming updates — partial redraws via buffer diff, virtualization = viewport offset + slicing, streaming = `draw()` per token arrival. No viable Rust VDOM/retained framework exists (`ink-rs` on crates.io is not a thing). Hand-rolled virtualization + incremental markdown parsing are well-understood patterns. Immediate mode gives full control per frame — exactly what tool-call trees, diff views, and streaming need.
