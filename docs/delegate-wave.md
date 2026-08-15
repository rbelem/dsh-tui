# delegate-wave

A bash driver that runs a wave of cheap `dsh-tui --light` workers against a
dsh gateway — one fresh herdr pane and one gateway session per task, FIFO,
capped at a concurrency window.

The orchestrator (you, or another agent) supplies a list of tasks; the wave
splits sibling panes, launches `dsh-tui --light --file <task>` in each, waits
for the worker's sentinel line, and saves each pane's output for later
review. Workers report their lifecycle to herdr, so they appear as
`dsh-worker` agents in `herdr agent list` while the wave runs.

## Prerequisites

- **A running herdr session** — the script refuses to run unless
  `HERDR_ENV=1` (the herdr-skill rule: verify the environment before
  controlling a herdr session). Run it from inside a herdr pane.
- **A dsh gateway reachable via `DSH_PORT`** — the worker connects to
  `127.0.0.1:$DSH_PORT` (attach mode; the gateway is never started by
  anything here). The wave passes the caller's `DSH_PORT` into the worker
  panes when it is set.
- **`dsh-tui` in PATH** — the worker command runs in the spawned panes; the
  wave forwards the caller's `PATH` explicitly (herdr panes get the server's
  environment, so a `dsh-tui` installed only in your shell profile would not
  be found otherwise). `dsh-tui` must be built with the `--light` worker
  (`cargo build`).

## Usage

```
scripts/delegate-wave.sh [--file <tasks.txt>] [--max N] [--timeout MS] [--results <dir>]
```

- `--file <path>` — one task per line (blank lines skipped). Without
  `--file`, tasks are read from piped stdin (same one-per-line format).
- `--max N` — concurrent panes (default `6`).
- `--timeout MS` — per-task wait for the worker sentinel (default
  `300000` = 5 minutes).
- `--results <dir>` — where `<n>.out` files land (default `results`; task
  `n` = 0-based task index). A results dir that already contains `.out`
  files is refused — pick a fresh dir or move the old ones.

Examples:

```bash
# Piped tasks, defaults (max 6 concurrent).
printf 'explain the wire protocol\ndraft a commit message\n' | scripts/delegate-wave.sh

# A task file, 3 at a time, longer waits.
scripts/delegate-wave.sh --file tasks.txt --max 3 --timeout 600000

# Keep results somewhere specific.
scripts/delegate-wave.sh --file tasks.txt --results wave-2026-08-15
```

Per-task status lines print as jobs finish (`0: DONE`, `1: FAILED(timeout)`,
…), then a summary with the totals. The exit code is 0 when every task
succeeded and 1 when any failed or timed out.

## The worker contract it depends on

`delegate-wave.sh` drives the `--light` worker (`src/app/light.rs`), which
implements:

- **Transport**: `dsh-tui --light --file <path>` reads the task verbatim
  from the file (quotes and newlines survive). `--task TEXT` and piped
  stdin work too, but the wave always uses `--file`.
- **One session per worker**: each invocation creates a fresh gateway
  session, submits the task, and streams the assistant text to stdout.
- **Sentinel line**: the last line of stdout is `dsh-worker: done` on
  success or `dsh-worker: error: <reason>` on failure. The wave waits for
  `dsh-worker: (done|error)` and then classifies the task from the captured
  output.
- **Exit codes**: 0 success · 1 RPC/stream error · 2 usage / no gateway.
- **herdr lifecycle reporting**: when running inside a herdr pane
  (`HERDR_PANE_ID` set and a herdr binary reachable), the worker reports
  `working` → `idle` (success) or `blocked` (failure) through
  `herdr pane report-agent`, so the wave's workers show up as `dsh-worker`
  agents in `herdr agent list`. `herdr agent wait` / `herdr agent read` /
  `herdr pane read` all work against them while they are alive.

## What the wave leaves behind

Panes are **never closed** — by design. After the wave, each worker's pane
still shows its output (and the worker's exit state). Inspect with:

```bash
herdr pane list
herdr pane read <pane-id> --source recent-unwrapped --lines 200
```

and close by hand once reviewed:

```bash
herdr pane close <pane-id>
```

The script-owned temp dir (`$TMPDIR/dsh-wave`) is removed on exit
(EXIT/INT/TERM traps); only the panes and the results files remain.

## Troubleshooting

- **`FAILED(timeout)`** — the worker never printed its sentinel within
  `--timeout`. Usually a slow gateway or a long model delay: raise
  `--timeout` and re-run. The partial pane output is still captured in
  `results/<n>.out`.
- **`FAILED(worker error)`** — the worker finished with the
  `dsh-worker: error: <reason>` sentinel (e.g. the gateway rejected the
  prompt). The reason is in `results/<n>.out`.
- **`FAILED(launch)`** — the pane split or the worker launch failed (herdr
  error, or `dsh-tui` not found in the pane's PATH — see the PATH note in
  Prerequisites).
- **No gateway** — the worker exits 2 with the
  `no DSH_PORT set — attach to a running gateway` message; the wave reports
  `FAILED(worker error)`. Start the gateway (or set `DSH_PORT`) first.
- **Ctrl+C mid-wave** — the temp dir is cleaned; already-spawned panes are
  left behind. Inspect them with `herdr pane list` and close by hand.
- **"results dir already contains .out files"** — the double-run guard: use
  a fresh `--results` dir or move the previous wave's output.
