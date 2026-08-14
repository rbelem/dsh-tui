#!/usr/bin/env bash
# Keyless smoke for the dsh-tui bundle: install it into a temp profile with
# `dsh plugin --profile tui add` and boot-check with `--help` (which starts
# no server). Skipped gracefully when dsh is missing.
set -euo pipefail

BUNDLE_DIR="$(cd "$(dirname "$0")/.." && pwd)/bundle"

if ! command -v dsh >/dev/null 2>&1; then
  echo "smoke-install: dsh not found on PATH — skipping (install dsh and re-run for the real smoke)"
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export DSH_HOME="$WORK"

echo "smoke-install: installing $BUNDLE_DIR into a temp profile"
dsh plugin --profile tui add "$BUNDLE_DIR"

echo "smoke-install: boot-check (--help starts no server)"
timeout 60 dsh --profile tui --help

echo "smoke-install: ok"
