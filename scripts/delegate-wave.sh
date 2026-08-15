#!/usr/bin/env bash
# delegate-wave: run a wave of dsh-tui --light workers, one herdr pane each.
#
# Usage:
#   delegate-wave.sh [--file <tasks.txt>] [--max N] [--timeout MS] [--results <dir>]
#
# Tasks arrive one per line via --file or piped on stdin (blank lines are
# skipped); --file wins when both are given. (There is no --task here — the
# worker's --task/--file/stdin precedence belongs to dsh-tui, not the wave.)
# Each task runs in its own freshly split sibling pane
# (`dsh-tui --light --file <tmpfile>`); the wave waits for the worker's
# sentinel (`dsh-worker: done` / `dsh-worker: error: <reason>`) and saves
# the pane output to <results>/<n>.out (n = task index, 0-based).
#
# Safety:
#   - Requires HERDR_ENV=1 (the herdr-skill rule): run inside a herdr pane.
#   - Every control uses --current / --no-focus / explicit pane ids.
#   - Panes are NEVER closed: they are left behind for inspection after the
#     wave (review with `herdr pane list`, close by hand with
#     `herdr pane close <pane-id>`).
#   - The script-owned per-run temp dir (mktemp under $TMPDIR/dsh-wave.*) is
#     removed on exit; a results dir that already exists is refused (the
#     wave is the only thing that creates it, so its presence means a prior
#     run — pick a fresh --results path).
set -euo pipefail

usage() {
  sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
}

# ---------------------------------------------------------------------------
# config
# ---------------------------------------------------------------------------

file=""
max=6
timeout_ms=300000
results_dir="results"

while [ $# -gt 0 ]; do
  case "$1" in
    --file) file="$2"; shift 2 ;;
    --max) max="$2"; shift 2 ;;
    --timeout) timeout_ms="$2"; shift 2 ;;
    --results) results_dir="$2"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *)
      echo "delegate-wave: unknown argument '$1'" >&2
      usage >&2
      exit 1
      ;;
  esac
done

# ---------------------------------------------------------------------------
# guards
# ---------------------------------------------------------------------------

if [ "${HERDR_ENV:-}" != "1" ]; then
  echo "delegate-wave: HERDR_ENV=1 required — run this inside a herdr pane (herdr-skill rule: verify HERDR_ENV before controlling a herdr session)." >&2
  exit 1
fi
case "$max" in
  '' | *[!0-9]*)
    echo "delegate-wave: --max must be a positive integer" >&2
    exit 1
    ;;
esac
[ "$max" -ge 1 ] || {
  echo "delegate-wave: --max must be >= 1" >&2
  exit 1
}
case "$timeout_ms" in
  '' | *[!0-9]*)
    echo "delegate-wave: --timeout must be a positive integer (milliseconds)" >&2
    exit 1
    ;;
esac

# Refuse to run twice against the same results dir: the wave is the only
# thing that creates it, so an existing dir means a prior run.
if [ -d "$results_dir" ]; then
  echo "delegate-wave: results dir '$results_dir' already exists — pick a fresh --results path" >&2
  exit 1
fi
mkdir -p "$results_dir"

# ---------------------------------------------------------------------------
# tasks (one per line; blank lines skipped)
# ---------------------------------------------------------------------------

# Per-run temp dir: concurrent waves never collide, and the EXIT/INT/TERM
# traps remove exactly this run's dir.
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/dsh-wave.XXXXXX")"
status_dir="$tmp_dir/status"
mkdir -p "$status_dir"
trap 'rm -rf "$tmp_dir"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

tasks=()
if [ -n "$file" ]; then
  while IFS= read -r line || [ -n "$line" ]; do
    [[ "$line" == *[![:space:]]* ]] && tasks+=("$line")
  done <"$file"
else
  if [ -t 0 ]; then
    echo "delegate-wave: no tasks — pass --file <tasks.txt> or pipe tasks on stdin" >&2
    usage >&2
    exit 1
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    [[ "$line" == *[![:space:]]* ]] && tasks+=("$line")
  done
fi
total="${#tasks[@]}"
if [ "$total" -eq 0 ]; then
  echo "delegate-wave: no tasks" >&2
  exit 1
fi

echo "delegate-wave: $total tasks, max $max concurrent panes, wait timeout ${timeout_ms}ms"
echo "delegate-wave: results -> $results_dir (panes are left open for inspection)"

# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

