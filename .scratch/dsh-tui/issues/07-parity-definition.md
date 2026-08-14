# Parity definition

Type: grilling
Status: resolved
Blocked by: 03, 04

## Question

What does "full parity" mean per feature where literal replication is impossible in
a terminal?

For each feature flagged infeasible by the Web feature inventory and the Rust TUI
ecosystem mapping, agree the terminal-native equivalent:

- Images/screenshots from tools → protocol rendering with fallback tiers.
- Drag-drop attachments → file picker / paste path.
- Hover affordances → keybind or focused panel.
- Theme system → terminal palette mapping.
- Per-feature acceptance bar: what counts as "replicated" for that feature.

Without this table, full parity cannot be scoped or tested; it feeds Testing strategy.

## Answer

Settled across two grilling rounds (Q1–Q11). This table graduates into **PARITY.md** in `rbelem/dsh-tui` — the living contract: per feature, web behavior → TUI equivalent → acceptance bar → testing hook (ticket 08).

### Per-feature parity targets

| Feature (web) | TUI equivalent | Acceptance bar |
|---|---|---|
| **Images** (tool output, gallery/lightbox, drop overlay) | Inline via ratatui-image with fallback tier (kitty → iTerm2 → sixel → halfblocks → `[image]` placeholder); full-screen viewer (focus image → takeover, `n`/`p` cycle, `t` fit/actual, `Esc`/`q` close); draft-image rail → attachments strip above composer | Every image renders or shows a placeholder; viewer cycles/fits/closes; **zoom/pan out of scope** |
| **Drag-drop attachments** | File browser popup (`host.listDirectory`-style) + paste/typed absolute path resolved on submit; attachments strip above composer | Attach by browse and by path; attachment appears in prompt content |
| **Hover affordances** (tooltips, hover cards, hover actions) | Focused-row action bar + footer context line; every hover action has a keybind or menu entry | **Nothing reachable only by hover** — verified by acceptance tests |
| **Theme** | Terminal-following default (truecolor if `COLORTERM`, else 256-color) + **bundled theme registry: ~6 popular families with variants (catppuccin ×4, kanagawa, tokyonight ×3, gruvbox, dracula, solarized — ≈15 TOML files)** mapping the semantic palette (accent/muted/error/warning/success/code); user-extensible via `~/.config/dsh-tui/themes/*.toml`; picker in settings; optional light/dark override | Theme switch applies to all surfaces; custom theme file loads; no token-for-token parity (semantic styles only) |
| **Onboarding** (welcome + DeepSeek credential) | First-run modal, both steps (`credentials.set`); skipped if credential exists | Modal shows once; credential persists |
| **Settings** (General, Models, Plugins config+inventory, Agent presets, Permission presets) | Full settings view, two-pane (nav list + form); plugin-config cards as generic schema-driven forms via `settings.describe`; loopback RPCs (TUI is loopback) | All web sections reachable; each section reads/writes via the same RPCs |
| **i18n** | Full zh/en bilingual — keyed string tables ported from per-plugin locale dictionaries; locale setting mirrors web; CJK rendering settled (ticket 04) | Both locales render correctly incl. CJK width |
| **Acceptance artifact** | PARITY.md in `rbelem/dsh-tui` — the table above is its seed; each row's acceptance bar maps to automated checks (ticket 08) | Every row has a check or an explicit out-of-scope note |
