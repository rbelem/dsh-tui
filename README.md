# dsh-tui

A Rust terminal client for the [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)
gateway (`dsh`) — a terminal surface at parity with its web UI. It
attaches to the gateway over the wire protocol (RPC + host frames):
browse workspaces and sessions, chat with the agent, cancel or retry
turns, rename/fork/archive sessions, create sessions, and switch themes
or UI locale.

The gateway lifecycle follows the herdr model: if nothing is listening on
the resolved port at launch, dsh-tui starts `dsh web` itself in the
background (it keeps running after the TUI exits) and stops it only via
`dsh-tui server stop`.

Version 0.1.0. ~36 commits, 293 tests.

## Install

### npm (entry bundle + platform prebuilds)

The published packages:

| Package | Contents |
|---|---|
| `@rbelem/dsh-tui` | entry bundle: cordis patch layer + runtime glue plugin that spawns the TUI with `DSH_PORT` |
| `@rbelem/dsh-tui-linux-x64` / `-linux-arm64` | prebuilt binary for linux x64 / arm64 |
| `@rbelem/dsh-tui-darwin-x64` / `-darwin-arm64` | prebuilt binary for macOS x64 / arm64 |

The bundle pins the four platform packages as exact-version
`optionalDependencies`; npm picks the matching one by its `os`/`cpu` fields.
Install through the harness plugin mechanism:

```sh
dsh plugin --profile tui add @rbelem/dsh-tui
```

Boot (gateway + TUI) or attach-only:

```sh
dsh --profile tui              # boots the gateway (OS-assigned port) and spawns the TUI
dsh --profile tui --port 8080  # fixed port
dsh --profile tui --no-spawn   # gateway only; attach later with the binary + DSH_PORT
dsh-tui --port <port>          # attach to any running gateway
```

See `bundle/README.md` for the full bundle contract.

### From source

