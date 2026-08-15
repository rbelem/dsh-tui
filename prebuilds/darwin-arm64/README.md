# `@rbelem/dsh-tui-darwin-arm64`

The prebuilt dsh-tui binary for darwin-arm64 (macOS arm64), resolved as a file
path by the entry bundle (`@rbelem/dsh-tui`) — never imported. Selected by
npm through the `os`/`cpu` fields; the entry bundle lists this package as
an optional dependency (a missing or skipped install falls back to the
bundle's source-build `bin/dsh-tui`).

The binary is byte-pinned: `bin/SHA256SUMS` records the sha256 of
`bin/dsh-tui` at assemble time. No lifecycle install scripts (v1
invariant: installs never build or download anything).

## Not yet

- Published to npm (the release workflow assembles this package; the
  publish step is manual/CI TODO — see scripts/release/).
- Windows prebuilds: builds from source (`cargo build --release`).
