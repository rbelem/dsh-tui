# PARITY.md — dsh-tui parity contract

Living per-feature contract: web UI behavior → TUI equivalent → acceptance bar →
testing hook. Seed taken from `.scratch/dsh-tui/issues/07-parity-definition.md`;
each row's acceptance bar maps to automated checks (see testing strategy,
`.scratch/dsh-tui/issues/08-testing-definition.md` — in progress).

Out-of-scope (per wayfinder map): serving the UI over HTTP to remote browsers
(`dsh web` keeps that surface); non-terminal GUI clients.

## Per-feature parity targets

| Feature (web) | TUI equivalent | Acceptance bar | Testing hook |
|---|---|---|---|
| **Images** (tool output, gallery/lightbox, drop overlay) | Inline via ratatui-image with fallback tier (kitty → iTerm2 → sixel → halfblocks → `[image]` placeholder); full-screen viewer (focus image → takeover, `n`/`p` cycle, `t` fit/actual, `Esc`/`q` close); draft-image rail → attachments strip above composer | Every image renders or shows a placeholder; viewer cycles/fits/closes; **zoom/pan out of scope** | e2e scenario + snapshot |
| **Drag-drop attachments** | File browser popup (`host.listDirectory`-style) + paste/typed absolute path resolved on submit; attachments strip above composer | Attach by browse and by path; attachment appears in prompt content | e2e scenario |
| **Hover affordances** (tooltips, hover cards, hover actions) | Focused-row action bar + footer context line; every hover action has a keybind or menu entry | **Nothing reachable only by hover** — verified by acceptance tests | acceptance test sweep |
| **Theme** | Terminal-following default (truecolor if `COLORTERM`, else 256-color) + **bundled theme registry: ~6 popular families with variants (catppuccin ×4, kanagawa, tokyonight ×3, gruvbox, dracula, solarized — ≈15 TOML files)** mapping the semantic palette (accent/muted/error/warning/success/code); user-extensible via `~/.config/dsh-tui/themes/*.toml`; picker in settings; optional light/dark override | Theme switch applies to all surfaces; custom theme file loads; no token-for-token parity (semantic styles only) | settings e2e + theme-load unit tests |
| **Onboarding** (welcome + DeepSeek credential) | First-run modal, both steps (`credentials.set`); skipped if credential exists | Modal shows once; credential persists | e2e scenario |
| **Settings** (General, Models, Plugins config+inventory, Agent presets, Permission presets) | Full settings view, two-pane (nav list + form); plugin-config cards as generic schema-driven forms via `settings.describe`; loopback RPCs (TUI is loopback) | All web sections reachable; each section reads/writes via the same RPCs | wire-level mock + e2e |
| **i18n** | Full zh/en bilingual — keyed string tables ported from per-plugin locale dictionaries; locale setting mirrors web; CJK width handling | Both locales render correctly incl. CJK width | snapshot at both locales |
| **Acceptance artifact** | This table; each row's acceptance bar maps to automated checks | Every row has a check or an explicit out-of-scope note | test-plan ledger |

## Notes

- Parity baseline is the current web UI: `apps/web`, `packages/client/**` in the
  deepseek-harness monorepo (reference, read-only).
- The TUI is a second surface over the same gateway; session state is the single
  source of truth — switching surfaces mid-conversation must resume seamlessly.

## e2e coverage (level-3 harness, `tests/e2e.rs`)

The real binary runs in a PTY against the in-process mock gateway (keyless,
`DSH_PORT`-attached, locale/XDG isolated). Scenario → row mapping:

| Scenario (tests/e2e.rs) | PARITY row(s) |
|---|---|
| `attach_resumes_most_recent_session` | session list/resume rows — resume the most recent non-blank session with its history |
| `prompt_submit_streams_response` | composer / prompt row — typed submit posts `session.prompt` (mode queue); streamed assistant text renders |
| `approval_takeover_answers_with_echoed_rpc_id` | approvals row — takeover shows the tool + hints; `y` answers with the echoed rpcId; resolved frame toasts and restores chat |
| `theme_picker_opens_and_closes` | theme row — Ctrl+T popup lists bundled themes; Esc closes (keys reach the composer again) |
| `ctrl_q_exits_cleanly` | acceptance artifact — clean exit, status 0, no panic text |

Rows without an e2e scenario are covered by the unit/integration suites
(wire round-trips, store fold, render snapshots at both locales, keymap
tables, settings/theme/i18n tests) or are explicitly out of scope (images,
drag-drop, hover-only interactions, onboarding, remote-browser serving).