Prerequisites: a Rust toolchain (`rustup`) and [devbox](https://www.jetify.com/devbox)
(`devbox.json` pins `rustup@latest`).

```sh
git clone <repo> dsh-tui
cd dsh-tui
devbox run -- cargo build --release
# binary at target/release/dsh-tui
```

### Gateway: auto-start by default

`dsh-tui` attaches to a `dsh web` gateway from the deepseek-harness repo.
By default it boots the gateway itself when nothing is listening: the
resolved port probes at launch, and a dead port spawns `dsh web` detached
(stdout+stderr → `$XDG_STATE_HOME/dsh-tui/gateway.log`). The gateway
persists after the TUI exits — stop it explicitly with `dsh-tui server
stop`. The gateway boots **without a provider key** (browse/attach/list
all work); submitting prompts requires a provider configured in the
environment — without one, a run fails with a turn error surfaced in the
UI (no crash).

The port resolves CLI > env > config > default **3080** (the dsh web
profile's composed default):

```sh
dsh-tui --port 4000              # CLI wins
DSH_PORT=4000 dsh-tui            # env
# config.toml: [gateway] port = 4000
dsh-tui                          # default 3080, nothing to set
```

A manually started gateway works the same way — it is detected by the
probe and attached to as-is:

```sh
# terminal 1: the gateway
dsh web --port 8765

# terminal 2: the TUI
dsh-tui --port 8765
```

To disable auto-start (keep the pure manual flow), set
`[gateway] auto_start = false` in `~/.config/dsh-tui/config.toml` —
a dead port then errors with the "no gateway reachable" message.

## Usage

With no sessions, a hero screen invites a new session. Once attached, the
layout is:

- **Sidebar** — workspace groups, an ungrouped group for sessions no
  workspace claims, and a collapsed `archived (N)` header at the foot
  (archived sessions are excluded from navigation in v1).
- **Chat panel** — the active session's history: user messages, assistant
  responses (markdown, reasoning, images where the terminal supports them),
  tool activity, approvals, and queue items.
- **Composer** — the prompt input at the bottom; `Enter` submits, `Shift+Enter`
  inserts a newline, `/` and `@` open command/skill completion popups.

The TUI attaches to the most recently updated non-blank session and streams
new events over the mux downlink.

## Keymap

| Keys | Action |
|---|---|
| `j`/`k`, `↑`/`↓` | scroll chat / move sidebar selection / move picker selection |
| `g`/`Home`, `G`/`End` | jump to top / bottom of the chat |
| `Ctrl+d` / `Ctrl+u` | scroll half a page (chat) |
| `Ctrl+d` | quit from the composer (EOF) |
| `Enter` | submit composer; switch to the selected sidebar session; apply picker selection |
| `Tab` | cycle focus: chat → composer → sidebar |
| `Ctrl+w` then `h`/`j`/`k`/`l` | move focus between panes (sidebar / chat / composer) |
| `Esc` | return to chat (closes popups, pickers, editors) |
| `n` | new-session picker (chat or sidebar focus; `j`/`k` move, `Enter` create) |
| `r` | rename the selected sidebar session (inline editor: type, `Enter` commit, `Esc` cancel) |
| `f` | fork the selected sidebar session |
| `a` | archive the selected sidebar session |
| `v` | arm mouse selection mode in the chat (`v select · esc cancel`; drag to select, release to copy) |
| `i` | open the image viewer on the session's images (chat focus) |
| `t` | toggle the tool details line (started/duration/schema) of the tool row in view (chat focus) |
| `s` | toggle the narrow-terminal session drawer (below 80 columns) |
| `q` | quit (chat or sidebar focus) |
| `Ctrl+p` | launcher: fuzzy search over commands, cached skills, and settings actions |
| `Ctrl+t` | theme picker (`j`/`k` move, `Enter` apply, `Esc` close) |
| `Ctrl+,` | settings view (note: unreachable from a raw terminal byte stream — crossterm maps `0x0c` to `Ctrl+l`; use the launcher's "open settings" action) |
| `Ctrl+l` | cycle UI locale (en ↔ zh), persisted |
| `Ctrl+c` | cancel the running turn; quit when idle |
| `Ctrl+q` | quit |
| `Alt+q` | queue popup: `j`/`k` scroll, `x` remove, `s` steer, `e` edit, `Esc` close |
| `Shift+Enter` | insert a newline in the composer (see composer notes below) |

Composer editing: arrows / `Home` / `End` move the caret, `Backspace` /
`Delete` edit, `Esc` returns to the chat. `Shift+Enter` inserts a newline
(web parity; requires a CSI-u / kitty-keyboard-protocol terminal —
kitty, WezTerm, Alacritty ≥0.13, foot, Ghostty, Windows Terminal ≥1.19.
On legacy terminals Shift+Enter arrives as plain `Enter` and submits
instead — graceful degradation, nothing breaks).

## Mouse

Mouse capture is on (click-to-select sessions, wheel scrolls 3 lines per
tick, status indicators). In the chat, `v` arms selection mode:

- `v`, then drag: select text; release copies it to the clipboard
  (OSC 52) and exits the mode; `Esc` cancels. The status line shows a
  `copied · N chars` flash on success.
- double-click a word: selects the word (CJK runs stay whole); dragging
  after extends the selection from the word.
- wheel while selecting scrolls the viewport — the selection stays
  anchored to the text underneath.
- clicking a `▸ N skills` header row expands or collapses the folded
  skill list in that message (a header click never starts a selection).

The chat's margins (the 2/2 padding) anchor at the clamped edge, so a
drag always has a starting point. Below 80 columns, `s` opens the session
drawer (full titles; `Esc`/click-outside closes); the `≡` affordance at
the chat's top-left toggles it.

**Terminal escape hatch**: while mouse capture is active, holding `Shift`
while dragging or wheeling bypasses the app's capture — the terminal's
own selection and scrolling take over (standard `xterm`/kitty behavior;
the app never sees those events). Use that when you want the terminal's
native copy instead of dsh-tui's.

## Configuration

- **Settings view** — opened from the launcher (Ctrl+P → "open settings").
  Driven by the gateway's `settings.describe`/`settings.update`; the live
  gateway exposes namespaces including `ui-theme`, `locale`,
  `ui-conversation`, and `ui-onboarding`, rendered as schema-driven forms.
- **Themes** — 15 bundled themes (catppuccin ×4, kanagawa, tokyonight ×3,
  gruvbox, dracula, solarized, nord, rose-pine, everforest, one-dark);
  Ctrl+T opens the picker, `Enter` applies and persists. User themes load
  from `~/.config/dsh-tui/themes/*.toml`. With no explicit theme, the
  default follows the detected terminal/system scheme — catppuccin frappe
  (dark) / catppuccin latte (light) on truecolor terminals, falling back
  to the terminal-following neutral look (truecolor when `COLORTERM` is
  set, else 256-color) when detection fails.
- **Locale** — zh/en, keyed string tables; Ctrl+L cycles and persists;
  CJK width is handled.
- **Config file** — `~/.config/dsh-tui/` (isolated from the host config in
  tests via `XDG_CONFIG_HOME`). A `[keymap]` section rebinds shortcuts by
  action name (see the table above for the defaults); key specs look like
  `"ctrl+q"`, `"shift+enter"`, `"alt+q"`, `"g"`. An absent or unparseable
  spec falls back to the built-in default; the config applies at startup.

## Development

Toolchain: devbox (`devbox.json` — `rustup@latest`).

```sh
devbox run -- cargo test        # 293 tests
devbox run -- cargo check --all-targets
devbox run -- cargo clippy --all-targets -- -D warnings
devbox run -- cargo fmt --check
```

### Live-gateway smoke test

`tests/live_smoke.rs` runs the real binary in a PTY against a REAL `dsh web`
gateway that the test starts itself (isolated `DSH_HOME`, port 18765 or a
free port) and kills on teardown. Gated by an env var so the default suite
stays green without external infra:

```sh
DSH_LIVE_SMOKE=1 devbox run -- cargo test --test live_smoke -- --nocapture --test-threads=1
```

The smoke is provider-adaptive: it probes `session.models.routable` — with a
provider it asserts the prompt round-trip and streamed answer; without one
it asserts the graceful turn-error surface. Attach, sidebar, nav, settings,
theme, and catalog flows run either way.

## Release

The release pipeline (`scripts/release/`, `.github/workflows/release.yml`):

1. `build.sh --target <triple>` builds one target into
   `dist/dsh-tui-<target>/` (binary + `SHA256SUMS`, byte-pinned) — darwin
   targets build on macOS runners.
2. `prebuild-packages.sh` assembles the four `prebuilds/<platform>/`
   packages from the dist artifacts.
3. The `v*` tag workflow uploads each target's assets to the GitHub release
   under target-unique names: `dsh-tui-<target>` and
   `dsh-tui-<target>.SHA256SUMS` (verify with `sha256sum -c` after renaming
   the binary to the listed name `dsh-tui`).

npm publishing is manual (`workflow_dispatch`, `.github/workflows/publish.yml`),
reusing the same artifacts — no rebuild:

```sh
bash scripts/release/publish.sh            # dry-run: pack + publish --dry-run for all 5
bash scripts/release/publish.sh --publish  # real publish (requires npm auth)
```

Platform packages publish first, the entry bundle last; the script checks
`npm whoami` and refuses to publish unauthenticated, and fail-fasts with a
report of which packages landed.

## Parity

See [PARITY.md](PARITY.md) for the feature-by-feature parity contract with
the web UI (acceptance bars + testing hooks), and
`.scratch/dsh-tui/issues/` for the decision records.
