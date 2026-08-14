# Testing strategy

Type: grilling
Status: resolved
Blocked by: 05, 07

## Question

How is the TUI tested?

Decide:

- Whether the existing keyless snapshot replay infrastructure (`examples/*/snapshots/*.jsonl`,
  `pnpm run test:snapshot`) can drive the TUI headlessly as a fixture source.
- Terminal output testing: ratatui test backend snapshots vs pty-based e2e; what
  harness support is missing and must ship with the TUI (matching the repo rule that
  every capability seam plans unit + snapshot coverage).
- How the Parity definition acceptance table becomes automated acceptance tests.
- Protocol-level tests: does the TUI reuse or duplicate the SDK client tests?

## Answer

Settled in one grilling round (Q1–Q5).

- **Q1 (three test levels)**: (a) store-level — replay pinned `session.jsonl` fixtures (copied from the harness repo's `examples/*/snapshots/`, sync note tied to `SESSION_FORMAT_VERSION`) into the Rust `SessionStore`; (b) wire-level — a small Rust mock gateway replaying session.jsonl as real mux frames over WS (tests client + protocol, deterministic); (c) full e2e — mock LLM server + real gateway + TUI in a PTY, a small acceptance set mapping PARITY.md rows. All keyless.
- **Q2 (snapshots)**: insta snapshots on ratatui `TestBackend` at 120×30 canonical + 60×15 narrow set (reflow + CJK bugs live at narrow widths); streaming states snapshotted from frozen frame batches (no timing dependence).
- **Q3 (keymap)**: thin `KeyMap → Action` mapping layer with exhaustive table-driven state-delta tests (vim mode on/off, Ctrl+P, viewer keys, approvals, `/`+`@` menus) — keybinding bugs can't hide behind rendering.
- **Q4 (e2e harness)**: a `tests/e2e` crate — portable-pty + serial_test, scenario = keys → insta-snapshotted screen regions; CI linux (macOS optional); PARITY.md acceptance rows map 1:1 to scenarios.
- **Q5 (protocol + smoke)**: serde round-trip tests against JSON examples hand-built from the harness's zod schemas (source of truth per ticket 02), plus one real-gateway smoke test in CI (dsh installed from npm, mock-LLM-backed, one prompt→response assert) to catch wire drift the mock can't. Use the published `dsh-llm-mock-server` npm package if available; otherwise vendor the minimal mock into the TUI repo.
