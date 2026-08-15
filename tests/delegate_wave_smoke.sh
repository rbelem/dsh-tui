#!/usr/bin/env bash
# delegate-wave smoke test — MANUAL, NOT wired into cargo test.
#
# Runs the real scripts/delegate-wave.sh against a stub `dsh-tui` in a live
# herdr session, then asserts the results.
#
# Requirements:
#   - A live herdr session (HERDR_ENV=1) — run this inside a herdr pane.
#   - The real `herdr` CLI on PATH.
#
# What it does:
#   1. Builds a stub `dsh-tui` (echoes the task text + the done sentinel,
#      like the real --light worker's last line) in a temp dir.
#   2. Runs delegate-wave.sh with 2 tasks (--file, --max 2).
#   3. Asserts: wave exit 0, summary reports 0: DONE and 1: DONE,
#      results/0.out and results/1.out contain the stub output.
#   4. Asserts the guards: a second run against the same results dir is
#      refused, and HERDR_ENV=0 is refused.
#
# WARNING: leaves 2 herdr panes behind (the wave never closes panes by
# design) — inspect with `herdr pane list` and close by hand afterwards.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WAVE="$ROOT/scripts/delegate-wave.sh"

if [ "${HERDR_ENV:-}" != "1" ]; then
  echo "delegate-wave-smoke: HERDR_ENV=1 required — run inside a herdr pane" >&2
  exit 1
fi
command -v herdr >/dev/null 2>&1 || {
  echo "delegate-wave-smoke: herdr not found on PATH" >&2
  exit 1
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Stub dsh-tui: finds the --file argument and echoes the task text, then
# the done sentinel (the worker contract's last line).
cat >"$WORK/dsh-tui" <<'EOF'
#!/bin/sh
file=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--file" ]; then file="$arg"; fi
  prev="$arg"
done
echo "stub worker for task: $(cat "$file")"
echo "dsh-worker: done"
EOF
chmod +x "$WORK/dsh-tui"

printf 'task one\n' >"$WORK/tasks.txt"
printf 'task two\n' >>"$WORK/tasks.txt"

export PATH="$WORK:$PATH"
RESULTS="$WORK/results"

echo "delegate-wave-smoke: running the wave (leaves 2 panes behind)"
set +e
WAVE_OUT="$("$WAVE" --file "$WORK/tasks.txt" --max 2 --results "$RESULTS" 2>&1)"
WAVE_STATUS=$?
set -e
printf '%s\n' "$WAVE_OUT"
if [ "$WAVE_STATUS" -ne 0 ]; then
  echo "delegate-wave-smoke: wave failed (exit $WAVE_STATUS)" >&2
  exit 1
fi

# The summary reported both tasks DONE.
printf '%s\n' "$WAVE_OUT" | grep -q "0: DONE" || {
  echo "delegate-wave-smoke: summary lacks '0: DONE'" >&2
  exit 1
}
printf '%s\n' "$WAVE_OUT" | grep -q "1: DONE" || {
  echo "delegate-wave-smoke: summary lacks '1: DONE'" >&2
  exit 1
}

# Both outputs landed, each with its task text and the sentinel.
[ -f "$RESULTS/0.out" ] || {
  echo "delegate-wave-smoke: missing $RESULTS/0.out" >&2
  exit 1
}
[ -f "$RESULTS/1.out" ] || {
  echo "delegate-wave-smoke: missing $RESULTS/1.out" >&2
  exit 1
}
grep -q "stub worker for task: task one" "$RESULTS/0.out" || {
  echo "delegate-wave-smoke: 0.out lacks task one: $(tr '\n' ' ' <"$RESULTS/0.out")" >&2
  exit 1
}
grep -q "stub worker for task: task two" "$RESULTS/1.out" || {
  echo "delegate-wave-smoke: 1.out lacks task two: $(tr '\n' ' ' <"$RESULTS/1.out")" >&2
  exit 1
}
grep -q "dsh-worker: done" "$RESULTS/0.out" || {
  echo "delegate-wave-smoke: 0.out lacks the done sentinel" >&2
  exit 1
}
grep -q "dsh-worker: done" "$RESULTS/1.out" || {
  echo "delegate-wave-smoke: 1.out lacks the done sentinel" >&2
  exit 1
}

# Double-run guard: a second wave on the same results dir is refused — the
# dir exists now, and the wave only ever creates fresh dirs.
set +e
GUARD_OUT="$("$WAVE" --file "$WORK/tasks.txt" --results "$RESULTS" 2>&1)"
GUARD_STATUS=$?
set -e
if [ "$GUARD_STATUS" -eq 0 ]; then
  echo "delegate-wave-smoke: double-run guard did not fire" >&2
  exit 1
fi
printf '%s\n' "$GUARD_OUT" | grep -q "already exists" || {
  echo "delegate-wave-smoke: guard message missing: $GUARD_OUT" >&2
  exit 1
}

# herdr-env guard: HERDR_ENV != 1 is refused.
if HERDR_ENV=0 "$WAVE" --file "$WORK/tasks.txt" --results "$WORK/other" >/dev/null 2>&1; then
  echo "delegate-wave-smoke: HERDR_ENV guard did not fire" >&2
  exit 1
fi

echo "delegate-wave-smoke: ok — 2/2 DONE, outputs in $RESULTS (panes left open for inspection)"
