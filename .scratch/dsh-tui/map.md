# Map — dsh tui: replicate the web UI as a Rust TUI

## Destination

A settled build plan for `dsh tui` — a Rust TUI client that replicates every feature
of the current web UI, living in a **separate repository** (`rbelem/dsh-tui`) and
shipped as a **plugin/bundle installed into the harness**, able to run **alongside
the web UI** against the same gateway and resume conversation state where the web
left off. The map is done when nothing is left to decide: per-feature parity
definitions, the Rust protocol-binding strategy, rendering architecture, toolchain
integration, and testing are all resolved and the plan can be handed off for
implementation.

## Notes

- Parity baseline is the current web UI: `apps/web`, `packages/client/**`
  (~74k LOC of client UI). The harness server half stays as-is; the TUI is a
  second surface over the same gateway (coexists with `dsh web`; session state is
  the single source of truth — switching surfaces mid-conversation must resume
  seamlessly).
- **The TUI code lives in `rbelem/dsh-tui` (new repo), not this monorepo** —
  harness-side changes are limited to whatever the installed plugin needs; this
  monorepo is untouched by default.
- Protocol knowledge lives in `packages/api/gateway`, `packages/client/connection`,
  `packages/sdk` (read-only reference from the new repo). Read `docs/architecture.md`
  before touching the server half.
- The harness has no Rust today (`native/` is a C++ node-addon); the new repo is
  greenfield Rust. Pre-release stance applies: rename freely, no compatibility shims.
- Skills each session should consult: grilling, domain-modeling; dsh-prose-standard
  for prose that may graduate into repo docs.
- Tracker: local-markdown under `.scratch/dsh-tui/` — this file is the map; tickets
  are one file each under `issues/`.

## Decisions so far

<!-- the index — one line per resolved ticket: gist + link to the ticket that holds the detail -->

- [Web UI feature inventory](issues/03-web-feature-inventory.md) — full inventory table: 33 modules, 12 chat-node kinds, settings surfaces, global affordances; parity baseline for 05/07/08. Settings RPCs are loopback-only; feedback is a CAS sidecar outside the session log.
- [Rust TUI ecosystem](issues/04-rust-tui-ecosystem.md) — stack locked: ratatui 0.30 (immediate-mode verdict: suitable, better than VDOM), termimad/streaming markdown hand-rolled, ratatui-image with Kitty>iTerm2>Sixel>Halfblocks fallback, unicode-width+segmentation, arboard, syntect+prettydiff, TestBackend+insta+portable-pty.
- [Typert → Rust codegen](issues/02-typert-rust-codegen.md) — decision: hand-maintained serde for frozen subset (b), not generator JSON-Schema emit; zod schemas stay source of truth, enforced by verify-type-equiv-style gate. Revisit (a) only if the wire surface grows.
- [Wire protocol surface](issues/01-wire-protocol-surface.md) — wire is custom 4-quadrant RPC (not JSON-RPC): 52 POST methods + `/api/respond`, 2 downlink-only WS (`events.mux`/`events.host`); loopback trust fence only — spawned local TUI passes as-is; chat-loop subset fully specified.
- [Rendering architecture](issues/05-rendering-architecture.md) — Q1–Q19 settled: SessionStore projection mirroring web; workspace browser + sidebar; tokio→mpsc, single main thread, coalesced draws; cached rows + dirty set; re-parse-per-chunk markdown; full-screen approval takeover; `/`+`@` menus + Ctrl+P launcher; vim-mode toggle (off default, Ctrl+J); jobs popover + queue dock + trajectory view-ring tab; paging at 200 rows, 5k-event cap, full re-render on resize.
- [Repo + toolchain integration](issues/06-repo-toolchain-integration.md) — code lives in **external repo `rbelem/dsh-tui`**, shipped as a bundle installed via `dsh plugin --profile tui add @rbelem/dsh-tui` (external-only surface; `dsh tui` alias later); TUI is a pure client — attach-or-boot (attach to running gateway, else boot gateway + spawn with DSH_PORT); resumes the web's active session mid-turn (wire-native); interchangeable with web (no client exclusivity); standalone crate, prebuilds linux/darwin + Windows source-build, cargo CI in the new repo.
- [Parity definition](issues/07-parity-definition.md) — Q1–Q11 settled; graduates into PARITY.md in rbelem/dsh-tui. Highlights: images inline w/ fallback tier + full-screen viewer (n/p/t/Esc, no zoom); drag-drop → file browser + paste-path; nothing hover-only (focused-row action bar); theme = terminal-following + ~6 bundled families with variants (catppuccin/kanagawa/tokyonight/gruvbox/dracula/solarized, ≈15 TOML files) + user themes dir; full settings view (two-pane, schema-driven forms); full zh/en bilingual; first-run onboarding modal.
- [Testing strategy](issues/08-testing-strategy.md) — three keyless levels: store-level replay of pinned session.jsonl fixtures, wire-level Rust mock gateway (real mux frames over WS), full e2e (mock LLM + real gateway + PTY, PARITY.md rows → scenarios); insta snapshots at 120×30 + 60×15; table-driven KeyMap→Action tests; portable-pty e2e crate; serde round-trip vs zod-schema JSON + one real-gateway smoke in CI.

## Not yet specified

<!-- fog: in-scope questions not yet sharp enough to ticket; graduates as the frontier advances -->

<!-- All fog graduated during the effort: i18n + theme + terminal matrix + browser-only equivalents → Parity definition (07); gateway auth → Wire protocol surface (01); streaming render parity → Rendering architecture (05). -->

## Out of scope

<!-- scoped out by the destination; never graduates -->

- Serving the UI over HTTP to remote browsers — `dsh web` keeps that surface.
- Non-terminal GUI clients (desktop/mobile apps).