# The pane id from a `herdr pane split` response (`.result.pane.pane_id`):
# jq when available, otherwise a sed/grep fallback on the JSON line.
parse_pane_id() {
  local json="$1"
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "$json" | jq -r '.result.pane.pane_id // empty'
  else
    printf '%s' "$json" | sed -n 's/.*"pane_id":"\([^"]*\)".*/\1/p' | head -n 1
  fi
}

# Run one task in a fresh sibling pane and record the outcome in
# $status_dir/<n>.status: done | worker-error | timeout | launch. Runs in a
# background subshell: prints nothing and always writes the status file
# (`set +e` keeps the subshell alive on any herdr failure). The split passes
# the caller's PATH (and DSH_PORT, when set) explicitly — herdr panes get
# the server's environment, so the caller's exported vars would not reach
# the worker otherwise.
run_one() {
  set +e
  local index="$1" task_file="$2" direction="$3"
  local pane_id="" split_json status="launch"

  split_json="$(herdr pane split --current --direction "$direction" --cwd "$PWD" --env "PATH=$PATH" ${DSH_PORT:+"--env" "DSH_PORT=$DSH_PORT"} --no-focus 2>/dev/null)"
  [ -n "$split_json" ] && pane_id="$(parse_pane_id "$split_json")"
  if [ -n "$pane_id" ]; then
    if herdr pane run "$pane_id" "dsh-tui --light --file '$task_file'" >/dev/null 2>&1; then
      if herdr pane wait-output "$pane_id" --regex 'dsh-worker: (done|error)' --timeout "$timeout_ms" >/dev/null 2>&1; then
        status="done"
      else
        status="timeout"
      fi
    fi
    # Collect the pane output (also on timeout — the partial output helps).
    herdr pane read "$pane_id" --source recent-unwrapped --lines 200 >"$results_dir/$index.out" 2>/dev/null
    # The wait regex matches BOTH sentinels; classify by the collected output.
    if [ "$status" = "done" ]; then
      if grep -q 'dsh-worker: done' "$results_dir/$index.out" 2>/dev/null; then
        status="done"
      elif grep -q 'dsh-worker: error' "$results_dir/$index.out" 2>/dev/null; then
        status="worker-error"
      else
        status="no-sentinel"
      fi
    fi
  fi
  printf '%s\n' "$status" >"$status_dir/$index.status"
}

# ---------------------------------------------------------------------------
# wave (FIFO, capped at --max concurrent)
# ---------------------------------------------------------------------------

running=()
next=0
launched=0
direction_index=0
done_count=0
failed_count=0

while [ "$launched" -lt "$total" ] || [ "${#running[@]}" -gt 0 ]; do
  # Fill the concurrency window.
  while [ "${#running[@]}" -lt "$max" ] && [ "$next" -lt "$total" ]; do
    index="$next"
    task_file="$tmp_dir/$index.txt"
    printf '%s\n' "${tasks[$index]}" >"$task_file"
    # Alternate split directions per spawned pane (avoids repeated
    # same-direction degenerate columns).
    direction="right"
    [ $((direction_index % 2)) -eq 1 ] && direction="down"
    direction_index=$((direction_index + 1))
    run_one "$index" "$task_file" "$direction" &
    running+=("$index")
    next=$((next + 1))
    launched=$((launched + 1))
  done
  if [ "${#running[@]}" -gt 0 ]; then
    wait -n || true
  fi
  # Reap finished jobs (each wrote its status file before exiting).
  still=()
  for idx in "${running[@]}"; do
    if [ -f "$status_dir/$idx.status" ]; then
      status="$(cat "$status_dir/$idx.status" 2>/dev/null || echo launch)"
      case "$status" in
        done)
          done_count=$((done_count + 1))
          echo "$idx: DONE"
          ;;
        timeout)
          failed_count=$((failed_count + 1))
          echo "$idx: FAILED(timeout)"
          ;;
        worker-error)
          failed_count=$((failed_count + 1))
          echo "$idx: FAILED(worker error)"
          ;;
        launch)
          failed_count=$((failed_count + 1))
          echo "$idx: FAILED(launch)"
          ;;
        *)
          failed_count=$((failed_count + 1))
          echo "$idx: FAILED($status)"
          ;;
      esac
      rm -f "$status_dir/$idx.status"
    else
      still+=("$idx")
    fi
  done
  running=("${still[@]}")
done

echo "delegate-wave: $done_count done, $failed_count failed — panes left open for inspection (close with: herdr pane close <pane-id>)"
if [ "$failed_count" -gt 0 ]; then
  exit 1
fi
exit 0
